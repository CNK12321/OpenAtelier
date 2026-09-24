//! Direct manipulation in the viewer: pick a layer, grab a handle, drag.
//!
//! Geometry comes from [`oa_plan::scene`], the same placement math the renderer uses,
//! so handles sit exactly on the pixels. A [`Gesture`] remembers where it started and
//! recomputes the edit from that start on every pointer move (never accumulating
//! deltas), so rounding can't drift and the result depends only on where the pointer is.
//!
//! Edits are keyframe-aware: an animated property gets a key at the playhead instead
//! of losing its animation, and procedural motion (wiggle) stays layered on top. In a
//! format variant, edits can go to that variant's override so other aspect ratios keep
//! their layout.

use oa_doc::{schema, EditError, FormatVariant, Item, ItemId, Op, ParamTarget, Project, SeqId, TrackId, VariantId};
use oa_graph::Affine2;
use oa_params::{EvalContext, ParamId, ParamSource, Value};
use oa_plan::scene::{self, Placement};
use oa_time::Time;

type R<T> = Result<T, EditError>;

/// A grabbable part of a selected layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The layer itself: move.
    Body,
    /// Corner `0..4` (top-left, top-right, bottom-right, bottom-left): scale both axes.
    Corner(usize),
    /// Edge midpoint `0..4` (top, right, bottom, left): scale one axis.
    Edge(usize),
    /// The knob above the top edge: rotate around the anchor.
    Rotate,
    /// The anchor point: move the pivot without moving the picture.
    Anchor,
}

/// Where the handles of a layer are, in canvas px.
#[derive(Clone, Debug)]
pub struct Handles {
    pub corners: [[f64; 2]; 4],
    pub edges: [[f64; 2]; 4],
    pub rotate: [f64; 2],
    pub pivot: [f64; 2],
}

fn mid(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = sub(a, b);
    d[0].hypot(d[1])
}

fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// The linear part of a transform applied to a vector.
fn linear(m: &Affine2, v: [f64; 2]) -> [f64; 2] {
    sub(m.apply(v), m.apply([0.0, 0.0]))
}

impl Handles {
    /// `rotate_offset` is how far (canvas px) the rotation knob sits outside the top edge;
    /// callers pass a screen distance divided by the view zoom.
    pub fn new(p: &Placement, rotate_offset: f64) -> Self {
        let corners = p.corners();
        let edges = [mid(corners[0], corners[1]), mid(corners[1], corners[2]), mid(corners[2], corners[3]), mid(corners[3], corners[0])];
        // "Up" for the layer: from the bottom edge's midpoint through the top one.
        let up = sub(edges[0], edges[2]);
        let len = up[0].hypot(up[1]).max(1e-9);
        let rotate = [edges[0][0] + up[0] / len * rotate_offset, edges[0][1] + up[1] / len * rotate_offset];
        Handles { corners, edges, rotate, pivot: p.pivot }
    }

    /// The handle under `pointer` within `tolerance` canvas px. Small handles win over
    /// the body so they stay reachable on tiny layers.
    pub fn pick(&self, p: &Placement, pointer: [f64; 2], tolerance: f64) -> Option<Handle> {
        let near = |q: [f64; 2]| dist(q, pointer) <= tolerance;
        if near(self.pivot) {
            return Some(Handle::Anchor);
        }
        if let Some(i) = (0..4).find(|&i| near(self.corners[i])) {
            return Some(Handle::Corner(i));
        }
        if near(self.rotate) {
            return Some(Handle::Rotate);
        }
        if let Some(i) = (0..4).find(|&i| near(self.edges[i])) {
            return Some(Handle::Edge(i));
        }
        p.contains(pointer).then_some(Handle::Body)
    }
}

/// Where transform edits are written.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The item's own params: every format variant follows (unless it overrides).
    AllFormats,
    /// Only this variant's override for the item.
    Variant(VariantId),
}

/// Keyboard modifiers that change a drag.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Corners: scale x and y independently (default keeps the aspect ratio).
    /// Move: lock to the dominant axis.
    pub free: bool,
    /// Rotation: 15° steps. Move/anchor: disable snapping.
    pub step: bool,
}

/// A snap line to draw while dragging, in canvas px.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Guide {
    Vertical(f64),
    Horizontal(f64),
}

/// The result of one pointer move.
#[derive(Clone, Debug, Default)]
pub struct Update {
    pub ops: Vec<Op>,
    pub guides: Vec<Guide>,
}

/// One drag of one handle, from pointer-down to pointer-up.
#[derive(Clone, Debug)]
pub struct Gesture {
    pub seq: SeqId,
    pub item: ItemId,
    pub track: TrackId,
    pub variant: VariantId,
    pub scope: Scope,
    pub handle: Handle,
    /// The playhead: keyed properties get their key here.
    pub t: Time,
    start_pointer: [f64; 2],
    start: Placement,
    canvas: [f64; 2],
}

fn locate(p: &Project, seq: SeqId, item: ItemId) -> R<(&Item, TrackId)> {
    let s = p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))?;
    let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
    Ok((&s.tracks[ti].items[ii], s.tracks[ti].id))
}

fn variant(p: &Project, seq: SeqId, id: VariantId) -> R<&FormatVariant> {
    p.sequence(seq).and_then(|s| s.variant(id)).ok_or(EditError::NotFound("variant", id.0))
}

/// The placement of `item` at `t` as seen in `variant`.
pub fn placement_of(p: &Project, seq: SeqId, variant_id: VariantId, item: ItemId, t: Time) -> R<Placement> {
    let (it, track) = locate(p, seq, item)?;
    let v = variant(p, seq, variant_id)?;
    scene::placement(p, track, it, v, t).map_err(|_| EditError::NotFound("placeable item", item.0))
}

impl Gesture {
    /// Starts a drag of `handle` on `item` with the pointer at `pointer` (canvas px of
    /// `variant`) and the playhead at `t`.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        p: &Project,
        seq: SeqId,
        variant_id: VariantId,
        item: ItemId,
        t: Time,
        handle: Handle,
        pointer: [f64; 2],
        scope: Scope,
    ) -> R<Gesture> {
        let start = placement_of(p, seq, variant_id, item, t)?;
        let size = variant(p, seq, variant_id)?.size;
        Ok(Gesture {
            seq,
            item,
            track: start.track,
            variant: variant_id,
            scope,
            handle,
            t,
            start_pointer: pointer,
            canvas: [size.width as f64, size.height as f64],
            start,
        })
    }

    /// The layer's placement when the drag started.
    pub fn start(&self) -> &Placement {
        &self.start
    }

    /// The ops that take the item from its start state to where the pointer now says.
    /// `snap` is the snapping distance in canvas px (0 disables snapping).
    pub fn update(&self, p: &Project, pointer: [f64; 2], mods: Modifiers, snap: f64) -> R<Update> {
        let v0 = &self.start.values;
        let mut out = Update::default();
        let mut values: Vec<(&'static str, Value)> = Vec::new();
        match self.handle {
            Handle::Body => {
                let mut d = sub(pointer, self.start_pointer);
                if mods.free {
                    if d[0].abs() >= d[1].abs() { d[1] = 0.0 } else { d[0] = 0.0 }
                }
                if snap > 0.0 && !mods.step {
                    let (adjust, guides) = self.snap_move(d, snap);
                    d = [d[0] + adjust[0], d[1] + adjust[1]];
                    out.guides = guides;
                }
                let pos = v0.vec2(schema::POSITION);
                values.push((schema::POSITION, Value::Vec2([pos[0] + d[0] / self.canvas[0], pos[1] + d[1] / self.canvas[1]])));
            }
            Handle::Corner(_) | Handle::Edge(_) => {
                values.push((schema::SCALE, Value::Vec2(self.scale_for(pointer, mods))));
            }
            Handle::Rotate => {
                let (a, b) = (sub(self.start_pointer, self.start.pivot), sub(pointer, self.start.pivot));
                let mut turn = (b[1].atan2(b[0]) - a[1].atan2(a[0])).to_degrees();
                turn = (turn + 180.0).rem_euclid(360.0) - 180.0;
                let mut angle = v0.float(schema::ROTATION) + turn;
                if mods.step {
                    angle = (angle / 15.0).round() * 15.0;
                }
                values.push((schema::ROTATION, Value::Float(angle)));
            }
            Handle::Anchor => {
                let s = &self.start;
                let inv = s.to_canvas.invert().ok_or(EditError::InvalidRange)?;
                let local = inv.apply(pointer);
                let mut a = [local[0] / s.native[0], local[1] / s.native[1]];
                if snap > 0.0 && !mods.step {
                    // Snap to the layer's corners, edge midpoints and center.
                    for (i, len) in [s.native[0], s.native[1]].into_iter().enumerate() {
                        let scale = linear(&s.to_canvas, if i == 0 { [1.0, 0.0] } else { [0.0, 1.0] });
                        let tol = snap / scale[0].hypot(scale[1]).max(1e-9) / len;
                        if let Some(t) = [0.0, 0.5, 1.0].into_iter().find(|t| (a[i] - t).abs() <= tol) {
                            a[i] = t;
                        }
                    }
                }
                // Keep the picture still: position absorbs the pivot move (L·w − w, where
                // w is the pivot's move in reframed space and L the user scale+rotation).
                let a0 = v0.vec2(schema::ANCHOR);
                let w = [
                    (a[0] - a0[0]) * s.native[0] * s.reframe.scale[0],
                    (a[1] - a0[1]) * s.native[1] * s.reframe.scale[1],
                ];
                let lw = linear(&s.to_canvas, [w[0] / s.reframe.scale[0], w[1] / s.reframe.scale[1]]);
                let pos = v0.vec2(schema::POSITION);
                let pos = [pos[0] + (lw[0] - w[0]) / self.canvas[0], pos[1] + (lw[1] - w[1]) / self.canvas[1]];
                values.push((schema::ANCHOR, Value::Vec2(a)));
                values.push((schema::POSITION, Value::Vec2(pos)));
            }
        }
        for (param, value) in values {
            out.ops.push(write_param(p, self.seq, self.item, self.variant, self.scope, self.t, param, value)?);
        }
        Ok(out)
    }

    /// New `transform.scale` for a corner or edge drag: scale about the anchor so the
    /// grabbed point follows the pointer.
    fn scale_for(&self, pointer: [f64; 2], mods: Modifiers) -> [f64; 2] {
        let s = &self.start;
        let scale0 = s.values.vec2(schema::SCALE);
        // Into the layer's rotated frame (rotation only, so ratios are scale ratios).
        let rot = Affine2::rotate_degrees(-s.values.float(schema::ROTATION));
        let a = rot.apply(sub(self.start_pointer, s.pivot));
        let b = rot.apply(sub(pointer, s.pivot));
        let ratio = |i: usize| if a[i].abs() > 1e-6 { b[i] / a[i] } else { 1.0 };
        let k = match self.handle {
            Handle::Edge(0) | Handle::Edge(2) => [1.0, ratio(1)],
            Handle::Edge(_) => [ratio(0), 1.0],
            _ if mods.free => [ratio(0), ratio(1)],
            _ => {
                // Uniform: project the pointer onto the line from the pivot to the grab point.
                let u = if dot(a, a) > 1e-9 { dot(a, b) / dot(a, a) } else { 1.0 };
                [u, u]
            }
        };
        let keep = |v: f64| if v.abs() < 1e-3 { 1e-3f64.copysign(v) } else { v };
        [keep(scale0[0] * k[0]), keep(scale0[1] * k[1])]
    }

    /// How far to nudge a move so the layer's edges or center land on the canvas's
    /// edges or center, plus the guides to show.
    fn snap_move(&self, d: [f64; 2], tolerance: f64) -> ([f64; 2], Vec<Guide>) {
        let corners = self.start.corners().map(|c| [c[0] + d[0], c[1] + d[1]]);
        let mut adjust = [0.0, 0.0];
        let mut guides = Vec::new();
        for axis in 0..2 {
            let lo = corners.iter().map(|c| c[axis]).fold(f64::INFINITY, f64::min);
            let hi = corners.iter().map(|c| c[axis]).fold(f64::NEG_INFINITY, f64::max);
            let len = self.canvas[axis];
            let mut best: Option<(f64, f64)> = None; // (adjustment, guide position)
            for edge in [lo, (lo + hi) / 2.0, hi] {
                for target in [0.0, len / 2.0, len] {
                    let delta = target - edge;
                    if delta.abs() <= tolerance && best.is_none_or(|(b, _)| delta.abs() < b.abs()) {
                        best = Some((delta, target));
                    }
                }
            }
            if let Some((delta, at)) = best {
                adjust[axis] = delta;
                guides.push(if axis == 0 { Guide::Vertical(at) } else { Guide::Horizontal(at) });
            }
        }
        (adjust, guides)
    }
}

/// Moves a layer by `delta` canvas px (arrow-key nudging), as one edit.
pub fn nudge(p: &Project, seq: SeqId, variant: VariantId, item: ItemId, t: Time, delta: [f64; 2], scope: Scope) -> R<Vec<Op>> {
    let g = Gesture::begin(p, seq, variant, item, t, Handle::Body, [0.0, 0.0], scope)?;
    Ok(g.update(p, delta, Modifiers { step: true, ..Default::default() }, 0.0)?.ops)
}

/// Puts a layer's transform back to its defaults (fit, centered, unrotated).
pub fn reset_transform(p: &Project, seq: SeqId, item: ItemId, scope: Scope) -> R<Vec<Op>> {
    let (it, _) = locate(p, seq, item)?;
    let params = [schema::POSITION, schema::SCALE, schema::SQUASH, schema::ROTATION, schema::ANCHOR];
    let target = match scope {
        Scope::AllFormats => ParamTarget::Item,
        Scope::Variant(v) => ParamTarget::VariantOverride(v),
    };
    let has = |param: &str| match scope {
        Scope::AllFormats => it.params.get(param).is_some(),
        Scope::Variant(v) => variant(p, seq, v).is_ok_and(|v| v.overrides.get(&item).is_some_and(|o| o.get(param).is_some())),
    };
    Ok(params
        .into_iter()
        .filter(|param| has(param))
        .map(|param| Op::SetParam { seq, item, target: target.clone(), param: ParamId::new(param), source: None })
        .collect())
}

/// Sets `param` to `value` at timeline time `t`, keyframe-aware, writing to wherever the
/// value seen in `viewed` comes from:
/// * if `viewed` already overrides the param, that override is edited;
/// * otherwise `scope` decides — the item's own value, or a new override for one
///   variant (seeded from the item's value so its animation carries over).
#[allow(clippy::too_many_arguments)]
pub fn write_param(
    p: &Project,
    seq: SeqId,
    item: ItemId,
    viewed: VariantId,
    scope: Scope,
    t: Time,
    param: &str,
    value: Value,
) -> R<Op> {
    let (it, _) = locate(p, seq, item)?;
    let s = p.sequence(seq).expect("located above");
    let override_of = |v: VariantId| s.variant(v).and_then(|v| v.overrides.get(&item)).and_then(|o| o.get(param));
    let (target, existing) = match (override_of(viewed), scope) {
        (Some(src), _) => (ParamTarget::VariantOverride(viewed), Some(src)),
        (None, Scope::Variant(v)) => (ParamTarget::VariantOverride(v), override_of(v).or_else(|| it.params.get(param))),
        (None, Scope::AllFormats) => (ParamTarget::Item, it.params.get(param)),
    };
    let ctx: EvalContext = it.eval_context(t);
    let source = match existing {
        Some(src) => {
            let mut src = src.clone();
            src.set_at(&ctx, value);
            src
        }
        None => ParamSource::Static(value),
    };
    Ok(Op::SetParam { seq, item, target, param: ParamId::new(param), source: Some(source) })
}
