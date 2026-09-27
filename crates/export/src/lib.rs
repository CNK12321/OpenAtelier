//! Writing a sequence out to a file.
//!
//! Export runs the **same planner, optimizer and executor as the preview**, at full
//! quality with no proxies — that's what makes what you saw what you get. Frames are
//! converted to NV12 on the GPU, so only 1.5 bytes per pixel cross to the CPU on their
//! way to the encoder (a float RGBA readback would be 8).
//!
//! Encoders sit behind [`VideoSink`]: Media Foundation's sink writer on Windows (the GPU
//! vendor's hardware encoder, sound muxed in-process) and an `ffmpeg` process everywhere
//! (and for ProRes). Handing the encoder GPU surfaces instead of the NV12 readback is the
//! next step, and then frames never leave the GPU at all.

mod ffmpeg;
#[cfg(windows)]
mod mf;
mod mixdown;
mod wav;
pub use mixdown::AudioTarget;
/// Renders a sound source to a 16-bit WAV (used for export, and for transcribing captions).
pub use wav::write as write_wav;

pub use ffmpeg::{audio_extensions, find_hardware, FfmpegSink, HwEncoder, VideoCodec};
#[cfg(windows)]
pub use mf::MfSink;

use oa_audio::AudioFormat;
use oa_doc::{Project, SeqId, VariantId};
use oa_gpu::{FrameSource, GpuContext, GpuImage, Nv12Plane, Renderer};
use oa_graph::registry::Registry;
use oa_graph::{optimize, KeyContext, OptLevel};
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// Frames rendered ahead of the one being encoded. Three keep the GPU busy through the
/// encoder's hiccups; each holds its read-back staging buffers (1.5 bytes a pixel).
const IN_FLIGHT: usize = 3;

/// The audible clips of a sequence, for the mixer — shared by playback and export so
/// they can't disagree about what you hear.
///
/// `audio_path` resolves a media id to the file to decode, or `None` if it has no sound
/// (or is missing). Disabled tracks and items are silent. Frozen clips, and reversed
/// compound clips, have no sound and are returned in the second list; any other speed
/// plays — reversed clips backwards — compound clips included (their clips at the
/// product of speeds).
pub fn audio_clips(
    project: &Project,
    seq: SeqId,
    audio_path: impl Fn(oa_doc::MediaId) -> Option<std::path::PathBuf>,
) -> (Vec<oa_audio::AudioClip>, Vec<oa_doc::ItemId>) {
    let (clips, _, skipped) = audio_mix(project, seq, audio_path);
    (clips, skipped)
}

/// Loudness envelopes for every file `clips` play (analyzed side by side; blocking), as
/// a follower that answers properties connected to the sound.
pub fn follower_for(clips: &[oa_audio::AudioClip]) -> oa_audio::envelope::Follower {
    use oa_audio::envelope::{media_of, Envelope, Follower};
    let files = media_of(clips);
    let envelopes = std::thread::scope(|s| {
        let jobs: Vec<_> = files.iter().map(|(media, path)| s.spawn(move || (*media, Envelope::analyze(path)))).collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).filter_map(|(m, e)| Some((m, Arc::new(e?)))).collect()
    });
    Follower::new(clips, &envelopes)
}

/// [`audio_clips`], and the sound side of the sequence's effect tracks: a bus per
/// effect container, running over the layers below its track.
///
/// Layers, bottom up: the sound of clips on picture tracks, then the audio tracks from
/// the lowest on the timeline up — so an audio effect track covers the audio tracks
/// drawn below it, and the sound of picture tracks too (at the top of the audio tracks,
/// it's a master effect).
pub fn audio_mix(
    project: &Project,
    seq: SeqId,
    audio_path: impl Fn(oa_doc::MediaId) -> Option<std::path::PathBuf>,
) -> (Vec<oa_audio::AudioClip>, Vec<oa_audio::AudioBus>, Vec<oa_doc::ItemId>) {
    let (mut clips, mut buses, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
    collect_audio(project, seq, &audio_path, 0, &mut clips, &mut buses, &mut skipped);
    clips.sort_by_key(|c| c.range.start);
    (clips, buses, skipped)
}

/// A track's place in the sound stack (see [`audio_mix`]).
fn sound_layer(s: &oa_doc::Sequence, track: &oa_doc::Track) -> u32 {
    if track.kind != oa_doc::TrackKind::Audio {
        return 0;
    }
    let audio: Vec<_> = s.tracks.iter().filter(|t| t.kind == oa_doc::TrackKind::Audio).map(|t| t.id).collect();
    let index = audio.iter().position(|t| *t == track.id).unwrap_or(0);
    (audio.len() - index) as u32
}

/// An item's sound effects, as the mixer runs them.
fn sound_effects(item: &oa_doc::Item) -> Vec<oa_audio::AudioEffect> {
    item.active_effects()
        .iter()
        .filter(|e| e.enabled && oa_audio::fx::info(&e.type_id).is_some())
        .map(|e| oa_audio::AudioEffect { role: e.role, ..oa_audio::AudioEffect::new(e.id.0, &e.type_id, e.params.clone()) })
        .collect()
}

fn collect_audio(
    project: &Project,
    seq: SeqId,
    audio_path: &dyn Fn(oa_doc::MediaId) -> Option<std::path::PathBuf>,
    depth: usize,
    clips: &mut Vec<oa_audio::AudioClip>,
    buses: &mut Vec<oa_audio::AudioBus>,
    skipped: &mut Vec<oa_doc::ItemId>,
) {
    let Some(s) = project.sequence(seq) else { return };
    use oa_doc::ClipEnd;
    for track in s.tracks.iter().filter(|t| t.enabled) {
        let layer = sound_layer(s, track);
        if track.effects {
            // An audio effect track's containers (inside a compound clip, they'd have to
            // run on its own mix, which isn't heard separately yet).
            if track.kind == oa_doc::TrackKind::Audio && depth == 0 {
                for item in track.items.iter().filter(|i| i.enabled) {
                    let effects = sound_effects(item);
                    if !effects.is_empty() {
                        buses.push(oa_audio::AudioBus { id: item.id.0, range: item.range, effects, layer, members: None });
                    }
                }
            }
            continue;
        }
        for (i, item) in track.items.iter().enumerate().filter(|(_, i)| i.enabled) {
            if let oa_doc::ItemKind::Nested { sequence } = item.kind {
                // A compound clip: its sound, moved to where the clip sits and cut to it,
                // at its own track's layer.
                if depth < 16 && item.audio_enabled() {
                    let before = clips.len();
                    nested_audio(project, sequence, item, audio_path, depth, clips, skipped);
                    clips[before..].iter_mut().for_each(|c| c.layer = layer);
                    // Its own sound effects: over its clips alone, mixed apart (a group),
                    // then added in at its layer. (A compound clip inside another keeps
                    // its clips' effects; its own aren't heard yet.)
                    let effects = sound_effects(item);
                    if !effects.is_empty() && clips.len() > before {
                        let members = clips[before..].iter().map(|c| c.id).collect();
                        buses.push(oa_audio::AudioBus { id: item.id.0 ^ 0x5EED_B055, range: item.range, effects, layer, members: Some(members) });
                    }
                }
                continue;
            }
            let oa_doc::ItemKind::Media { media } = item.kind else { continue };
            if !item.audio_enabled() {
                continue; // its sound was extracted to its own clip
            }
            let Some(path) = audio_path(media) else { continue };
            // Freeze frames have no sound. Reversed clips play their sound backwards: the
            // mixer reads the file down from where the clip starts (`AudioClip::reverse`),
            // counting forwards in that reversed time from 0.
            let signed = item.time_map.speed.num() as f64 / item.time_map.speed.den() as f64;
            if signed == 0.0 {
                skipped.push(item.id);
                continue;
            }
            let speed = signed.abs();
            let (source_in, reverse) = if signed < 0.0 { (Time::ZERO, Some(item.time_map.source_in)) } else { (item.time_map.source_in, None) };
            let mut clip = oa_audio::AudioClip::new(item.id.0, media.0, path, item.range, source_in);
            clip.speed = speed;
            clip.reverse = reverse;
            clip.keep_pitch = !matches!(
                item.params.get(oa_doc::schema::AUDIO_KEEP_PITCH).map(|s| s.eval(&item.eval_context(item.range.start))),
                Some(oa_params::Value::Bool(false))
            );
            if let Some(gain) = item.params.get(oa_doc::schema::AUDIO_GAIN) {
                clip.gain_db = gain.clone();
            }
            // Sound effects (bass boost, echo, denoise…), in the clip's order.
            // Intros and outros run over the clip's ends, as picture ones do (a reversed
            // intro included).
            clip.effects = sound_effects(item);
            clip.layer = layer;
            // Transitions crossfade the sound over the same windows the picture uses: a
            // cut transition extends both clips past the cut (into their handles) and
            // ramps them against each other; fades ramp to or from silence.
            let head = oa_plan::transitions::window(track, i, ClipEnd::Head);
            let joined_before = i > 0 && track.items[i - 1].range.end() == item.range.start && track.items[i - 1].enabled;
            if let Some(w) = head {
                if joined_before {
                    // How far before the cut the clip can start: limited by the file's
                    // handle before its in point (at the clip's speed).
                    let handle = oa_time::Time::from_seconds_f64(clip.source_in.as_seconds_f64() / speed);
                    let lead = (item.range.start - w.start).min(handle);
                    clip.range = oa_time::TimeRange::new(item.range.start - lead, item.range.duration + lead);
                    clip.source_in -= oa_time::Time::from_seconds_f64(lead.as_seconds_f64() * speed);
                }
                clip.fade_in = w.duration;
            }
            if let Some(w) = oa_plan::transitions::window(track, i, ClipEnd::Tail) {
                clip.fade_out = w.duration;
            }
            // The next clip's cut transition extends this one past its end.
            if let Some(w) = track.items.get(i + 1).filter(|n| n.range.start == item.range.end() && n.enabled).and_then(|_| oa_plan::transitions::window(track, i + 1, ClipEnd::Head)) {
                let tail = w.end() - item.range.end();
                clip.range = oa_time::TimeRange::new(clip.range.start, clip.range.duration + tail);
                clip.fade_out = w.duration;
            }
            clips.push(clip);
        }
    }
}

/// The sound inside compound clip `item` (playing `sequence`), in the outer timeline.
fn nested_audio(
    project: &Project,
    sequence: SeqId,
    item: &oa_doc::Item,
    audio_path: &dyn Fn(oa_doc::MediaId) -> Option<std::path::PathBuf>,
    depth: usize,
    clips: &mut Vec<oa_audio::AudioClip>,
    skipped: &mut Vec<oa_doc::ItemId>,
) {
    // A retimed compound clip plays its timeline `k` times as fast: inner time τ is heard
    // at outer time start + (τ − in) / k, and every clip inside plays k times its own
    // speed (its pitch following the compound clip's "keep pitch"). Reversed or frozen:
    // silent, as for plain clips.
    let k = item.time_map.speed.num() as f64 / item.time_map.speed.den() as f64;
    if k <= 0.0 {
        skipped.push(item.id);
        return;
    }
    let keep_pitch = !matches!(
        item.params.get(oa_doc::schema::AUDIO_KEEP_PITCH).map(|s| s.eval(&item.eval_context(item.range.start))),
        Some(oa_params::Value::Bool(false))
    );
    let outer = |tau: Time| item.range.start + Time::from_seconds_f64((tau - item.time_map.source_in).as_seconds_f64() / k);
    let squeeze = |d: Time| Time::from_seconds_f64(d.as_seconds_f64() / k);
    let mut inner = Vec::new();
    collect_audio(project, sequence, audio_path, depth + 1, &mut inner, &mut Vec::new(), skipped);
    let (lo, hi) = (item.range.start, item.range.end());
    for mut c in inner {
        let mut start = outer(c.range.start);
        let mut end = outer(c.range.end());
        if k != 1.0 {
            c.speed *= k;
            c.keep_pitch = keep_pitch;
            c.fade_in = squeeze(c.fade_in);
            c.fade_out = squeeze(c.fade_out);
            c.length = squeeze(c.length);
        }
        c.clock_start = outer(c.clock_start);
        if start < lo {
            c.source_in += Time::from_seconds_f64((lo - start).as_seconds_f64() * c.speed);
            c.fade_in = Time::ZERO;
            start = lo;
        }
        if end > hi {
            end = hi;
            c.fade_out = Time::ZERO;
        }
        if end <= start {
            continue;
        }
        c.range = oa_time::TimeRange::new(start, end - start);
        // The same sequence can be used more than once: keep the mixer's ids apart.
        c.id = c.id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ item.id.0;
        clips.push(c);
    }
}

/// Where encoded frames go. One call per frame, in order.
pub trait VideoSink {
    /// `luma` is width×height bytes, `chroma` is (width/2)×(height/2) interleaved Cb/Cr.
    fn push_nv12(&mut self, luma: &[u8], chroma: &[u8]) -> Result<(), ExportError>;
    /// Takes straight-alpha sRGB RGBA frames instead of NV12 (formats that keep
    /// transparency).
    fn wants_rgba(&self) -> bool {
        false
    }
    /// `rgba` is width×height×4 bytes, alpha not premultiplied.
    fn push_rgba(&mut self, _rgba: &[u8]) -> Result<(), ExportError> {
        Err(ExportError::Encode("this encoder takes NV12 frames only".into()))
    }
    /// Closes the stream and waits for the file to be written.
    fn finish(self: Box<Self>) -> Result<(), ExportError>;

    /// Which encoder this is, for the export summary.
    fn describe(&self) -> String;

    /// Where the sound's mixer should send it, if this takes sound (see mixdown.rs).
    fn audio_target(&self) -> Option<AudioTarget> {
        None
    }
}

/// Which encoder to use.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Encoder {
    /// The platform's hardware encoder when it can do the codec, otherwise ffmpeg.
    #[default]
    Auto,
    /// The GPU's encoder, preferred ("Hardware"): Media Foundation's sink writer on
    /// Windows, the GPU through ffmpeg elsewhere — and software when there's none.
    MediaFoundation,
    /// An `ffmpeg` process (software x264/x265, or ProRes).
    Ffmpeg,
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// `None` = the sequence's active format variant.
    pub variant: Option<VariantId>,
    /// 1.0 = the variant's full canvas size.
    pub scale: f64,
    pub codec: VideoCodec,
    /// Constant-rate factor: lower is better quality, 18 is visually lossless-ish.
    pub crf: u32,
    /// Render the unoptimized graph (for comparing against the optimizer).
    pub reference: bool,
    /// Sound to mux in, if any.
    pub audio: Vec<oa_audio::AudioClip>,
    /// Audio effect tracks' containers, run over the layers below them.
    pub buses: Vec<oa_audio::AudioBus>,
    pub encoder: Encoder,
    /// Only this part of the timeline (clamped to it); `None` = all of it.
    pub range: Option<TimeRange>,
    /// Anti-aliased text: titles drawn at twice the size and averaged down before any
    /// effect runs on them (on by default; see `PlanOptions::text_supersample`).
    pub text_antialias: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        ExportOptions {
            variant: None,
            scale: 1.0,
            codec: VideoCodec::H264,
            crf: 18,
            reference: false,
            audio: Vec::new(),
            buses: Vec::new(),
            encoder: Encoder::Auto,
            range: None,
            text_antialias: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    pub frames: u64,
    pub size: [u32; 2],
    pub rate: FrameRate,
    pub duration: Time,
    pub seconds_elapsed: f64,
    pub audio_seconds: f64,
    /// Which encoder wrote the file.
    pub encoder: String,
    /// Where the time went.
    pub timings: ExportTimings,
}

/// Seconds spent on each part of an export, on the thread running it. What's left of
/// the elapsed time is the GPU and the encoder working while this thread waited.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ExportTimings {
    /// Planning each frame and optimizing its graph (CPU).
    pub plan: f64,
    /// Recording each frame's GPU work and submitting it, the decoders included.
    pub render: f64,
    /// Waiting for frames to finish on the GPU and cross to the CPU.
    pub readback: f64,
    /// Handing frames to the encoder.
    pub encode: f64,
    /// Mixing the sound down, on a thread of its own while the frames render (so not
    /// part of the other times, nor added to them).
    pub sound: f64,
}

impl fmt::Display for ExportTimings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sound {:.2}s · plan {:.2}s · render {:.2}s · readback {:.2}s · encode {:.2}s", self.sound, self.plan, self.render, self.readback, self.encode)
    }
}

impl ExportSummary {
    pub fn frames_per_second(&self) -> f64 {
        if self.seconds_elapsed > 0.0 { self.frames as f64 / self.seconds_elapsed } else { 0.0 }
    }
}

#[derive(Debug)]
pub enum ExportError {
    Plan(String),
    Render(String),
    Encode(String),
    Io(String),
    Empty,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Plan(e) => write!(f, "planning failed: {e}"),
            ExportError::Render(e) => write!(f, "rendering failed: {e}"),
            ExportError::Encode(e) => write!(f, "encoding failed: {e}"),
            ExportError::Io(e) => write!(f, "i/o error: {e}"),
            ExportError::Empty => write!(f, "the sequence is empty"),
        }
    }
}

impl std::error::Error for ExportError {}

/// A running export. Split into steps so a UI can keep drawing (and show progress)
/// while frames are rendered and encoded.
pub struct Exporter {
    sink: Option<Box<dyn VideoSink>>,
    seq: SeqId,
    plan_options: PlanOptions,
    level: OptLevel,
    rate: FrameRate,
    duration: Time,
    size: [u32; 2],
    total: u64,
    /// Frames encoded.
    frame: u64,
    /// Frames rendered (ahead of `frame` by the readbacks in flight).
    rendered: u64,
    /// Readbacks submitted and not yet encoded, oldest first (up to [`IN_FLIGHT`]).
    pending: std::collections::VecDeque<oa_gpu::readback::PendingRead>,
    /// Staging buffers handed back by finished readbacks, for the next ones.
    spare: Vec<wgpu::Buffer>,
    /// The frame being handed to the encoder, packed (reused frame to frame).
    packed: Vec<u8>,
    /// The live WAV the mixer may write (removed at the end).
    audio_path: std::path::PathBuf,
    audio_seconds: f64,
    /// The sound being mixed beside the frames.
    mixdown: Option<mixdown::Mixdown>,
    /// Exporting the sound alone: its mix, whose progress is the export's.
    live: Option<mixdown::LiveAudio>,
    started: std::time::Instant,
    /// The timeline frame the export starts at.
    first: i64,
    /// The latest frame rendered, and its timeline time (for a live preview).
    last: Option<(GpuImage, Time)>,
    timings: ExportTimings,
    /// Levels for properties connected to the sound (only when some are).
    follower: Option<Arc<oa_audio::envelope::Follower>>,
}

impl Exporter {
    /// Opens the encoder and starts mixing the sound beside it; no video frames yet.
    pub fn start(project: &Project, seq: SeqId, out: &Path, options: &ExportOptions) -> Result<Self, ExportError> {
        let sequence = project.sequence(seq).ok_or_else(|| ExportError::Plan(format!("sequence {} not found", seq.0)))?;
        let rate = sequence.rate;
        // The part to write: all of it, or the range asked for (clamped to the timeline,
        // starting on a frame).
        let whole = sequence.duration();
        let (first, end) = match options.range {
            Some(r) => (rate.frame_at(r.start.max(Time::ZERO)), r.end().min(whole)),
            None => (0, whole),
        };
        let start = rate.frame_start(first);
        let duration = end - start;
        if duration <= Time::ZERO {
            return Err(ExportError::Empty);
        }
        let total = (duration.as_seconds_f64() * rate.as_f64()).ceil() as u64;

        // Encoders need even dimensions; the canvas is even, but a scaled one may not be.
        let canvas = options.variant.and_then(|v| sequence.variant(v)).unwrap_or_else(|| sequence.active()).size;
        let size = [
            (((canvas.width as f64 * options.scale).round() as u32).max(2) / 2) * 2,
            (((canvas.height as f64 * options.scale).round() as u32).max(2) / 2) * 2,
        ];
        let scale = size[0] as f64 / canvas.width as f64;

        // The sound: mixed on its own thread while the frames render, into whatever the
        // encoder takes (see mixdown.rs). Exactly as long as the frames: the clock that
        // counts is the frame count.
        let audio_path = out.with_extension("export-audio.wav");
        let format = AudioFormat::stereo_48k();
        let samples = wav::samples_for_frames(rate, first, total, format.sample_rate);
        let audio_only = options.codec == VideoCodec::Audio;
        if audio_only && options.audio.is_empty() {
            return Err(ExportError::Plan("there's no sound to export".into()));
        }
        // The sound alone: a WAV is the live file itself; anything else is encoded
        // straight into the output.
        let extension = out.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_ascii_lowercase();
        let sound_only = audio_only.then(|| mixdown::sound_only_codec(&extension));
        let live_path = if sound_only == Some(None) { out.to_path_buf() } else { audio_path.clone() };
        let live = match options.audio.is_empty() {
            true => None,
            false => Some(mixdown::LiveAudio::create(live_path, format, samples)?),
        };
        let follower = (project.follows_sound() && !options.audio.is_empty()).then(|| Arc::new(follower_for(&options.audio)));

        let (sink, target) = match &sound_only {
            Some(None) => (None, Some(AudioTarget::LiveWav)),
            Some(Some(args)) => (None, Some(AudioTarget::Encoded { path: out.to_path_buf(), args: args.clone() })),
            None => {
                let sink = open_sink(options, out, size, rate, live.as_ref())?;
                let target = sink.audio_target();
                (Some(sink), target)
            }
        };
        let mixdown = live.as_ref().zip(target).map(|(live, target)| {
            let state = oa_audio::MixState { clips: options.audio.clone(), buses: options.buses.clone(), end: end.max(options.audio.iter().map(|c| c.range.end()).max().unwrap_or(Time::ZERO)) };
            mixdown::Mixdown::start(state, start, live, target)
        });
        let audio_seconds = match (&live, &mixdown) {
            (Some(live), Some(_)) => live.seconds(),
            _ => 0.0,
        };
        Ok(Exporter {
            sink,
            live: live.filter(|_| audio_only),
            seq,
            plan_options: PlanOptions {
                variant: options.variant,
                render_scale: scale,
                use_proxies: false,
                key_context: KeyContext::default(),
                text_supersample: if options.text_antialias { 2 } else { 1 },
                see_through: false,
            },
            level: if options.reference { OptLevel::Reference } else { OptLevel::Full },
            rate,
            duration,
            size,
            total,
            frame: 0,
            rendered: 0,
            pending: Default::default(),
            spare: Vec::new(),
            packed: Vec::new(),
            audio_path,
            audio_seconds,
            mixdown,
            started: std::time::Instant::now(),
            first,
            last: None,
            timings: ExportTimings::default(),
            follower,
        })
    }

    /// (frames done, frames total)
    /// The latest frame rendered (in the working format) and where it is on the timeline:
    /// a live preview of the export.
    pub fn last_frame(&self) -> Option<&(GpuImage, Time)> {
        self.last.as_ref()
    }

    pub fn progress(&self) -> (u64, u64) {
        (self.frame, self.total)
    }

    pub fn is_done(&self) -> bool {
        self.frame >= self.total
    }

    /// Renders and encodes up to `count` more frames. Returns `true` once finished.
    ///
    /// Pipelined: frame N's readback is only waited for after frame N+1 has been
    /// rendered and submitted, so the GPU works on the next picture while this one
    /// crosses to the CPU and into the encoder.
    pub fn step(
        &mut self,
        project: &Project,
        ctx: &Arc<GpuContext>,
        renderer: &mut Renderer,
        registry: &Registry,
        source: &mut dyn FrameSource,
        count: u64,
    ) -> Result<bool, ExportError> {
        // The sound alone: nothing to render; the mix's progress is the export's (in
        // frames of the timeline, as for a video).
        if let Some(live) = &self.live {
            if !live.is_done() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.frame = match live.is_done() {
                true => self.total,
                false => (live.written() as u128 * self.total as u128 / live.total_frames.max(1) as u128) as u64,
            };
            self.rendered = self.frame;
            return Ok(self.is_done());
        }
        // An export needs every frame exactly: wait for shaders, glyphs and stills that
        // an interactive renderer would otherwise still be preparing in the background.
        let was_waiting = std::mem::replace(&mut renderer.options.wait, true);
        let result = self.step_frames(project, ctx, renderer, registry, source, count);
        renderer.options.wait = was_waiting;
        result
    }

    fn step_frames(
        &mut self,
        project: &Project,
        ctx: &Arc<GpuContext>,
        renderer: &mut Renderer,
        registry: &Registry,
        source: &mut dyn FrameSource,
        count: u64,
    ) -> Result<bool, ExportError> {
        for _ in 0..count {
            if self.rendered >= self.total {
                break;
            }
            let t = self.rate.frame_start(self.first + self.rendered as i64);
            let clock = std::time::Instant::now();
            let plan = || plan_frame(project, self.seq, t, &self.plan_options, registry);
            let planned = match &self.follower {
                Some(f) => oa_params::signal::with(f.at(t), plan),
                None => plan(),
            }
            .map_err(|e| ExportError::Plan(e.to_string()))?;
            let graph = optimize(&planned.graph, self.level, KeyContext::default());
            self.timings.plan += clock.elapsed().as_secs_f64();
            let clock = std::time::Instant::now();
            let image = renderer.render(&graph, registry, source).map_err(|e| ExportError::Render(e.to_string()))?;
            let rgba = self.sink.as_ref().is_some_and(|s| s.wants_rgba());
            let read = if rgba {
                // Formats that keep transparency: one straight-alpha RGBA plane.
                let plane = renderer.run(|gpu| gpu.to_rgba8(&image)).map_err(|e| ExportError::Render(e.to_string()))?;
                oa_gpu::readback::start_read_reusing(ctx, &[(&plane, image.size, 4)], &mut self.spare)
            } else {
                let (luma, chroma) = renderer.run(|gpu| {
                    (gpu.to_nv12_plane(&image, Nv12Plane::Luma), gpu.to_nv12_plane(&image, Nv12Plane::Chroma))
                });
                let luma = luma.map_err(|e| ExportError::Render(e.to_string()))?;
                let chroma = chroma.map_err(|e| ExportError::Render(e.to_string()))?;
                let half = [image.size[0] / 2, image.size[1] / 2];
                oa_gpu::readback::start_read_reusing(ctx, &[(&luma, image.size, 1), (&chroma, half, 2)], &mut self.spare)
            };
            self.timings.render += clock.elapsed().as_secs_f64();
            self.last = Some((image.clone(), t));
            self.rendered += 1;
            self.pending.push_back(read);
            // Several frames in flight: the GPU renders ahead while older frames cross to
            // the CPU and into the encoder, so neither waits on the other.
            while self.pending.len() > IN_FLIGHT
                && let Some(oldest) = self.pending.pop_front()
            {
                self.encode(ctx, oldest)?;
            }
        }
        if self.rendered >= self.total {
            while let Some(read) = self.pending.pop_front() {
                self.encode(ctx, read)?;
            }
        }
        Ok(self.is_done())
    }

    /// Hands one read-back frame to the encoder: its planes packed one after another in
    /// a reused buffer (NV12 as the encoder wants it: all of Y, then the UV rows).
    fn encode(&mut self, ctx: &GpuContext, read: oa_gpu::readback::PendingRead) -> Result<(), ExportError> {
        let clock = std::time::Instant::now();
        let starts = read.wait_packed(ctx, &mut self.packed, &mut self.spare).map_err(|e| ExportError::Render(e.to_string()))?;
        self.timings.readback += clock.elapsed().as_secs_f64();
        // Both encoders already work in parallel with this thread (Media Foundation's is
        // asynchronous, ffmpeg is its own process behind a pipe): a thread of our own for
        // the hand-over measured no faster (bench_export), so it's handed over here.
        let clock = std::time::Instant::now();
        let sink = self.sink.as_mut().ok_or_else(|| ExportError::Encode("export already finished".into()))?;
        match starts.as_slice() {
            [_] => sink.push_rgba(&self.packed)?,
            [_, chroma, ..] => {
                let (luma, chroma) = self.packed.split_at(*chroma);
                sink.push_nv12(luma, chroma)?
            }
            [] => return Err(ExportError::Render("nothing was read back".into())),
        }
        self.timings.encode += clock.elapsed().as_secs_f64();
        self.frame += 1;
        Ok(())
    }

    /// Which encoder is writing the file.
    pub fn encoder(&self) -> String {
        match (&self.sink, &self.live) {
            (Some(s), _) => s.describe(),
            (None, Some(_)) => "the sound alone (ffmpeg, or a WAV as it is)".into(),
            _ => String::new(),
        }
    }

    /// Closes the encoder and reports what was written.
    pub fn finish(mut self) -> Result<ExportSummary, ExportError> {
        let encoder = self.encoder();
        // The sound first: the encoder's last step takes all of it. An export stopped
        // early stops the mix where it is (the rest is cut to the picture anyway).
        if let Some(mut mixdown) = self.mixdown.take() {
            if self.frame < self.total {
                mixdown.stop();
            }
            let mixed = mixdown.finish();
            self.timings.sound = mixdown.seconds;
            if let Err(e) = mixed {
                let _ = std::fs::remove_file(&self.audio_path);
                return Err(e);
            }
        }
        let finished = self.sink.take().map_or(Ok(()), |sink| sink.finish());
        let _ = std::fs::remove_file(&self.audio_path);
        finished?;
        Ok(ExportSummary {
            frames: self.frame,
            size: self.size,
            rate: self.rate,
            duration: self.duration,
            seconds_elapsed: self.started.elapsed().as_secs_f64(),
            audio_seconds: self.audio_seconds,
            encoder,
            timings: self.timings,
        })
    }
}

impl Drop for Exporter {
    /// A canceled export: the mixer stops first (it may be writing to the sink's files),
    /// then the encoder, and the temporary files go.
    fn drop(&mut self) {
        drop(self.mixdown.take());
        drop(self.sink.take());
        let _ = std::fs::remove_file(&self.audio_path);
    }
}

/// Opens the encoder `options` ask for. Automatic and Hardware (`MediaFoundation`) both
/// go down the same list and take the first that works: Media Foundation (Windows), the
/// GPU's encoder through ffmpeg (NVENC, VA-API, Quick Sync…, each tried on a few blank
/// frames first), then ffmpeg's software encoder — so asking for the GPU never fails an
/// export on a computer (or an OS) without one. Only H.264 and HEVC have GPU encoders.
fn open_sink(options: &ExportOptions, out: &Path, size: [u32; 2], rate: FrameRate, audio: Option<&mixdown::LiveAudio>) -> Result<Box<dyn VideoSink>, ExportError> {
    let software = || -> Result<Box<dyn VideoSink>, ExportError> { Ok(Box::new(FfmpegSink::start(out, size, rate, options.codec, options.crf, audio.is_some())?)) };
    if options.encoder == Encoder::Ffmpeg || !matches!(options.codec, VideoCodec::H264 | VideoCodec::Hevc) {
        return software();
    }
    // Constant-quality factor → average bitrate for encoders that only take a bitrate:
    // crf 18 ≈ 0.12 bits per pixel, halving every 6 steps like x264's scale.
    let bits_per_pixel = 0.12 * 2f64.powf((18.0 - options.crf as f64) / 6.0);
    #[cfg(windows)]
    if let Ok(sink) = MfSink::start(out, size, rate, options.codec, bits_per_pixel, audio) {
        return Ok(Box::new(sink));
    }
    let bitrate = (bits_per_pixel * size[0] as f64 * size[1] as f64 * rate.num as f64 / rate.den.max(1) as f64) as u64;
    if let Some(hw) = ffmpeg::find_hardware(options.codec, size)
        && let Ok(sink) = FfmpegSink::start_hardware(out, size, rate, options.codec, audio.is_some(), hw, bitrate)
    {
        return Ok(Box::new(sink));
    }
    software()
}

/// Renders every frame of `seq` into `out`, muxing `options.audio` alongside.
///
/// `progress` is called with (frame, total) as it goes; return `false` from it to stop
/// early (the part written so far is still muxed).
#[allow(clippy::too_many_arguments)]
pub fn export(
    project: &Project,
    seq: SeqId,
    out: &Path,
    options: &ExportOptions,
    ctx: &Arc<GpuContext>,
    renderer: &mut Renderer,
    registry: &Registry,
    source: &mut dyn FrameSource,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<ExportSummary, ExportError> {
    let mut exporter = Exporter::start(project, seq, out, options)?;
    loop {
        let done = exporter.step(project, ctx, renderer, registry, source, 1)?;
        let (frame, total) = exporter.progress();
        if done || !progress(frame, total) {
            break;
        }
    }
    exporter.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::*;
    use oa_time::TimeRange;

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    /// Asking for the GPU encoder (or Automatic) gets a working one on any computer and
    /// any OS — Media Foundation, the GPU through ffmpeg, or software as the last resort —
    /// never an error because the preferred one isn't there.
    #[test]
    fn hardware_export_falls_back_instead_of_failing() {
        if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-export-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (size, rate) = ([128u32, 72u32], FrameRate::new(30, 1));
        for (encoder, name) in [(Encoder::MediaFoundation, "hardware.mp4"), (Encoder::Auto, "auto.mp4")] {
            let out = dir.join(name);
            let options = ExportOptions { encoder, ..Default::default() };
            let mut sink = open_sink(&options, &out, size, rate, None).unwrap_or_else(|e| panic!("{encoder:?}: {e}"));
            let described = sink.describe();
            let (luma, chroma) = (vec![128u8; 128 * 72], vec![128u8; 128 * 72 / 2]);
            for _ in 0..15 {
                sink.push_nv12(&luma, &chroma).unwrap();
            }
            sink.finish().unwrap_or_else(|e| panic!("{described}: {e}"));
            assert!(std::fs::metadata(&out).is_ok_and(|m| m.len() > 0), "{described} wrote nothing");
            eprintln!("{encoder:?} → {described}");
        }
        // The GPU through ffmpeg, where this computer has one ffmpeg can use.
        if let Some(hw) = find_hardware(VideoCodec::H264, size) {
            let out = dir.join("gpu.mp4");
            let mut sink: Box<dyn VideoSink> = Box::new(FfmpegSink::start_hardware(&out, size, rate, VideoCodec::H264, false, hw, 500_000).unwrap());
            let (luma, chroma) = (vec![128u8; 128 * 72], vec![128u8; 128 * 72 / 2]);
            for _ in 0..15 {
                sink.push_nv12(&luma, &chroma).unwrap();
            }
            let described = sink.describe();
            sink.finish().unwrap_or_else(|e| panic!("{described}: {e}"));
            assert!(std::fs::metadata(&out).is_ok_and(|m| m.len() > 0));
            eprintln!("GPU through ffmpeg → {described}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A compound clip's sound plays where the clip sits, cut to the clip.
    #[test]
    fn compound_clips_carry_their_sound() {
        let p = AspectPreset::by_id("landscape-16x9").unwrap();
        let variant = |id| FormatVariant { id: VariantId(id), name: "wide".into(), size: p.size(1080), overrides: Default::default() };
        let mut project = Project::new("t");
        let media = MediaId(1);
        let info = MediaInfo { width: 0, height: 0, duration: secs(60.0), rate: None, has_video: false, has_audio: true, still: false, ..Default::default() };
        project.media.insert(media, Arc::new(MediaRef { id: media, path: "a.wav".into(), fingerprint: None, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
        // Inside: a sound from 1 s to 5 s.
        let mut inner = Sequence::new(SeqId(2), "Group", FrameRate::FPS_30, variant(3));
        let mut a = Track::new(TrackId(4), "A1", TrackKind::Audio);
        a.items.push(Item::new(ItemId(5), "sound", ItemKind::Media { media }, TimeRange::new(secs(1.0), secs(4.0))));
        inner.tracks.push(Arc::new(a));
        project.sequences.insert(SeqId(2), Arc::new(inner));
        // Outside: the group at 10 s, trimmed to start 2 s in and last 2 s.
        let mut main = Sequence::new(SeqId(6), "Main", FrameRate::FPS_30, variant(7));
        let mut v = Track::new(TrackId(8), "V1", TrackKind::Video);
        let mut nest = Item::new(ItemId(9), "nest", ItemKind::Nested { sequence: SeqId(2) }, TimeRange::new(secs(10.0), secs(2.0)));
        nest.time_map.source_in = secs(2.0);
        v.items.push(nest);
        main.tracks.push(Arc::new(v));
        project.sequences.insert(SeqId(6), Arc::new(main));

        let (clips, skipped) = audio_clips(&project, SeqId(6), |_| Some("a.wav".into()));
        assert!(skipped.is_empty());
        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        // Inner 1–5 s is outer 9–13 s, cut to the clip's 10–12 s: 1 s into the sound.
        assert_eq!((c.range.start, c.range.end()), (secs(10.0), secs(12.0)));
        assert_eq!(c.source_in, secs(1.0));
        assert_eq!(c.clock_start, secs(9.0));

        // Twice as fast (the clip now 1 s long, still from 2 s inside): inner 1–5 s is
        // heard at 9.5–11.5 s, cut to 10–11 s, the sound 1 s in and playing at 2×.
        Arc::make_mut(Arc::make_mut(project.sequences.get_mut(&SeqId(6)).unwrap()).tracks.get_mut(0).unwrap()).items[0] = {
            let mut fast = Item::new(ItemId(9), "nest", ItemKind::Nested { sequence: SeqId(2) }, TimeRange::new(secs(10.0), secs(1.0)));
            fast.time_map.source_in = secs(2.0);
            fast.time_map.speed = oa_time::Rational::new(2, 1);
            fast
        };
        let (clips, skipped) = audio_clips(&project, SeqId(6), |_| Some("a.wav".into()));
        assert!(skipped.is_empty(), "a sped-up compound clip keeps its sound");
        let c = &clips[0];
        assert_eq!((c.range.start, c.range.end()), (secs(10.0), secs(11.0)));
        assert_eq!(c.source_in, secs(1.0));
        assert_eq!(c.speed, 2.0);
        assert_eq!(c.clock_start, secs(9.5));

        // A sound effect on the compound clip itself: a group over its clip, heard.
        Arc::make_mut(Arc::make_mut(project.sequences.get_mut(&SeqId(6)).unwrap()).tracks.get_mut(0).unwrap()).items[0]
            .effects
            .push(oa_doc::EffectInstance::new(oa_doc::EffectId(40), "oa.audio.reverb"));
        let (clips, buses, _) = audio_mix(&project, SeqId(6), |_| Some("a.wav".into()));
        assert_eq!(buses.len(), 1);
        assert_eq!(buses[0].members.as_deref(), Some(&[clips[0].id][..]), "over its own clip");
        assert_eq!(buses[0].range, TimeRange::new(secs(10.0), secs(1.0)));
    }

    /// An audio effect track becomes a bus above the audio tracks drawn below it (and the
    /// sound of picture tracks); tracks drawn above it sit higher, out of its reach.
    #[test]
    fn audio_effect_tracks_become_buses_over_what_is_below_them() {
        let p = AspectPreset::by_id("landscape-16x9").unwrap();
        let mut project = Project::new("t");
        let media = MediaId(1);
        let info = MediaInfo { width: 0, height: 0, duration: secs(60.0), rate: None, has_video: false, has_audio: true, still: false, ..Default::default() };
        project.media.insert(media, Arc::new(MediaRef { id: media, path: "a.wav".into(), fingerprint: None, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
        let mut main = Sequence::new(SeqId(6), "Main", FrameRate::FPS_30, FormatVariant { id: VariantId(7), name: "wide".into(), size: p.size(1080), overrides: Default::default() });
        let sound = |id: u64| Item::new(ItemId(id), "sound", ItemKind::Media { media }, TimeRange::new(secs(0.0), secs(4.0)));
        // Rows, top down: A1, the effect track, A2.
        let mut a1 = Track::new(TrackId(10), "A1", TrackKind::Audio);
        a1.items.push(sound(11));
        let mut fx = Track::effects(TrackId(20), "AFX1", TrackKind::Audio);
        let mut container = Item::new(ItemId(21), "Effects", ItemKind::Adjustment, TimeRange::new(secs(1.0), secs(2.0)));
        container.effects.push(oa_doc::EffectInstance::new(oa_doc::EffectId(22), "oa.audio.reverb"));
        fx.items.push(container);
        let mut a2 = Track::new(TrackId(30), "A2", TrackKind::Audio);
        a2.items.push(sound(31));
        let mut v1 = Track::new(TrackId(40), "V1", TrackKind::Video);
        v1.items.push(sound(41));
        main.tracks = vec![Arc::new(v1), Arc::new(a1), Arc::new(fx), Arc::new(a2)];
        project.sequences.insert(SeqId(6), Arc::new(main));

        let (clips, buses, _) = audio_mix(&project, SeqId(6), |_| Some("a.wav".into()));
        assert_eq!(buses.len(), 1);
        let bus = &buses[0];
        assert_eq!((bus.id, bus.range, bus.effects.len()), (21, TimeRange::new(secs(1.0), secs(2.0)), 1));
        let layer = |id: u64| clips.iter().find(|c| c.id == id).unwrap().layer;
        assert!(layer(31) < bus.layer, "A2, drawn below it, runs through it");
        assert!(layer(41) < bus.layer, "so does the sound of picture tracks");
        assert!(layer(11) > bus.layer, "A1, drawn above it, doesn't");
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use oa_doc::*;
    use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamSource, Value};
    use oa_time::TimeRange;

    /// A clip's volume is keyframable, and the curve reaches the mixer intact — the
    /// fade the user drew is the fade that plays and exports.
    #[test]
    fn keyframed_volume_reaches_the_mixer() {
        let p = AspectPreset::by_id("landscape-16x9").expect("preset");
        let variant = FormatVariant { id: VariantId(1), name: "wide".into(), size: p.size(1080), overrides: Default::default() };
        let mut project = Project::new("t");
        let media = MediaId(2);
        let info = MediaInfo { width: 0, height: 0, duration: Time::from_seconds(60), rate: None, has_video: false, has_audio: true, still: false, ..Default::default() };
        project.media.insert(media, Arc::new(MediaRef { id: media, path: "a.wav".into(), fingerprint: None, info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
        let mut seq = Sequence::new(SeqId(3), "Main", FrameRate::FPS_30, variant);
        let mut track = Track::new(TrackId(4), "A1", TrackKind::Audio);
        let mut item = Item::new(ItemId(5), "sound", ItemKind::Media { media }, TimeRange::new(Time::ZERO, Time::from_seconds(4)));
        // Fade up over the first two seconds.
        item.params.set(
            oa_doc::schema::AUDIO_GAIN,
            ParamSource::Animated(Curve::new(
                KeyframeAnchor::ClipStart,
                vec![
                    Keyframe::linear(Time::ZERO, Value::Float(-60.0)),
                    Keyframe::linear(Time::from_seconds(2), Value::Float(0.0)),
                ],
            )),
        );
        track.items.push(item);
        seq.tracks.push(Arc::new(track));
        project.sequences.insert(SeqId(3), Arc::new(seq));

        let (clips, _) = audio_clips(&project, SeqId(3), |_| Some("a.wav".into()));
        let clip = clips.first().expect("one audible clip");
        let db = |t: f64| {
            clip.gain_db
                .eval(&oa_params::EvalContext::at(Time::from_seconds_f64(t), Time::from_seconds_f64(t)))
                .as_float()
                .expect("a number")
        };
        assert!((db(0.0) + 60.0).abs() < 1e-6, "silent at the start: {}", db(0.0));
        assert!((db(1.0) + 30.0).abs() < 0.01, "halfway up at 1 s: {}", db(1.0));
        assert!(db(3.0).abs() < 1e-6, "unity after the fade: {}", db(3.0));
    }
}
