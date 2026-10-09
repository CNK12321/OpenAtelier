//! The timeline mixer: every audible clip at the right moment, summed, with per-clip
//! keyframable gain and silence in the gaps (DESIGN.md §12).
//!
//! * Positions are counted in **sample frames**, not flicks, so long playback never
//!   drifts and clip boundaries land on exact samples.
//! * Each clip has its own decoder (keyed by clip id), so two clips of the same file —
//!   or a clip overlapping itself after a split — never fight over one read position.
//! * The clip list can be **replaced while playing** through a [`MixHandle`]: decoders
//!   of clips that survive an edit carry on, and only a real jump in source position
//!   (a trim, a slip, a move under the playhead) costs a re-seek. Gain changes apply at
//!   the next block.
//! * Gain is evaluated every [`GAIN_STEP`] frames and ramped linearly between, so fades
//!   are smooth and cheap.

use crate::fx::{self, AudioEffect, Processor};
use crate::{AudioError, AudioFormat, AudioSource, FfmpegAudioSource};
use oa_params::{EvalContext, ParamSource, Value};
use oa_time::{Time, TimeRange, FLICKS_PER_SECOND};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One audible clip on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClip {
    /// The timeline item's id: its decoder follows it across edits.
    pub id: u64,
    pub media: u64,
    pub path: PathBuf,
    pub range: TimeRange,
    /// Where in the file the clip starts.
    pub source_in: Time,
    /// Clip gain in decibels (keyframable; evaluated on the clip's clocks).
    pub gain_db: ParamSource,
    /// Linear ramps from silence at the start and to silence at the end of `range`
    /// (transitions: both sides of a cross dissolve overlap and ramp).
    pub fade_in: Time,
    pub fade_out: Time,
    /// Where the clip's own clock starts: its timeline start before a transition
    /// extended `range`. Gain keyframes are evaluated against it.
    pub clock_start: Time,
    /// The clip's own length on the timeline (before a transition extended `range`):
    /// intro and outro effects run over its first and last seconds.
    pub length: Time,
    /// Sound effects, in order, applied to the decoded samples before the gain.
    pub effects: Vec<AudioEffect>,
    /// Playback speed (> 0): 2 plays twice as fast. `range` is timeline time, so the
    /// clip covers `range.duration × speed` of the file.
    pub speed: f64,
    /// At speed ≠ 1, keep the original pitch instead of letting it follow the speed.
    pub keep_pitch: bool,
    /// Its track's place in the stack: an effect track's [`AudioBus`] runs over every clip
    /// on a lower layer.
    pub layer: u32,
    /// Played backwards: the place in the file the clip starts at, from which it plays
    /// down. `source_in` and `speed` then count forwards from there in reversed time
    /// (the file read through [`Reversed`]), so everything else — speed, keep pitch,
    /// effects, transition handles — works on it as on any clip.
    pub reverse: Option<Time>,
    /// How fast its own clock runs against the timeline's: a clip inside a compound clip
    /// played at 2× reaches its keyframes twice as soon.
    pub clock_rate: f64,
    /// The compound clips it's inside (innermost first): their volume and fades apply
    /// to it too.
    pub outer: Vec<OuterGain>,
    /// A speed ramp: where in the file (past `source_in`) the clip is at each moment. `speed`
    /// is then ignored; each block plays at the ramp's average speed across it.
    pub ramp: Option<Arc<SpeedRamp>>,
}

/// A speed ramp's path through a file, sampled: seconds of file past the clip's in point
/// at every `step` seconds of timeline time from its start.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeedRamp {
    pub step: f64,
    pub offsets: Vec<f64>,
}

impl SpeedRamp {
    /// Samples `offset` (timeline seconds from the clip's start → file seconds past its in
    /// point) every 10 ms over `duration` seconds.
    pub fn sample(duration: f64, offset: impl Fn(f64) -> f64) -> SpeedRamp {
        let step = 0.01;
        let n = (duration.max(0.0) / step).ceil() as usize + 2;
        SpeedRamp { step, offsets: (0..n).map(|i| offset(i as f64 * step)).collect() }
    }

    /// File seconds past the in point at `local` timeline seconds (straight lines between
    /// samples; past the ends, on at the end's speed).
    pub fn offset_at(&self, local: f64) -> f64 {
        let n = self.offsets.len();
        if n < 2 {
            return self.offsets.first().copied().unwrap_or(0.0) + local;
        }
        let x = local / self.step;
        let i = (x.floor().max(0.0) as usize).min(n - 2);
        let (a, b) = (self.offsets[i], self.offsets[i + 1]);
        a + (b - a) * (x - i as f64)
    }
}

/// A compound clip's say over the sound inside it: its volume (keyframable, on its own
/// clock) and the fades at its ends.
#[derive(Clone, Debug, PartialEq)]
pub struct OuterGain {
    pub gain_db: ParamSource,
    /// Where its clock starts on the timeline, and how fast it runs.
    pub clock_start: Time,
    pub clock_rate: f64,
    /// Where it plays on the timeline, and its fades there.
    pub range: TimeRange,
    pub fade_in: Time,
    pub fade_out: Time,
}

impl OuterGain {
    /// Its gain (dB) and fade at timeline time `t`.
    fn at(&self, t: Time) -> (f64, f64) {
        let clock = Time::from_seconds_f64((t - self.clock_start).as_seconds_f64() * self.clock_rate);
        let db = self.gain_db.eval(&EvalContext::at(clock, clock)).as_float().unwrap_or(0.0);
        (db, fade(t, self.range, self.fade_in, self.fade_out))
    }
}

/// A linear ramp up over `fade_in` from `range`'s start and down over `fade_out` to its end.
fn fade(t: Time, range: TimeRange, fade_in: Time, fade_out: Time) -> f64 {
    let ramp = |into: Time, len: Time| if len > Time::ZERO { (into.0 as f64 / len.0 as f64).clamp(0.0, 1.0) } else { 1.0 };
    ramp(t - range.start, fade_in) * ramp(range.end() - t, fade_out)
}

/// A file played backwards from `pivot`: reversed time `r` is the file at `pivot − r`.
/// Decoded forward a block at a time just below where it's playing, each block turned
/// round — so reading on goes back through the file.
pub struct Reversed {
    inner: Box<dyn AudioSource>,
    format: AudioFormat,
    pivot: i64,
    /// File frame the next block ends at (it plays down from just below here).
    at: i64,
    /// The current block, reversed: read from the front.
    block: Vec<f32>,
    used: usize,
}

/// Frames decoded at a time when playing backwards (a seek each).
const REVERSE_BLOCK_SECONDS: f64 = 1.0;

impl Reversed {
    pub fn new(inner: Box<dyn AudioSource>, pivot: Time) -> Self {
        let format = inner.format();
        let pivot = (pivot.as_seconds_f64() * format.sample_rate as f64).round() as i64;
        Reversed { inner, format, pivot, at: pivot, block: Vec::new(), used: 0 }
    }

    /// The next block below `at`, decoded and turned round. False at the file's start.
    fn fill(&mut self) -> bool {
        let ch = self.format.channels as usize;
        if self.at <= 0 {
            return false;
        }
        let from = (self.at - (REVERSE_BLOCK_SECONDS * self.format.sample_rate as f64) as i64).max(0);
        let frames = (self.at - from) as usize;
        if self.inner.seek(Time::from_seconds_f64(from as f64 / self.format.sample_rate as f64)).is_err() {
            return false;
        }
        let mut buf = vec![0.0f32; frames * ch];
        let mut got = 0;
        while got < buf.len() {
            let r = self.inner.read(&mut buf[got..]);
            if r == 0 {
                break;
            }
            got += r;
        }
        // Past the file's end (a handle beyond it): silence there.
        buf[got..].fill(0.0);
        self.block.clear();
        for frame in buf.chunks(ch).rev() {
            self.block.extend_from_slice(frame);
        }
        self.used = 0;
        self.at = from;
        true
    }
}

impl AudioSource for Reversed {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let mut written = 0;
        while written < out.len() {
            if self.used == self.block.len() && !self.fill() {
                break;
            }
            let n = (self.block.len() - self.used).min(out.len() - written);
            out[written..written + n].copy_from_slice(&self.block[self.used..self.used + n]);
            self.used += n;
            written += n;
        }
        written
    }

    fn seek(&mut self, t: Time) -> Result<(), AudioError> {
        self.at = self.pivot - (t.as_seconds_f64() * self.format.sample_rate as f64).round() as i64;
        self.block.clear();
        self.used = 0;
        Ok(())
    }

    fn finished(&self) -> bool {
        self.at <= 0 && self.used == self.block.len()
    }
}

impl AudioClip {
    pub fn new(id: u64, media: u64, path: PathBuf, range: TimeRange, source_in: Time) -> Self {
        AudioClip {
            id,
            media,
            path,
            source_in,
            gain_db: ParamSource::Static(Value::Float(0.0)),
            fade_in: Time::ZERO,
            fade_out: Time::ZERO,
            clock_start: range.start,
            length: range.duration,
            effects: Vec::new(),
            speed: 1.0,
            keep_pitch: false,
            layer: 0,
            reverse: None,
            clock_rate: 1.0,
            outer: Vec::new(),
            ramp: None,
            range,
        }
    }

    /// Where its own clock is at timeline time `t`.
    fn clip_time(&self, t: Time) -> Time {
        let d = t - self.clock_start;
        if self.clock_rate == 1.0 { d } else { Time::from_seconds_f64(d.as_seconds_f64() * self.clock_rate) }
    }

    /// Linear amplitude at timeline time `t`: its own volume and fades, and those of the
    /// compound clips it's inside.
    pub(crate) fn amplitude(&self, t: Time) -> f32 {
        let source_time = self.source_at(t);
        let mut db = self.gain_db.eval(&EvalContext::at(self.clip_time(t), source_time)).as_float().unwrap_or(0.0);
        let mut level = fade(t, self.range, self.fade_in, self.fade_out);
        for o in &self.outer {
            let (d, f) = o.at(t);
            db += d;
            level *= f;
        }
        db_to_amplitude(db) * level as f32
    }

    /// Where in the file timeline time `t` falls.
    /// Its speed where the file is at `source` (for positioning effects that look ahead):
    /// the steady speed, or near enough on a ramp (the ramp's at the clip's start).
    fn speed_near(&self, _source: Time) -> f64 {
        match &self.ramp {
            Some(r) => ((r.offset_at(r.step) - r.offset_at(0.0)) / r.step).abs().max(1e-6),
            None => self.speed,
        }
    }

    pub fn source_at(&self, t: Time) -> Time {
        let local = (t - self.range.start).as_seconds_f64();
        match &self.ramp {
            Some(r) => self.source_in + Time::from_seconds_f64(r.offset_at(local)),
            None => self.source_in + Time::from_seconds_f64(local * self.speed),
        }
    }

    /// The clip's clocks at timeline time `t` (for keyframed effect params).
    fn context(&self, t: Time) -> EvalContext {
        EvalContext::at(self.clip_time(t), self.source_at(t))
    }
}

/// A clip's running sound effects, in order.
struct Chain {
    fx: Vec<(u64, String, Box<dyn Processor>)>,
}

impl Chain {
    fn matches(&self, effects: &[AudioEffect]) -> bool {
        self.fx.len() == effects.len() && self.fx.iter().zip(effects).all(|(a, b)| a.0 == b.id && a.1 == b.type_id)
    }

    /// The chain for `effects`, keeping processors (and their state) that carry over.
    fn rebuild(mut self, effects: &[AudioEffect], format: AudioFormat) -> Chain {
        let fx = effects
            .iter()
            .filter_map(|e| {
                let kept = self.fx.iter().position(|f| f.0 == e.id && f.1 == e.type_id).map(|i| self.fx.swap_remove(i).2);
                let p = kept.or_else(|| fx::processor(&e.type_id, format.channels as usize, format.sample_rate as f32))?;
                Some((e.id, e.type_id.clone(), p))
            })
            .collect();
        Chain { fx }
    }

    fn latency(&self) -> usize {
        self.fx.iter().map(|f| f.2.latency()).sum()
    }

    fn reset(&mut self) {
        self.fx.iter_mut().for_each(|f| f.2.reset());
    }

    /// Runs the chain over `buf`, which starts at timeline time `t` in `owner` (a clip,
    /// or an effect track's container): each effect with its params evaluated there and
    /// its clock (an intro or outro only runs inside its window, like a picture effect).
    /// Every effect's levels go to its live meter.
    fn process(&mut self, owner: &Owner<'_>, buf: &mut [f32], t: Time, format: AudioFormat) {
        let ch = format.channels.max(1) as usize;
        let frames = buf.len() / ch;
        let end = t + Time::from_seconds_f64(frames as f64 / format.sample_rate as f64);
        let (local, local_end) = (t - owner.clock_start, end - owner.clock_start);
        let mut before = Vec::new();
        for (id, type_id, p) in &mut self.fx {
            let Some(e) = owner.effects.iter().find(|e| e.id == *id) else { continue };
            let Some(info) = fx::info(type_id) else { continue };
            let Some(start) = e.role.clock(local, owner.length) else { continue };
            // Where it ends up by the block's end (held at the window's edge).
            let stop = e.role.clock(local_end, owner.length).unwrap_or(match e.role {
                oa_doc::EffectRole::In { .. } => oa_doc::EffectClock { visibility: 1.0, progress: 1.0, ..start },
                _ => start,
            });
            let mut values = e.params.eval(&info.params, None, &owner.context);
            // An effect for the whole clip with an off state, as an intro or an outro:
            // eased between full and off as the clip comes and goes.
            if e.role != oa_doc::EffectRole::Passive && info.usage == fx::FxUsage::Passive {
                fx::eased(&info.off, &mut values, 1.0 - start.visibility);
            }
            before.clear();
            before.extend_from_slice(buf);
            p.process(buf, &values, &fx::ClockSpan { start, end: stop });
            crate::dynamics::publish(*id, &before, buf, ch, p.reduction_db());
        }
    }
}

/// What a chain runs for: its effects, the clocks they run on, and where params are
/// evaluated.
struct Owner<'a> {
    effects: &'a [AudioEffect],
    clock_start: Time,
    length: Time,
    context: EvalContext,
}

impl AudioClip {
    fn owner(&self, t: Time) -> Owner<'_> {
        Owner { effects: &self.effects, clock_start: self.clock_start, length: self.length, context: self.context(t) }
    }
}

/// Effects over a mix of clips, while they last — and ringing on for their tails after.
/// Either an effect track's container (`members: None`: everything on the layers below
/// it, every clip whose `layer` is lower) or a compound clip's own sound effects
/// (`members`: just its clips, mixed apart, run through its effects, then added in at
/// its layer).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBus {
    /// The container's id: its effects' state follows it across edits.
    pub id: u64,
    pub range: TimeRange,
    pub effects: Vec<AudioEffect>,
    pub layer: u32,
    /// The clips (by id) it runs over, when it's a compound clip's.
    pub members: Option<Vec<u64>>,
}

impl AudioBus {
    fn owner(&self, t: Time) -> Owner<'_> {
        let local = t - self.range.start;
        Owner { effects: &self.effects, clock_start: self.range.start, length: self.range.duration, context: EvalContext::at(local, local) }
    }
}

/// How long `effects` keep sounding after their input stops, evaluated at the end of
/// their owner (`context`): the longest tail among them.
fn tail_of(effects: &[AudioEffect], context: &EvalContext) -> Time {
    let seconds = effects
        .iter()
        .filter_map(|e| {
            let info = fx::info(&e.type_id)?;
            Some(fx::tail_seconds(&e.type_id, &e.params.eval(&info.params, None, context)))
        })
        .fold(0.0, f64::max);
    Time::from_seconds_f64(seconds)
}

/// Anything at or below this is silence.
pub const SILENCE_DB: f64 = -60.0;

pub fn db_to_amplitude(db: f64) -> f32 {
    if db <= SILENCE_DB { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

/// Frames between gain evaluations (~1.3 ms at 48 kHz).
pub const GAIN_STEP: usize = 64;

/// Re-seek a decoder when it's further than this from where its clip needs it.
const DRIFT_SECONDS: f64 = 0.03;

/// Open decoders for clips starting within this much of the read position, so the
/// process is warm by the time the clip plays.
const WARM_SECONDS: f64 = 0.5;

/// What the mixer plays: the clips and where the sequence ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixState {
    pub clips: Vec<AudioClip>,
    /// Effect tracks' containers, run over the layers below them.
    pub buses: Vec<AudioBus>,
    /// The sequence's duration: silence runs out to here, so the clock keeps time over
    /// stretches with no sound.
    pub end: Time,
}

/// Lets the UI swap in a new clip list while the mixer plays on another thread.
#[derive(Clone, Default)]
pub struct MixHandle {
    inner: Arc<Mutex<(u64, Arc<MixState>)>>,
}

impl MixHandle {
    pub fn new(state: MixState) -> Self {
        MixHandle { inner: Arc::new(Mutex::new((1, Arc::new(state)))) }
    }

    /// Publishes a new state; the mixer picks it up at its next block. No-op if equal.
    pub fn set(&self, state: MixState) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if *guard.1 != state {
            guard.0 += 1;
            guard.1 = Arc::new(state);
        }
    }

    pub fn get(&self) -> Arc<MixState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
    }

    fn if_newer(&self, seen: u64) -> Option<(u64, Arc<MixState>)> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (guard.0 != seen).then(|| (guard.0, guard.1.clone()))
    }
}

/// Opens a decoder for a clip's file at the mixer's format.
pub type Opener = Box<dyn FnMut(&AudioClip, AudioFormat) -> Result<Box<dyn AudioSource>, AudioError> + Send>;

fn ffmpeg_opener() -> Opener {
    Box::new(|clip, format| Ok(Box::new(FfmpegAudioSource::open(&clip.path, format)?) as Box<dyn AudioSource>))
}

struct Decoder {
    /// `None` if the file couldn't be opened: the clip stays silent until the next seek.
    source: Option<Box<dyn AudioSource>>,
    /// Source frame the next read returns, if known.
    next: Option<i64>,
    /// The file the decoder reads (a clip relinked to another file needs a new one).
    path: PathBuf,
    /// The clip's sound effects, running on what this decoder reads.
    chain: Chain,
    /// Speed ≠ 1: source frames read but not yet passed, and where between the first two
    /// of them the next output sample falls.
    hold: Vec<f32>,
    phase: f64,
    /// Exact source position (frames) of the next output sample.
    pos: f64,
    /// Plays at another speed at the source's own pitch ("keep pitch").
    stretch: Option<crate::stretch::Stretch>,
}

impl Decoder {
    fn new(source: Option<Box<dyn AudioSource>>, path: PathBuf) -> Self {
        Decoder { source, next: None, path, chain: Chain { fx: Vec::new() }, hold: Vec::new(), phase: 0.0, pos: 0.0, stretch: None }
    }

    /// Positioned at source frame `at`: forget everything carried over.
    fn restart(&mut self, at: i64) {
        self.next = Some(at);
        self.pos = at as f64;
        self.hold.clear();
        self.phase = 0.0;
        if let Some(s) = &mut self.stretch {
            s.reset();
        }
        self.chain.reset();
    }

    /// Fills `out` with the clip's next samples at its speed: straight from the file at
    /// speed 1; time-stretched at its own pitch when the clip keeps it (`stretch`);
    /// otherwise resampled (linear), the pitch following the speed like tape.
    fn pull(&mut self, out: &mut [f32], format: AudioFormat, clip: &AudioClip, speed: f64) {
        let ch = format.channels as usize;
        let Some(source) = self.source.as_mut() else {
            out.fill(0.0);
            return;
        };
        let frames = out.len() / ch;
        let speed = speed.max(1e-6);
        if (speed - 1.0).abs() < 1e-9 {
            let mut got = 0;
            while got < out.len() {
                let r = source.read(&mut out[got..]);
                if r == 0 {
                    break;
                }
                got += r;
            }
            out[got..].fill(0.0); // the file ran out early: silence
            self.pos += (got / ch) as f64;
        } else if clip.keep_pitch {
            let stretch = self.stretch.get_or_insert_with(|| crate::stretch::Stretch::new(ch, format.sample_rate));
            stretch.pull(out, speed, &mut |buf| source.read(buf));
            self.pos += frames as f64 * speed;
        } else {
            let mut chunk = vec![0.0f32; 256 * ch];
            for f in 0..frames {
                let i = self.phase.floor() as usize;
                while self.hold.len() / ch < i + 2 {
                    let r = source.read(&mut chunk);
                    if r == 0 {
                        // Out of file: silence from here.
                        self.hold.extend(std::iter::repeat_n(0.0, 256 * ch));
                    } else {
                        self.hold.extend_from_slice(&chunk[..r - r % ch]);
                    }
                }
                let k = (self.phase - i as f64) as f32;
                for c in 0..ch {
                    let (x0, x1) = (self.hold[i * ch + c], self.hold[(i + 1) * ch + c]);
                    out[f * ch + c] = x0 + (x1 - x0) * k;
                }
                self.phase += speed;
            }
            let used = self.phase.floor() as usize;
            self.hold.drain(..(used * ch).min(self.hold.len()));
            self.phase -= used as f64;
            self.pos += frames as f64 * speed;
        }
        self.next = Some(self.pos.round() as i64);
    }
}

/// One thing the mixer adds in: a clip, or a compound clip's group (a bus with members:
/// its clips — and the groups of compound clips inside it — then its effects over them).
#[derive(Clone, Debug)]
enum Unit {
    Clip(usize),
    Group(usize, Vec<Unit>),
}

/// How deep compound clips' groups nest (a guard: a project can't nest itself).
const MAX_GROUP_DEPTH: usize = 16;

pub struct TimelineAudio {
    handle: MixHandle,
    generation: u64,
    state: Arc<MixState>,
    format: AudioFormat,
    /// Timeline frame of the next sample to produce.
    position: i64,
    decoders: HashMap<u64, Decoder>,
    open: Opener,
    scratch: Vec<f32>,
    /// Frames each clip (by id) keeps sounding past its end: its effects' tails.
    tails: HashMap<u64, i64>,
    /// What mixes, in layer order: clips on their own and compound clips' groups — and
    /// the effect tracks' buses between them (indices into the state's lists).
    order: Vec<Unit>,
    bus_order: Vec<usize>,
    /// Compound clips' groups, mixed apart before their effects run over them: a buffer
    /// per level of nesting.
    groups: Vec<Vec<f32>>,
    /// Each bus's running effects, and the frame it last stopped at.
    buses: HashMap<u64, (Chain, Option<i64>)>,
}

impl TimelineAudio {
    /// A mixer over a fixed clip list (export, tests).
    pub fn new(clips: Vec<AudioClip>, format: AudioFormat, start: Time, end: Time) -> Self {
        let end = end.max(clips.iter().map(|c| c.range.end()).max().unwrap_or(Time::ZERO));
        Self::with_handle(MixHandle::new(MixState { clips, buses: Vec::new(), end }), format, start)
    }

    /// A mixer that follows `handle` (live playback while editing).
    pub fn with_handle(handle: MixHandle, format: AudioFormat, start: Time) -> Self {
        let mut mixer = TimelineAudio {
            handle,
            generation: 0,
            state: Arc::default(),
            format,
            position: 0,
            decoders: HashMap::new(),
            open: ffmpeg_opener(),
            scratch: Vec::new(),
            tails: HashMap::new(),
            order: Vec::new(),
            bus_order: Vec::new(),
            groups: Vec::new(),
            buses: HashMap::new(),
        };
        mixer.refresh();
        mixer.position = mixer.frame_of(start.max(Time::ZERO));
        mixer
    }

    /// Replaces how clip files are decoded (tests use synthetic sources).
    pub fn with_opener(mut self, open: Opener) -> Self {
        self.open = open;
        self.decoders.clear();
        self
    }

    /// Timeline time of the next sample.
    pub fn position(&self) -> Time {
        self.time_of(self.position)
    }

    /// Number of open decoders (for diagnostics and tests).
    pub fn open_decoders(&self) -> usize {
        self.decoders.values().filter(|d| d.source.is_some()).count()
    }

    fn rate(&self) -> i128 {
        self.format.sample_rate as i128
    }

    /// Nearest sample frame to `t`.
    fn frame_of(&self, t: Time) -> i64 {
        let x = t.0 as i128 * self.rate();
        let d = FLICKS_PER_SECOND as i128;
        (if x >= 0 { (2 * x + d) / (2 * d) } else { -((-2 * x + d) / (2 * d)) }) as i64
    }

    fn time_of(&self, frame: i64) -> Time {
        Time((frame as i128 * FLICKS_PER_SECOND as i128 / self.rate()) as i64)
    }

    fn refresh(&mut self) {
        if let Some((generation, state)) = self.handle.if_newer(self.generation) {
            self.generation = generation;
            // Forget decoders for clips that are gone or now read another file.
            let paths: HashMap<u64, &PathBuf> = state.clips.iter().map(|c| (c.id, &c.path)).collect();
            self.decoders.retain(|id, d| paths.get(id).is_some_and(|p| **p == d.path));
            self.buses.retain(|id, _| state.buses.iter().any(|b| b.id == *id));
            // How long each clip rings on, and the order layers mix in.
            let rate = self.format.sample_rate as f64;
            self.tails = state
                .clips
                .iter()
                .filter(|c| !c.effects.is_empty())
                .map(|c| (c.id, (tail_of(&c.effects, &c.context(c.range.end())).as_seconds_f64() * rate).round() as i64))
                .filter(|(_, t)| *t > 0)
                .collect();
            // Clips in a compound clip's group mix with it, not on their own; a group
            // inside another (a compound clip in a compound clip) mixes with that one.
            let index: HashMap<u64, usize> = state.clips.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
            let groups: HashMap<u64, usize> = state.buses.iter().enumerate().filter(|(_, b)| b.members.is_some()).map(|(i, b)| (b.id, i)).collect();
            let inner: std::collections::HashSet<usize> =
                state.buses.iter().filter_map(|b| b.members.as_ref()).flatten().filter_map(|id| groups.get(id).copied()).collect();
            let mut grouped = std::collections::HashSet::new();
            fn build(state: &MixState, b: usize, index: &HashMap<u64, usize>, groups: &HashMap<u64, usize>, grouped: &mut std::collections::HashSet<usize>, depth: usize) -> Unit {
                let members = state.buses[b].members.as_deref().unwrap_or_default();
                let units = members
                    .iter()
                    .filter_map(|id| match (index.get(id), groups.get(id)) {
                        (Some(&i), _) => {
                            grouped.insert(i);
                            Some(Unit::Clip(i))
                        }
                        (None, Some(&g)) if depth < MAX_GROUP_DEPTH && g != b => Some(build(state, g, index, groups, grouped, depth + 1)),
                        _ => None,
                    })
                    .collect();
                Unit::Group(b, units)
            }
            let mut order: Vec<(u32, Unit)> = Vec::new();
            for (b, bus) in state.buses.iter().enumerate() {
                if bus.members.is_some() && !inner.contains(&b) {
                    order.push((bus.layer, build(&state, b, &index, &groups, &mut grouped, 0)));
                }
            }
            order.extend((0..state.clips.len()).filter(|i| !grouped.contains(i)).map(|i| (state.clips[i].layer, Unit::Clip(i))));
            order.sort_by_key(|(layer, _)| *layer);
            self.order = order.into_iter().map(|(_, u)| u).collect();
            let mut buses: Vec<usize> = (0..state.buses.len()).filter(|b| state.buses[*b].members.is_none()).collect();
            buses.sort_by_key(|i| state.buses[*i].layer);
            self.bus_order = buses;
            self.state = state;
        }
    }

    /// Makes sure `clip` has a decoder positioned at source frame `want`.
    fn prepare(&mut self, clip: &AudioClip, want: i64) {
        let drift = (DRIFT_SECONDS * self.format.sample_rate as f64) as i64;
        let format = self.format;
        let want_time = self.time_of(want);
        let open = &mut self.open;
        let d = self.decoders.entry(clip.id).or_insert_with(|| {
            // A reversed clip reads its file backwards from where it starts.
            let source = open(clip, format).ok().map(|s| match clip.reverse {
                Some(pivot) => Box::new(Reversed::new(s, pivot)) as Box<dyn AudioSource>,
                None => s,
            });
            Decoder::new(source, clip.path.clone())
        });
        // Effects added or removed: rebuild the chain and start it cleanly from here
        // (its latency may have changed).
        if !d.chain.matches(&clip.effects) {
            let chain = std::mem::replace(&mut d.chain, Chain { fx: Vec::new() });
            d.chain = chain.rebuild(&clip.effects, format);
            d.next = None;
        }
        let Some(source) = d.source.as_mut() else { return };
        if d.next.is_none_or(|n| (n - want).abs() > drift) {
            if source.seek(want_time).is_err() {
                d.source = None;
                return;
            }
            d.restart(want);
            // Effects that look ahead (denoise) are fed their latency's worth first, so
            // what comes out lines up with `want`.
            let latency = d.chain.latency() * format.channels as usize;
            if latency > 0 {
                let mut prime = vec![0.0f32; latency];
                d.pull(&mut prime, format, clip, clip.speed_near(want_time));
                let t = clip.range.start + Time::from_seconds_f64((want_time - clip.source_in).as_seconds_f64() / clip.speed_near(want_time));
                d.chain.process(&clip.owner(t), &mut prime, t, format);
                // The output continues from `want`; the file is read that far ahead.
                d.pos = want as f64;
                d.next = Some(want);
            }
        }
    }
}

impl TimelineAudio {
    /// Adds `clip`'s share of the block `start..stop` to `out`: decoded, through its
    /// effects, with its gain. After its end it runs on for its effects' tail, feeding
    /// them silence, so an echo or a reverb rings out instead of stopping dead.
    fn mix_clip(&mut self, clip: &AudioClip, out: &mut [f32], start: i64, stop: i64) {
        let ch = self.format.channels as usize;
        let (cs, ce) = (self.frame_of(clip.range.start), self.frame_of(clip.range.end()));
        let tail = self.tails.get(&clip.id).copied().unwrap_or(0);
        let (a, b) = (start.max(cs), stop.min(ce + tail));
        if a >= b {
            return;
        }
        if a < ce {
            let want = match &clip.ramp {
                Some(_) => self.frame_of(clip.source_at(self.time_of(a))),
                None => self.frame_of(clip.source_in) + ((a - cs) as f64 * clip.speed).round() as i64,
            };
            self.prepare(clip, want);
        } else if !self.decoders.contains_key(&clip.id) {
            return; // a tail of nothing that played: nothing to ring
        }
        let (format, block_time) = (self.format, self.time_of(a));
        // A speed ramp: this block at the ramp's average speed across it, so the file is
        // read exactly as far as the ramp goes by the block's end.
        let block_speed = match &clip.ramp {
            Some(r) => {
                let sr = format.sample_rate as f64;
                let frames = (stop.min(ce) - a).max(1) as f64;
                let t0 = (a - cs) as f64 / sr;
                let t1 = t0 + frames / sr;
                ((r.offset_at(t1) - r.offset_at(t0)) / (t1 - t0)).abs()
            }
            None => clip.speed,
        };
        let Some(d) = self.decoders.get_mut(&clip.id) else { return };
        if d.source.is_none() {
            return;
        }
        // Sound from the file up to the clip's end; silence after it, for the tail.
        let n = (b - a) as usize * ch;
        let decoded = (stop.min(ce) - a).max(0) as usize * ch;
        self.scratch.resize(n, 0.0);
        if decoded > 0 {
            d.pull(&mut self.scratch[..decoded], format, clip, block_speed);
        }
        self.scratch[decoded..n].fill(0.0);
        if !clip.effects.is_empty() {
            d.chain.process(&clip.owner(block_time), &mut self.scratch[..n], block_time, format);
        }

        // Sum with gain, ramped between evaluations every GAIN_STEP frames. In the tail,
        // the gain the clip ended on.
        let offset = (a - start) as usize * ch;
        let total = (b - a) as usize;
        let gain_at = |me: &Self, frame: i64| clip.amplitude(me.time_of(frame.min(ce - 1)));
        let mut f = 0;
        let mut g0 = gain_at(self, a);
        while f < total {
            let step = GAIN_STEP.min(total - f);
            let g1 = gain_at(self, a + (f + step) as i64);
            if g0 != 0.0 || g1 != 0.0 {
                for i in 0..step {
                    let g = g0 + (g1 - g0) * (i as f32 / step as f32);
                    let base = (f + i) * ch;
                    for c in 0..ch {
                        out[offset + base + c] += self.scratch[base + c] * g;
                    }
                }
            }
            g0 = g1;
            f += step;
        }
    }

    /// Adds one unit to `out`: a clip, or a compound clip's group — its clips mixed apart,
    /// its own effects run over them, then added in.
    fn mix_unit(&mut self, state: &MixState, unit: &Unit, out: &mut [f32], start: i64, stop: i64) {
        self.mix_unit_at(state, unit, out, start, stop, 0);
    }

    /// [`TimelineAudio::mix_unit`] for a group `depth` groups deep (each level mixes into
    /// a buffer of its own).
    fn mix_unit_at(&mut self, state: &MixState, unit: &Unit, out: &mut [f32], start: i64, stop: i64, depth: usize) {
        match unit {
            Unit::Clip(i) => self.mix_clip(&state.clips[*i], out, start, stop),
            Unit::Group(b, members) => {
                if self.groups.len() <= depth {
                    self.groups.resize_with(depth + 1, Vec::new);
                }
                let mut group = std::mem::take(&mut self.groups[depth]);
                group.clear();
                group.resize(out.len(), 0.0);
                for member in members {
                    self.mix_unit_at(state, member, &mut group, start, stop, depth + 1);
                }
                self.run_bus(&state.buses[*b], &mut group, start, stop);
                for (o, g) in out.iter_mut().zip(&group) {
                    *o += g;
                }
                self.groups[depth] = group;
            }
        }
    }

    /// Runs an effect track's container over what's mixed so far (the layers below it)
    /// while it lasts; after its end, what its effects still ring with is added on top.
    fn run_bus(&mut self, bus: &AudioBus, out: &mut [f32], start: i64, stop: i64) {
        let ch = self.format.channels as usize;
        let (bs, be) = (self.frame_of(bus.range.start), self.frame_of(bus.range.end()));
        let tail = (tail_of(&bus.effects, &bus.owner(bus.range.end()).context).as_seconds_f64() * self.format.sample_rate as f64).round() as i64;
        let (a, b) = (start.max(bs), stop.min(be + tail));
        if a >= b {
            return;
        }
        let inside = b.min(be).max(a);
        let (format, ta, ti) = (self.format, self.time_of(a), self.time_of(inside));
        let entry = self.buses.entry(bus.id).or_insert_with(|| (Chain { fx: Vec::new() }, None));
        if !entry.0.matches(&bus.effects) {
            let chain = std::mem::replace(&mut entry.0, Chain { fx: Vec::new() });
            entry.0 = chain.rebuild(&bus.effects, format);
            entry.1 = None;
        }
        // Not carrying on from the last block (a seek, a gap): nothing's left ringing.
        if entry.1 != Some(a) {
            entry.0.reset();
        }
        if inside > a {
            let (o0, o1) = ((a - start) as usize * ch, (inside - start) as usize * ch);
            entry.0.process(&bus.owner(ta), &mut out[o0..o1], ta, format);
        }
        if b > inside {
            let (o0, o1) = ((inside - start) as usize * ch, (b - start) as usize * ch);
            let mut ring = vec![0.0f32; o1 - o0];
            entry.0.process(&bus.owner(ti), &mut ring, ti, format);
            for (o, r) in out[o0..o1].iter_mut().zip(&ring) {
                *o += r;
            }
        }
        entry.1 = Some(b);
    }
}

impl AudioSource for TimelineAudio {
    fn format(&self) -> AudioFormat {
        self.format
    }
    fn read(&mut self, out: &mut [f32]) -> usize {
        self.refresh();
        let ch = self.format.channels as usize;
        let end = self.frame_of(self.state.end);
        let frames = ((out.len() / ch) as i64).min(end - self.position).max(0) as usize;
        if frames == 0 {
            return 0;
        }
        let out = &mut out[..frames * ch];
        out.fill(0.0);
        let (start, stop) = (self.position, self.position + frames as i64);
        let state = self.state.clone();

        // Layer by layer, bottom up: every effect track's bus runs over what's been
        // mixed below it, then the layers above it are added.
        let (order, bus_order) = (std::mem::take(&mut self.order), std::mem::take(&mut self.bus_order));
        let layer_of = |u: &Unit| match u {
            Unit::Clip(i) => state.clips[*i].layer,
            Unit::Group(b, _) => state.buses[*b].layer,
        };
        let mut next = 0;
        for &b in &bus_order {
            let bus = &state.buses[b];
            while next < order.len() && layer_of(&order[next]) < bus.layer {
                self.mix_unit(&state, &order[next], out, start, stop);
                next += 1;
            }
            self.run_bus(bus, out, start, stop);
        }
        for unit in &order[next..] {
            self.mix_unit(&state, unit, out, start, stop);
        }
        (self.order, self.bus_order) = (order, bus_order);

        // Close decoders whose clips (and tails) are behind us; warm up the ones coming
        // next.
        let warm = (WARM_SECONDS * self.format.sample_rate as f64) as i64;
        let mut upcoming = Vec::new();
        for clip in &state.clips {
            let (cs, ce) = (self.frame_of(clip.range.start), self.frame_of(clip.range.end()));
            let tail = self.tails.get(&clip.id).copied().unwrap_or(0);
            if ce + tail <= stop {
                self.decoders.remove(&clip.id);
            } else if cs > stop && cs - stop <= warm && !self.decoders.contains_key(&clip.id) {
                upcoming.push((clip.clone(), self.frame_of(clip.source_in)));
            }
        }
        for (clip, want) in upcoming {
            self.prepare(&clip, want);
        }

        self.position = stop;
        frames * ch
    }

    fn seek(&mut self, t: Time) -> Result<(), AudioError> {
        self.refresh();
        self.position = self.frame_of(t.max(Time::ZERO));
        // Decoders re-seek lazily when next used; failed ones get another chance.
        self.decoders.retain(|_, d| d.source.is_some());
        for d in self.decoders.values_mut() {
            d.next = None;
        }
        for b in self.buses.values_mut() {
            b.1 = None;
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.position >= self.frame_of(self.state.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_params::{Curve, Keyframe, KeyframeAnchor};

    /// A synthetic "file" whose every sample equals `level + seconds into the file / 100`,
    /// so a sample's value says which file and which moment it came from.
    struct Probe {
        level: f32,
        format: AudioFormat,
        frame: i64,
        seeks: Arc<Mutex<u32>>,
    }

    impl AudioSource for Probe {
        fn format(&self) -> AudioFormat {
            self.format
        }
        fn read(&mut self, out: &mut [f32]) -> usize {
            let ch = self.format.channels as usize;
            for frame in out.chunks_mut(ch) {
                let v = self.level + self.frame as f32 / self.format.sample_rate as f32 / 100.0;
                frame.fill(v);
                self.frame += 1;
            }
            out.len()
        }
        fn seek(&mut self, t: Time) -> Result<(), AudioError> {
            *self.seeks.lock().unwrap() += 1;
            self.frame = (t.as_seconds_f64() * self.format.sample_rate as f64).round() as i64;
            Ok(())
        }
        fn finished(&self) -> bool {
            false
        }
    }

    const FORMAT: AudioFormat = AudioFormat { sample_rate: 1000, channels: 2 };

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    fn clip(id: u64, media: u64, start: f64, dur: f64, source_in: f64) -> AudioClip {
        AudioClip::new(id, media, PathBuf::from(format!("{media}")), TimeRange::new(secs(start), secs(dur)), secs(source_in))
    }

    fn mixer(clips: Vec<AudioClip>, end: f64, seeks: Arc<Mutex<u32>>) -> TimelineAudio {
        TimelineAudio::new(clips, FORMAT, Time::ZERO, secs(end)).with_opener(Box::new(move |c, format| {
            Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: seeks.clone() }) as Box<dyn AudioSource>)
        }))
    }

    /// Reads everything, returning the left channel.
    fn drain(m: &mut TimelineAudio) -> Vec<f32> {
        let mut all = Vec::new();
        let mut block = vec![0.0; 2 * 37]; // odd block size: boundaries mid-block
        loop {
            let n = m.read(&mut block);
            if n == 0 {
                break;
            }
            all.extend(block[..n].chunks(2).map(|f| f[0]));
        }
        all
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn clips_play_at_their_place_with_silence_between() {
        // File 1 from its 2 s mark at timeline 0.5..1.0; file 3 from its start at 1.5..2.0.
        let mut m = mixer(vec![clip(10, 1, 0.5, 0.5, 2.0), clip(11, 3, 1.5, 0.5, 0.0)], 2.5, Default::default());
        let s = drain(&mut m);
        assert_eq!(s.len(), 2500, "silence runs out to the sequence end");
        assert!(s[..500].iter().all(|v| *v == 0.0));
        assert!(close(s[500], 1.0 + 2.0 / 100.0), "{}", s[500]);
        assert!(close(s[999], 1.0 + 2.499 / 100.0), "{}", s[999]);
        assert!(s[1000..1500].iter().all(|v| *v == 0.0));
        assert!(close(s[1500], 3.0) && close(s[1999], 3.0 + 0.499 / 100.0));
        assert!(s[2000..].iter().all(|v| *v == 0.0));
        assert!(m.finished());
    }

    #[test]
    fn overlapping_clips_mix_with_their_own_decoders() {
        // Two clips of the *same* file, overlapping, from different source points.
        let mut m = mixer(vec![clip(10, 1, 0.0, 1.0, 0.0), clip(11, 1, 0.5, 1.0, 5.0)], 1.5, Default::default());
        let s = drain(&mut m);
        // At 0.75 s: first clip at source 0.75, second at 5.25.
        assert!(close(s[750], (1.0 + 0.0075) + (1.0 + 0.0525)), "{}", s[750]);
        assert!(close(s[1250], 1.0 + 0.0575), "{}", s[1250]);
    }

    #[test]
    fn gain_is_keyframable_in_decibels() {
        let mut c = clip(10, 1, 0.0, 2.0, 0.0);
        c.gain_db = ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::linear(secs(0.0), Value::Float(-60.0)), Keyframe::linear(secs(1.0), Value::Float(0.0))],
        ));
        let mut quiet = clip(11, 2, 0.0, 2.0, 0.0);
        quiet.gain_db = ParamSource::Static(Value::Float(-6.0206)); // half amplitude
        let mut m = mixer(vec![c, quiet], 2.0, Default::default());
        let s = drain(&mut m);
        let quiet_at = |i: usize| 0.5 * (2.0 + i as f32 / 100_000.0);
        assert!(close(s[0], quiet_at(0)), "the fade starts silent: {}", s[0]);
        // Halfway through the fade: -30 dB ≈ 0.0316.
        let half = (s[500] - quiet_at(500)) / (1.0 + 0.005);
        assert!((half - 0.0316).abs() < 0.002, "{half}");
        let full = s[1500] - quiet_at(1500);
        assert!(close(full, 1.0 + 0.015), "unity after the fade: {full}");
    }

    fn effect_with(id: u64, type_id: &str, set: &[(&str, Value)]) -> AudioEffect {
        let mut params = oa_params::ParamSet::default();
        for (k, v) in set {
            params.set(k, ParamSource::Static(v.clone()));
        }
        AudioEffect::new(id, type_id, params)
    }

    /// An echo keeps sounding after its clip ends: the mixer feeds it silence for as
    /// long as its repeats last, instead of cutting it off.
    #[test]
    fn an_echo_rings_on_past_the_end_of_its_clip() {
        let mut c = clip(10, 1, 0.0, 0.5, 0.0);
        c.effects.push(effect_with(90, "oa.audio.echo", &[("delay", Value::Float(0.2)), ("feedback", Value::Float(0.5)), ("mix", Value::Float(1.0))]));
        let mut m = mixer(vec![c], 3.0, Default::default());
        let s = drain(&mut m);
        assert!(s[400] > 0.9, "the clip itself");
        assert!(s[600] > 0.3, "0.1 s after it ends, its echo: {}", s[600]);
        assert!(s[900].abs() > 0.05, "still ringing: {}", s[900]);
        assert!(s[2900].abs() < 0.01, "and gone once the repeats die away: {}", s[2900]);
        // Without it, silence straight after the end.
        let mut m = mixer(vec![clip(10, 1, 0.0, 0.5, 0.0)], 3.0, Default::default());
        assert_eq!(drain(&mut m)[600], 0.0);
    }

    /// An effect track's container runs over the layers below it, for as long as it
    /// lasts, and leaves the layers above alone.
    #[test]
    fn a_bus_affects_only_the_layers_below_it() {
        let low = clip(10, 1, 0.0, 2.0, 0.0);
        let mut high = clip(11, 3, 0.0, 2.0, 0.0);
        high.layer = 2;
        // A tone with none of the original and next to no level: it silences its input.
        let silence = effect_with(90, "oa.audio.tone", &[("original", Value::Float(0.0)), ("gain", Value::Float(-60.0))]);
        let bus = AudioBus { id: 70, range: TimeRange::new(secs(0.5), secs(0.5)), effects: vec![silence], layer: 1, members: None };
        let state = MixState { clips: vec![high, low], buses: vec![bus], end: secs(2.0) };
        let mut m = TimelineAudio::with_handle(MixHandle::new(state), FORMAT, Time::ZERO)
            .with_opener(Box::new(|c, format| Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: Default::default() }) as Box<dyn AudioSource>)));
        let s = drain(&mut m);
        assert!(s[250] > 3.9, "both layers before it: {}", s[250]);
        assert!((s[750] - (3.0 + 0.0075)).abs() < 0.01, "only the layer above it while it lasts: {}", s[750]);
        assert!(s[1250] > 3.9, "both again after it: {}", s[1250]);
    }

    /// A compound clip's own sound effects run over its clips alone: here they silence
    /// its two clips while a clip beside it (same layer, not in the group) plays on.
    #[test]
    fn a_compound_clips_effects_run_over_its_own_clips() {
        let (a, b, beside) = (clip(20, 1, 0.0, 2.0, 0.0), clip(21, 2, 0.0, 2.0, 0.0), clip(22, 4, 0.0, 2.0, 0.0));
        let silence = effect_with(91, "oa.audio.tone", &[("original", Value::Float(0.0)), ("gain", Value::Float(-60.0))]);
        let group = AudioBus { id: 71, range: TimeRange::new(secs(0.0), secs(2.0)), effects: vec![silence], layer: 0, members: Some(vec![20, 21]) };
        let state = MixState { clips: vec![a, b, beside], buses: vec![group], end: secs(2.0) };
        let mut m = TimelineAudio::with_handle(MixHandle::new(state), FORMAT, Time::ZERO)
            .with_opener(Box::new(|c, format| Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: Default::default() }) as Box<dyn AudioSource>)));
        let s = drain(&mut m);
        // Only the clip beside it: level 4 (plus its tiny timecode ramp).
        assert!((s[500] - (4.0 + 0.005)).abs() < 0.01, "the group's clips are silenced, the other plays: {}", s[500]);
    }

    /// A compound clip inside another: its group mixes inside the outer group, each clip
    /// heard once — and the outer compound clip's volume and fade reach the clips.
    #[test]
    fn groups_inside_groups() {
        let (inner_clip, outer_clip) = (clip(30, 1, 0.0, 2.0, 0.0), clip(31, 2, 0.0, 2.0, 0.0));
        // No effects to speak of (a plain echo at no mix): the groups just pass sound on.
        let pass = |id| effect_with(id, "oa.audio.echo", &[("mix", Value::Float(0.0))]);
        let inner = AudioBus { id: 80, range: TimeRange::new(secs(0.0), secs(2.0)), effects: vec![pass(92)], layer: 0, members: Some(vec![30]) };
        let outer = AudioBus { id: 81, range: TimeRange::new(secs(0.0), secs(2.0)), effects: vec![pass(93)], layer: 0, members: Some(vec![80, 31]) };
        let mix = |clips: Vec<AudioClip>| {
            let state = MixState { clips, buses: vec![inner.clone(), outer.clone()], end: secs(2.0) };
            let mut m = TimelineAudio::with_handle(MixHandle::new(state), FORMAT, Time::ZERO)
                .with_opener(Box::new(|c, format| Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: Default::default() }) as Box<dyn AudioSource>)));
            drain(&mut m)
        };
        let s = mix(vec![inner_clip.clone(), outer_clip.clone()]);
        assert!((s[500] - (3.0 + 0.005)).abs() < 0.02, "each clip once: {}", s[500]);
        // The compound clip's −6 dB with a 1 s fade in, on both.
        let under = OuterGain { gain_db: ParamSource::Static(Value::Float(-6.0206)), clock_start: Time::ZERO, clock_rate: 1.0, range: TimeRange::new(secs(0.0), secs(2.0)), fade_in: secs(1.0), fade_out: Time::ZERO };
        let quieter: Vec<AudioClip> = [inner_clip, outer_clip].into_iter().map(|mut c| {
            c.outer.push(under.clone());
            c
        }).collect();
        let s = mix(quieter);
        assert!((s[1500] - 1.5).abs() < 0.02, "half as loud once faded in: {}", s[1500]);
        assert!((s[250] - 0.375).abs() < 0.03, "a quarter of the way into the fade: {}", s[250]);
    }

    #[test]
    fn edits_while_playing_keep_decoders_and_seek_only_on_jumps() {
        let seeks: Arc<Mutex<u32>> = Default::default();
        let handle = MixHandle::new(MixState { clips: vec![clip(10, 1, 0.0, 4.0, 0.0)], buses: Vec::new(), end: secs(4.0) });
        let s2 = seeks.clone();
        let mut m = TimelineAudio::with_handle(handle.clone(), FORMAT, Time::ZERO).with_opener(Box::new(move |c, format| {
            Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: s2.clone() }) as Box<dyn AudioSource>)
        }));
        let mut block = vec![0.0; 2 * 500];
        m.read(&mut block);
        assert_eq!(*seeks.lock().unwrap(), 1, "opening positions once");

        // A gain change: same decoder, no seek, new gain at the next block.
        let mut louder = clip(10, 1, 0.0, 4.0, 0.0);
        louder.gain_db = ParamSource::Static(Value::Float(6.0206));
        handle.set(MixState { clips: vec![louder.clone()], buses: Vec::new(), end: secs(4.0) });
        m.read(&mut block);
        assert!(close(block[0], 2.0 * (1.0 + 0.005)), "{}", block[0]);
        assert_eq!(*seeks.lock().unwrap(), 1);

        // A slip by 2 s under the playhead: one re-seek, and the new footage plays.
        let mut slipped = louder;
        slipped.source_in = secs(2.0);
        handle.set(MixState { clips: vec![slipped], buses: Vec::new(), end: secs(4.0) });
        m.read(&mut block);
        assert_eq!(*seeks.lock().unwrap(), 2);
        assert!(close(block[0], 2.0 * (1.0 + 0.03)), "{}", block[0]);

        // The clip removed: silence, and its decoder closes.
        handle.set(MixState { clips: vec![], buses: Vec::new(), end: secs(4.0) });
        m.read(&mut block);
        assert!(block.iter().all(|v| *v == 0.0));
        assert_eq!(m.open_decoders(), 0);
    }

    #[test]
    fn seeking_lands_on_the_right_sample() {
        let mut m = mixer(vec![clip(10, 1, 1.0, 3.0, 0.5)], 4.0, Default::default());
        m.seek(secs(2.25)).unwrap();
        let mut block = vec![0.0; 2];
        m.read(&mut block);
        assert!(close(block[0], 1.0 + 1.75 / 100.0), "{}", block[0]);
        m.seek(secs(9.0)).unwrap();
        assert!(m.finished());
        assert_eq!(m.read(&mut block), 0);
    }

    #[test]
    fn crossfades_ramp_both_sides() {
        // A (file 1) and B (file 2) overlap for 1 s from 1.0 s, ramping against each other.
        let mut a = clip(10, 1, 0.0, 2.0, 0.0);
        a.fade_out = secs(1.0);
        let mut b = clip(11, 2, 1.0, 2.0, 0.0);
        b.fade_in = secs(1.0);
        let mut m = mixer(vec![a, b], 3.0, Default::default());
        let s = drain(&mut m);
        let at_a = |i: usize| 1.0 + i as f32 / 100_000.0;
        // Half way: half of each.
        let mid = s[1500];
        let expected = 0.5 * at_a(1500) + 0.5 * (2.0 + 500.0 / 100_000.0);
        assert!(close(mid, expected), "{mid} vs {expected}");
        assert!(close(s[999], at_a(999)), "before the overlap A is at full level");
        assert!(close(s[2500], 2.0 + 1500.0 / 100_000.0), "after it B is");
    }

    #[test]
    fn upcoming_clips_are_opened_early() {
        let mut m = mixer(vec![clip(10, 1, 1.0, 1.0, 0.0)], 2.0, Default::default());
        let mut block = vec![0.0; 2 * 400];
        m.read(&mut block);
        assert_eq!(m.open_decoders(), 0, "1 s away: not yet");
        m.read(&mut block);
        assert_eq!(m.open_decoders(), 1, "0.2 s away: warming up");
    }

    /// A 440 Hz tone "file".
    struct Tone {
        format: AudioFormat,
        frame: i64,
    }

    impl AudioSource for Tone {
        fn format(&self) -> AudioFormat {
            self.format
        }
        fn read(&mut self, out: &mut [f32]) -> usize {
            for f in out.chunks_mut(self.format.channels as usize) {
                f.fill(0.5 * (2.0 * std::f32::consts::PI * 440.0 * self.frame as f32 / self.format.sample_rate as f32).sin());
                self.frame += 1;
            }
            out.len()
        }
        fn seek(&mut self, t: Time) -> Result<(), AudioError> {
            self.frame = (t.as_seconds_f64() * self.format.sample_rate as f64).round() as i64;
            Ok(())
        }
        fn finished(&self) -> bool {
            false
        }
    }

    fn tone_mixer(effects: Vec<AudioEffect>) -> TimelineAudio {
        let format = AudioFormat::stereo_48k();
        let mut c = clip(1, 1, 0.0, 3.0, 0.0);
        c.effects = effects;
        TimelineAudio::new(vec![c], format, Time::ZERO, secs(3.0))
            .with_opener(Box::new(|_, format| Ok(Box::new(Tone { format, frame: 0 }) as Box<dyn AudioSource>)))
    }

    fn effect(id: u64, type_id: &str, params: &[(&str, f64)]) -> AudioEffect {
        let mut set = oa_params::ParamSet::default();
        for (k, v) in params {
            set.set(k, ParamSource::Static(Value::Float(*v)));
        }
        AudioEffect::new(id, type_id, set)
    }

    /// A sound effect as an intro runs only over the clip's first seconds, with its clock:
    /// Fade (a sound shader) brings the sound up from silence, then leaves it alone.
    #[test]
    fn sound_intros_run_in_their_window() {
        let mut fade = effect(9, "oa.audio.fade", &[("curve", 1.0)]);
        fade.role = oa_doc::EffectRole::In { duration: secs(1.0) };
        let mut dry = tone_mixer(vec![]);
        let mut wet = tone_mixer(vec![fade]);
        let (a, b) = (drain(&mut dry), drain(&mut wet));
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        // `drain` keeps the left channel: 48 000 samples a second.
        let quarter = |x: &[f32], i: usize| rms(&x[i * 12_000..(i + 1) * 12_000]);
        assert!(quarter(&b, 0) < quarter(&a, 0) * 0.3, "starts near silence");
        assert!(quarter(&b, 0) < quarter(&b, 1) && quarter(&b, 1) < quarter(&b, 2) && quarter(&b, 2) < quarter(&b, 3), "rises");
        let after = a[50_000..140_000].iter().zip(&b[50_000..140_000]).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        assert!(after < 1e-6, "untouched after the intro: {after}");
    }

    #[test]
    fn look_ahead_effects_stay_in_sync_even_after_seeks() {
        // Denoise with no reduction passes the sound through unchanged — but a frame
        // later. The mixer primes it, so it must line up with the dry mix exactly.
        let mut dry = tone_mixer(vec![]);
        let mut wet = tone_mixer(vec![effect(7, "oa.audio.denoise", &[("reduction", 0.0)])]);
        let (a, b) = (drain(&mut dry), drain(&mut wet));
        assert_eq!(a.len(), b.len());
        let worst = a.iter().zip(&b).skip(4800).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-3, "{worst}");

        for m in [&mut dry, &mut wet] {
            m.seek(secs(1.5)).unwrap();
        }
        let (a, b) = (drain(&mut dry), drain(&mut wet));
        let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-3, "after a seek: {worst}");
    }

    #[test]
    fn sound_effects_apply_and_follow_edits_while_playing() {
        let mut m = tone_mixer(vec![]);
        let mut block = vec![0.0; 2 * 4800];
        m.read(&mut block);
        let rms = |b: &[f32]| (b.iter().map(|x| x * x).sum::<f32>() / b.len() as f32).sqrt();
        let before = rms(&block);
        // Add a gate that shuts everything below -3 dBFS (the tone peaks at -6).
        let mut state = (*m.handle.get()).clone();
        state.clips[0].effects.push(effect(8, "oa.audio.gate", &[("threshold", -3.0), ("release", 0.01)]));
        m.handle.set(state);
        m.read(&mut block);
        m.read(&mut block);
        assert!(before > 0.3 && rms(&block) < 0.01, "{before} → {}", rms(&block));
    }

    #[test]
    fn reversed_clips_play_their_file_backwards() {
        // Starting at 3 s in the file and playing down: half-way along a 1 s clip the
        // file is at 2.5 s, at its end nearly 2 s; at 2× it covers 3 → 1 s.
        for speed in [1.0, 2.0] {
            let mut c = clip(1, 3, 0.0, 1.0, 0.0);
            c.speed = speed;
            c.reverse = Some(secs(3.0));
            let seeks: Arc<Mutex<u32>> = Default::default();
            let mut m = mixer(vec![c], 1.0, seeks.clone());
            let s = drain(&mut m);
            let at = |i: usize| (s[i] - 3.0) * 100.0; // the file's seconds
            // The sample just below 3 s (a sample's length at the test's rate).
            assert!((at(0) - 3.0).abs() < 0.002, "{speed}×: starts at {}", at(0));
            assert!((at(500) - (3.0 - 0.5 * speed as f32)).abs() < 0.01, "{speed}×: half-way at {}", at(500));
            assert!(at(900) < at(100), "{speed}×: going down");
            // A seek per second of the file, not per block the mixer asks for.
            assert!(*seeks.lock().unwrap() <= 1 + speed as u32 + 1, "{speed}×: {} seeks", seeks.lock().unwrap());
        }
    }

    #[test]
    fn speed_plays_through_the_file_faster_or_slower() {
        // Probe samples say where in the file they came from: level + seconds / 100.
        for speed in [2.0, 0.5] {
            let mut c = clip(1, 3, 0.0, 1.0, 1.0);
            c.speed = speed;
            let mut m = mixer(vec![c], 1.0, Default::default());
            let s = drain(&mut m);
            // Half-way along the clip, the file is at 1 s + 0.5 s × speed.
            let expect = 3.0 + (1.0 + 0.5 * speed as f32) / 100.0;
            assert!((s[500] - expect).abs() < 2e-4, "speed {speed}: {} vs {expect}", s[500]);
        }
    }

    /// A speed ramp reads the file as the ramp goes: from 1× to 3× over the clip, it's at
    /// 1 s + 0.5 × 1.5 s of file (the ramp's integral) a quarter of the way… and the
    /// positions keep rising at the ramp's pace, without a seek's jump.
    #[test]
    fn a_speed_ramp_reads_the_file_as_it_goes() {
        let mut c = clip(1, 3, 0.0, 1.0, 1.0);
        // 1× → 3× linearly over the clip's 1 s: offset(x) = x + x².
        c.ramp = Some(Arc::new(SpeedRamp::sample(1.0, |x| x + x * x)));
        let mut m = mixer(vec![c], 1.0, Default::default());
        let s = drain(&mut m);
        let at = |i: usize| (s[i] - 3.0) * 100.0;
        for (i, x) in [(250usize, 0.25f32), (500, 0.5), (900, 0.9)] {
            let want = 1.0 + x + x * x;
            assert!((at(i) - want).abs() < 0.02, "at {x} s: {} vs {want}", at(i));
        }
        assert!(s.windows(2).skip(10).take(980).all(|w| w[1] >= w[0] - 1e-5), "only ever forward");
    }

    #[test]
    fn speed_changes_pitch_unless_kept() {
        let crossings = |x: &[f32]| x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32;
        let run = |keep: bool| {
            let format = AudioFormat::stereo_48k();
            let mut c = clip(1, 1, 0.0, 2.0, 0.0);
            c.speed = 2.0;
            c.keep_pitch = keep;
            let mut m = TimelineAudio::new(vec![c], format, Time::ZERO, secs(2.0))
                .with_opener(Box::new(|_, format| Ok(Box::new(Tone { format, frame: 0 }) as Box<dyn AudioSource>)));
            let s = drain(&mut m);
            crossings(&s[9600..]) / ((s.len() - 9600) as f32 / 48_000.0)
        };
        let fast = run(false);
        let kept = run(true);
        assert!((fast / 880.0 - 1.0).abs() < 0.03, "tape-style: {fast} Hz");
        assert!((kept / 440.0 - 1.0).abs() < 0.1, "pitch kept: {kept} Hz");
    }
}
