use crate::model::Project;
use crate::ops::{EditError, Op};
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
    // Backgrounds follow their timeline's length (derived: undo lands on the same length).
    for s in next.sequences.values_mut() {
        if !s.background_in_sync() {
            Arc::make_mut(s).sync_background();
        }
    }
    Ok((Arc::new(next), undo))
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
