//! Previews drawn on timeline clips: a filmstrip of frames for pictures and a waveform
//! for sound. Both are made once per media file on worker threads (ffmpeg), so the
//! timeline never waits for them — a clip shows its plain color until they're in.

use eframe::egui;
use oa_doc::MediaId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// Frames across a filmstrip image (it wraps into rows).
const COLS: usize = 10;
/// Width of one filmstrip frame, px.
const FRAME_W: u32 = 96;
/// Waveform peaks per second of media.
pub const PEAKS_PER_SECOND: f64 = 100.0;

enum Slot<T> {
    Loading(Receiver<Option<T>>),
    Ready(T),
    Failed,
}

/// Frames sampled evenly across a file, in one texture (`COLS` per row).
pub struct Strip {
    pub texture: egui::TextureHandle,
    pub frames: usize,
    /// Seconds of media the frames span.
    pub duration: f64,
    /// Frame width / height.
    pub aspect: f32,
}

impl Strip {
    /// UV rect of the frame nearest `seconds` into the file.
    pub fn uv(&self, seconds: f64) -> egui::Rect {
        let i = ((seconds / self.duration.max(1e-6)) * self.frames as f64).floor().clamp(0.0, (self.frames - 1) as f64) as usize;
        let rows = self.frames.div_ceil(COLS);
        let (c, r) = (i % COLS, i / COLS);
        let (w, h) = (1.0 / COLS as f32, 1.0 / rows as f32);
        egui::Rect::from_min_size(egui::pos2(c as f32 * w, r as f32 * h), egui::vec2(w, h))
    }
}

/// (min, max) sample per 1/`PEAKS_PER_SECOND` s, mono.
pub type Peaks = Arc<Vec<(f32, f32)>>;

struct StripPixels {
    image: egui::ColorImage,
    frames: usize,
    duration: f64,
    aspect: f32,
}

#[derive(Default)]
pub struct ClipPreviews {
    strips: HashMap<MediaId, Slot<Strip>>,
    strip_jobs: HashMap<MediaId, Receiver<Option<StripPixels>>>,
    waves: HashMap<MediaId, Slot<Peaks>>,
    envelopes: HashMap<MediaId, Slot<Arc<oa_audio::envelope::Envelope>>>,
}

impl ClipPreviews {
    /// The filmstrip for `media`, starting it if needed. `size` is the picture's size
    /// (for the aspect), `duration` seconds (0 for a still).
    /// Whether `media`'s frames are still being extracted.
    pub fn strip_pending(&self, media: MediaId) -> bool {
        self.strip_jobs.contains_key(&media)
    }

    pub fn strip(&mut self, ctx: &egui::Context, media: MediaId, path: &Path, size: [u32; 2], duration: f64) -> Option<&Strip> {
        if !self.strips.contains_key(&media) && !self.strip_jobs.contains_key(&media) {
            let (tx, rx) = channel();
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(make_strip(&path, size, duration));
            });
            self.strip_jobs.insert(media, rx);
        }
        if let Some(rx) = self.strip_jobs.get(&media) {
            match rx.try_recv() {
                Ok(Some(px)) => {
                    let texture = ctx.load_texture(format!("strip-{}", media.0), px.image, egui::TextureOptions::LINEAR);
                    self.strips.insert(media, Slot::Ready(Strip { texture, frames: px.frames, duration: px.duration, aspect: px.aspect }));
                    self.strip_jobs.remove(&media);
                }
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    self.strips.insert(media, Slot::Failed);
                    self.strip_jobs.remove(&media);
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        match self.strips.get(&media) {
            Some(Slot::Ready(s)) => Some(s),
            _ => None,
        }
    }

    /// The waveform peaks for `media`, starting them if needed.
    pub fn peaks(&mut self, ctx: &egui::Context, media: MediaId, path: &Path) -> Option<Peaks> {
        let slot = self.waves.entry(media).or_insert_with(|| {
            let (tx, rx) = channel();
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(make_peaks(&path).map(Arc::new));
            });
            Slot::Loading(rx)
        });
        if let Slot::Loading(rx) = slot {
            match rx.try_recv() {
                Ok(Some(p)) => *slot = Slot::Ready(p),
                Ok(None) | Err(TryRecvError::Disconnected) => *slot = Slot::Failed,
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        match slot {
            Slot::Ready(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// The loudness envelope of `media` (for properties connected to the sound),
    /// starting its analysis if needed.
    pub fn envelope(&mut self, ctx: &egui::Context, media: MediaId, path: &Path) -> Option<Arc<oa_audio::envelope::Envelope>> {
        let slot = self.envelopes.entry(media).or_insert_with(|| {
            let (tx, rx) = channel();
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(oa_audio::envelope::Envelope::analyze(&path).map(Arc::new));
            });
            Slot::Loading(rx)
        });
        if let Slot::Loading(rx) = slot {
            match rx.try_recv() {
                Ok(Some(e)) => *slot = Slot::Ready(e),
                Ok(None) | Err(TryRecvError::Disconnected) => *slot = Slot::Failed,
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        match slot {
            Slot::Ready(e) => Some(e.clone()),
            _ => None,
        }
    }

    /// Whether `media`'s loudness envelope is still being analyzed.
    pub fn envelope_pending(&self, media: MediaId) -> bool {
        matches!(self.envelopes.get(&media), Some(Slot::Loading(_)))
    }

    /// Forgets a file's previews (after a relink, say).
    pub fn forget(&mut self, media: MediaId) {
        self.strips.remove(&media);
        self.strip_jobs.remove(&media);
        self.waves.remove(&media);
        self.envelopes.remove(&media);
    }
}

fn ffmpeg(args: &[&str], input: &PathBuf, rest: &[&str]) -> Option<Vec<u8>> {
    let out = oa_media::tool("ffmpeg").args(["-v", "error", "-nostdin"]).args(args).arg("-i").arg(input).args(rest).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn make_strip(path: &Path, size: [u32; 2], duration: f64) -> Option<StripPixels> {
    let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
    let w = FRAME_W;
    let h = ((w as f32 / aspect / 2.0).round() as u32 * 2).max(2);
    // One frame per second, 1..=60, spread evenly over the file.
    let frames = if duration <= 0.0 { 1 } else { (duration.ceil() as usize).clamp(1, 60) };
    let rows = frames.div_ceil(COLS);
    let filter = if frames == 1 {
        format!("scale={w}:{h},tile={COLS}x{rows}")
    } else {
        format!("fps={frames}/{duration:.4},scale={w}:{h},tile={COLS}x{rows}")
    };
    let path = path.to_path_buf();
    let bytes = ffmpeg(&[], &path, &["-vf", &filter, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])?;
    let (iw, ih) = (w as usize * COLS, h as usize * rows);
    if bytes.len() < iw * ih * 4 {
        return None;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([iw, ih], &bytes[..iw * ih * 4]);
    Some(StripPixels { image, frames, duration: duration.max(1e-3), aspect })
}

fn make_peaks(path: &Path) -> Option<Vec<(f32, f32)>> {
    const RATE: f64 = 4000.0;
    let path = path.to_path_buf();
    let bytes = ffmpeg(&[], &path, &["-vn", "-ac", "1", "-ar", "4000", "-f", "f32le", "-"])?;
    let per = (RATE / PEAKS_PER_SECOND) as usize;
    let samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    Some(
        samples
            .chunks(per)
            .map(|c| c.iter().fold((0f32, 0f32), |(lo, hi), &x| (lo.min(x), hi.max(x))))
            .collect(),
    )
}
