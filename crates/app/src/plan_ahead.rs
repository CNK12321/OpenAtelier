//! Pipelining playback: while one frame renders on the GPU, the next is planned on a
//! thread of its own.
//!
//! Playback renders on the sequence's own frames (`App::render_time`), so the next frame
//! is known — the one after this. Its plan (the render graph, optimized) is worked out
//! here from the same snapshot of the project while the UI thread is busy rendering
//! this one; when the next UI frame comes to it, the graph is ready and planning costs
//! nothing. Anything that doesn't match — an edit, a jump, another format — is planned
//! as before, on the spot. Only the newest request is worked on.

use oa_doc::{Project, SeqId};
use oa_graph::registry::Registry;
use oa_graph::{optimize, Graph, KeyContext, OptLevel};
use oa_plan::{plan_frame, PlanOptions, PlanReport};
use oa_time::Time;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// What a plan was made from; a plan is used only for exactly this.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanKey {
    pub document: usize,
    pub seq: SeqId,
    pub t: Time,
    pub scale: u32,
    pub variant: u64,
    pub see_through: bool,
    pub reference: bool,
    pub follow: usize,
    pub text_supersample: u32,
}

pub struct Job {
    pub key: PlanKey,
    pub project: Arc<Project>,
    pub opts: PlanOptions,
    pub registry: Arc<Registry>,
    pub follower: Option<Arc<oa_audio::envelope::Follower>>,
}

/// A frame planned ahead.
pub struct Planned {
    pub key: PlanKey,
    pub graph: Graph,
    pub report: PlanReport,
}

pub struct PlanAhead {
    tx: Sender<Job>,
    rx: Receiver<Planned>,
    ready: Option<Planned>,
}

impl PlanAhead {
    pub fn start() -> Self {
        let (tx, jobs) = channel::<Job>();
        let (done, rx) = channel();
        let spawned = std::thread::Builder::new().name("oa-plan-ahead".into()).spawn(move || {
            while let Ok(first) = jobs.recv() {
                // Only the newest: playback moved on past the others.
                let job = std::iter::once(first).chain(std::iter::from_fn(|| jobs.try_recv().ok())).last().expect("one at least");
                if let Some(planned) = plan(&job)
                    && done.send(planned).is_err()
                {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            // Every frame is then planned on the spot, as it was.
            eprintln!("couldn't start the planning thread: {e}");
        }
        PlanAhead { tx, rx, ready: None }
    }

    /// Plans a frame in the background.
    pub fn ask(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// The plan for exactly `key`, if it's been made.
    pub fn take(&mut self, key: &PlanKey) -> Option<Planned> {
        while let Ok(p) = self.rx.try_recv() {
            self.ready = Some(p);
        }
        self.ready.take_if(|p| p.key == *key)
    }
}

/// The frame's graph, optimized — what `App::render_frame` would make of it.
pub fn plan(job: &Job) -> Option<Planned> {
    let level = if job.key.reference { OptLevel::Reference } else { OptLevel::Full };
    let make = || plan_frame(&job.project, job.key.seq, job.key.t, &job.opts, &job.registry);
    let planned = match &job.follower {
        Some(f) => oa_params::signal::with(f.at(job.key.t), make),
        None => make(),
    }
    .ok()?;
    Some(Planned { key: job.key.clone(), graph: optimize(&planned.graph, level, KeyContext::default()), report: planned.report })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::*;
    use oa_time::{FrameRate, TimeRange};

    /// A plan made ahead is the same graph as one made on the spot, and it's only
    /// handed out for exactly the frame it was made for.
    #[test]
    fn plans_ahead_match_and_only_serve_their_frame() {
        let mut p = Project::new("ahead");
        let v = FormatVariant { id: VariantId(3), name: "16:9".into(), size: CanvasSize { width: 320, height: 180 }, overrides: Default::default() };
        let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, v);
        let mut track = Track::new(TrackId(5), "V1", TrackKind::Video);
        track.items.push(Item::new(ItemId(6), "red", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(5))));
        seq.tracks.push(Arc::new(track));
        p.sequences.insert(SeqId(1), Arc::new(seq));
        let project = Arc::new(p);
        let registry = Arc::new(Registry::with_builtins());
        let key = |t: Time| PlanKey { document: Arc::as_ptr(&project) as usize, seq: SeqId(1), t, scale: 0, variant: 3, see_through: false, reference: false, follow: 0, text_supersample: 1 };
        let job = |t: Time| Job { key: key(t), project: project.clone(), opts: PlanOptions { variant: Some(VariantId(3)), ..Default::default() }, registry: registry.clone(), follower: None };

        let mut ahead = PlanAhead::start();
        let t = FrameRate::FPS_30.frame_start(10);
        ahead.ask(job(t));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let got = loop {
            // Another frame's key never takes it.
            assert!(ahead.take(&key(FrameRate::FPS_30.frame_start(11))).is_none());
            if let Some(p) = ahead.take(&key(t)) {
                break p;
            }
            assert!(std::time::Instant::now() < deadline, "never planned");
            ahead.ask(job(t));
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let here = plan(&job(t)).unwrap();
        assert_eq!(format!("{:?}", got.graph), format!("{:?}", here.graph));
    }
}
