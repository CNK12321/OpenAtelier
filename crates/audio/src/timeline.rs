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
            range,
        }
    }

    /// Linear amplitude at timeline time `t`.
    pub(crate) fn amplitude(&self, t: Time) -> f32 {
        let clip_time = t - self.clock_start;
        let source_time = self.source_at(t);
        let db = self.gain_db.eval(&EvalContext::at(clip_time, source_time)).as_float().unwrap_or(0.0);
        let ramp = |into: Time, len: Time| if len > Time::ZERO { (into.0 as f64 / len.0 as f64).clamp(0.0, 1.0) } else { 1.0 };
        let fade = ramp(t - self.range.start, self.fade_in) * ramp(self.range.end() - t, self.fade_out);
        db_to_amplitude(db) * fade as f32
    }

    /// Where in the file timeline time `t` falls.
    pub(crate) fn source_at(&self, t: Time) -> Time {
        self.source_in + Time::from_seconds_f64((t - self.range.start).as_seconds_f64() * self.speed)
    }

    /// The clip's clocks at timeline time `t` (for keyframed effect params).
    fn context(&self, t: Time) -> EvalContext {
        EvalContext::at(t - self.clock_start, self.source_at(t))
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
            let values = e.params.eval(&info.params, None, &owner.context);
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

/// An effect track's container on the sound side: its effects run over everything on the
/// layers below it (every clip whose `layer` is lower), mixed, while it lasts — and ring
/// on for their tails after.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBus {
    /// The container's id: its effects' state follows it across edits.
    pub id: u64,
    pub range: TimeRange,
    pub effects: Vec<AudioEffect>,
    pub layer: u32,
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
    /// Undoes the pitch change of a speed change ("keep pitch").
    pitch: Option<Box<dyn Processor>>,
}

impl Decoder {
    fn new(source: Option<Box<dyn AudioSource>>, path: PathBuf) -> Self {
        Decoder { source, next: None, path, chain: Chain { fx: Vec::new() }, hold: Vec::new(), phase: 0.0, pos: 0.0, pitch: None }
    }

    /// Positioned at source frame `at`: forget everything carried over.
    fn restart(&mut self, at: i64) {
        self.next = Some(at);
        self.pos = at as f64;
        self.hold.clear();
        self.phase = 0.0;
        if let Some(p) = &mut self.pitch {
            p.reset();
        }
        self.chain.reset();
    }

    /// Fills `out` with the clip's next samples at its speed: straight from the file at
    /// speed 1, otherwise resampled (linear), pitch following speed like tape unless the
    /// clip keeps its pitch.
    fn pull(&mut self, out: &mut [f32], format: AudioFormat, clip: &AudioClip) {
        let ch = format.channels as usize;
        let Some(source) = self.source.as_mut() else {
            out.fill(0.0);
            return;
        };
        let frames = out.len() / ch;
        let speed = clip.speed;
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
            if clip.keep_pitch {
                let p = self.pitch.get_or_insert_with(|| fx::plain_pitch(ch, format.sample_rate as f32));
                let values = oa_params::Evaluated(vec![
                    (oa_params::ParamId::new("semitones"), Value::Float(-12.0 * speed.log2())),
                    (oa_params::ParamId::new("mix"), Value::Float(1.0)),
                ]);
                p.process(out, &values, &fx::ClockSpan::default());
            }
        }
        self.next = Some(self.pos.round() as i64);
    }
}

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
    /// Clips, and buses, in layer order (indices into the state's lists).
    order: Vec<usize>,
    bus_order: Vec<usize>,
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
            let mut order: Vec<usize> = (0..state.clips.len()).collect();
            order.sort_by_key(|i| state.clips[*i].layer);
            self.order = order;
            let mut buses: Vec<usize> = (0..state.buses.len()).collect();
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
        let d = self.decoders.entry(clip.id).or_insert_with(|| Decoder::new(open(clip, format).ok(), clip.path.clone()));
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
                d.pull(&mut prime, format, clip);
                let t = clip.range.start + Time::from_seconds_f64((want_time - clip.source_in).as_seconds_f64() / clip.speed);
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
            let want = self.frame_of(clip.source_in) + ((a - cs) as f64 * clip.speed).round() as i64;
            self.prepare(clip, want);
        } else if !self.decoders.contains_key(&clip.id) {
            return; // a tail of nothing that played: nothing to ring
        }
        let (format, block_time) = (self.format, self.time_of(a));
        let Some(d) = self.decoders.get_mut(&clip.id) else { return };
        if d.source.is_none() {
            return;
        }
        // Sound from the file up to the clip's end; silence after it, for the tail.
        let n = (b - a) as usize * ch;
        let decoded = (stop.min(ce) - a).max(0) as usize * ch;
        self.scratch.resize(n, 0.0);
        if decoded > 0 {
            d.pull(&mut self.scratch[..decoded], format, clip);
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
        let mut next = 0;
        for &b in &bus_order {
            let bus = &state.buses[b];
            while next < order.len() && state.clips[order[next]].layer < bus.layer {
                self.mix_clip(&state.clips[order[next]], out, start, stop);
                next += 1;
            }
            self.run_bus(bus, out, start, stop);
        }
        for &i in &order[next..] {
            self.mix_clip(&state.clips[i], out, start, stop);
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
        let bus = AudioBus { id: 70, range: TimeRange::new(secs(0.5), secs(0.5)), effects: vec![silence], layer: 1 };
        let state = MixState { clips: vec![high, low], buses: vec![bus], end: secs(2.0) };
        let mut m = TimelineAudio::with_handle(MixHandle::new(state), FORMAT, Time::ZERO)
            .with_opener(Box::new(|c, format| Ok(Box::new(Probe { level: c.media as f32, format, frame: 0, seeks: Default::default() }) as Box<dyn AudioSource>)));
        let s = drain(&mut m);
        assert!(s[250] > 3.9, "both layers before it: {}", s[250]);
        assert!((s[750] - (3.0 + 0.0075)).abs() < 0.01, "only the layer above it while it lasts: {}", s[750]);
        assert!(s[1250] > 3.9, "both again after it: {}", s[1250]);
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
