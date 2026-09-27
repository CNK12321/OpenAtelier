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
pub const COLS: usize = 10;
/// Width of one filmstrip frame, px.
pub const FRAME_W: u32 = 96;
/// Waveform peaks per second of media.
pub const PEAKS_PER_SECOND: f64 = 100.0;

enum Slot<T> {
    Loading(Receiver<Option<T>>),
    Ready(T),
    Failed,
}

/// Frames sampled evenly across a file, in one texture (`COLS` per row).
#[derive(Clone)]
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
    /// Compound clips' filmstrips, rendered (on the preview thread) from what the
    /// compound is — (the version shown, the strip), and a render in flight.
    compounds: HashMap<oa_doc::SeqId, (u64, Strip)>,
    compound_jobs: HashMap<oa_doc::SeqId, Receiver<Option<crate::preview_worker::StripFrames>>>,
}

/// Frames in a compound clip's filmstrip: one a second, 1 to this many.
pub const COMPOUND_FRAMES: usize = 30;

impl ClipPreviews {
    /// A compound clip's filmstrip at `version` (what the compound is now). The last one
    /// made stays up while a newer one renders; `start` asks for a render (it's handed
    /// where the result goes) — only when none is running, so a compound being edited
    /// re-renders once per finished strip, never piling up. `None` until the first is in.
    pub fn compound_strip(&mut self, ctx: &egui::Context, seq: oa_doc::SeqId, version: u64, start: impl FnOnce(std::sync::mpsc::Sender<Option<crate::preview_worker::StripFrames>>)) -> Option<&Strip> {
        if let Some(rx) = self.compound_jobs.get(&seq) {
            match rx.try_recv() {
                Ok(made) => {
                    self.compound_jobs.remove(&seq);
                    if let Some(f) = made {
                        let image = egui::ColorImage::from_rgba_unmultiplied(f.size, &f.pixels);
                        let texture = ctx.load_texture(format!("compound-strip-{}", seq.0), image, egui::TextureOptions::LINEAR);
                        self.compounds.insert(seq, (f.version, Strip { texture, frames: f.frames, duration: f.duration, aspect: f.aspect }));
                    }
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(TryRecvError::Disconnected) => {
                    self.compound_jobs.remove(&seq);
                }
            }
        }
        let current = self.compounds.get(&seq).is_some_and(|(v, _)| *v == version);
        if !current && !self.compound_jobs.contains_key(&seq) {
            let (tx, rx) = channel();
            start(tx);
            self.compound_jobs.insert(seq, rx);
        }
        self.compounds.get(&seq).map(|(_, s)| s)
    }

    /// Whether a compound's first filmstrip is still being made.
    pub fn compound_pending(&self, seq: oa_doc::SeqId) -> bool {
        self.compound_jobs.contains_key(&seq) && !self.compounds.contains_key(&seq)
    }

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
            std::thread::spawn(move || make_strip(&path, size, duration, &tx));
            self.strip_jobs.insert(media, rx);
        }
        // The strip arrives in stages (the first frame at once, the rest as they're
        // found); each replaces the last.
        while let Some(rx) = self.strip_jobs.get(&media) {
            match rx.try_recv() {
                Ok(Some(px)) => {
                    let texture = ctx.load_texture(format!("strip-{}", media.0), px.image, egui::TextureOptions::LINEAR);
                    self.strips.insert(media, Slot::Ready(Strip { texture, frames: px.frames, duration: px.duration, aspect: px.aspect }));
                }
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    if !matches!(self.strips.get(&media), Some(Slot::Ready(_))) {
                        self.strips.insert(media, Slot::Failed);
                    }
                    self.strip_jobs.remove(&media);
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    break;
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

/// Frames fetched at once while making a strip.
const STRIP_THREADS: usize = 4;

/// Makes `path`'s filmstrip — one frame per second, 1 to 60, spread over the file — and
/// sends it in stages: the first frame straight away (in every cell, so skimming shows
/// something), then the whole strip. Each frame is its own quick seek, so a long or
/// large file (hours, 4K, gigabytes) costs no more than a short one: nothing is decoded
/// but the frames shown. Sends `None` if not even the first frame could be read.
fn make_strip(path: &Path, size: [u32; 2], duration: f64, tx: &std::sync::mpsc::Sender<Option<StripPixels>>) {
    let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
    let (w, h) = (FRAME_W as usize, ((FRAME_W as f32 / aspect / 2.0).round() as usize * 2).max(2));
    let frames = if duration <= 0.0 { 1 } else { (duration.ceil() as usize).clamp(1, 60) };
    let rows = frames.div_ceil(COLS);
    let (iw, ih) = (w * COLS, h * rows);
    let path = path.to_path_buf();
    let scale = format!("scale={w}:{h}");
    // One frame at `seconds` (in the middle of its share of the file), as RGBA.
    let frame_at = |i: usize| -> Option<Vec<u8>> {
        // A drawing: drawn at the cell's size (ffmpeg may not read SVG).
        if oa_media::svg::is_svg(&path) {
            return oa_media::svg::rasterize(&path, [w as u32, h as u32]).ok();
        }
        let seconds = if frames == 1 { 0.0 } else { (i as f64 + 0.5) * duration / frames as f64 };
        let bytes = ffmpeg(&["-ss", &format!("{seconds:.3}")], &path, &["-frames:v", "1", "-vf", &scale, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])?;
        (bytes.len() >= w * h * 4).then(|| bytes[..w * h * 4].to_vec())
    };
    let mut canvas = vec![0u8; iw * ih * 4];
    let blit = |canvas: &mut [u8], i: usize, px: &[u8]| {
        let (c, r) = (i % COLS, i / COLS);
        for y in 0..h {
            let at = ((r * h + y) * iw + c * w) * 4;
            canvas[at..at + w * 4].copy_from_slice(&px[y * w * 4..(y + 1) * w * 4]);
        }
    };
    let send = |canvas: &[u8]| tx.send(Some(StripPixels { image: egui::ColorImage::from_rgba_unmultiplied([iw, ih], canvas), frames, duration: duration.max(1e-3), aspect }));
    let Some(first) = frame_at(0) else {
        let _ = tx.send(None);
        return;
    };
    for i in 0..frames {
        blit(&mut canvas, i, &first);
    }
    if send(&canvas).is_err() || frames == 1 {
        return;
    }
    // The rest, a few at a time; a frame that can't be read keeps the first one.
    let rest: Vec<usize> = (1..frames).collect();
    for batch in rest.chunks(STRIP_THREADS) {
        let got: Vec<(usize, Option<Vec<u8>>)> = std::thread::scope(|s| {
            let jobs: Vec<_> = batch.iter().map(|&i| (i, s.spawn(move || frame_at(i)))).collect();
            jobs.into_iter().map(|(i, j)| (i, j.join().ok().flatten())).collect()
        });
        for (i, px) in got {
            if let Some(px) = px {
                blit(&mut canvas, i, &px);
            }
        }
    }
    let _ = send(&canvas);
}

/// `path`'s waveform: the lowest and highest sample of each 1/`PEAKS_PER_SECOND` s. The
/// samples are folded into peaks as ffmpeg streams them, so an hour of sound holds a few
/// MB of peaks, not the ~115 MB of samples (twice over) it once buffered.
fn make_peaks(path: &Path) -> Option<Vec<(f32, f32)>> {
    use std::io::Read;
    const RATE: f64 = 4000.0;
    let per = (RATE / PEAKS_PER_SECOND) as usize;
    let mut child = oa_media::tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "4000", "-f", "f32le", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut peaks = Vec::new();
    let (mut peak, mut count) = ((0f32, 0f32), 0);
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry = 0;
    loop {
        let n = match stdout.read(&mut buf[carry..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let filled = carry + n;
        let (samples, rest) = buf[..filled].as_chunks::<4>();
        for x in samples.iter().map(|b| f32::from_le_bytes(*b)) {
            peak = (peak.0.min(x), peak.1.max(x));
            count += 1;
            if count == per {
                peaks.push(peak);
                (peak, count) = ((0.0, 0.0), 0);
            }
        }
        // A sample split across reads waits for its other bytes.
        carry = rest.len();
        buf.copy_within(filled - carry..filled, 0);
    }
    if count > 0 {
        peaks.push(peak);
    }
    child.wait().ok()?.success().then_some(peaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strip comes in two stages — the first frame in every cell at once, then each
    /// cell its own frame — and a file ffmpeg can't read says so rather than hanging.
    #[test]
    fn strips_arrive_first_frame_first() {
        if !oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-strip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("counting.mp4");
        // Five seconds whose brightness climbs second by second.
        let made = oa_media::tool("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=160x90:r=10:d=5", "-vf", "geq=lum='20+T*40':cb=128:cr=128", "-c:v", "libx264", "-g", "10"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let (tx, rx) = channel();
        make_strip(&clip, [160, 90], 5.0, &tx);
        drop(tx);
        let stages: Vec<StripPixels> = rx.iter().map(|s| s.expect("frames")).collect();
        assert_eq!(stages.len(), 2);
        let brightness = |s: &StripPixels, i: usize| {
            let (w, h) = (FRAME_W as usize, s.image.size[1]);
            s.image.pixels[(h / 2) * s.image.size[0] + (i % COLS) * w + w / 2].r()
        };
        assert_eq!(stages[0].frames, 5);
        assert!((1..5).all(|i| brightness(&stages[0], i) == brightness(&stages[0], 0)), "stage one: the first frame everywhere");
        assert!((1..5).all(|i| brightness(&stages[1], i) > brightness(&stages[1], i - 1)), "stage two: each second its own frame");

        let (tx, rx) = channel();
        make_strip(&dir.join("missing.mp4"), [160, 90], 5.0, &tx);
        assert!(rx.recv().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Streamed peaks: one per 1/100 s, loud where the sound is loud, and nothing for a
    /// file that can't be read.
    #[test]
    fn peaks_follow_the_sound() {
        if !oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-peaks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("swell.wav");
        // Three seconds: silence, then a half-loud tone, then a full one.
        let made = oa_media::tool("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "aevalsrc='if(lt(t,1),0,if(lt(t,2),0.5,1))*sin(2*PI*220*t)':s=48000:d=3"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let peaks = make_peaks(&clip).expect("peaks");
        assert!((299..=301).contains(&peaks.len()), "{} peaks", peaks.len());
        let loudest = |from: usize| peaks[from + 10..from + 90].iter().map(|p| p.1).fold(0f32, f32::max);
        assert!(loudest(0) < 0.01);
        assert!((0.45..0.55).contains(&loudest(100)), "{}", loudest(100));
        assert!(loudest(200) > 0.9);
        assert!(make_peaks(&dir.join("missing.wav")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
