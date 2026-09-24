//! Exports run on a thread of their own, with their own renderer and decoders.
//!
//! Rendering an export inside the UI loop tied it to the screen: a few frames per UI
//! frame, then a wait for vsync and for the whole editor to redraw, with the viewer's
//! renderer and decoders shared (and their caches thrashed) between the two. Here the
//! GPU is fed frame after frame as fast as it and the encoder can go, from a snapshot
//! of the project taken when the export starts (edits made meanwhile don't leak into
//! the file). The UI only reads the progress, now and then picks up a small picture of
//! the latest frame, and asks it to stop.

use crate::sources::Sources;
use oa_doc::{Project, SeqId};
use oa_export::{ExportOptions, ExportSummary, Exporter};
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer};
use oa_graph::registry::Registry;
use oa_media::{MediaKind, VideoTrack};
use oa_time::Time;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the Exporting window's picture is refreshed.
const PREVIEW_EVERY: Duration = Duration::from_millis(250);
/// Frames rendered between checks for a cancel and a new preview.
const BATCH: u64 = 8;

/// A file the export may need to decode.
pub struct MediaEntry {
    pub id: u64,
    pub kind: MediaKind,
    pub path: PathBuf,
    pub track: Option<VideoTrack>,
}

/// The latest frame, ready to show (display-encoded), and where it is on the timeline.
pub struct Preview {
    pub texture: wgpu::Texture,
    pub size: [u32; 2],
    pub at: Time,
}

/// What the export thread shares with the UI.
#[derive(Default)]
struct Shared {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    preview: Mutex<Option<Preview>>,
    encoder: Mutex<String>,
}

/// A running export.
pub struct ExportRun {
    shared: Arc<Shared>,
    result: Receiver<Result<ExportSummary, String>>,
}

pub enum Poll {
    Running,
    Finished(Result<ExportSummary, String>),
}

impl ExportRun {
    /// (frames written, frames in all) — 0 of 0 while the sound is being mixed down.
    pub fn progress(&self) -> (u64, u64) {
        (self.shared.done.load(Ordering::Relaxed), self.shared.total.load(Ordering::Relaxed))
    }

    /// The newest picture of the export, if there's one the UI hasn't taken yet.
    pub fn take_preview(&self) -> Option<Preview> {
        self.shared.preview.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Which encoder is writing the file (once it's open).
    pub fn encoder(&self) -> String {
        self.shared.encoder.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn poll(&self) -> Poll {
        match self.result.try_recv() {
            Ok(result) => Poll::Finished(result),
            Err(TryRecvError::Empty) => Poll::Running,
            Err(TryRecvError::Disconnected) => Poll::Finished(Err("the export stopped unexpectedly".into())),
        }
    }
}

impl Drop for ExportRun {
    /// Dropping the handle cancels the export (the thread stops within a batch).
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

/// Everything an export thread needs, owned.
pub struct Job {
    pub gpu: Arc<GpuContext>,
    /// How video files get decoded (as the editor does).
    pub decoders: oa_media::DecoderChoice,
    pub registry: Arc<Registry>,
    pub project: Arc<Project>,
    pub media: Vec<MediaEntry>,
    pub seq: SeqId,
    pub path: PathBuf,
    pub options: ExportOptions,
    pub vram_budget: u64,
}

/// Starts exporting on a new thread.
pub fn start(job: Job) -> ExportRun {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_shared = shared.clone();
    let spawned = std::thread::Builder::new().name("oa-export".into()).spawn(move || {
        let result = run(job, &thread_shared);
        let _ = tx.send(result);
    });
    if let Err(e) = spawned {
        // The receiver sees the sender dropped: reported as a failed export.
        eprintln!("couldn't start the export thread: {e}");
    }
    ExportRun { shared, result: rx }
}

fn run(job: Job, shared: &Shared) -> Result<ExportSummary, String> {
    let mut exporter = Exporter::start(&job.project, job.seq, &job.path, &job.options).map_err(|e| e.to_string())?;
    shared.total.store(exporter.progress().1, Ordering::Relaxed);
    *shared.encoder.lock().unwrap_or_else(|e| e.into_inner()) = exporter.encoder();

    // Its own renderer: fused pipelines built before use (an export waits for them
    // anyway), and a cache of its own.
    let mut renderer = Renderer::new(
        job.gpu.clone(),
        RenderOptions { fusion: FusionMode::Blocking, wait: true, vram_budget: job.vram_budget, ..Default::default() },
    );
    let mut sources = sources_for(&job);
    sources.set_interactive(false);

    let mut last_preview: Option<Instant> = None;
    loop {
        if shared.cancel.load(Ordering::Relaxed) {
            return Err("canceled".into());
        }
        let finished = exporter
            .step(&job.project, &job.gpu, &mut renderer, &job.registry, &mut sources, BATCH)
            .map_err(|e| e.to_string())?;
        shared.done.store(exporter.progress().0, Ordering::Relaxed);
        if last_preview.is_none_or(|t| t.elapsed() >= PREVIEW_EVERY)
            && let Some((image, at)) = exporter.last_frame()
            && let Ok(texture) = oa_gpu::readback::display_texture_into(&job.gpu, renderer.pipelines(), image, None)
        {
            *shared.preview.lock().unwrap_or_else(|e| e.into_inner()) = Some(Preview { texture, size: image.size, at: *at });
            last_preview = Some(Instant::now());
        }
        if finished {
            return exporter.finish().map_err(|e| e.to_string());
        }
    }
}

/// Decoders for the export's own use: stills uploaded now, videos through the hardware
/// decoder (a separate instance from the viewer's, so neither seeks the other's).
fn sources_for(job: &Job) -> Sources {
    let mut sources = Sources::new();
    let mut video = oa_media::frame_source(&job.gpu, job.decoders.clone());
    for m in &job.media {
        sources.register(m.id, m.kind);
        let Some(track) = &m.track else { continue };
        match m.kind {
            MediaKind::Still => {
                if let Err(e) = sources.stills_mut().add(&job.gpu, m.id, &m.path, track) {
                    eprintln!("export: {}: {e}", m.path.display());
                }
            }
            MediaKind::Video => video.add(m.id, &m.path, track.clone()),
            _ => {}
        }
    }
    sources.set_video(Some(video));
    sources
}
