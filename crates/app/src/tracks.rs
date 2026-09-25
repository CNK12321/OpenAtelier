//! The track editor: a position following something in the picture. Opened from a
//! position's right-click menu (Animate → "Edit track…"), it takes over the viewer — the
//! track drawn over the picture — with a small panel of ways to make one:
//!
//! * **Auto** — click a point; CoTracker (AI) follows it through the footage under it (the
//!   clip's own footage for a point on the clip, or when stabilizing it), for as long before
//!   and after as you set, at the detail you pick. The tracker is an optional download
//!   (`oa_track::engine`), on the CPU or an NVIDIA GPU; it's removed in Settings.
//! * **By hand** — click where it is; the playhead steps on a few frames; click again,
//!   and so on, the track growing as you go.
//! * **Record** — arm it, click in the picture to start: the timeline plays (at a speed
//!   you pick) while you follow the thing with the mouse; click or Space to stop. The
//!   rough path is smoothed (how much is yours to set) and thinned.
//! * Or pick a track made before.
//!
//! A position then uses its track to **Follow** it or to **Stabilize** the clip against it
//! (locked, or smoothed; see `oa_params::TrackUse`).
//!
//! Tracks are the project's (`Project::tracks`, `oa_params::PointTrack`), so one can be
//! followed by several things. A clip's **position** follows its track live: the
//! property becomes an **offset** from the track (`Modulator::Track`) until the track is
//! let go. A point on a clip (anchor, focus, an effect's point) is keyed to the track
//! instead, since where a canvas point lands on a clip depends on the clip.

use crate::App;
use eframe::egui;
use oa_doc::{ItemId, ItemKind, Op, ParamTarget};
use oa_params::{Curve, Interp, KeyframeAnchor, Keyframe, ParamId, ParamSource, PointTrack, Stabilize, TrackUse, Unit, Value};
use oa_plan::scene::Placement;
use oa_time::{Time, TimeRange};
use oa_track::engine::{Compute, Footage, Job, Msg, Tracked, Tracker};
use oa_track::Sample;
use std::sync::Arc;
use std::time::Instant;

/// How a property maps to the picture.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Space {
    /// A clip's position: where its anchor lands, as a fraction of the canvas.
    Position,
    /// A point on the clip, as a fraction of its own size.
    OnClip,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Mode {
    Follow,
    Hand,
    Record,
}

/// What the edited position does with its track (the editor's choice; the property
/// keeps its own copy in `Modulator::Track`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Usage {
    /// Goes where the track goes.
    Follow,
    /// Moves against it: `lock` holds the point still — at `anchor` (track units: a
    /// point picked on the frame) or, with none, where it is at the playhead; otherwise
    /// only movement quicker than `smoothing` seconds goes. `strength` is how much of it
    /// (0–1).
    Stabilize { strength: f64, lock: bool, smoothing: f64, anchor: Option<[f64; 2]> },
}

impl Usage {
    const STABILIZE: Usage = Usage::Stabilize { strength: 1.0, lock: false, smoothing: 1.0, anchor: None };

    fn of(mode: &TrackUse) -> Usage {
        match mode {
            TrackUse::Follow => Usage::Follow,
            TrackUse::Stabilize(s) => Usage::Stabilize { strength: s.strength, lock: s.lock, smoothing: s.smoothing, anchor: s.lock.then_some(s.anchor) },
        }
    }

    /// The property's mode for `track`; a lock without a picked point holds it where it
    /// is at `at`.
    fn to_track_use(self, track: &PointTrack, at: Time) -> TrackUse {
        match self {
            Usage::Follow => TrackUse::Follow,
            Usage::Stabilize { strength, lock, smoothing, anchor } => {
                let anchor = anchor.or_else(|| track.at(at)).unwrap_or([0.0; 2]);
                TrackUse::Stabilize(Stabilize::new(track, strength, lock, smoothing, anchor))
            }
        }
    }
}

/// What the editor was opened for (Animate → Edit track… or Edit stabilization…).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Making a track and having the property follow it.
    Track,
    /// Steadying the clip with a track of its own picture.
    Stabilize,
}

/// The open track editor.
pub struct TrackEditor {
    pub item: ItemId,
    pub target: ParamTarget,
    pub param: String,
    space: Space,
    mode: Mode,
    /// The track being made or changed (the project's), if there is one yet.
    track: Option<u64>,
    /// Following the track or stabilizing with it (positions only).
    usage: Usage,
    /// What it was opened for.
    purpose: Purpose,
    /// The next click in the picture sets where a locked stabilization holds the point.
    picking_anchor: bool,
    /// Follow: the point to start from (timeline time, canvas px).
    start: Option<(Time, [f64; 2])>,
    /// Follow: how long before and after the start point to follow it (seconds), and
    /// every how-many frames to look (1: every frame).
    before: f64,
    after: f64,
    detail: u32,
    /// By hand: frames the playhead steps on after each click.
    step: u32,
    /// Record: playback speed and smoothing (0–1); armed waits for the click that starts.
    speed: f64,
    smoothness: f64,
    armed: bool,
    recording: Option<Recording>,
    run: Option<Run>,
    /// What happened last, for the panel.
    note: Option<String>,
}

struct Recording {
    began: Instant,
    from: Time,
    to: Time,
    samples: Vec<Sample>,
}

/// A CoTracker run: the job, and what it needs to bring the result back.
struct Run {
    job: Job,
    step: String,
    progress: Option<f32>,
    /// Where it runs, once it says.
    device: Option<String>,
    /// The result, once it comes (it can come a frame before the job ends).
    result: Option<Tracked>,
    /// Frames done so far (reading the footage, then following), for the panel.
    frames_done: usize,
    /// Where the point has been found so far: (footage frame, canvas px), drawn as it grows.
    live: Vec<(usize, [f64; 2])>,
    footage: Footage,
    /// The clip whose footage is followed, its native size, and the timeline stretch.
    clip: ItemId,
    native: [f64; 2],
    range: TimeRange,
}

/// The tracker's setup, which outlives the editor (it takes minutes).
#[derive(Default)]
pub struct TrackerSetup {
    job: Option<Job>,
    step: String,
    progress: Option<f32>,
    pub error: Option<String>,
}

fn tracker() -> Tracker {
    Tracker::new(Tracker::default_root())
}

/// `source` with its keyframes (or plain value) replaced by `curve`, keeping any motion
/// (wiggle, wave, sound) layered on it.
fn with_curve(source: ParamSource, curve: Curve) -> ParamSource {
    match source {
        ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(with_curve(*base, curve)), modulator },
        _ => ParamSource::Animated(curve),
    }
}

impl App {
    /// Whether `param` is a position the track editor can drive, and how.
    pub(crate) fn track_space(&self, item: ItemId, target: &ParamTarget, param: &str) -> Option<Space> {
        let it = self.editor.item(item)?;
        let schema = match target {
            ParamTarget::Item => oa_doc::schema::visual().iter().find(|s| s.id.as_str() == param)?.clone(),
            ParamTarget::Effect(id) => {
                let fx = it.effects.iter().find(|e| e.id == *id)?;
                // A Surface's points are offsets, dragged in the viewer.
                if self.registry.effect(&fx.type_id).is_some_and(|d| d.editor.as_deref() == Some(oa_graph::registry::EDITOR_SURFACE)) {
                    return None;
                }
                self.registry.effect(&fx.type_id)?.params.iter().find(|s| s.id.as_str() == param)?.clone()
            }
            _ => return None,
        };
        if !matches!(schema.default, Value::Vec2(_)) {
            return None;
        }
        match schema.unit {
            Unit::CanvasFraction if param == oa_doc::schema::POSITION => Some(Space::Position),
            Unit::SourceFraction => Some(Space::OnClip),
            _ => None,
        }
    }

    /// Whether the clip can be stabilized (its position, and moving footage to track).
    pub(crate) fn can_stabilize(&self, item: ItemId, target: &ParamTarget, param: &str) -> bool {
        self.track_space(item, target, param) == Some(Space::Position)
            && match self.editor.item(item).map(|i| &i.kind) {
                Some(ItemKind::Media { media }) => self.editor.pool_item(*media).is_some_and(|p| p.probe.video.as_ref().is_some_and(|v| !v.still)),
                _ => false,
            }
    }

    pub(crate) fn edit_track(&mut self, item: ItemId, target: ParamTarget, param: &str, purpose: Purpose) {
        let Some(space) = self.track_space(item, &target, param) else { return };
        self.set_playing(false);
        self.selection = Some(item);
        let source = self.editor.param_source(item, &target, param);
        let track = source.as_ref().and_then(|s| s.find_track().map(|(t, _)| t.id)).filter(|id| self.editor.doc.project().tracks.contains_key(id));
        let current = source.as_ref().and_then(|s| s.track_use().map(Usage::of));
        // Opened to stabilize: the stabilization it has, or a new one; opened for the
        // track: whatever the property does with it now (following, if nothing yet).
        let usage = match purpose {
            Purpose::Stabilize => current.filter(|u| matches!(u, Usage::Stabilize { .. })).unwrap_or(Usage::STABILIZE),
            Purpose::Track => current.unwrap_or(Usage::Follow),
        };
        self.track_editor = Some(TrackEditor {
            item,
            target,
            param: param.to_string(),
            space,
            mode: Mode::Follow,
            track,
            usage,
            purpose,
            picking_anchor: false,
            start: None,
            before: 0.0,
            after: 5.0,
            detail: 1,
            step: 5,
            speed: 0.5,
            smoothness: 0.4,
            armed: false,
            recording: None,
            run: None,
            note: None,
        });
    }

    fn placement_at(&self, item: ItemId, t: Time) -> Option<Placement> {
        oa_edit::transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), item, t).ok()
    }

    fn canvas_size(&self) -> [f64; 2] {
        self.editor.sequence().variant(self.variant_id()).map_or([1920.0, 1080.0], |v| [v.size.width as f64, v.size.height as f64])
    }

    /// Canvas px → track units (fractions of the canvas from its center), and back.
    fn canvas_to_track(&self, c: [f64; 2]) -> [f64; 2] {
        let s = self.canvas_size();
        [c[0] / s[0] - 0.5, c[1] / s[1] - 0.5]
    }

    fn track_to_canvas(&self, p: [f64; 2]) -> [f64; 2] {
        let s = self.canvas_size();
        [(p[0] + 0.5) * s[0], (p[1] + 0.5) * s[1]]
    }

    /// The property's value that puts the point at canvas px `c` at `t`.
    fn value_for(&self, ed: &TrackEditor, t: Time, c: [f64; 2]) -> Option<[f64; 2]> {
        let p = self.placement_at(ed.item, t)?;
        match ed.space {
            // The anchor lands at the pivot; the position moves it one canvas per unit.
            Space::Position => {
                let now = self.editor.param_value(ed.item, &ed.target, &ed.param, t).and_then(|v| v.as_vec2()).unwrap_or([0.0; 2]);
                let size = self.canvas_size();
                Some([now[0] + (c[0] - p.pivot[0]) / size[0], now[1] + (c[1] - p.pivot[1]) / size[1]])
            }
            Space::OnClip => p.to_layer_fraction(c),
        }
    }

    /// Where the property puts its point on the canvas at `t`.
    fn canvas_of(&self, ed: &TrackEditor, t: Time) -> Option<[f64; 2]> {
        let p = self.placement_at(ed.item, t)?;
        match ed.space {
            Space::Position => Some(p.pivot),
            Space::OnClip => {
                let v = self.editor.param_value(ed.item, &ed.target, &ed.param, t).and_then(|v| v.as_vec2())?;
                Some(p.to_canvas.apply([v[0] * p.native[0], v[1] * p.native[1]]))
            }
        }
    }

    /// The working track (a copy), or a new empty one with a fresh id and name.
    fn working_track(&mut self) -> PointTrack {
        let existing = self.track_editor.as_ref().and_then(|e| e.track).and_then(|id| self.editor.doc.project().tracks.get(&id).cloned());
        match existing {
            Some(t) => (*t).clone(),
            None => {
                let n = self.editor.doc.project().tracks.len() + 1;
                let id = self.editor.doc.alloc_id();
                let name = self.editor.item(self.track_editor.as_ref().map_or(ItemId(0), |e| e.item)).map_or(format!("Track {n}"), |i| format!("Track {n} ({})", i.name));
                PointTrack::new(id, &name)
            }
        }
    }

    /// Stores `track` in the project, brings every property following it up to date,
    /// and has the edited property follow it (a position, live, as an offset; a point on
    /// a clip, keyed to it). One undo step (`coalesce` joins steps with the same key).
    fn commit_track(&mut self, track: PointTrack, label: &str, coalesce: Option<&str>) {
        let Some(ed) = self.track_editor.as_ref() else { return };
        let track = Arc::new(track);
        let mut ops = vec![Op::SetPointTrack { id: track.id, track: Some(track.clone()) }];
        // Everything already following it.
        let project = self.editor.doc.snapshot();
        for (seq, s) in &project.sequences {
            for it in std::iter::once(&s.background).chain(s.tracks.iter().flat_map(|t| t.items.iter())) {
                let sets = std::iter::once((ParamTarget::Item, &it.params)).chain(it.effects.iter().map(|e| (ParamTarget::Effect(e.id), &e.params)));
                for (target, set) in sets {
                    for (param, source) in &set.0 {
                        if source.find_track().is_some_and(|(t, _)| t.id == track.id) {
                            ops.push(Op::SetParam { seq: *seq, item: it.id, target: target.clone(), param: param.clone(), source: Some(source.clone().retracked(&track)) });
                        }
                    }
                }
            }
        }
        let (item, target, param) = (ed.item, ed.target.clone(), ed.param.clone());
        let source = self.editor.param_source(item, &target, &param);
        let follows = source.as_ref().and_then(|s| s.find_track()).is_some_and(|(t, _)| t.id == track.id);
        let Some(it) = self.editor.item(item).cloned() else { return };
        match ed.space {
            Space::Position if !follows => {
                let t0 = self.playhead.max(it.range.start).min(it.range.end() - Time(1));
                let t0 = if track.points.first().is_some_and(|p| p.0 > t0) || track.points.last().is_some_and(|p| p.0 < t0) { track.points[0].0.max(it.range.start).min(it.range.end() - Time(1)) } else { t0 };
                let clock = it.range.start;
                match ed.usage.to_track_use(&track, t0) {
                    // Following: the offset is what puts the anchor right on the tracked
                    // point (at the playhead, or where the track starts).
                    TrackUse::Follow => {
                        if let Some(at) = track.at(t0)
                            && let Some(v) = self.value_for(ed, t0, self.track_to_canvas(at))
                        {
                            let offset = [v[0] - at[0], v[1] - at[1]];
                            let base = source.unwrap_or(ParamSource::Static(Value::Vec2(v)));
                            ops.push(Op::SetParam { seq: self.editor.seq, item, target, param: ParamId::new(&param), source: Some(base.with_track(track.clone(), clock, offset)) });
                        }
                    }
                    // Stabilizing: the clip stays where it is at the playhead; the
                    // correction moves it from there.
                    mode @ TrackUse::Stabilize(_) => {
                        let now = self.editor.param_value(item, &target, &param, t0).and_then(|v| v.as_vec2()).unwrap_or([0.0; 2]);
                        let c = mode.contribution(&track, t0).unwrap_or([0.0; 2]);
                        let offset = [now[0] - c[0], now[1] - c[1]];
                        let base = source.unwrap_or(ParamSource::Static(Value::Vec2(now)));
                        ops.push(Op::SetParam { seq: self.editor.seq, item, target, param: ParamId::new(&param), source: Some(base.with_track_as(track.clone(), clock, offset, mode)) });
                    }
                }
            }
            Space::Position => {}
            Space::OnClip => {
                // Keyed where the track has points inside the clip.
                let keys: Vec<(Time, [f64; 2])> = track
                    .points
                    .iter()
                    .filter(|p| it.range.contains(p.0))
                    .filter_map(|p| Some((p.0, self.value_for(ed, p.0, self.track_to_canvas(p.1))?)))
                    .collect();
                if let Some(source) = self.keyed(item, &target, &param, &keys) {
                    ops.push(Op::SetParam { seq: self.editor.seq, item, target, param: ParamId::new(&param), source: Some(source) });
                }
            }
        }
        let result = match coalesce {
            Some(key) => self.editor.apply_drag(label, key, ops),
            None => self.editor.apply(label, ops),
        };
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
        if let Some(ed) = self.track_editor.as_mut() {
            ed.track = Some(track.id);
        }
    }

    /// `keys` (timeline time, value) as keyframes on the property: they replace the keys
    /// between the first and last of them; the others stay.
    fn keyed(&self, item: ItemId, target: &ParamTarget, param: &str, keys: &[(Time, [f64; 2])]) -> Option<ParamSource> {
        let (first, last) = (keys.first()?, keys.last()?);
        let it = self.editor.item(item)?;
        let source = self.editor.param_source(item, target, param);
        let default_anchor = if param == oa_doc::schema::FOCUS { KeyframeAnchor::SourceMedia } else { KeyframeAnchor::ClipStart };
        let anchor = source.as_ref().and_then(|s| s.curve()).map_or(default_anchor, |c| c.anchor);
        let clock = |t: Time| {
            let ctx = it.eval_context(t);
            if anchor == KeyframeAnchor::SourceMedia { ctx.source_time } else { ctx.clip_time }
        };
        let (lo, hi) = (clock(first.0).min(clock(last.0)), clock(first.0).max(clock(last.0)));
        let mut out: Vec<Keyframe> = source.as_ref().and_then(|s| s.curve()).map(|c| c.keys.iter().filter(|k| k.t < lo || k.t > hi).cloned().collect()).unwrap_or_default();
        out.extend(keys.iter().map(|(t, v)| Keyframe { t: clock(*t), value: Value::Vec2(*v), interp: Interp::Linear }));
        let mut curve = Curve::new(anchor, out);
        curve.keys.dedup_by_key(|k| k.t);
        Some(with_curve(source.unwrap_or(ParamSource::Static(Value::Vec2(first.1))), curve))
    }

    /// Stops the edited property following its track; it stays where it is now.
    fn let_go_of_track(&mut self) {
        let Some(ed) = self.track_editor.as_ref() else { return };
        let (item, target, param) = (ed.item, ed.target.clone(), ed.param.clone());
        self.let_go(item, &target, &param);
        if let Some(ed) = self.track_editor.as_mut() {
            ed.track = None;
        }
    }

    /// `param` of `item` stops following its track, keeping its place at the playhead.
    pub(crate) fn let_go(&mut self, item: ItemId, target: &ParamTarget, param: &str) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let Some(source) = self.editor.param_source(item, target, param) else { return };
        let t = self.playhead.max(it.range.start).min(it.range.end() - Time(1));
        let Some(shift) = source.track_contribution(&it.eval_context(t)) else { return };
        let op = Op::SetParam { seq: self.editor.seq, item, target: target.clone(), param: ParamId::new(param), source: Some(source.without_track(shift)) };
        if let Err(e) = self.editor.apply("Stop following track", vec![op]) {
            self.error = Some(e.to_string());
        }
    }

    /// Canvas-px samples → the working track: thinned to within `tolerance` px, put in
    /// over the stretch they cover, and committed.
    fn track_from_canvas(&mut self, samples: &[Sample], tolerance: f64, label: &str) -> usize {
        let points: Vec<(Time, [f64; 2])> = oa_track::simplify(samples, tolerance).into_iter().map(|s| (Time::from_seconds_f64(s.t), self.canvas_to_track(s.p))).collect();
        let n = points.len();
        let mut track = self.working_track();
        track.replace(&points);
        self.commit_track(track, label, None);
        n
    }

    /// Checks on the tracker's setup and a running track, every frame.
    pub(crate) fn poll_tracks(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.tracker_setup.job {
            let mut ended = None;
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    Msg::Step { label, .. } => {
                        self.tracker_setup.step = label;
                        self.tracker_setup.progress = None;
                    }
                    Msg::Progress(p) => self.tracker_setup.progress = Some(p),
                    Msg::Failed(e) => ended = Some(Some(e)),
                    Msg::Finished => ended = Some(None),
                    _ => {}
                }
            }
            if let Some(error) = ended {
                self.tracker_setup.job = None;
                self.tracker_setup.error = error;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(ed) = self.track_editor.as_mut() else { return };
        let Some(run) = ed.run.as_mut() else { return };
        let mut ended = None;
        let mut found = Vec::new();
        while let Ok(m) = run.job.rx.try_recv() {
            match m {
                Msg::Step { label, .. } => {
                    run.step = label;
                    run.progress = None;
                    run.frames_done = 0;
                }
                Msg::Progress(p) => run.progress = Some(p),
                // ffmpeg reading the footage: its frame count.
                Msg::Log(line) => {
                    if let Some(n) = oa_track::engine::ffmpeg_frames(&line) {
                        run.frames_done = n.min(run.footage.frames);
                        run.progress = Some(n as f32 / run.footage.frames.max(1) as f32);
                    }
                }
                Msg::Data(line) => {
                    if let Some(d) = oa_track::engine::parse_device(&line) {
                        run.device = Some(d);
                    }
                    if let Some(at) = oa_track::engine::parse_at(&line) {
                        found.push(at);
                    }
                    if let Some(t) = Tracked::parse(&line) {
                        run.result = Some(t);
                    }
                }
                Msg::Failed(e) => ended = Some(Err(e)),
                Msg::Finished => ended = Some(Ok(())),
                _ => {}
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        // Points found this frame: onto the growing path, and the playhead to the newest.
        if !found.is_empty() {
            self.track_live(found);
        }
        let Some(ed) = self.track_editor.as_mut() else { return };
        let Some(ended) = ended else { return };
        let run = ed.run.take().expect("checked");
        match (ended, run.result.clone()) {
            (Ok(()), Some(tracked)) => self.apply_tracked(&run, &tracked),
            (Ok(()), None) => ed.note = Some("The tracker finished without a track.".into()),
            (Err(e), _) => ed.note = Some(format!("Couldn't follow it: {e}")),
        }
    }

    /// The timeline time of a footage frame of `run`.
    fn run_time(run: &Run, frame: usize) -> Time {
        let f = (run.footage.time_of(frame) - run.footage.start) / run.footage.duration.max(1e-9);
        run.range.start + Time::from_seconds_f64(run.range.duration.as_seconds_f64() * f.clamp(0.0, 1.0))
    }

    /// Points the tracker has found so far (footage frame, footage px at its size): onto
    /// the path drawn over the picture, with the playhead on the newest.
    fn track_live(&mut self, found: Vec<(usize, [f64; 2])>) {
        let Some(run) = self.track_editor.as_ref().and_then(|e| e.run.as_ref()) else { return };
        let k = [run.native[0] / run.footage.size[0] as f64, run.native[1] / run.footage.size[1] as f64];
        let placed: Vec<(usize, [f64; 2], Time)> = found
            .into_iter()
            .filter_map(|(frame, p)| {
                let t = Self::run_time(run, frame);
                let place = self.placement_at(run.clip, t)?;
                Some((frame, place.to_canvas.apply([p[0] * k[0], p[1] * k[1]]), t))
            })
            .collect();
        let Some(&(_, _, newest)) = placed.last() else { return };
        if let Some(run) = self.track_editor.as_mut().and_then(|e| e.run.as_mut()) {
            for (frame, c, _) in placed {
                let at = run.live.partition_point(|p| p.0 < frame);
                run.live.insert(at, (frame, c));
            }
        }
        self.set_playhead(newest);
    }

    /// A finished run becomes the track: footage px → the footage clip's place on the
    /// canvas at each moment.
    fn apply_tracked(&mut self, run: &Run, tracked: &Tracked) {
        let (from, to) = (run.range.start.as_seconds_f64(), run.range.end().as_seconds_f64());
        let file = (run.footage.start, run.footage.start + run.footage.duration);
        // Footage that's already stabilized by this track is tracked in the track's own
        // terms: without the stabilization's move.
        let own = self.track_editor.as_ref().is_some_and(|e| e.item == run.clip);
        let samples = oa_track::engine::to_samples(&tracked.samples(&run.footage, run.native), |ft, px| {
            let t = from + (ft - file.0) / (file.1 - file.0).max(1e-9) * (to - from);
            let at = Time::from_seconds_f64(t);
            let c = self.placement_at(run.clip, at)?.to_canvas.apply(px);
            Some((t, if own { self.unstabilized(at, c) } else { c }))
        });
        let n = self.track_from_canvas(&samples, 0.75, "Follow with the tracker");
        self.set_track_note(&format!("Followed it for {:.1} s — {n} points.", to - from));
    }

    /// Starts CoTracker on the footage under the start point.
    fn start_follow(&mut self) {
        let Some(ed) = self.track_editor.as_ref() else { return };
        let Some((t, c)) = ed.start else { return };
        let (item, space, stabilizing) = (ed.item, ed.space, matches!(ed.usage, Usage::Stabilize { .. }));
        // The footage: the clip itself for a point on it or for stabilizing it (if it's
        // video), else the top video under the point.
        let is_video = |app: &App, id: ItemId| match app.editor.item(id).map(|i| &i.kind) {
            Some(ItemKind::Media { media }) => app.editor.pool_item(*media).is_some_and(|p| !p.missing && p.probe.has_video()),
            _ => false,
        };
        let clip = if stabilizing {
            if !is_video(self, item) {
                self.set_track_note("Only a video clip can be stabilized: this one has no moving picture to track.");
                return;
            }
            if !self.placement_at(item, t).is_some_and(|p| p.contains(c)) {
                self.set_track_note("Click a point on this clip itself — the thing in it that should hold still.");
                return;
            }
            Some(item)
        } else if space == Space::OnClip && is_video(self, item) {
            Some(item)
        } else {
            let project = self.editor.doc.snapshot();
            self.editor
                .sequence()
                .variant(self.variant_id())
                .map(|v| oa_plan::scene::layers_at(&project, self.editor.seq, v, t))
                .unwrap_or_default()
                .into_iter()
                .rev()
                .find(|l| l.item != item && is_video(self, l.item) && l.contains(c))
                .map(|l| l.item)
        };
        let Some(clip) = clip else {
            self.set_track_note("There's no video under that point to follow it through.");
            return;
        };
        let (Some(footage_item), Some(edited)) = (self.editor.item(clip).cloned(), self.editor.item(item).cloned()) else { return };
        let ItemKind::Media { media } = footage_item.kind else { return };
        let Some(pool) = self.editor.pool_item(media) else { return };
        let path = pool.decode_path.clone();
        // Where both clips are on the timeline.
        // Where both clips are, as long before and after the start point as asked.
        let (before, after, detail) = (ed.before, ed.after, ed.detail.max(1));
        let a = footage_item.range.start.max(edited.range.start).max(t - Time::from_seconds_f64(before.max(0.0)));
        let b = footage_item.range.end().min(edited.range.end()).min(t + Time::from_seconds_f64(after.max(0.0)));
        if b <= a {
            self.set_track_note("Nothing to follow there: set Before or After, over a stretch where the footage and this clip both play.");
            return;
        }
        let range = TimeRange::new(a, b - a);
        let (s0, s1) = (footage_item.eval_context(a).source_time.as_seconds_f64(), footage_item.eval_context(b).source_time.as_seconds_f64());
        if s1 <= s0 {
            self.set_track_note("Reversed or frozen footage can't be followed.");
            return;
        }
        let Some(p) = self.placement_at(clip, t) else { return };
        let Some(uv) = p.to_layer_fraction(c) else { return };
        let native = p.native;
        let frames = ((b - a).as_seconds_f64() * self.editor.sequence().rate.as_f64() / detail as f64).ceil() as usize + 1;
        let footage = Footage::new(path, s0, s1 - s0, frames, native);
        let at_file = footage_item.eval_context(t).source_time.as_seconds_f64();
        let small = [uv[0] * footage.size[0] as f64, uv[1] * footage.size[1] as f64];
        let tracker = tracker();
        let job = oa_track::engine::run(tracker.engine(), tracker.track_steps("ffmpeg", &footage, at_file, small));
        if let Some(ed) = self.track_editor.as_mut() {
            ed.note = None;
            ed.run = Some(Run { job, step: "Starting".into(), progress: None, device: None, result: None, frames_done: 0, live: Vec::new(), footage, clip, native, range });
        }
    }

    fn set_track_note(&mut self, note: &str) {
        if let Some(ed) = self.track_editor.as_mut() {
            ed.note = Some(note.to_string());
        }
    }

    /// Recording: the playhead runs at the chosen speed (no sound) while the pointer's
    /// path is taken down.
    fn step_recording(&mut self, pointer: Option<[f64; 2]>) {
        let Some(t) = self.track_editor.as_ref().and_then(|ed| ed.recording.as_ref().map(|rec| (rec.from + Time::from_seconds_f64(rec.began.elapsed().as_secs_f64() * ed.speed)).min(rec.to))) else { return };
        let pointer = pointer.map(|c| self.unstabilized(t, c));
        let Some(ed) = self.track_editor.as_mut() else { return };
        let Some(rec) = ed.recording.as_mut() else { return };
        if let Some(c) = pointer {
            rec.samples.push(Sample::new(t.as_seconds_f64(), c));
        }
        let done = t >= rec.to;
        self.set_playhead(t);
        if done {
            self.finish_recording();
        }
    }

    fn finish_recording(&mut self) {
        let Some(ed) = self.track_editor.as_mut() else { return };
        let Some(rec) = ed.recording.take() else { return };
        if rec.samples.len() < 2 {
            ed.note = Some("Nothing recorded — keep the pointer over the picture while it plays.".into());
            return;
        }
        let smooth = ed.smoothness;
        let samples = oa_track::smooth(&rec.samples, smooth * 0.25);
        let n = self.track_from_canvas(&samples, 0.5 + smooth * 3.0, "Record track");
        self.set_track_note(&format!("Recorded — {n} points."));
    }

    /// The viewer while the track editor is open: the track, the start point, and the
    /// pointer doing what the mode says. Everything else in the viewer waits.
    pub(crate) fn track_overlay(&mut self, ui: &mut egui::Ui, response: &egui::Response, to_canvas: &dyn Fn(egui::Pos2) -> [f64; 2], to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, clip_rect: egui::Rect) {
        let Some(ed) = self.track_editor.as_ref() else { return };
        let Some(it) = self.editor.item(ed.item).cloned() else {
            self.track_editor = None;
            return;
        };
        let keys = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Space)));
        let recording = ed.recording.is_some();
        if keys.0 {
            if recording {
                self.finish_recording();
            } else if ed.armed {
                self.track_editor.as_mut().expect("open").armed = false;
            } else if ed.picking_anchor {
                self.track_editor.as_mut().expect("open").picking_anchor = false;
            } else {
                self.track_editor = None;
                return;
            }
        }
        let pointer = response.hover_pos().map(to_canvas);
        let t = self.playhead;
        let mode = self.track_editor.as_ref().map(|e| e.mode);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if recording {
            // Click or Space stops it.
            if response.clicked() || (keys.1 && !ui.ctx().egui_wants_keyboard_input()) {
                self.finish_recording();
            } else {
                self.step_recording(pointer);
                ui.ctx().request_repaint();
            }
        } else if response.clicked()
            && self.track_editor.as_ref().is_some_and(|e| e.picking_anchor)
            && let Some(c) = pointer
        {
            // Where a locked stabilization holds the tracked point: here.
            let at = self.canvas_to_track(c);
            if let Some(ed) = self.track_editor.as_mut() {
                ed.picking_anchor = false;
                if let Usage::Stabilize { anchor, .. } = &mut ed.usage {
                    *anchor = Some(at);
                }
            }
            self.apply_usage(None);
        } else if response.clicked()
            && let Some(c) = pointer
        {
            match mode {
                Some(Mode::Follow) => {
                    if let Some(ed) = self.track_editor.as_mut() {
                        ed.start = Some((t, c));
                    }
                }
                // A point here, now; then on a few frames for the next.
                Some(Mode::Hand) => {
                    let mut track = self.working_track();
                    track.set(t, self.canvas_to_track(self.unstabilized(t, c)));
                    self.commit_track(track, "Track point", None);
                    let step = self.track_editor.as_ref().map_or(5, |e| e.step) as i64;
                    let rate = self.editor.sequence().rate;
                    let next = rate.frame_start(rate.frame_at(t) + step).min(it.range.end() - Time(1));
                    self.set_playhead(next);
                }
                // Armed: this click starts it, from the playhead to the clip's end.
                Some(Mode::Record) if self.track_editor.as_ref().is_some_and(|e| e.armed) => {
                    let from = t.max(it.range.start).min(it.range.end() - Time(1));
                    if let Some(ed) = self.track_editor.as_mut() {
                        ed.armed = false;
                        ed.note = None;
                        ed.recording = Some(Recording { began: Instant::now(), from, to: it.range.end() - Time(1), samples: vec![Sample::new(from.as_seconds_f64(), c)] });
                    }
                    ui.ctx().request_repaint();
                }
                _ => {}
            }
        }

        let Some(ed) = self.track_editor.as_ref() else { return };
        let painter = ui.painter_at(clip_rect);
        let gold = crate::style::GOLD;
        // The track: its line and points, and where it is now.
        if let Some(track) = ed.track.and_then(|id| self.editor.doc.project().tracks.get(&id).cloned()) {
            let pts: Vec<egui::Pos2> = track.points.iter().map(|p| to_screen(self.track_to_canvas(p.1))).collect();
            painter.add(egui::Shape::line(pts.clone(), egui::Stroke::new(2.0, gold)));
            for (p, (at, _)) in pts.iter().zip(&track.points) {
                let now = *at == t;
                painter.circle(*p, if now { 5.0 } else { 3.0 }, if now { gold } else { egui::Color32::WHITE }, egui::Stroke::new(1.0, gold));
            }
            if let Some(p) = track.at(t) {
                let s = to_screen(self.track_to_canvas(p));
                painter.circle_stroke(s, 9.0, egui::Stroke::new(1.5, gold.gamma_multiply(0.7)));
            }
            // Stabilizing: where the tracked point ends up (still, or smoothed).
            if let Some(TrackUse::Stabilize(s)) = self.stabilized_source().and_then(|src| src.track_use().cloned()) {
                let held: Vec<egui::Pos2> = track
                    .points
                    .iter()
                    .filter_map(|(at, p)| {
                        let c = s.correction.at(*at)?;
                        Some(to_screen(self.track_to_canvas([p[0] + c[0], p[1] + c[1]])))
                    })
                    .collect();
                let green = egui::Color32::from_rgb(80, 220, 140);
                painter.add(egui::Shape::line(held, egui::Stroke::new(2.0, green)));
            }
        }
        // Where the property puts its point now (with its offset).
        if let Some(now) = self.canvas_of(ed, t.max(it.range.start).min(it.range.end() - Time(1))) {
            let at = to_screen(now);
            let accent = crate::style::ACCENT;
            painter.circle_stroke(at, 6.0, egui::Stroke::new(2.0, accent));
            painter.circle_filled(at, 2.0, accent);
        }
        if let Some(rec) = &ed.recording {
            let pts: Vec<egui::Pos2> = rec.samples.iter().map(|s| to_screen(s.p)).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, crate::style::ERROR)));
        }
        // The tracker at work: the path so far, and the newest point.
        if let Some(run) = &ed.run && !run.live.is_empty() {
            let green = egui::Color32::from_rgb(80, 220, 140);
            let pts: Vec<egui::Pos2> = run.live.iter().map(|p| to_screen(p.1)).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, green)));
            let newest = run.live.iter().min_by_key(|p| (Self::run_time(run, p.0) - t).0.abs()).map(|p| to_screen(p.1));
            if let Some(p) = newest {
                painter.circle(p, 5.0, green, egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
        }
        // Where a locked stabilization holds the point (and, while picking it, the pointer).
        let lock_at = match ed.usage {
            Usage::Stabilize { lock: true, anchor: Some(a), .. } => Some(self.track_to_canvas(a)),
            _ => None,
        };
        let pointer_at = ed.picking_anchor.then(|| response.hover_pos().map(to_canvas)).flatten();
        for (spot, bright) in [(lock_at, false), (pointer_at, true)] {
            let Some(c) = spot else { continue };
            let p = to_screen(c);
            let green = egui::Color32::from_rgb(80, 220, 140);
            let color = if bright { egui::Color32::WHITE } else { green };
            painter.rect_stroke(egui::Rect::from_center_size(p, egui::vec2(14.0, 14.0)), 2.0, egui::Stroke::new(1.5, color), egui::StrokeKind::Middle);
            painter.circle_filled(p, 2.0, color);
        }
        if let (Mode::Follow, Some((at, c))) = (ed.mode, ed.start) {
            let p = to_screen(c);
            let green = egui::Color32::from_rgb(80, 220, 140);
            let color = if at == t { green } else { green.gamma_multiply(0.5) };
            painter.circle_stroke(p, 10.0, egui::Stroke::new(2.0, color));
            for d in [egui::vec2(14.0, 0.0), egui::vec2(0.0, 14.0)] {
                painter.line_segment([p - d, p - d * 0.45], egui::Stroke::new(2.0, color));
                painter.line_segment([p + d * 0.45, p + d], egui::Stroke::new(2.0, color));
            }
        }
    }

    /// The editor's panel, floating over the viewer's top left corner.
    pub(crate) fn track_panel(&mut self, ctx: &egui::Context) {
        self.poll_tracks(ctx);
        let Some(ed) = self.track_editor.as_ref() else { return };
        let Some(name) = self.editor.item(ed.item).map(|i| i.name.clone()) else { return };
        let Some((viewer, _)) = self.viewer_canvas else { return };
        let label = ed.param.rsplit('.').next().unwrap_or(&ed.param).replace('_', " ");
        let title = match ed.purpose {
            Purpose::Track => format!("Track — {name} · {label}"),
            Purpose::Stabilize => format!("Stabilize — {name}"),
        };
        let mut close = false;
        egui::Area::new(egui::Id::new("track-editor-panel")).order(egui::Order::Foreground).fixed_pos(viewer.left_top() + egui::vec2(10.0, 10.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(330.0);
                ui.horizontal(|ui| {
                    ui.strong(title);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").on_hover_text("Close the track editor (Esc)").clicked() {
                            close = true;
                        }
                    });
                });
                let busy = self.track_editor.as_ref().is_some_and(|e| e.recording.is_some() || e.run.is_some());
                ui.add_enabled_ui(!busy, |ui| {
                    self.track_picker(ui);
                    self.usage_ui(ui);
                });
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        let ed = self.track_editor.as_mut().expect("open");
                        for (m, name, tip) in [
                            (Mode::Follow, "Auto", "Click a point; the AI tracker follows it through the footage for you"),
                            (Mode::Hand, "By hand", "Click where it is; the playhead steps on; click again"),
                            (Mode::Record, "Record", "Follow it with the mouse while the timeline plays"),
                        ] {
                            if ui.selectable_label(ed.mode == m, name).on_hover_text(tip).clicked() {
                                ed.mode = m;
                                ed.armed = false;
                            }
                        }
                    });
                });
                ui.separator();
                let mode = self.track_editor.as_ref().map(|e| e.mode);
                match mode {
                    Some(Mode::Follow) => self.follow_panel(ui),
                    Some(Mode::Hand) => {
                        let ed = self.track_editor.as_mut().expect("open");
                        ui.label(egui::RichText::new("Click where it is. The playhead steps on; click again for the next point.").small());
                        ui.horizontal(|ui| {
                            ui.label("Step");
                            ui.add(egui::Slider::new(&mut ed.step, 1..=30).suffix(" frames"));
                        });
                    }
                    Some(Mode::Record) => self.record_panel(ui),
                    None => {}
                }
                if let Some(note) = self.track_editor.as_ref().and_then(|e| e.note.clone()) {
                    ui.label(egui::RichText::new(note).small().color(crate::style::ACCENT));
                }
            });
        });
        if close {
            self.track_editor = None;
            self.editor.doc.seal();
        }
    }

    /// The edited position's source when it's stabilized by the editor's track.
    fn stabilized_source(&self) -> Option<ParamSource> {
        let ed = self.track_editor.as_ref().filter(|e| e.space == Space::Position)?;
        let source = self.editor.param_source(ed.item, &ed.target, &ed.param)?;
        (matches!(source.track_use(), Some(TrackUse::Stabilize(_))) && source.find_track().map(|(t, _)| t.id) == ed.track).then_some(source)
    }

    /// How far the clip is moved by its stabilization at `t` (canvas px).
    fn stabilizing_shift(&self, t: Time) -> [f64; 2] {
        let (Some(source), Some(it)) = (self.stabilized_source(), self.track_editor.as_ref().and_then(|e| self.editor.item(e.item))) else { return [0.0; 2] };
        let c = source.track_contribution(&it.eval_context(t)).unwrap_or([0.0; 2]);
        let s = self.canvas_size();
        [c[0] * s[0], c[1] * s[1]]
    }

    /// Where canvas px `c` (as seen at `t`) was in the footage before the stabilization
    /// moved it: tracking a stabilized clip adds to its track in the track's own terms.
    fn unstabilized(&self, t: Time, c: [f64; 2]) -> [f64; 2] {
        let d = self.stabilizing_shift(t);
        [c[0] - d[0], c[1] - d[1]]
    }

    /// Puts the editor's Follow/Stabilize settings on the position (when it uses the
    /// editor's track). Switching between following and stabilizing keeps the clip where
    /// it is at the playhead; between two stabilizations the clip's resting place stays
    /// and only the correction changes — so a picked point to hold at is really reached.
    fn apply_usage(&mut self, coalesce: Option<&str>) {
        let Some(ed) = self.track_editor.as_ref() else { return };
        let (item, target, param, usage, id) = (ed.item, ed.target.clone(), ed.param.clone(), ed.usage, ed.track);
        let Some(source) = self.editor.param_source(item, &target, &param) else { return };
        let Some((track, clock)) = source.find_track().filter(|(t, _)| Some(t.id) == id).map(|(t, c)| (t.clone(), c)) else { return };
        let Some(it) = self.editor.item(item).cloned() else { return };
        let ctx = it.eval_context(self.playhead.max(it.range.start).min(it.range.end() - Time(1)));
        let mode = usage.to_track_use(&track, ctx.clip_time + clock);
        let label = if matches!(mode, TrackUse::Follow) { "Follow track" } else { "Stabilize" };
        let both_stabilizing = matches!(mode, TrackUse::Stabilize(_)) && matches!(source.track_use(), Some(TrackUse::Stabilize(_)));
        let changed = if both_stabilizing { source.replace_track_use(mode) } else { source.with_track_use(mode, &ctx) };
        let op = Op::SetParam { seq: self.editor.seq, item, target, param: ParamId::new(&param), source: Some(changed) };
        let result = match coalesce {
            Some(key) => self.editor.apply_drag(label, key, vec![op]),
            None => self.editor.apply(label, vec![op]),
        };
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }

    /// The scale that keeps a stabilized clip's edges out of the frame: the largest
    /// correction inside the clip, on both sides (for a clip filling the canvas at 100%).
    fn edge_hiding_scale(&self) -> Option<f64> {
        let source = self.stabilized_source()?;
        let ed = self.track_editor.as_ref()?;
        let it = self.editor.item(ed.item)?;
        let Some(TrackUse::Stabilize(s)) = source.track_use() else { return None };
        // The track's times the clip covers (its clock over its length).
        let (_, clock) = source.find_track()?;
        let (from, to) = (clock + it.eval_context(it.range.start).clip_time, clock + it.eval_context(it.range.end() - Time(1)).clip_time);
        let reach = s.correction.points.iter().filter(|(t, _)| (from..=to).contains(t)).map(|(_, c)| c[0].abs().max(c[1].abs())).fold(0.0, f64::max);
        Some((1.0 + 2.0 * reach).min(4.0))
    }

    /// How firmly a stabilization holds (Edit stabilization…); a note when the track's
    /// position is stabilized rather than following (Edit track…).
    fn usage_ui(&mut self, ui: &mut egui::Ui) {
        let Some(ed) = self.track_editor.as_ref().filter(|e| e.space == Space::Position) else { return };
        if ed.purpose == Purpose::Track {
            if matches!(ed.usage, Usage::Stabilize { .. }) {
                ui.label(egui::RichText::new("This clip is stabilized with the track: changes to the track steady it anew. Edit stabilization… sets how firmly.").small().weak());
            }
            return;
        }
        let mut usage = ed.usage;
        let was = usage;
        let picking = ed.picking_anchor;
        let mut pick = None;
        let mut finished = false;
        if let Usage::Stabilize { strength, lock, smoothing, anchor } = &mut usage {
            let track_changed = |r: &egui::Response, finished: &mut bool| *finished |= r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
            egui::Grid::new("track-stabilize").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Hold");
                ui.horizontal(|ui| {
                    if ui.selectable_label(!*lock, "Smooth").on_hover_text("Take out the shake, keep the camera's own moves (pans, walks)").clicked() && *lock {
                        *lock = false;
                        finished = true;
                    }
                    if ui.selectable_label(*lock, "Locked").on_hover_text("Rigid: the tracked point never moves, as on a tripod").clicked() && !*lock {
                        *lock = true;
                        finished = true;
                    }
                });
                ui.end_row();
                if *lock {
                    // Where it's held: no reference point needed — where it is now, or
                    // anywhere on the frame.
                    ui.label("Hold it");
                    ui.horizontal_wrapped(|ui| {
                        if ui.selectable_label(anchor.is_none() && !picking, "where it is now").on_hover_text("Where the tracked point is at the playhead").clicked() {
                            *anchor = None;
                            finished = true;
                            pick = Some(false);
                        }
                        if ui.selectable_label(*anchor == Some([0.0, 0.0]) && !picking, "at the center").on_hover_text("The middle of the frame").clicked() {
                            *anchor = Some([0.0, 0.0]);
                            finished = true;
                            pick = Some(false);
                        }
                        let custom = anchor.is_some_and(|a| a != [0.0, 0.0]);
                        if ui.selectable_label(picking || custom, if picking { "click in the picture…" } else { "at a point I pick" }).on_hover_text("Click anywhere on the frame: the tracked point is always kept there").clicked() {
                            pick = Some(!picking);
                        }
                    });
                    ui.end_row();
                }
                ui.label("Smoothing");
                let r = ui
                    .add_enabled(!*lock, egui::Slider::new(smoothing, 0.05..=5.0).logarithmic(true).suffix(" s").max_decimals(2))
                    .on_hover_text("Movement quicker than this is taken out. Longer is steadier; shorter keeps more of the camera's motion.");
                track_changed(&r, &mut finished);
                ui.end_row();
                ui.label("Strength");
                let mut pct = *strength * 100.0;
                let r = ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %").max_decimals(0)).on_hover_text("How much of the movement is taken out: less keeps some of it, for a natural feel");
                *strength = pct / 100.0;
                track_changed(&r, &mut finished);
                ui.end_row();
            });
        }
        if let Some(p) = pick
            && let Some(ed) = self.track_editor.as_mut()
        {
            ed.picking_anchor = p;
        }
        if usage != was || finished {
            let changed = usage != was;
            if let Some(ed) = self.track_editor.as_mut() {
                ed.usage = usage;
            }
            if changed {
                self.apply_usage(Some("track-usage"));
            }
            if finished {
                self.editor.doc.seal();
            }
        }
        // Moving the clip shows its edges: zoom in just enough to keep them out.
        if let Some(scale) = self.edge_hiding_scale().filter(|s| *s > 1.0005)
            && let Some(ed) = self.track_editor.as_ref()
        {
            let item = ed.item;
            let now = self.editor.param_value(item, &ParamTarget::Item, oa_doc::schema::SCALE, self.playhead).and_then(|v| v.as_vec2()).unwrap_or([1.0, 1.0]);
            let needed = [now[0].max(scale), now[1].max(scale)];
            let covered = needed == now;
            let label = if covered { "Edges hidden".to_string() } else { format!("Zoom in to hide the edges ({:.0}%)", scale * 100.0) };
            if ui.add_enabled(!covered, egui::Button::new(label)).on_hover_text("Scales the clip up just enough that the stabilization never shows past its edges").clicked() {
                self.editor.set_value_at(item, ParamTarget::Item, oa_doc::schema::SCALE, Value::Vec2(needed), self.playhead, "stabilize-zoom");
                self.editor.doc.seal();
            }
        }
    }

    /// Which track: a new one, or one made before; its name; letting go of it.
    fn track_picker(&mut self, ui: &mut egui::Ui) {
        let tracks: Vec<(u64, String, usize)> = self.editor.doc.project().tracks.values().map(|t| (t.id, t.name.clone(), t.points.len())).collect();
        let Some(ed) = self.track_editor.as_ref() else { return };
        let current = ed.track;
        let follows = self.editor.param_source(ed.item, &ed.target, &ed.param).and_then(|s| s.find_track().map(|(t, _)| t.id));
        let shown = current.and_then(|id| tracks.iter().find(|t| t.0 == id)).map_or("New track".to_string(), |t| t.1.clone());
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.label("Track");
            egui::ComboBox::from_id_salt("track-pick").selected_text(shown).width(200.0).show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "New track").on_hover_text("Start a new track with the tools below").clicked() {
                    pick = Some(None);
                }
                for (id, name, n) in &tracks {
                    if ui.selectable_label(current == Some(*id), format!("{name}  ({n} points)")).on_hover_text("Follow this track").clicked() {
                        pick = Some(Some(*id));
                    }
                }
            });
        });
        match pick {
            Some(None) => {
                if let Some(ed) = self.track_editor.as_mut() {
                    ed.track = None;
                }
            }
            Some(Some(id)) => {
                if let Some(t) = self.editor.doc.project().tracks.get(&id).cloned() {
                    self.commit_track((*t).clone(), "Follow track", None);
                }
            }
            None => {}
        }
        let Some(id) = self.track_editor.as_ref().and_then(|e| e.track) else { return };
        let Some(t) = self.editor.doc.project().tracks.get(&id).cloned() else { return };
        ui.horizontal(|ui| {
            let mut name = t.name.clone();
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(170.0)).on_hover_text("The track's name");
            if r.changed() {
                let mut renamed = (*t).clone();
                renamed.name = name;
                if let Err(e) = self.editor.apply_drag("Rename track", "track-rename", vec![Op::SetPointTrack { id, track: Some(Arc::new(renamed)) }]) {
                    self.error = Some(e.to_string());
                }
            }
            if r.lost_focus() {
                self.editor.doc.seal();
            }
            if follows == Some(id) && ui.button("Stop following").on_hover_text("The position stops following the track and stays where it is now").clicked() {
                self.let_go_of_track();
            }
        });
        ui.horizontal(|ui| {
            let others = self.editor.doc.project().tracks.len();
            if ui
                .button("🗑 Delete track")
                .on_hover_text(format!("Delete “{}” from the project; anything following it stays where it is now ({others} track{} in the project)", t.name, if others == 1 { "" } else { "s" }))
                .clicked()
            {
                self.delete_point_track(id);
            }
        });
        if follows == Some(id) {
            ui.label(egui::RichText::new("The position is now an offset from this track.").small().weak());
        }
    }

    fn record_panel(&mut self, ui: &mut egui::Ui) {
        let ed = self.track_editor.as_mut().expect("open");
        if ed.recording.is_some() {
            ui.label(egui::RichText::new("Recording — follow it with the pointer. Click or Space to stop.").small().color(crate::style::ERROR));
            if ui.button("■ Stop").clicked() {
                self.finish_recording();
            }
            return;
        }
        egui::Grid::new("track-record").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Speed");
            ui.add(egui::Slider::new(&mut ed.speed, 0.1..=1.0).suffix("×").max_decimals(2)).on_hover_text("How fast the timeline plays while you follow: slower is easier");
            ui.end_row();
            ui.label("Smoothness");
            ui.add(egui::Slider::new(&mut ed.smoothness, 0.0..=1.0).max_decimals(2)).on_hover_text("How much of the mouse's shake is taken out (and how few points are kept)");
            ui.end_row();
        });
        if ed.armed {
            ui.label(egui::RichText::new("Click in the picture to start. It plays from the playhead to the clip's end, without sound; click or Space stops it.").small().color(crate::style::ACCENT));
            if ui.button("Cancel").clicked() {
                ed.armed = false;
            }
        } else if ui.button("● Record").on_hover_text("Then click in the picture to start").clicked() {
            self.set_playing(false);
            let ed = self.track_editor.as_mut().expect("open");
            ed.armed = true;
            ed.note = None;
        }
    }

    fn follow_panel(&mut self, ui: &mut egui::Ui) {
        let tracker = tracker();
        if !tracker.is_installed() {
            self.tracker_setup_ui(ui);
            return;
        }
        let rate = self.editor.sequence().rate.as_f64();
        let ed = self.track_editor.as_mut().expect("open");
        if let Some(run) = &ed.run {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(&run.step).small());
            });
            let p = run.progress.unwrap_or(0.0);
            let text = if run.step.starts_with("Reading") && run.frames_done > 0 {
                format!("{:.0}% — frame {} of {}", p * 100.0, run.frames_done, run.footage.frames)
            } else {
                format!("{:.0}%", p * 100.0)
            };
            ui.add(egui::ProgressBar::new(p).desired_width(280.0).text(text).animate(run.progress.is_none()));
            if let Some(d) = &run.device {
                ui.label(egui::RichText::new(format!("Running on {d}")).small().weak());
            }
            if ui.button("Cancel").clicked() {
                run.job.cancel();
            }
            return;
        }
        // How long, and how closely.
        egui::Grid::new("track-follow").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Before");
            ui.add(egui::DragValue::new(&mut ed.before).range(0.0..=600.0).speed(0.05).suffix(" s")).on_hover_text("Follow it back this long from the start point (0: only forward)");
            ui.end_row();
            ui.label("After");
            ui.add(egui::DragValue::new(&mut ed.after).range(0.0..=600.0).speed(0.05).suffix(" s")).on_hover_text("Follow it on this long from the start point (stops at the clip's end)");
            ui.end_row();
            ui.label("Detail");
            ui.horizontal(|ui| {
                for (k, name, tip) in [(1, "Every frame", "Most exact, slowest"), (2, "Every 2nd", "About twice as fast; in-between frames are filled in"), (4, "Every 4th", "Fastest; for slow, smooth movement")] {
                    ui.selectable_value(&mut ed.detail, k, name).on_hover_text(tip);
                }
            });
            ui.end_row();
        });
        let frames = ((ed.before + ed.after) * rate / ed.detail.max(1) as f64).ceil() as usize + 1;
        if frames > oa_track::engine::MAX_FRAMES {
            ui.label(egui::RichText::new(format!("That's {frames} frames; up to {} are looked at, spread over it. Less time or less detail keeps every one.", oa_track::engine::MAX_FRAMES)).small().color(crate::style::WARNING));
        }
        let ed = self.track_editor.as_ref().expect("open");
        match ed.start {
            None if matches!(ed.usage, Usage::Stabilize { .. }) => {
                ui.label(egui::RichText::new("Click a point on this clip that should hold still — something fixed in the scene, with detail (a corner, a sign, a window).").small());
            }
            None => {
                ui.label(egui::RichText::new("Click the thing to follow in the picture.").small());
            }
            Some((at, _)) => {
                ui.label(egui::RichText::new(format!("Start point at {:.2} s. Click again to move it.", at.as_seconds_f64())).small());
                ui.horizontal(|ui| {
                    if ui.button("Follow it").on_hover_text("Follow this point through the footage, before and after, into the track").clicked() {
                        self.start_follow();
                    }
                    if ui.small_button("Clear").clicked()
                        && let Some(ed) = self.track_editor.as_mut()
                    {
                        ed.start = None;
                    }
                });
            }
        }
    }

    /// Setting the tracker up (or switching it between the CPU and the GPU), with its
    /// progress while that runs. Shared by the Follow panel and Settings.
    fn tracker_setup_ui(&mut self, ui: &mut egui::Ui) {
        let tracker = tracker();
        let installed = tracker.is_installed();
        if let Some(job) = &self.tracker_setup.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(&self.tracker_setup.step).small());
            });
            if let Some(p) = self.tracker_setup.progress {
                ui.add(egui::ProgressBar::new(p).desired_width(280.0));
            }
            if ui.button("Cancel").clicked() {
                job.cancel();
            }
            return;
        }
        if let Some(e) = &self.tracker_setup.error {
            ui.label(egui::RichText::new(e).small().color(crate::style::ERROR));
        }
        let gpu = nvidia();
        if !installed {
            ui.label(
                egui::RichText::new(format!(
                    "Following uses CoTracker, Meta's AI point tracker, which runs on your computer. Setting it up downloads Python, PyTorch and the model into {}. CoTracker is for non-commercial use only (CC BY-NC 4.0).",
                    tracker.root().display()
                ))
                .small(),
            );
        }
        let mut start = None;
        ui.horizontal_wrapped(|ui| {
            let cpu = Compute::Cpu;
            if !installed && ui.button(format!("Set up (CPU, ~{} MB)", cpu.download_mb())).on_hover_text("Works on any computer, using every core").clicked() {
                start = Some(tracker.install_steps(cpu));
            }
            if let Some(name) = &gpu {
                let nv = Compute::Nvidia;
                let label = if installed { format!("Use the GPU ({name}, ~{} MB)", nv.download_mb()) } else { format!("Set up for {name} (~{} MB)", nv.download_mb()) };
                if (!installed || tracker.compute() == Compute::Cpu) && ui.button(label).on_hover_text("Runs on the graphics card: many times faster than the CPU").clicked() {
                    start = Some(if installed { tracker.switch_steps(nv) } else { tracker.install_steps(nv) });
                }
            }
            if installed && tracker.compute() == Compute::Nvidia && ui.button("Use the CPU instead").on_hover_text("Swap in the smaller CPU build of PyTorch").clicked() {
                start = Some(tracker.switch_steps(Compute::Cpu));
            }
        });
        if let Some(steps) = start {
            self.tracker_setup.error = None;
            self.tracker_setup.job = Some(oa_track::engine::run(tracker.engine(), steps));
        }
    }

    /// The AI tracker in Settings: where it is, what it runs on, switching, removing.
    pub(crate) fn tracker_settings(&mut self, ui: &mut egui::Ui) {
        let tracker = tracker();
        if tracker.is_installed() {
            let on = match tracker.compute() {
                Compute::Cpu => "the CPU".to_string(),
                Compute::Nvidia => nvidia().map_or("an NVIDIA GPU".into(), |n| n.to_string()),
            };
            ui.label(egui::RichText::new(format!("Set up in {} — runs on {on}.", tracker.root().display())).small());
        } else {
            ui.label(egui::RichText::new("Not set up. It's set up from a position's track editor (Follow), or here.").small().weak());
        }
        self.tracker_setup_ui(ui);
        if tracker.is_installed() && self.tracker_setup.job.is_none() {
            let confirm = egui::Id::new("remove-tracker-confirm");
            let asked = ui.data(|d| d.get_temp::<bool>(confirm)).unwrap_or(false);
            ui.horizontal(|ui| {
                if !asked {
                    if ui.button("Remove the AI tracker").on_hover_text(format!("Delete {} and everything in it (tracks in projects stay)", tracker.root().display())).clicked() {
                        ui.data_mut(|d| d.insert_temp(confirm, true));
                    }
                } else {
                    ui.label(egui::RichText::new("Delete it?").color(crate::style::ERROR));
                    if ui.button("Remove").clicked() {
                        if let Err(e) = tracker.remove() {
                            self.tracker_setup.error = Some(e.to_string());
                        }
                        ui.data_mut(|d| d.remove::<bool>(confirm));
                    }
                    if ui.button("Keep it").clicked() {
                        ui.data_mut(|d| d.remove::<bool>(confirm));
                    }
                }
            });
        }
    }

    /// Deletes a track from the project; whatever follows it lets go, staying where it
    /// is at the playhead. One undo step.
    fn delete_point_track(&mut self, id: u64) {
        let mut ops = vec![Op::SetPointTrack { id, track: None }];
        let project = self.editor.doc.snapshot();
        for (seq, s) in &project.sequences {
            for it in std::iter::once(&s.background).chain(s.tracks.iter().flat_map(|t| t.items.iter())) {
                let sets = std::iter::once((ParamTarget::Item, &it.params)).chain(it.effects.iter().map(|e| (ParamTarget::Effect(e.id), &e.params)));
                for (target, set) in sets {
                    for (param, source) in &set.0 {
                        if !source.find_track().is_some_and(|(t, _)| t.id == id) {
                            continue;
                        }
                        let t = self.playhead.max(it.range.start).min(it.range.end() - Time(1));
                        let shift = source.track_contribution(&it.eval_context(t)).unwrap_or([0.0; 2]);
                        ops.push(Op::SetParam { seq: *seq, item: it.id, target: target.clone(), param: param.clone(), source: Some(source.clone().without_track(shift)) });
                    }
                }
            }
        }
        if let Err(e) = self.editor.apply("Delete track", ops) {
            self.error = Some(e.to_string());
        }
        if let Some(ed) = self.track_editor.as_mut()
            && ed.track == Some(id)
        {
            ed.track = None;
        }
    }
}

/// The NVIDIA card's name, asked once in the background (`None` until it answers, and
/// when there isn't one).
fn nvidia() -> Option<String> {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    static GPU: OnceLock<Option<String>> = OnceLock::new();
    static ASKED: AtomicBool = AtomicBool::new(false);
    if !ASKED.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            let _ = GPU.set(oa_track::engine::nvidia_gpu());
        });
    }
    GPU.get().cloned().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keying_a_track_keeps_motion_on_top() {
        let curve = Curve::new(KeyframeAnchor::ClipStart, vec![Keyframe { t: Time::ZERO, value: Value::Vec2([0.1, 0.2]), interp: Interp::Linear }]);
        let shaky = ParamSource::Static(Value::Vec2([0.0, 0.0])).wiggle(0.01, 2.0, 7);
        let out = with_curve(shaky, curve.clone());
        assert!(matches!(&out, ParamSource::Modulated { base, .. } if **base == ParamSource::Animated(curve.clone())), "{out:?}");
        assert_eq!(with_curve(ParamSource::Static(Value::Vec2([0.0, 0.0])), curve.clone()), ParamSource::Animated(curve));
    }
}
