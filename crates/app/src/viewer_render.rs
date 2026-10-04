//! The viewer's picture, rendered on a thread of its own — the coordinator between the
//! UI and the GPU.
//!
//! The UI thread used to plan and render every viewer frame itself: a heavy frame (a
//! big blur, a new shader, a title's glyphs) held up the whole editor — the timeline,
//! the panels, the next mouse move. Now the UI only says what it wants (a [`Request`]:
//! the project snapshot, the moment, the options, the clips to warm up) and draws the
//! last picture that came back; this thread owns the viewer's renderer, its decoders
//! and the plan-ahead planner, and works on the newest request only (scrubbing never
//! builds a queue).
//!
//! **GPU hand-off**, as the preview thread does it: each picture is drawn into a display
//! texture and *submitted* here before it's handed over, so the UI's frame (submitted
//! after it gets it) sees it finished. The textures cycle three deep, so the one the UI
//! is drawing is not the one being drawn into (and the queue orders a later write after
//! an earlier read anyway).
//!
//! **Why no separate GPU-submit thread**: wgpu's queue already is one serialized
//! submission point for every thread, and each thread's `write_buffer`/`write_texture`
//! uploads are ordered against its own submissions. Routing submissions through another
//! thread would reorder them against those uploads — a correctness risk, for nothing.

use crate::export_worker::MediaEntry;
use crate::plan_ahead;
use crate::sources::Sources;
use oa_doc::{Project, SeqId};
use oa_gpu::{GpuContext, RenderOptions, RenderStats, Renderer};
use oa_graph::registry::Registry;
use oa_media::MediaKind;
use oa_plan::{PlanOptions, PlanReport};
use oa_time::Time;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A viewer frame the UI wants.
pub struct Request {
    /// The UI's number for it (results say which request they answer).
    pub id: u64,
    pub project: Arc<Project>,
    pub seq: SeqId,
    pub t: Time,
    pub opts: PlanOptions,
    pub reference: bool,
    pub registry: Arc<Registry>,
    pub follower: Option<Arc<oa_audio::envelope::Follower>>,
    /// Which envelopes the follower has (part of what a plan is made from).
    pub follow: usize,
    /// Scrubbing: never wait for a decode (see `Sources::set_interactive`).
    pub interactive: bool,
    /// Playing: how long a frame may wait on its decoder.
    pub wait_budget: Option<Duration>,
    /// Decoders to get ready for clips about to start: (media, source time).
    pub warm: Vec<(u64, Time)>,
    /// Playing: the next frame, to plan while this one renders.
    pub next: Option<Time>,
}

/// What came of a request.
pub struct Rendered {
    pub id: u64,
    /// The picture (display-ready, submitted), or `None`: not ready yet — a shader or
    /// glyphs still being prepared, a file's first frame not decoded — keep the last one.
    pub texture: Option<wgpu::Texture>,
    pub size: [u32; 2],
    /// Built from exact frames (no stand-ins).
    pub settled: bool,
    pub error: Option<String>,
    pub report: PlanReport,
    pub stats: RenderStats,
    pub memory: oa_gpu::health::Memory,
    /// The decoders' status line.
    pub status: Option<String>,
    /// How long planning and rendering took here.
    pub ms: f32,
}

enum Msg {
    Frame(Box<Request>),
    /// Files the viewer may show (`reset`: forget everything first — a project opened).
    Media { entries: Vec<MediaEntry>, reset: bool },
    ClearCache,
    Budget(u64),
    WarmUp(Arc<Registry>),
}

/// The UI's handle on the viewer's render thread.
pub struct ViewerRender {
    tx: Sender<Msg>,
    rx: Receiver<Rendered>,
}

impl ViewerRender {
    pub fn start(gpu: Arc<GpuContext>, decoders: oa_media::DecoderChoice, options: RenderOptions, registry: Arc<Registry>, repaint: eframe::egui::Context, watch: crate::gpu_watch::GpuWatch) -> Self {
        let (tx, jobs) = channel();
        let (done, rx) = channel();
        let spawned = std::thread::Builder::new().name("oa-viewer".into()).spawn(move || run(gpu, decoders, options, registry, jobs, done, repaint, watch));
        if let Err(e) = spawned {
            eprintln!("couldn't start the viewer's render thread: {e}");
        }
        ViewerRender { tx, rx }
    }

    pub fn request(&self, r: Request) {
        let _ = self.tx.send(Msg::Frame(Box::new(r)));
    }

    pub fn set_media(&self, entries: Vec<MediaEntry>, reset: bool) {
        let _ = self.tx.send(Msg::Media { entries, reset });
    }

    pub fn clear_cache(&self) {
        let _ = self.tx.send(Msg::ClearCache);
    }

    pub fn set_budget(&self, bytes: u64) {
        let _ = self.tx.send(Msg::Budget(bytes));
    }

    /// New effects (plugins changed): their pipelines built ahead, the cache emptied.
    pub fn warm_up(&self, registry: Arc<Registry>) {
        let _ = self.tx.send(Msg::WarmUp(registry));
    }

    /// Every result since the last call, oldest first.
    pub fn results(&self) -> Vec<Rendered> {
        std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    gpu: Arc<GpuContext>,
    decoders: oa_media::DecoderChoice,
    options: RenderOptions,
    registry: Arc<Registry>,
    jobs: Receiver<Msg>,
    done: Sender<Rendered>,
    repaint: eframe::egui::Context,
    watch: crate::gpu_watch::GpuWatch,
) {
    let mut renderer = Renderer::new(gpu.clone(), options);
    let _ = renderer.warm_up(&registry);
    let mut sources = Sources::new();
    let mut ahead = plan_ahead::PlanAhead::start();
    let mut ring: [Option<wgpu::Texture>; 3] = [None, None, None];
    let mut turn = 0;
    while let Ok(first) = jobs.recv() {
        // Everything waiting, in order; of the frames, only the newest.
        let mut frame = None;
        for msg in std::iter::once(first).chain(std::iter::from_fn(|| jobs.try_recv().ok())) {
            match msg {
                Msg::Frame(r) => frame = Some(r),
                Msg::Media { entries, reset } => add_media(&gpu, &decoders, &mut sources, &mut renderer, entries, reset),
                Msg::ClearCache => renderer.clear_cache(),
                Msg::Budget(bytes) => renderer.options.vram_budget = bytes,
                Msg::WarmUp(registry) => {
                    let _ = renderer.warm_up(&registry);
                    renderer.clear_cache();
                }
            }
        }
        let Some(r) = frame else { continue };
        let started = Instant::now();
        let result = render(&gpu, &mut renderer, &mut sources, &mut ahead, &mut ring, &mut turn, &r);
        let (texture, size, settled, report, error) = match result {
            Ok(f) => {
                // On the GPU now: watched until it's done (`gpu_watch.rs`).
                if f.texture.is_some() {
                    watch.submitted(&gpu.queue, f.effects);
                }
                (f.texture, f.size, f.settled, f.report, None)
            }
            Err(e) => (None, [0, 0], false, PlanReport::default(), Some(e)),
        };
        let rendered = Rendered {
            id: r.id,
            settled: texture.is_some() && settled,
            texture,
            size,
            error,
            report,
            stats: renderer.stats.clone(),
            memory: renderer.memory(),
            status: oa_gpu::FrameSource::status(&sources),
            ms: started.elapsed().as_secs_f32() * 1000.0,
        };
        if done.send(rendered).is_err() {
            return;
        }
        repaint.request_repaint();
    }
}

/// One request's outcome.
struct Frame {
    /// `None`: not ready yet.
    texture: Option<wgpu::Texture>,
    size: [u32; 2],
    settled: bool,
    report: PlanReport,
    /// The effects it used (for the GPU watch).
    effects: Vec<Arc<str>>,
}

/// Plans (or takes the plan made ahead) and renders one request into the next display
/// texture of the ring.
#[allow(clippy::too_many_arguments)]
fn render(
    gpu: &Arc<GpuContext>,
    renderer: &mut Renderer,
    sources: &mut Sources,
    ahead: &mut plan_ahead::PlanAhead,
    ring: &mut [Option<wgpu::Texture>; 3],
    turn: &mut usize,
    r: &Request,
) -> Result<Frame, String> {
    let key = |t: Time| plan_ahead::PlanKey {
        document: Arc::as_ptr(&r.project) as usize,
        seq: r.seq,
        t,
        scale: (r.opts.render_scale as f32).to_bits(),
        variant: r.opts.variant.map_or(0, |v| v.0),
        see_through: r.opts.see_through,
        reference: r.reference,
        follow: r.follow,
        text_supersample: r.opts.text_supersample,
    };
    let job = |t: Time| plan_ahead::Job { key: key(t), project: r.project.clone(), opts: r.opts.clone(), registry: r.registry.clone(), follower: r.follower.clone() };
    // Planned while the last frame rendered, if playback got here as expected.
    let planned = match ahead.take(&key(r.t)) {
        Some(p) => p,
        None => plan_ahead::plan(&job(r.t)).ok_or_else(|| {
            oa_plan::plan_frame(&r.project, r.seq, r.t, &r.opts, &r.registry).err().map_or_else(|| "the frame couldn't be planned".to_string(), |e| e.to_string())
        })?,
    };
    let report = planned.report;
    let effects = planned.graph.effect_types();
    if let Some(next) = r.next {
        ahead.ask(job(next));
    }
    sources.set_interactive(r.interactive);
    sources.set_wait_budget(r.wait_budget);
    for (media, at) in &r.warm {
        sources.warm(*media, *at);
    }
    let image = renderer.render(&planned.graph, &r.registry, sources);
    sources.set_interactive(false);
    sources.set_wait_budget(None);
    let image = match image {
        Ok(image) => image,
        Err(oa_gpu::RenderError::NotReady) => return Ok(Frame { texture: None, size: [0, 0], settled: false, report, effects }),
        Err(e) => return Err(e.to_string()),
    };
    let settled = oa_gpu::FrameSource::settled(sources);
    *turn = (*turn + 1) % ring.len();
    let texture = oa_gpu::readback::display_texture_into(gpu, renderer.pipelines(), &image, ring[*turn].take()).map_err(|e| e.to_string())?;
    ring[*turn] = Some(texture.clone());
    Ok(Frame { texture: Some(texture), size: image.size, settled, report, effects })
}

/// Files the viewer may show: registered with its decoders (stills decode in the
/// background), after forgetting everything when `reset`.
fn add_media(gpu: &Arc<GpuContext>, decoders: &oa_media::DecoderChoice, sources: &mut Sources, renderer: &mut Renderer, entries: Vec<MediaEntry>, reset: bool) {
    if reset {
        sources.forget_all();
        renderer.clear_cache();
        sources.set_video(None);
    }
    let mut video = sources.take_video().unwrap_or_else(|| oa_media::frame_source(gpu, decoders.clone()));
    for m in entries {
        if sources.knows(m.id) {
            continue;
        }
        sources.register(m.id, m.kind);
        let Some(track) = m.track else { continue };
        match m.kind {
            MediaKind::Still => sources.stills_mut().add_in_background(m.id, &m.path, &track),
            MediaKind::Video => video.add(m.id, &m.path, track),
            MediaKind::Audio => {}
        }
    }
    sources.set_video(Some(video));
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::{CanvasSize, FormatVariant, Item, ItemId, ItemKind, Sequence, Track, TrackId, TrackKind, VariantId};
    use oa_time::{FrameRate, TimeRange};

    fn project() -> Arc<Project> {
        let mut p = Project::new("viewer");
        let v = FormatVariant { id: VariantId(3), name: "16:9".into(), size: CanvasSize { width: 320, height: 180 }, overrides: Default::default() };
        let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, v);
        let mut track = Track::new(TrackId(5), "V1", TrackKind::Video);
        track.items.push(Item::new(ItemId(6), "red", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(5))));
        seq.tracks.push(Arc::new(track));
        p.sequences.insert(SeqId(1), Arc::new(seq));
        Arc::new(p)
    }

    fn request(id: u64, project: &Arc<Project>, registry: &Arc<Registry>, seq: SeqId) -> Request {
        Request {
            id,
            project: project.clone(),
            seq,
            t: Time::from_seconds(1),
            opts: PlanOptions { variant: Some(oa_doc::VariantId(3)), render_scale: 0.5, ..Default::default() },
            reference: false,
            registry: registry.clone(),
            follower: None,
            follow: 0,
            interactive: true,
            wait_budget: None,
            warm: Vec::new(),
            next: None,
        }
    }

    fn wait_for(v: &ViewerRender, id: u64) -> Vec<Rendered> {
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !got.iter().any(|r: &Rendered| r.id == id) {
            assert!(Instant::now() < deadline, "no answer to {id}");
            got.extend(v.results());
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    }

    /// The viewer's frames come from its own thread: a picture of the asked size, exact,
    /// submitted; a burst of requests renders the newest (not a queue of them all); a
    /// frame that can't be planned says so instead of stopping the thread.
    #[test]
    fn frames_render_off_the_ui_thread() {
        let Ok(gpu) = GpuContext::new_headless() else { return };
        let gpu = Arc::new(gpu);
        let registry = Arc::new(Registry::with_builtins());
        let options = RenderOptions { fusion: oa_gpu::FusionMode::Blocking, wait: true, ..Default::default() };
        let v = ViewerRender::start(gpu, oa_media::DecoderChoice::default(), options, registry.clone(), eframe::egui::Context::default(), Default::default());
        let p = project();

        v.request(request(1, &p, &registry, SeqId(1)));
        let first = wait_for(&v, 1);
        let r = first.iter().find(|r| r.id == 1).unwrap();
        assert!(r.error.is_none(), "{:?}", r.error);
        let texture = r.texture.as_ref().expect("a picture");
        assert_eq!((texture.width(), texture.height(), r.size), (160, 90, [160, 90]));
        assert!(r.settled);

        for id in 2..=9 {
            v.request(request(id, &p, &registry, SeqId(1)));
        }
        let burst = wait_for(&v, 9);
        assert!(burst.len() < 8, "{} renders for a burst of 8", burst.len());
        assert_eq!(burst.last().unwrap().id, 9);

        v.request(request(10, &p, &registry, SeqId(42)));
        let bad = wait_for(&v, 10);
        assert!(bad.last().unwrap().error.is_some(), "a sequence that isn't there is an error");
        v.request(request(11, &p, &registry, SeqId(1)));
        assert!(wait_for(&v, 11).last().unwrap().texture.is_some(), "and the thread carries on");
    }
}
