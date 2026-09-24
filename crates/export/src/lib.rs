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
mod wav;
/// Renders a sound source to a 16-bit WAV (used for export, and for transcribing captions).
pub use wav::write as write_wav;

pub use ffmpeg::{FfmpegSink, VideoCodec};
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
/// (or is missing). Disabled tracks and items are silent. Clips at a speed other than
/// 1× are skipped for now (the mixer doesn't resample) and returned in the second list.
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
                        buses.push(oa_audio::AudioBus { id: item.id.0, range: item.range, effects, layer });
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
                }
                continue;
            }
            let oa_doc::ItemKind::Media { media } = item.kind else { continue };
            if !item.audio_enabled() {
                continue; // its sound was extracted to its own clip
            }
            let Some(path) = audio_path(media) else { continue };
            // Freeze frames and reverse play have no sound (yet).
            let speed = item.time_map.speed.num() as f64 / item.time_map.speed.den() as f64;
            if speed <= 0.0 {
                skipped.push(item.id);
                continue;
            }
            let mut clip = oa_audio::AudioClip::new(item.id.0, media.0, path, item.range, item.time_map.source_in);
            clip.speed = speed;
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
    if item.time_map.speed.num() != item.time_map.speed.den() {
        skipped.push(item.id); // retimed compound clips are silent (for now)
        return;
    }
    let mut inner = Vec::new();
    collect_audio(project, sequence, audio_path, depth + 1, &mut inner, &mut Vec::new(), skipped);
    let shift = item.range.start - item.time_map.source_in;
    let (lo, hi) = (item.range.start, item.range.end());
    for mut c in inner {
        let mut start = c.range.start + shift;
        let mut end = c.range.end() + shift;
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
        c.clock_start += shift;
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
}

/// Which encoder to use.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Encoder {
    /// The platform's hardware encoder when it can do the codec, otherwise ffmpeg.
    #[default]
    Auto,
    /// Media Foundation's sink writer (Windows).
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
    /// Mixing the sound down to a file, before the first frame (grows with the length).
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
    audio_path: std::path::PathBuf,
    audio_seconds: f64,
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
    /// Prepares the encoder and renders the sound; no video frames yet.
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

        // Sound first: it is quick, and the encoder takes it as a second input.
        let audio_path = out.with_extension("export-audio.wav");
        let mixing = std::time::Instant::now();
        let audio_seconds = if options.audio.is_empty() {
            0.0
        } else {
            let state = oa_audio::MixState { clips: options.audio.clone(), buses: options.buses.clone(), end: end.max(options.audio.iter().map(|c| c.range.end()).max().unwrap_or(Time::ZERO)) };
            let mut timeline = oa_audio::TimelineAudio::with_handle(oa_audio::MixHandle::new(state), AudioFormat::stereo_48k(), start);
            wav::write(&audio_path, &mut timeline, duration).map_err(|e| ExportError::Io(e.to_string()))?
        };
        let mixing = mixing.elapsed().as_secs_f64();
        let follower = (project.follows_sound() && !options.audio.is_empty()).then(|| Arc::new(follower_for(&options.audio)));

        let audio = (audio_seconds > 0.0).then_some(audio_path.as_path());
        let sink = open_sink(options, out, size, rate, audio)?;
        Ok(Exporter {
            sink: Some(sink),
            seq,
            plan_options: PlanOptions {
                variant: options.variant,
                render_scale: scale,
                use_proxies: false,
                key_context: KeyContext::default(),
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
            started: std::time::Instant::now(),
            first,
            last: None,
            timings: ExportTimings { sound: mixing, ..Default::default() },
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
        self.sink.as_ref().map(|s| s.describe()).unwrap_or_default()
    }

    /// Closes the encoder and reports what was written.
    pub fn finish(mut self) -> Result<ExportSummary, ExportError> {
        let encoder = self.encoder();
        if let Some(sink) = self.sink.take() {
            sink.finish()?;
        }
        let _ = std::fs::remove_file(&self.audio_path);
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

/// Opens the encoder `options` ask for. `Auto` tries the platform encoder first and falls
/// back to ffmpeg if it can't take this codec/size.
fn open_sink(options: &ExportOptions, out: &Path, size: [u32; 2], rate: FrameRate, audio: Option<&Path>) -> Result<Box<dyn VideoSink>, ExportError> {
    let ffmpeg = || -> Result<Box<dyn VideoSink>, ExportError> { Ok(Box::new(FfmpegSink::start(out, size, rate, options.codec, options.crf, audio)?)) };
    // Constant-quality factor → average bitrate for encoders that only take a bitrate:
    // crf 18 ≈ 0.12 bits per pixel, halving every 6 steps like x264's scale.
    let bits_per_pixel = 0.12 * 2f64.powf((18.0 - options.crf as f64) / 6.0);
    match options.encoder {
        Encoder::Ffmpeg => ffmpeg(),
        #[cfg(windows)]
        Encoder::MediaFoundation => Ok(Box::new(MfSink::start(out, size, rate, options.codec, bits_per_pixel, audio)?)),
        #[cfg(windows)]
        Encoder::Auto if matches!(options.codec, VideoCodec::H264 | VideoCodec::Hevc) => match MfSink::start(out, size, rate, options.codec, bits_per_pixel, audio) {
            Ok(sink) => Ok(Box::new(sink)),
            Err(_) => ffmpeg(),
        },
        #[cfg(not(windows))]
        Encoder::MediaFoundation => Err(ExportError::Encode("Media Foundation is only available on Windows".into())),
        _ => {
            let _ = bits_per_pixel;
            ffmpeg()
        }
    }
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
