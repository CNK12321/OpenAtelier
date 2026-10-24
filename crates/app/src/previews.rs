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
    /// The scaling it was made with (made again when the file's changes).
    pub scaling: oa_doc::MediaScaling,
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
    /// Drawn pixel by pixel (nearest neighbor), not smoothed.
    pixel: bool,
}

#[derive(Default)]
pub struct ClipPreviews {
    strips: HashMap<MediaId, Slot<Strip>>,
    strip_jobs: HashMap<MediaId, Receiver<Option<StripPixels>>>,
    waves: HashMap<MediaId, Slot<Peaks>>,
    envelopes: HashMap<MediaId, Slot<Arc<oa_audio::envelope::Envelope>>>,
    /// Compound clips' filmstrips, rendered (on the preview thread) from what the
    /// compound is — (the version shown, the strip), and a render in flight — by
    /// compound and cam (a multicam clip's cams have a filmstrip each).
    compounds: HashMap<(oa_doc::SeqId, Option<usize>), (u64, Strip)>,
    compound_jobs: HashMap<(oa_doc::SeqId, Option<usize>), Receiver<Option<crate::preview_worker::StripFrames>>>,
}

/// Frames in a compound clip's filmstrip: one a second, 1 to this many.
pub const COMPOUND_FRAMES: usize = 30;

impl ClipPreviews {
    /// A compound clip's filmstrip at `version` (what the compound is now). The last one
    /// made stays up while a newer one renders; `start` asks for a render (it's handed
    /// where the result goes) — only when none is running, so a compound being edited
    /// re-renders once per finished strip, never piling up. `None` until the first is in.
    /// `cam`: one cam of it only (a multicam clip's).
    pub fn compound_strip(&mut self, ctx: &egui::Context, seq: oa_doc::SeqId, cam: Option<usize>, version: u64, start: impl FnOnce(std::sync::mpsc::Sender<Option<crate::preview_worker::StripFrames>>)) -> Option<&Strip> {
        let key = (seq, cam);
        if let Some(rx) = self.compound_jobs.get(&key) {
            match rx.try_recv() {
                Ok(made) => {
                    self.compound_jobs.remove(&key);
                    if let Some(f) = made {
                        let image = egui::ColorImage::from_rgba_unmultiplied(f.size, &f.pixels);
                        let texture = ctx.load_texture(format!("compound-strip-{}-{}", seq.0, cam.map_or(0, |c| c + 1)), image, egui::TextureOptions::LINEAR);
                        self.compounds.insert(key, (f.version, Strip { texture, frames: f.frames, duration: f.duration, aspect: f.aspect, scaling: oa_doc::MediaScaling::Smooth }));
                    }
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(TryRecvError::Disconnected) => {
                    self.compound_jobs.remove(&key);
                }
            }
        }
        let current = self.compounds.get(&key).is_some_and(|(v, _)| *v == version);
        if !current && !self.compound_jobs.contains_key(&key) {
            let (tx, rx) = channel();
            start(tx);
            self.compound_jobs.insert(key, rx);
        }
        self.compounds.get(&key).map(|(_, s)| s)
    }

    /// Whether a compound's first filmstrip is still being made.
    pub fn compound_pending(&self, seq: oa_doc::SeqId) -> bool {
        self.compound_jobs.contains_key(&(seq, None)) && !self.compounds.contains_key(&(seq, None))
    }

    /// The filmstrip for `media`, starting it if needed. `size` is the picture's size
    /// (for the aspect), `duration` seconds (0 for a still).
    /// Whether `media`'s frames are still being extracted.
    pub fn strip_pending(&self, media: MediaId) -> bool {
        self.strip_jobs.contains_key(&media)
    }

    /// `size` [0, 0]: read from the file. `scaling`: how the file's pixels are scaled
    /// (`MediaRef::scaling`), so pixel art stays crisp in its preview too.
    pub fn strip(&mut self, ctx: &egui::Context, media: MediaId, path: &Path, size: [u32; 2], duration: f64, scaling: oa_doc::MediaScaling) -> Option<&Strip> {
        // Its scaling changed since it was made: made again.
        if matches!(self.strips.get(&media), Some(Slot::Ready(s)) if s.scaling != scaling) && !self.strip_jobs.contains_key(&media) {
            self.strips.remove(&media);
        }
        if !self.strips.contains_key(&media) && !self.strip_jobs.contains_key(&media) {
            let (tx, rx) = channel();
            let path = path.to_path_buf();
            // Two at a time: a bin full of new cards would start dozens of readers.
            let ticket = crate::gate::previews().ticket();
            std::thread::spawn(move || make_strip(&path, size, duration, scaling, &tx, ticket));
            self.strip_jobs.insert(media, rx);
        }
        // The strip arrives in stages (the first frame at once, the rest as they're
        // found); each replaces the last. Only the latest in is uploaded, into the
        // texture already there.
        let mut latest = None;
        while let Some(rx) = self.strip_jobs.get(&media) {
            match rx.try_recv() {
                Ok(Some(px)) => latest = Some(px),
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    self.strip_jobs.remove(&media);
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    break;
                }
            }
        }
        if let Some(px) = latest {
            let options = if px.pixel { egui::TextureOptions::NEAREST } else { egui::TextureOptions::LINEAR };
            if let Some(Slot::Ready(s)) = self.strips.get_mut(&media) {
                s.texture.set(px.image, options);
                (s.frames, s.duration, s.aspect, s.scaling) = (px.frames, px.duration, px.aspect, scaling);
            } else {
                let texture = ctx.load_texture(format!("strip-{}", media.0), px.image, options);
                self.strips.insert(media, Slot::Ready(Strip { texture, frames: px.frames, duration: px.duration, aspect: px.aspect, scaling }));
            }
        }
        if !self.strip_jobs.contains_key(&media) && !matches!(self.strips.get(&media), Some(Slot::Ready(_))) {
            self.strips.insert(media, Slot::Failed);
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
            let ticket = crate::gate::previews().ticket();
            std::thread::spawn(move || {
                // Read before: from the cache at once, without waiting in line.
                let key = cache_key(&path, &format!("peaks {PEAKS_PER_SECOND}"));
                if let Some(p) = key.as_deref().and_then(read_peaks) {
                    let _ = tx.send(Some(Arc::new(p)));
                    return;
                }
                let _pass = ticket.wait();
                let peaks = make_peaks(&path);
                if let (Some(k), Some(p)) = (&key, &peaks) {
                    write_peaks(k, p);
                }
                let _ = tx.send(peaks.map(Arc::new));
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
    let out = oa_media::background_tool("ffmpeg").args(["-v", "error", "-nostdin"]).args(args).arg("-i").arg(input).args(rest).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Frames fetched at once while making a strip.
const STRIP_THREADS: usize = 4;

/// Files up to this size have their filmstrip read in one pass over their keyframes;
/// bigger ones (mostly long ones) seek to each frame, rather than read through it all.
const ONE_PASS_BYTES: u64 = 2 << 30;

/// Bump to throw away every cached filmstrip and waveform (their format changed).
const CACHE_VERSION: u32 = 1;

/// Where finished filmstrips and waveforms are kept: `<config>/cache/<kind>` (tests keep
/// theirs in the temp folder).
pub(crate) fn cache_dir(kind: &str) -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("oa-test-cache-{}", std::process::id())).join(kind);
    }
    crate::settings::config_dir().join("cache").join(kind)
}

/// A file's cache name for a preview: it changes with the file (its path, size and when
/// it was last written) and with `extra` (what's made from it).
pub(crate) fn cache_key(path: &Path, extra: &str) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, meta.len(), modified, extra, CACHE_VERSION).hash(&mut h);
    Some(format!("{:016x}", h.finish()))
}

/// Keeps the preview cache under 512 MB: once a session (on its first write), the oldest
/// files go until it's under 384 MB.
fn prune_cache() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| {
            let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = ["strips", "peaks"]
                .iter()
                .flat_map(|k| std::fs::read_dir(cache_dir(k)).into_iter().flatten().flatten())
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some((m.modified().ok()?, m.len(), e.path()))
                })
                .collect();
            let mut total: u64 = files.iter().map(|f| f.1).sum();
            if total < 512 << 20 {
                return;
            }
            files.sort();
            for (_, len, path) in files {
                if total < 384 << 20 {
                    break;
                }
                if std::fs::remove_file(&path).is_ok() {
                    total -= len;
                }
            }
        });
    });
}

/// A cached filmstrip, if there's one for `key` the size expected.
fn read_strip(key: &str, size: [usize; 2]) -> Option<Vec<u8>> {
    read_strip_in(&cache_dir("strips"), key, size)
}

fn read_strip_in(dir: &Path, key: &str, size: [usize; 2]) -> Option<Vec<u8>> {
    let file = std::fs::File::open(dir.join(format!("{key}.png"))).ok()?;
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let ok = info.color_type == png::ColorType::Rgba && [info.width as usize, info.height as usize] == size;
    ok.then(|| buf[..size[0] * size[1] * 4].to_vec())
}

/// Keeps a finished filmstrip for next time (written aside, then moved into place).
fn write_strip(key: &str, size: [usize; 2], rgba: &[u8]) {
    if write_strip_in(&cache_dir("strips"), key, size, rgba) {
        prune_cache();
    }
}

/// Writes a filmstrip into `dir`; whether it was.
fn write_strip_in(dir: &Path, key: &str, size: [usize; 2], rgba: &[u8]) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let part = dir.join(format!("{key}.part"));
    let written = (|| -> Option<()> {
        let file = std::fs::File::create(&part).ok()?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), size[0] as u32, size[1] as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        enc.write_header().ok()?.write_image_data(rgba).ok()
    })();
    if written.is_some() && std::fs::rename(&part, dir.join(format!("{key}.png"))).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(&part);
    false
}

/// Cached waveform peaks: (low, high) pairs as little-endian floats.
fn read_peaks(key: &str) -> Option<Vec<(f32, f32)>> {
    let bytes = std::fs::read(cache_dir("peaks").join(format!("{key}.bin"))).ok()?;
    (bytes.len() % 8 == 0).then(|| bytes.as_chunks::<8>().0.iter().map(|c| (f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect())
}

fn write_peaks(key: &str, peaks: &[(f32, f32)]) {
    let dir = cache_dir("peaks");
    let _ = std::fs::create_dir_all(&dir);
    let bytes: Vec<u8> = peaks.iter().flat_map(|(lo, hi)| lo.to_le_bytes().into_iter().chain(hi.to_le_bytes())).collect();
    let part = dir.join(format!("{key}.part"));
    if std::fs::write(&part, &bytes).is_ok() {
        let _ = std::fs::rename(&part, dir.join(format!("{key}.bin")));
        prune_cache();
    }
}

/// The order a strip's frames are fetched in after the first: spread over the file and
/// then filled in (every 8th, then every 4th, every 2nd, the rest), so skimming shows
/// the whole file early and sharpens as it goes. Returns the levels, in order.
pub fn coarse_to_fine(frames: usize) -> Vec<Vec<usize>> {
    let mut done = vec![false; frames];
    if frames > 0 {
        done[0] = true;
    }
    let mut levels = Vec::new();
    for step in [8, 4, 2, 1] {
        let level: Vec<usize> = (0..frames).step_by(step).filter(|&i| !done[i]).collect();
        for &i in &level {
            done[i] = true;
        }
        if !level.is_empty() {
            levels.push(level);
        }
    }
    levels
}

/// Makes `path`'s filmstrip — one frame per second, 1 to 60, spread over the file — and
/// sends it in stages: the first frame straight away (in every cell, so skimming shows
/// something), then the rest as they're read. A strip made before comes from the cache at
/// once. Every frame is a keyframe (a thumbnail needn't be exact, and decoding to the
/// exact frame cost up to a GOP per frame): most files are read in one ffmpeg pass that
/// decodes only those ([`keyframes`]) — one process, not one per frame — and the rest
/// (huge files, all-keyframe codecs) seek to each, coarse to fine. Everything runs below
/// normal priority, so playback keeps the CPU. The first frame waits its turn with the
/// other files' first frames (`ticket`); the rest wait again behind every first frame
/// asked for meanwhile, so each card gets a picture before any fills in. Sends `None` if
/// not even the first frame could be read.
fn make_strip(path: &Path, size: [u32; 2], duration: f64, scaling: oa_doc::MediaScaling, tx: &std::sync::mpsc::Sender<Option<StripPixels>>, ticket: crate::gate::Ticket<'static>) {
    // Its size as shown (after any rotation), read from the file when not given.
    let size = if size[0] == 0 || size[1] == 0 { oa_media::probe(path).ok().and_then(|p| p.video).map_or([16, 9], |v| [v.width, v.height]) } else { size };
    // Pixel art stays crisp: scaled by nearest neighbor, and drawn that way.
    let pixel = match scaling {
        oa_doc::MediaScaling::Pixel => true,
        oa_doc::MediaScaling::Smooth => false,
        oa_doc::MediaScaling::Auto => size[0] <= oa_doc::MediaScaling::PIXEL_ART_MAX && size[1] <= oa_doc::MediaScaling::PIXEL_ART_MAX,
    };
    let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
    let (w, h) = (FRAME_W as usize, ((FRAME_W as f32 / aspect / 2.0).round() as usize * 2).max(2));
    let frames = if duration <= 0.0 { 1 } else { (duration.ceil() as usize).clamp(1, 60) };
    let rows = frames.div_ceil(COLS);
    let (iw, ih) = (w * COLS, h * rows);
    let path = path.to_path_buf();
    let send = |canvas: &[u8]| tx.send(Some(StripPixels { image: egui::ColorImage::from_rgba_unmultiplied([iw, ih], canvas), frames, duration: duration.max(1e-3), aspect, pixel }));
    // Made before: straight from the cache, without waiting in line. ("fit": frames fitted
    // inside their cell, never stretched to it — older strips were.)
    let key = cache_key(&path, &format!("strip fit {w}x{h} {frames} {duration:.3}{}", if pixel { " pixel" } else { "" }));
    if let Some(cached) = key.as_deref().and_then(|k| read_strip(k, [iw, ih])) {
        let _ = send(&cached);
        return;
    }
    let pass = ticket.wait();
    let scale = fit_filter(w, h, pixel);
    // One frame at `seconds` (in the middle of its share of the file), as RGBA.
    let frame_at = |i: usize| -> Option<Vec<u8>> {
        // A drawing: drawn at the cell's size (ffmpeg may not read SVG).
        if oa_media::svg::is_svg(&path) {
            return oa_media::svg::rasterize(&path, [w as u32, h as u32]).ok();
        }
        let seconds = if frames == 1 { 0.0 } else { (i as f64 + 0.5) * duration / frames as f64 };
        let bytes = ffmpeg(&["-threads", "1", "-noaccurate_seek", "-ss", &format!("{seconds:.3}")], &path, &["-an", "-sn", "-dn", "-frames:v", "1", "-vf", &scale, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])?;
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
    let Some(first) = frame_at(0) else {
        let _ = tx.send(None);
        return;
    };
    for i in 0..frames {
        blit(&mut canvas, i, &first);
    }
    if send(&canvas).is_err() {
        return;
    }
    if frames == 1 {
        if let Some(k) = &key {
            write_strip(k, [iw, ih], &canvas);
        }
        return;
    }
    // The rest, behind every other file's first frame.
    drop(pass);
    let _pass = crate::gate::previews().ticket().wait();
    let mut have = vec![false; frames];
    have[0] = true;
    // Until a cell has its own frame, it shows the nearest one before it that's in.
    let shown = |canvas: &[u8], have: &[bool]| -> Vec<u8> {
        let mut shown = canvas.to_vec();
        let mut last = 0;
        for (i, &got) in have.iter().enumerate() {
            if got {
                last = i;
            } else {
                let (c, r) = (last % COLS, last / COLS);
                let px: Vec<u8> = (0..h).flat_map(|y| canvas[((r * h + y) * iw + c * w) * 4..((r * h + y) * iw + c * w + w) * 4].to_vec()).collect();
                blit(&mut shown, i, &px);
            }
        }
        shown
    };
    // A file of a sensible size: one pass over its keyframes, each cell taking the one
    // nearest its middle. It gives up (leaving the rest to seeking) if they come too
    // thick to be worth decoding — every frame a keyframe, as in ProRes.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() <= ONE_PASS_BYTES) {
        let slot = duration / frames as f64;
        let middle = |i: usize| (i as f64 + 0.5) * slot;
        // The first cell not settled yet, and the keyframe before the latest.
        let mut next = 0;
        let mut before: Option<(f64, Vec<u8>)> = None;
        let (mut count, mut gone) = (0usize, false);
        let mut sent = std::time::Instant::now();
        let finished = keyframes(&path, w, h, pixel, |t, px| {
            count += 1;
            while next < frames && middle(next) < t {
                let pick = match &before {
                    Some((tb, pb)) if middle(next) - tb <= t - middle(next) => pb.as_slice(),
                    _ => px,
                };
                blit(&mut canvas, next, pick);
                have[next] = true;
                next += 1;
            }
            before = Some((t, px.to_vec()));
            if sent.elapsed() > std::time::Duration::from_millis(250) {
                if send(&shown(&canvas, &have)).is_err() {
                    gone = true;
                    return false;
                }
                sent = std::time::Instant::now();
            }
            count <= 12 || count as f64 <= 3.0 * (t / slot + 1.0)
        });
        if gone {
            return;
        }
        // To the end: the cells after the last keyframe take it.
        if finished && let Some((_, pb)) = &before {
            for (i, got) in have.iter_mut().enumerate().skip(next) {
                blit(&mut canvas, i, pb);
                *got = true;
            }
        }
    }
    // What's left, coarse to fine, a few seeks at a time; a frame that can't be read keeps
    // the one before it.
    for level in coarse_to_fine(frames) {
        let level: Vec<usize> = level.into_iter().filter(|&i| !have[i]).collect();
        if level.is_empty() {
            continue;
        }
        for batch in level.chunks(STRIP_THREADS) {
            let got: Vec<(usize, Option<Vec<u8>>)> = std::thread::scope(|s| {
                let jobs: Vec<_> = batch.iter().map(|&i| (i, s.spawn(move || frame_at(i)))).collect();
                jobs.into_iter().map(|(i, j)| (i, j.join().ok().flatten())).collect()
            });
            for (i, px) in got {
                if let Some(px) = px {
                    blit(&mut canvas, i, &px);
                    have[i] = true;
                }
            }
        }
        if send(&shown(&canvas, &have)).is_err() {
            return;
        }
    }
    if send(&shown(&canvas, &have)).is_err() {
        return;
    }
    if let Some(k) = &key {
        write_strip(k, [iw, ih], &canvas);
    }
}

/// ffmpeg's filter putting a frame in a `w`×`h` cell: fitted inside it keeping its own
/// shape (frames not the size the file said are never stretched), the rest of the cell
/// transparent; by nearest neighbor for pixel art, smoothly otherwise.
fn fit_filter(w: usize, h: usize, pixel: bool) -> String {
    let flags = if pixel { "neighbor" } else { "bicubic" };
    format!("scale={w}:{h}:force_original_aspect_ratio=decrease:flags={flags},format=rgba,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0")
}

/// Reads every keyframe of `path`'s picture, fitted in `w`×`h` (see [`fit_filter`]), in one pass that decodes
/// nothing else, handing each (its time in seconds, RGBA) to `each` as it comes; `each`
/// returns whether to go on. Whether it read to the end.
fn keyframes(path: &Path, w: usize, h: usize, pixel: bool, mut each: impl FnMut(f64, &[u8]) -> bool) -> bool {
    use std::io::{BufRead, Read};
    let Ok(mut child) = oa_media::background_tool("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "info", "-threads", "2", "-skip_frame", "nokey", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-fps_mode", "passthrough", "-vf", &format!("{},showinfo", fit_filter(w, h, pixel)), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    // Each frame's time, from showinfo's report of it ("… pts_time:1.5 …").
    let (tell, times) = channel::<f64>();
    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        let Some(stderr) = stderr else { return };
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let t = line.split_once("pts_time:").and_then(|(_, rest)| rest.split_whitespace().next()?.parse::<f64>().ok());
            if let Some(t) = t
                && tell.send(t).is_err()
            {
                return;
            }
        }
    });
    let Some(mut stdout) = child.stdout.take() else { return false };
    let mut px = vec![0u8; w * h * 4];
    let mut stopped = false;
    while stdout.read_exact(&mut px).is_ok() {
        let Ok(t) = times.recv() else { break };
        if !each(t, &px) {
            stopped = true;
            break;
        }
    }
    if stopped {
        let _ = child.kill();
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    ok && !stopped
}

/// `path`'s waveform: the lowest and highest sample of each 1/`PEAKS_PER_SECOND` s. The
/// samples are folded into peaks as ffmpeg streams them, so an hour of sound holds a few
/// MB of peaks, not the ~115 MB of samples (twice over) it once buffered.
fn make_peaks(path: &Path) -> Option<Vec<(f32, f32)>> {
    use std::io::Read;
    const RATE: f64 = 4000.0;
    let per = (RATE / PEAKS_PER_SECOND) as usize;
    let mut child = oa_media::background_tool("ffmpeg")
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

    /// A strip comes in stages — the first frame in every cell at once, then the rest
    /// until each cell has its own frame — and comes from the cache the next time, at
    /// once; a file ffmpeg can't read says so rather than hanging. Read in one pass over
    /// the keyframes, or (every frame a keyframe) by seeking, the end is the same.
    #[test]
    fn strips_arrive_first_frame_first() {
        if !oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-strip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Five seconds whose brightness climbs second by second: a keyframe a second, and
        // every frame a keyframe.
        for (name, gop) in [("counting.mp4", "10"), ("every.mp4", "1")] {
            let clip = dir.join(name);
            let made = oa_media::tool("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=160x90:r=10:d=5", "-vf", "geq=lum='20+T*40':cb=128:cr=128", "-c:v", "libx264", "-g", gop])
                .arg(&clip)
                .status()
                .is_ok_and(|s| s.success());
            if !made {
                return;
            }
            let (tx, rx) = channel();
            make_strip(&clip, [160, 90], 5.0, oa_doc::MediaScaling::Auto, &tx, crate::gate::previews().ticket());
            drop(tx);
            let stages: Vec<StripPixels> = rx.iter().map(|s| s.expect("frames")).collect();
            assert!(stages.len() >= 2, "{name}: the first frame, then the rest");
            let brightness = |s: &StripPixels, i: usize| {
                let (w, h) = (FRAME_W as usize, s.image.size[1]);
                s.image.pixels[(h / 2) * s.image.size[0] + (i % COLS) * w + w / 2].r()
            };
            assert_eq!(stages[0].frames, 5);
            assert!((1..5).all(|i| brightness(&stages[0], i) == brightness(&stages[0], 0)), "{name}: stage one, the first frame everywhere");
            let last = stages.last().unwrap();
            assert!((1..5).all(|i| brightness(last, i) > brightness(last, i - 1)), "{name}: in the end, each second its own frame");
            // Again: from the cache, all at once.
            let (tx, rx) = channel();
            make_strip(&clip, [160, 90], 5.0, oa_doc::MediaScaling::Auto, &tx, crate::gate::previews().ticket());
            drop(tx);
            let again: Vec<StripPixels> = rx.iter().map(|s| s.expect("frames")).collect();
            assert_eq!(again.len(), 1, "{name}: one stage, the finished strip");
            assert_eq!(again[0].image.pixels, last.image.pixels);
        }

        let (tx, rx) = channel();
        make_strip(&dir.join("missing.mp4"), [160, 90], 5.0, oa_doc::MediaScaling::Auto, &tx, crate::gate::previews().ticket());
        assert!(rx.recv().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A frame keeps its own shape in its cell: told the wrong size, a wide picture sits
    /// in a square cell with see-through bars above and below, never squashed into it;
    /// not told, the size is read from the file. Tiny pictures (pixel art) are scaled by
    /// nearest neighbor, so their pixels stay hard-edged squares.
    #[test]
    fn strip_frames_keep_their_shape_and_scale_mode() {
        if !oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-strip-shape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, args: &[&str]| {
            let file = dir.join(name);
            oa_media::tool("ffmpeg").args(["-v", "error", "-y", "-f", "lavfi"]).args(args).arg(&file).status().is_ok_and(|s| s.success()).then_some(file)
        };
        let strip = |file: &Path, size: [u32; 2], scaling: oa_doc::MediaScaling| {
            let (tx, rx) = channel();
            make_strip(file, size, 0.0, scaling, &tx, crate::gate::previews().ticket());
            drop(tx);
            rx.iter().last().flatten().expect("a frame")
        };
        // White, 160 × 90.
        let Some(wide) = make("wide.png", &["-i", "color=c=white:s=160x90", "-frames:v", "1"]) else { return };
        let s = strip(&wide, [1, 1], oa_doc::MediaScaling::Auto);
        let [w, h] = s.image.size;
        assert_eq!(w, FRAME_W as usize * COLS);
        let at = |x: usize, y: usize| s.image.pixels[y * w + x];
        assert_eq!(at(FRAME_W as usize / 2, 2).a(), 0, "a bar above, see-through");
        assert_eq!(at(FRAME_W as usize / 2, h - 3).a(), 0, "and below");
        assert!(at(FRAME_W as usize / 2, h / 2).r() > 240, "the picture in the middle");
        let s = strip(&wide, [0, 0], oa_doc::MediaScaling::Auto);
        assert!((s.aspect - 16.0 / 9.0).abs() < 0.01, "its size read from the file: {}", s.aspect);
        assert!(!s.pixel);
        // 4 × 4 checks (pixel art): nearest neighbor, so every pixel is black or white.
        let Some(tiny) = make("tiny.png", &["-i", "nullsrc=s=4x4", "-vf", "geq=lum='255*mod(X+Y,2)':cb=128:cr=128", "-frames:v", "1"]) else { return };
        let s = strip(&tiny, [4, 4], oa_doc::MediaScaling::Auto);
        assert!(s.pixel, "drawn pixel by pixel");
        let cell: Vec<u8> = (0..FRAME_W as usize).flat_map(|y| (0..FRAME_W as usize).map(move |x| (x, y))).map(|(x, y)| s.image.pixels[y * s.image.size[0] + x].r()).collect();
        assert!(cell.iter().all(|v| *v < 20 || *v > 235), "no gray in between");
        let s = strip(&tiny, [4, 4], oa_doc::MediaScaling::Smooth);
        assert!(!s.pixel, "unless it's set to smooth");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One pass hands over each keyframe once, in order, with its time.
    #[test]
    fn keyframes_come_in_order_with_their_times() {
        if !oa_media::tool("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oa-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("keys.mp4");
        let made = oa_media::tool("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=160x90:r=10:d=4", "-c:v", "libx264", "-g", "10", "-keyint_min", "10", "-sc_threshold", "0"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let mut times = Vec::new();
        assert!(keyframes(&clip, 32, 18, false, |t, px| {
            assert_eq!(px.len(), 32 * 18 * 4);
            times.push(t);
            true
        }));
        assert_eq!(times.len(), 4, "{times:?}");
        assert!(times.iter().enumerate().all(|(i, t)| (t - i as f64).abs() < 0.05), "{times:?}");
        // Stopped early: says so.
        assert!(!keyframes(&clip, 32, 18, false, |_, _| false));
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

#[cfg(test)]
mod cache_tests {
    use super::*;

    /// After the first frame, the rest come spread over the file and then filled in,
    /// each exactly once.
    #[test]
    fn strip_frames_come_coarse_to_fine() {
        let levels = coarse_to_fine(20);
        assert_eq!(levels[0], vec![8, 16], "every 8th first");
        assert_eq!(levels[1], vec![4, 12], "then every 4th");
        let mut all: Vec<usize> = levels.concat();
        all.sort();
        assert_eq!(all, (1..20).collect::<Vec<_>>(), "every frame but the first, once");
        assert!(coarse_to_fine(1).is_empty());
    }

    /// A strip kept in the cache comes back as it was; one of another size doesn't.
    #[test]
    fn strips_come_back_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("oa-strip-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| i as u8).collect();
        assert!(write_strip_in(&dir, "k", [4, 3], &rgba));
        assert_eq!(read_strip_in(&dir, "k", [4, 3]), Some(rgba));
        assert_eq!(read_strip_in(&dir, "k", [3, 4]), None, "made for another size");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file's cache name changes when the file does.
    #[test]
    fn a_changed_file_gets_a_new_cache_name() {
        let file = std::env::temp_dir().join(format!("oa-cache-key-{}.bin", std::process::id()));
        std::fs::write(&file, b"one").unwrap();
        let a = cache_key(&file, "strip").unwrap();
        assert_eq!(cache_key(&file, "strip"), Some(a.clone()), "the same file, the same name");
        assert_ne!(cache_key(&file, "peaks"), Some(a.clone()), "another kind of preview, another name");
        std::fs::write(&file, b"longer now").unwrap();
        assert_ne!(cache_key(&file, "strip"), Some(a), "written again: a new name");
        let _ = std::fs::remove_file(&file);
    }
}
