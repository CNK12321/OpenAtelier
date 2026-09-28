use crate::model::Project;
use crate::ops::{EditError, Op};
use oa_time::Time;
use std::sync::Arc;

struct Transaction {
    label: String,
    /// Applied in order to undo.
    undo: Vec<Op>,
    /// Consecutive edits with the same key (e.g. one slider drag) merge into one step.
    coalesce: Option<String>,
}

/// Undo steps kept. Each holds only the inverse ops of one edit (sharing unchanged data
/// with the snapshot), so this is generous; the oldest steps go first.
pub const MAX_UNDO: usize = 1000;

/// The live document: current snapshot plus undo/redo.
///
/// Edits apply to a clone of the project (cheap: `Arc`s are shared) and replace the
/// snapshot only if every op succeeds, so a failed edit never leaves partial changes
/// and render threads holding the old snapshot are never disturbed.
pub struct Document {
    project: Arc<Project>,
    /// Bumped every time the snapshot changes (an edit, undo or redo): a cheap test for
    /// "is what I worked out from the project still current?".
    revision: u64,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
}

impl Document {
    pub fn new(project: Project) -> Self {
        Document { project: Arc::new(project), revision: 0, undo: Vec::new(), redo: Vec::new() }
    }

    /// The current snapshot. Cheap to clone and send to other threads.
    pub fn snapshot(&self) -> Arc<Project> {
        self.project.clone()
    }

    /// Changes whenever the project does (see [`Document`]'s `revision`).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Renames the project. A label on the file rather than an edit to the film, so it
    /// isn't an undo step — but it does mark the document changed, so it gets saved.
    pub fn set_project_name(&mut self, name: &str) {
        if self.project.name != name {
            Arc::make_mut(&mut self.project).name = name.to_string();
            self.revision += 1;
        }
    }

    /// Allocates a fresh id. Ids are never reused, even across undo.
    pub fn alloc_id(&mut self) -> u64 {
        Arc::make_mut(&mut self.project).alloc_id()
    }

    pub fn edit(&mut self, label: &str, ops: Vec<Op>) -> Result<(), EditError> {
        self.edit_coalesced(label, None, ops)
    }

    /// Like [`edit`](Self::edit), but merges into the previous undo step if it used the
    /// same `key` and hasn't been sealed with [`seal`](Self::seal).
    pub fn edit_coalesced(&mut self, label: &str, key: Option<&str>, ops: Vec<Op>) -> Result<(), EditError> {
        let (project, mut undo) = run(&self.project, ops)?;
        self.project = project;
        self.revision += 1;
        self.redo.clear();
        if let (Some(k), Some(top)) = (key, self.undo.last_mut())
            && top.coalesce.as_deref() == Some(k)
        {
            undo.append(&mut top.undo);
            top.undo = undo;
            return Ok(());
        }
        self.undo.push(Transaction { label: label.into(), undo, coalesce: key.map(Into::into) });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        Ok(())
    }

    /// Ends the current coalescing run (call on mouse-up).
    pub fn seal(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.coalesce = None;
        }
    }

    /// Number of steps that can be undone.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    pub fn undo(&mut self) -> Result<bool, EditError> {
        self.step(false)
    }

    pub fn redo(&mut self) -> Result<bool, EditError> {
        self.step(true)
    }

    fn step(&mut self, forward: bool) -> Result<bool, EditError> {
        let (from, to) = if forward { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(t) = from.pop() else { return Ok(false) };
        match run(&self.project, t.undo.clone()) {
            Ok((project, inverse)) => {
                self.project = project;
                self.revision += 1;
                to.push(Transaction { label: t.label, undo: inverse, coalesce: None });
                Ok(true)
            }
            Err(e) => {
                from.push(t);
                Err(e)
            }
        }
    }
}

/// Applies `ops` to a copy; returns the new snapshot and the combined inverse. A project
/// that has a timeline never loses its last one — whatever the ops, an edit, undo or redo
/// that would do it is refused whole.
fn run(project: &Arc<Project>, ops: Vec<Op>) -> Result<(Arc<Project>, Vec<Op>), EditError> {
    let mut next = (**project).clone();
    let mut undo = Vec::new();
    for op in ops {
        let mut inv = op.apply(&mut next)?;
        inv.append(&mut undo);
        undo = inv;
    }
    if next.sequences.is_empty() && !project.sequences.is_empty() {
        return Err(EditError::LastSequence);
    }
    // Compound clips follow what's inside them: a timeline that got shorter or longer
    // takes the clips playing it along (real edits, so undo puts them back), and a
    // compound clip inside another passes its change on up.
    let mut before: std::collections::BTreeMap<crate::SeqId, Time> = project.sequences.iter().map(|(id, s)| (*id, s.duration())).collect();
    for _ in 0..16 {
        let changed: Vec<(crate::SeqId, Time, Time)> = next
            .sequences
            .iter()
            .filter_map(|(id, s)| before.get(id).map(|old| (*id, *old, s.duration())).filter(|(_, old, new)| old != new))
            .collect();
        if changed.is_empty() {
            break;
        }
        before = next.sequences.iter().map(|(id, s)| (*id, s.duration())).collect();
        let fits = fit_compound_clips(&next, &changed);
        if fits.is_empty() {
            break;
        }
        for op in fits {
            let mut inv = op.apply(&mut next)?;
            inv.append(&mut undo);
            undo = inv;
        }
    }
    // Backgrounds follow their timeline's length (derived: undo lands on the same length).
    for s in next.sequences.values_mut() {
        if !s.background_in_sync() {
            Arc::make_mut(s).sync_background();
        }
    }
    Ok((Arc::new(next), undo))
}

/// Timing edits for the compound clips playing sequences whose length changed (`changed`:
/// the sequence, its old length, its new): a clip that now runs past the end is cut to
/// it; a clip that showed everything up to the old end grows to the new one, as far as
/// the next clip on its track allows. Reversed and frozen clips are left alone.
fn fit_compound_clips(p: &Project, changed: &[(crate::SeqId, Time, Time)]) -> Vec<Op> {
    use crate::ItemKind;
    let mut ops = Vec::new();
    for (seq_id, s) in &p.sequences {
        let rate = s.rate;
        for track in &s.tracks {
            for (i, it) in track.items.iter().enumerate() {
                let ItemKind::Nested { sequence } = it.kind else { continue };
                let Some(&(_, old, new)) = changed.iter().find(|(id, ..)| *id == sequence) else { continue };
                let speed = it.time_map.speed;
                if speed.num() <= 0 || speed.den() <= 0 {
                    continue;
                }
                // How much of the inside the clip shows now, and where that ends.
                let inner_time = |d: Time| Time::from_rational_floor(d.as_rational() * speed);
                let outer_time = |d: Time| Time::from_rational_floor(d.as_rational() * speed.recip());
                let shown_end = it.time_map.source_in + inner_time(it.range.duration);
                // Past the new end (it shrank), or showing up to the old end (it grew).
                if !(shown_end > new || (new > old && shown_end >= old)) {
                    continue;
                }
                let target_end = new;
                if target_end <= it.time_map.source_in {
                    continue; // it would show nothing: leave it for the user to see
                }
                let mut end = it.range.start + outer_time(target_end - it.time_map.source_in);
                // Growing stops at the next clip.
                if let Some(next) = track.items.get(i + 1) {
                    end = end.min(next.range.start);
                }
                // On a frame, at least one frame long.
                let frame = rate.frame_start(1);
                let end = rate.frame_start(rate.frame_at(end)).max(it.range.start + frame);
                if end == it.range.end() {
                    continue;
                }
                ops.push(Op::SetItemTiming { seq: *seq_id, item: it.id, range: oa_time::TimeRange::new(it.range.start, end - it.range.start), time_map: it.time_map });
            }
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanvasSize, FormatVariant, SeqId, Sequence, VariantId};

    #[test]
    fn the_last_timeline_cannot_be_undone_or_removed() {
        let mut doc = Document::new(Project::new("t"));
        let v = FormatVariant { id: VariantId(2), name: "w".into(), size: CanvasSize::new(16, 9), overrides: Default::default() };
        let s = Sequence::new(SeqId(1), "Main", oa_time::FrameRate::FPS_30, v);
        // Building a project up from nothing is fine…
        doc.edit("add", vec![Op::AddSequence(Arc::new(s))]).unwrap();
        // …but undoing that, or removing it, would leave nothing to edit.
        assert_eq!(doc.undo(), Err(EditError::LastSequence));
        assert_eq!(doc.project().sequences.len(), 1);
        assert_eq!(doc.undo_depth(), 1, "the step stays, untouched");
        assert_eq!(doc.edit("remove", vec![Op::RemoveSequence(SeqId(1))]), Err(EditError::LastSequence));
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::{CanvasSize, FormatVariant, Item, ItemId, ItemKind, ParamTarget, SeqId, Sequence, Track, TrackId, TrackKind, VariantId};
    use oa_params::{Curve, KeyframeAnchor, ParamId, ParamSource, Value};
    use oa_time::{Time, TimeRange};

    /// Whatever a text box or a buggy control sends, the document keeps only values it
    /// can render: NaN, infinity, keyless curves and empty canvases are refused whole.
    #[test]
    fn invalid_values_are_refused() {
        let mut p = Project::new("t");
        let v = FormatVariant { id: VariantId(2), name: "w".into(), size: CanvasSize::new(16, 9), overrides: Default::default() };
        let mut s = Sequence::new(SeqId(1), "Main", oa_time::FrameRate::FPS_30, v);
        let mut track = Track::new(TrackId(3), "V1", TrackKind::Video);
        track.items.push(Item::new(ItemId(4), "c", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(1))));
        s.tracks.push(Arc::new(track));
        p.sequences.insert(SeqId(1), Arc::new(s));
        let mut doc = Document::new(p);
        let set = |src| Op::SetParam { seq: SeqId(1), item: ItemId(4), target: ParamTarget::Item, param: ParamId::new("opacity"), source: Some(src) };
        for bad in [
            ParamSource::Static(Value::Float(f64::NAN)),
            ParamSource::Static(Value::Vec2([1.0, f64::INFINITY])),
            ParamSource::Animated(Curve { anchor: KeyframeAnchor::ClipStart, keys: vec![] }),
        ] {
            assert_eq!(doc.edit("bad", vec![set(bad)]), Err(EditError::InvalidValue));
        }
        assert_eq!(doc.undo_depth(), 0, "nothing was recorded");
        assert!(doc.edit("fine", vec![set(ParamSource::Static(Value::Float(0.5)))]).is_ok());
        let zero = Op::SetVariantSize { seq: SeqId(1), variant: VariantId(2), size: CanvasSize::new(0, 9) };
        assert_eq!(doc.edit("zero", vec![zero]), Err(EditError::InvalidValue));
        // Typed seconds can't produce a time that overflows later.
        assert_eq!(Time::from_seconds_f64(f64::NAN), Time::ZERO);
        let huge = Time::from_seconds_f64(f64::INFINITY);
        assert!(huge.0.checked_add(huge.0).is_some());
    }
}

#[cfg(test)]
mod compound_fit_tests {
    use super::*;
    use crate::*;
    use oa_time::{FrameRate, TimeRange};

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    /// Main plays compound B (0–10 s, all of it) then a clip at 12 s; B plays compound C
    /// (all of it); C is a 10 s solid.
    fn project() -> Document {
        let mut doc = Document::new(Project::new("t"));
        let v = |id| FormatVariant { id: VariantId(id), name: "w".into(), size: CanvasSize::new(16, 9), overrides: Default::default() };
        let seq = |id, name, vid| Sequence::new(SeqId(id), name, FrameRate::FPS_30, v(vid));
        let track = |id| Track::new(TrackId(id), "V1", TrackKind::Video);
        let item = |id, kind, start, len| Item::new(ItemId(id), "x", kind, TimeRange::new(secs(start), secs(len)));
        let ops = vec![
            Op::AddSequence(Arc::new(seq(1, "Main", 2))),
            Op::AddSequence(Arc::new(seq(3, "B", 4))),
            Op::AddSequence(Arc::new(seq(5, "C", 6))),
            Op::InsertTrack { seq: SeqId(1), index: 0, track: Arc::new(track(10)) },
            Op::InsertTrack { seq: SeqId(3), index: 0, track: Arc::new(track(11)) },
            Op::InsertTrack { seq: SeqId(5), index: 0, track: Arc::new(track(12)) },
            Op::InsertItem { seq: SeqId(5), track: TrackId(12), item: item(20, ItemKind::Solid, 0, 10) },
            Op::InsertItem { seq: SeqId(3), track: TrackId(11), item: item(21, ItemKind::Nested { sequence: SeqId(5) }, 0, 10) },
            Op::InsertItem { seq: SeqId(1), track: TrackId(10), item: item(22, ItemKind::Nested { sequence: SeqId(3) }, 0, 10) },
            Op::InsertItem { seq: SeqId(1), track: TrackId(10), item: item(23, ItemKind::Solid, 12, 3) },
        ];
        doc.edit("setup", ops).unwrap();
        doc
    }

    fn length(doc: &Document, seq: u64, item: u64) -> Time {
        doc.project().sequence(SeqId(seq)).unwrap().item(ItemId(item)).unwrap().range.duration
    }

    #[test]
    fn compound_clips_follow_what_is_inside() {
        let mut doc = project();
        let set = |len| Op::SetItemTiming { seq: SeqId(5), item: ItemId(20), range: TimeRange::new(Time::ZERO, secs(len)), time_map: TimeMap::default() };
        // C shrinks to 6 s: B's clip of it, then Main's clip of B, follow.
        doc.edit("shrink", vec![set(6)]).unwrap();
        assert_eq!((length(&doc, 3, 21), length(&doc, 1, 22)), (secs(6), secs(6)));
        // One undo puts it all back.
        doc.undo().unwrap();
        assert_eq!((length(&doc, 5, 20), length(&doc, 3, 21), length(&doc, 1, 22)), (secs(10), secs(10), secs(10)));
        // C grows to 15 s: B's clip grows with it; Main's only to the clip at 12 s.
        doc.edit("grow", vec![set(15)]).unwrap();
        assert_eq!((length(&doc, 3, 21), length(&doc, 1, 22)), (secs(15), secs(12)));
        // A clip showing only part of the inside isn't stretched when it grows.
        let mut doc = project();
        let part = TimeRange::new(Time::ZERO, secs(4));
        doc.edit("trim", vec![Op::SetItemTiming { seq: SeqId(1), item: ItemId(22), range: part, time_map: TimeMap::default() }]).unwrap();
        doc.edit("grow", vec![set(15)]).unwrap();
        assert_eq!(length(&doc, 1, 22), secs(4));
    }
}
