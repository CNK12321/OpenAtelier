//! Small renders — effect-picker previews, their hover animations, the caption style
//! preview, the project picture written on save — made on a thread of their own, with
//! their own renderer and decoders, so none of them costs the UI a frame.
//!
//! They used to render inside the UI loop under a time budget: every preview was a full
//! plan + GPU render + (on save) a readback and a PNG encode between two UI frames, with
//! the viewer's renderer and decoders shared, so opening a picker or saving stuttered
//! and the previews had to settle for stand-in frames rather than wait on a decode.
//!
//! **GPU hand-off.** wgpu's device and queue are shared by every thread, and one queue
//! runs submissions in the order they're made. A preview's display texture is drawn
//! and *submitted* on this thread before it's handed to the UI, so the UI's frame —
//! submitted after it gets the texture — always sees it finished. Each result is a
//! texture of its own, never one the UI is showing (the UI frees what it replaces), so
//! nothing is drawn into while it's on screen. The thread keeps its own small VRAM
//! budget, and waits for shaders and decodes (off the UI thread there's no reason not
//! to), so every preview is the exact frame.
//!
//! **Requests.** Each has a `slot` (what it's for: one effect's cell, the hover
//! animation, the caption preview…) and only the newest request per slot is rendered —
//! a hover animation asking every frame, or a caption being typed, never builds a queue.
//! A request can also carry a generation it's for; once that has moved on (the playhead
//! or the document changed), it's skipped rather than rendered for nothing.

use crate::export_worker::MediaEntry;
use crate::sources::Sources;
use oa_doc::{Project, SeqId, VariantId};
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer};
use oa_graph::registry::Registry;
use oa_graph::{optimize, KeyContext, OptLevel};
use oa_media::MediaKind;
use oa_plan::{plan_frame, PlanOptions};
use oa_time::Time;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// The slot of the hover animation in the effect pickers.
pub const LIVE: u64 = 1;
/// The slot of the caption style preview.
pub const CAPTION: u64 = 2;
/// The slot of the project picture written on save.
pub const PROJECT_PICTURE: u64 = 3;

/// One frame to render.
pub struct Request {
    /// What it's for: only the newest request per slot is rendered.
    pub slot: u64,
    /// The caller's own note of what this was made from, handed back with the result.
    pub tag: u64,
    pub project: Arc<Project>,
    /// The effects it renders with (plugins can change while the editor runs).
    pub registry: Arc<Registry>,
    pub seq: SeqId,
    pub variant: VariantId,
    pub at: Time,
    /// × the canvas.
    pub scale: f64,
    /// Skip it once this generation has moved on: (current, the one it was asked for).
    pub wanted: Option<(Arc<AtomicU64>, u64)>,
    /// Write it as a PNG here instead of handing back a texture.
    pub png: Option<PathBuf>,
}

/// A finished preview: display-ready (sRGB RGBA8), already submitted on the GPU.
pub struct Rendered {
    pub tag: u64,
    pub texture: wgpu::Texture,
}

enum Msg {
    Render(Box<Request>),
    /// The files the renders may need (replaces the previous list).
    Media(Vec<MediaEntry>),
    Strip(Box<StripRequest>),
    /// A frame read back as pixels (for the scopes); only the newest is made.
    Pixels(Box<Request>, Sender<Option<FramePixels>>),
    /// A file renders may always use, whatever project is open (the effect previews'
    /// sample picture).
    Keep(Box<MediaEntry>),
}

/// A frame read back: straight-alpha sRGB RGBA8, `size` px.
pub struct FramePixels {
    pub pixels: Vec<u8>,
    pub size: [usize; 2],
}

/// A compound clip's filmstrip: `frames` frames spread over it, each `frame_w` wide, read
/// back and laid out `cols` to a row (as a file's filmstrip is) — for its card in the
/// bin and its clips on the timeline. Only the newest request per compound is made.
pub struct StripRequest {
    pub seq: SeqId,
    /// What the compound was when asked (handed back, so a stale strip is known).
    pub version: u64,
    pub project: Arc<Project>,
    pub registry: Arc<Registry>,
    pub frames: usize,
    pub frame_w: u32,
    pub cols: usize,
    pub reply: Sender<Option<StripFrames>>,
}

/// A rendered filmstrip: RGBA8 (straight alpha, sRGB) `size` px, `frames` of it.
pub struct StripFrames {
    pub version: u64,
    pub pixels: Vec<u8>,
    pub size: [usize; 2],
    pub frames: usize,
    pub duration: f64,
    pub aspect: f32,
}

/// The UI's handle on the preview thread.
pub struct PreviewWorker {
    tx: Sender<Msg>,
    rx: Receiver<(u64, Rendered)>,
    /// Finished previews not yet picked up, newest per slot.
    ready: HashMap<u64, Rendered>,
    /// The viewer is playing: previews wait (the frame on screen comes first).
    busy: Arc<AtomicBool>,
}

impl PreviewWorker {
    /// Starts the thread. `repaint` is asked for a repaint whenever a preview is done.
    pub fn start(gpu: Arc<GpuContext>, decoders: oa_media::DecoderChoice, vram_budget: u64, repaint: eframe::egui::Context) -> Self {
        let (tx, jobs) = channel::<Msg>();
        let (done, rx) = channel();
        let busy = Arc::new(AtomicBool::new(false));
        let waits = busy.clone();
        let spawned = std::thread::Builder::new().name("oa-previews".into()).spawn(move || run(gpu, decoders, vram_budget, jobs, done, repaint, waits));
        if let Err(e) = spawned {
            // Requests then go nowhere and previews stay placeholders; the editor works.
            eprintln!("couldn't start the preview thread: {e}");
        }
        PreviewWorker { tx, rx, ready: HashMap::new(), busy }
    }

    pub fn request(&self, request: Request) {
        let _ = self.tx.send(Msg::Render(Box::new(request)));
    }

    /// The media files renders may use (after an import, a relink, a project opened).
    pub fn set_media(&self, media: Vec<MediaEntry>) {
        let _ = self.tx.send(Msg::Media(media));
    }

    /// Keeps `media` available to renders from now on, whatever project is open.
    pub fn keep(&self, media: MediaEntry) {
        let _ = self.tx.send(Msg::Keep(Box::new(media)));
    }

    /// Renders a frame and reads it back as pixels (the Color tab's scopes).
    pub fn pixels(&self, request: Request, reply: Sender<Option<FramePixels>>) {
        let _ = self.tx.send(Msg::Pixels(Box::new(request), reply));
    }

    /// Renders a compound clip's filmstrip (see [`StripRequest`]).
    pub fn strip(&self, request: StripRequest) {
        let _ = self.tx.send(Msg::Strip(Box::new(request)));
    }

    /// The viewer is playing (true) or not: previews wait while it is.
    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::Release);
    }

    /// Takes the finished preview for `slot`, if there is one.
    pub fn take(&mut self, slot: u64) -> Option<Rendered> {
        while let Ok((slot, rendered)) = self.rx.try_recv() {
            self.ready.insert(slot, rendered);
        }
        self.ready.remove(&slot)
    }
}

#[allow(clippy::too_many_arguments)]
fn run(gpu: Arc<GpuContext>, decoders: oa_media::DecoderChoice, vram_budget: u64, jobs: Receiver<Msg>, done: Sender<(u64, Rendered)>, repaint: eframe::egui::Context, busy: Arc<AtomicBool>) {
    // A small budget of its own: previews are ~200 px, a caption preview a few hundred.
    let budget = (vram_budget / 8).clamp(64 << 20, 512 << 20);
    let mut renderer = Renderer::new(gpu.clone(), RenderOptions { fusion: FusionMode::Blocking, wait: true, vram_budget: budget, ..Default::default() });
    let mut sources = Sources::new();
    let mut registry: Option<Arc<Registry>> = None;
    // The project's files, and those kept whatever the project (`Msg::Keep`).
    let (mut project_media, mut kept): (Vec<MediaEntry>, Vec<MediaEntry>) = (Vec::new(), Vec::new());
    // Blocks when there's nothing to do; ends when the UI drops its handle.
    while let Ok(first) = jobs.recv() {
        let mut media = None;
        let mut renders: Vec<Box<Request>> = Vec::new();
        let mut strips: Vec<Box<StripRequest>> = Vec::new();
        let mut readback: Option<(Box<Request>, Sender<Option<FramePixels>>)> = None;
        for msg in std::iter::once(first).chain(std::iter::from_fn(|| jobs.try_recv().ok())) {
            match msg {
                Msg::Media(list) => media = Some(list),
                Msg::Keep(entry) => {
                    kept.retain(|k| k.id != entry.id);
                    kept.push(*entry);
                    media.get_or_insert_with(|| project_media.clone());
                }
                Msg::Pixels(r, reply) => readback = Some((r, reply)),
                Msg::Strip(s) => match strips.iter_mut().find(|q| q.seq == s.seq) {
                    Some(q) => *q = s,
                    None => strips.push(s),
                },
                Msg::Render(r) => {
                    // The newest per slot, where the first of that slot stood.
                    match renders.iter_mut().find(|q| q.slot == r.slot) {
                        Some(q) => *q = r,
                        None => renders.push(r),
                    }
                }
            }
        }
        if let Some(list) = media {
            project_media = list;
            let all: Vec<MediaEntry> = project_media.iter().chain(&kept).cloned().collect();
            sources = sources_for(&gpu, &decoders, &all);
            renderer.clear_cache();
        }
        // Request priorities: the viewer's frame first. While it plays, previews wait
        // (their requests keep coalescing meanwhile, so what renders after is the newest).
        while busy.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        for r in renders {
            if r.wanted.as_ref().is_some_and(|(now, asked)| now.load(Ordering::Acquire) != *asked) {
                continue;
            }
            // Plugins changed: cached results may be of effects that are different now.
            if registry.as_ref().is_none_or(|known| !Arc::ptr_eq(known, &r.registry)) {
                renderer.clear_cache();
                registry = Some(r.registry.clone());
            }
            match render(&mut renderer, &mut sources, &r) {
                Ok(image) => {
                    if let Some(path) = &r.png {
                        if let Err(e) = write_png(&renderer, &image, path) {
                            eprintln!("project picture: {e}");
                        }
                        continue;
                    }
                    match oa_gpu::readback::display_texture_into(&gpu, renderer.pipelines(), &image, None) {
                        Ok(texture) => {
                            if done.send((r.slot, Rendered { tag: r.tag, texture })).is_err() {
                                return;
                            }
                            repaint.request_repaint();
                        }
                        Err(e) => eprintln!("preview: {e}"),
                    }
                }
                Err(e) => eprintln!("preview: {e}"),
            }
        }
        if let Some((r, reply)) = readback {
            if registry.as_ref().is_none_or(|known| !Arc::ptr_eq(known, &r.registry)) {
                renderer.clear_cache();
                registry = Some(r.registry.clone());
            }
            let made = render(&mut renderer, &mut sources, &r).ok().and_then(|image| {
                let pixels = oa_gpu::readback::read_srgb8(renderer.context(), renderer.pipelines(), &image).ok()?;
                Some(FramePixels { pixels, size: [image.size[0] as usize, image.size[1] as usize] })
            });
            if reply.send(made).is_ok() {
                repaint.request_repaint();
            }
        }
        for s in strips {
            if registry.as_ref().is_none_or(|known| !Arc::ptr_eq(known, &s.registry)) {
                renderer.clear_cache();
                registry = Some(s.registry.clone());
            }
            let made = render_strip(&mut renderer, &mut sources, &s);
            if s.reply.send(made).is_ok() {
                repaint.request_repaint();
            }
        }
    }
}

/// A compound's filmstrip: each frame rendered small at the middle of its share of the
/// compound, read back, laid into the atlas. `None` if even the first frame fails.
fn render_strip(renderer: &mut Renderer, sources: &mut Sources, s: &StripRequest) -> Option<StripFrames> {
    let sequence = s.project.sequence(s.seq)?;
    let variant = sequence.active();
    let canvas = variant.size;
    let scale = (s.frame_w as f64 / canvas.width.max(1) as f64).clamp(0.02, 1.0);
    let duration = sequence.duration().as_seconds_f64();
    let frames = s.frames.max(1);
    let mut atlas: Vec<u8> = Vec::new();
    let mut cell = [0usize; 2];
    let rows = frames.div_ceil(s.cols);
    for i in 0..frames {
        let at = Time::from_seconds_f64(if frames == 1 { 0.0 } else { (i as f64 + 0.5) * duration / frames as f64 });
        let r = Request { slot: 0, tag: 0, project: s.project.clone(), registry: s.registry.clone(), seq: s.seq, variant: variant.id, at, scale, wanted: None, png: None };
        let pixels = render(renderer, sources, &r).ok().and_then(|image| {
            let px = oa_gpu::readback::read_srgb8(renderer.context(), renderer.pipelines(), &image).ok()?;
            Some((px, [image.size[0] as usize, image.size[1] as usize]))
        });
        let Some((px, size)) = pixels else {
            if i == 0 {
                return None;
            }
            continue;
        };
        if i == 0 {
            cell = size;
            atlas = vec![0u8; cell[0] * s.cols * cell[1] * rows * 4];
        }
        if size != cell {
            continue;
        }
        let (c, row) = (i % s.cols, i / s.cols);
        let stride = cell[0] * s.cols * 4;
        for y in 0..cell[1] {
            let at = (row * cell[1] + y) * stride + c * cell[0] * 4;
            atlas[at..at + cell[0] * 4].copy_from_slice(&px[y * cell[0] * 4..(y + 1) * cell[0] * 4]);
        }
    }
    Some(StripFrames {
        version: s.version,
        pixels: atlas,
        size: [cell[0] * s.cols, cell[1] * rows],
        frames,
        duration: duration.max(1e-3),
        aspect: cell[0] as f32 / cell[1].max(1) as f32,
    })
}

fn render(renderer: &mut Renderer, sources: &mut Sources, r: &Request) -> Result<oa_gpu::GpuImage, String> {
    let opts = PlanOptions { variant: Some(r.variant), render_scale: r.scale.clamp(0.02, 1.0), ..Default::default() };
    let plan = plan_frame(&r.project, r.seq, r.at, &opts, &r.registry).map_err(|e| e.to_string())?;
    let graph = optimize(&plan.graph, OptLevel::Full, KeyContext::default());
    renderer.render(&graph, &r.registry, sources).map_err(|e| e.to_string())
}

fn write_png(renderer: &Renderer, image: &oa_gpu::GpuImage, path: &std::path::Path) -> Result<(), String> {
    let pixels = oa_gpu::readback::read_srgb8(renderer.context(), renderer.pipelines(), image).map_err(|e| e.to_string())?;
    // Written beside it, then moved into place: the start page (on its own thread) never
    // reads half a picture.
    let partial = path.with_extension("png.part");
    {
        let file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.size[0], image.size[1]);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

/// Decoders for the previews' own use (as the export has): stills uploaded here, videos
/// through a decoder instance of their own, so previews never seek the viewer's.
fn sources_for(gpu: &Arc<GpuContext>, decoders: &oa_media::DecoderChoice, media: &[MediaEntry]) -> Sources {
    let mut sources = Sources::new();
    let mut video = oa_media::frame_source(gpu, decoders.clone());
    for m in media {
        sources.register(m.id, m.kind);
        let Some(track) = &m.track else { continue };
        match m.kind {
            MediaKind::Still => {
                if let Err(e) = sources.stills_mut().add(gpu, m.id, &m.path, track) {
                    eprintln!("previews: {}: {e}", m.path.display());
                }
            }
            MediaKind::Video => video.add(m.id, &m.path, track.clone()),
            _ => {}
        }
    }
    sources.set_video(Some(video));
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::{CanvasSize, FormatVariant, Item, ItemId, ItemKind, Sequence, Track, TrackId, TrackKind};
    use oa_time::{FrameRate, TimeRange};
    use std::time::{Duration, Instant};

    fn project() -> Arc<Project> {
        let mut p = Project::new("previews");
        let v = FormatVariant { id: VariantId(3), name: "16:9".into(), size: CanvasSize { width: 320, height: 180 }, overrides: Default::default() };
        let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, v);
        let mut track = Track::new(TrackId(5), "V1", TrackKind::Video);
        track.items.push(Item::new(ItemId(6), "red", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(5))));
        seq.tracks.push(Arc::new(track));
        p.sequences.insert(SeqId(1), Arc::new(seq));
        Arc::new(p)
    }

    fn request(slot: u64, tag: u64, wanted: Option<(Arc<AtomicU64>, u64)>, png: Option<PathBuf>) -> Request {
        Request {
            slot,
            tag,
            project: project(),
            registry: Arc::new(Registry::with_builtins()),
            seq: SeqId(1),
            variant: VariantId(3),
            at: Time::from_seconds(1),
            scale: 0.5,
            wanted,
            png,
        }
    }

    /// Everything `slot` hands back: waits (up to 20 s — a first render on a software GPU
    /// compiling its shaders is slow) for the first, then until nothing more comes for a
    /// moment.
    fn collect(worker: &mut PreviewWorker, slot: u64) -> Vec<Rendered> {
        let mut got = Vec::new();
        let mut quiet_since = None;
        let started = Instant::now();
        while quiet_since.is_none_or(|q: Instant| q.elapsed() < Duration::from_millis(400)) && started.elapsed() < Duration::from_secs(20) {
            match worker.take(slot) {
                Some(r) => {
                    got.push(r);
                    quiet_since = Some(Instant::now());
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        got
    }

    /// Renders come back as finished textures at the asked size; a burst for one slot
    /// renders its newest (never a queue of all of them); a request for a generation
    /// that has moved on isn't rendered; and the project picture is written as a PNG.
    #[test]
    fn previews_render_off_the_ui_thread() {
        let Ok(gpu) = GpuContext::new_headless() else { return };
        let gpu = Arc::new(gpu);
        let mut worker = PreviewWorker::start(gpu, oa_media::DecoderChoice::default(), 512 << 20, eframe::egui::Context::default());

        worker.request(request(10, 7, None, None));
        let got = collect(&mut worker, 10);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].tag, got[0].texture.width(), got[0].texture.height()), (7, 160, 90));

        for tag in 1..=5 {
            worker.request(request(11, tag, None, None));
        }
        let got = collect(&mut worker, 11);
        assert!(!got.is_empty() && got.len() < 5, "{} renders for a burst of 5", got.len());
        assert_eq!(got.last().unwrap().tag, 5, "the newest one is rendered last");

        let generation = Arc::new(AtomicU64::new(2));
        worker.request(request(12, 1, Some((generation.clone(), 1)), None));
        worker.request(request(13, 2, Some((generation, 2)), None));
        // The thread takes them in order: once 13 is back, 12 was either rendered or
        // (rightly) skipped.
        assert_eq!(collect(&mut worker, 13).len(), 1);
        assert!(worker.take(12).is_none(), "rendered for a moment that had passed");

        // A compound's filmstrip: its frames, rendered and laid out ten to a row.
        let (reply, strip) = channel();
        worker.strip(StripRequest { seq: SeqId(1), version: 9, project: project(), registry: Arc::new(Registry::with_builtins()), frames: 3, frame_w: 32, cols: 10, reply });
        let made = strip.recv_timeout(Duration::from_secs(20)).expect("answered").expect("rendered");
        assert_eq!((made.version, made.frames), (9, 3));
        assert_eq!(made.size, [32 * 10, 18], "cells of 32×18, ten to a row");
        assert_eq!(made.pixels.len(), 320 * 18 * 4);
        assert!(made.pixels[..4 * 32].chunks(4).all(|p| p[3] == 255), "the first cell is drawn");
        assert!((made.aspect - 32.0 / 18.0).abs() < 1e-3);

        let png = std::env::temp_dir().join(format!("oa-preview-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&png);
        worker.request(request(PROJECT_PICTURE, 0, None, Some(png.clone())));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !png.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let bytes = std::fs::read(&png).expect("the picture was written");
        assert_eq!(&bytes[1..4], b"PNG");
        let _ = std::fs::remove_file(&png);
    }
}
