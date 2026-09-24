//! Where layers land on the canvas.
//!
//! This is the exact placement math the planner renders with, exposed for interaction:
//! clicking a layer in the viewer, drawing its outline, dragging its handles. Keeping
//! one implementation means what you click is always what you see.

use oa_doc::{schema, FitMode, FormatVariant, Item, ItemId, ItemKind, Project, Reframe, SeqId, TrackId, TrackKind};
use oa_graph::Affine2;
use oa_params::{Evaluated, Value};
use oa_time::Time;

/// Why a layer has no placement.
#[derive(Clone, Debug, PartialEq)]
pub enum Unplaceable {
    /// Media whose probe info is missing, or that has no video.
    MediaWithoutInfo,
    /// A clip type the planner can't draw yet (text, plugins, too-deep nesting).
    Unsupported,
}

/// What a text clip says and how it's set, at one instant (em size in canvas px).
pub fn text_spec(values: &Evaluated) -> oa_text::TextSpec {
    let text = |id: &str| values.get(id).and_then(Value::as_text).unwrap_or_default().to_string();
    let flag = |id: &str| matches!(values.get(id), Some(Value::Bool(true)));
    let family = text(schema::TEXT_FONT);
    oa_text::TextSpec {
        content: text(schema::TEXT_CONTENT),
        family: if family.trim().is_empty() { oa_text::default_family().to_string() } else { family },
        bold: flag(schema::TEXT_BOLD),
        italic: flag(schema::TEXT_ITALIC),
        size: values.float(schema::TEXT_SIZE).clamp(1.0, 4000.0),
        align: oa_text::Align::parse(values.get(schema::TEXT_ALIGN).and_then(Value::as_enum).unwrap_or("center")),
        tracking: values.float(schema::TEXT_TRACKING),
        line_height: values.float(schema::TEXT_LINE_HEIGHT),
    }
}

/// Native size of a layer's content (at clip-local context `ctx`) and the most it may be
/// rasterized above 1:1.
pub(crate) fn layer_native(
    project: &Project,
    item: &Item,
    variant: &FormatVariant,
    ctx: &oa_params::EvalContext,
    depth_ok: bool,
) -> Result<([f64; 2], f64), Unplaceable> {
    match &item.kind {
        ItemKind::Text => {
            let values = item.params.eval(schema::text(), variant.overrides.get(&item.id), ctx);
            // Text is vector: sharp at any scale.
            Ok((oa_text::layout(&text_spec(&values)).size, 8.0))
        }
        ItemKind::Media { media } => {
            let info = project.media(*media).and_then(|m| m.info.as_ref());
            let info = info.filter(|i| i.has_video && i.width > 0 && i.height > 0).ok_or(Unplaceable::MediaWithoutInfo)?;
            Ok(([info.width as f64, info.height as f64], 1.0))
        }
        ItemKind::Solid => Ok(([variant.size.width as f64, variant.size.height as f64], 1.0)),
        ItemKind::Nested { sequence } if depth_ok => {
            let c = project.sequence(*sequence).ok_or(Unplaceable::Unsupported)?.canvas();
            // Nested vector-like content may rasterize above 1:1.
            Ok(([c.width as f64, c.height as f64], 4.0))
        }
        _ => Err(Unplaceable::Unsupported),
    }
}

/// The reframe for a layer: its fit mode and focus, applied to the canvas. Text is
/// always placed 1:1 (its size is the font size), centered.
pub(crate) fn reframe(item: &Item, vis: &Evaluated, native: [f64; 2], variant: &FormatVariant) -> Reframe {
    let fit = match item.kind {
        ItemKind::Text => FitMode::None,
        _ => vis.get(schema::FIT).and_then(Value::as_enum).and_then(FitMode::parse).unwrap_or_default(),
    };
    Reframe::compute(native, variant.size, fit, vis.vec2(schema::FOCUS))
}

/// Layer (native px) → canvas px: reframe, then anchor, scale × squash, rotation and
/// position (DESIGN.md §4).
pub(crate) fn user_transform(vis: &Evaluated, native: [f64; 2], base: &Reframe, variant: &FormatVariant) -> Affine2 {
    let canvas = variant.size;
    let anchor = vis.vec2(schema::ANCHOR);
    let pivot = base.apply([anchor[0] * native[0], anchor[1] * native[1]]);
    let pos = vis.vec2(schema::POSITION);
    let [sx, sy] = vis.vec2(schema::SCALE);
    let squash = squash_factor(vis.float(schema::SQUASH));
    Affine2::scale(base.scale[0], base.scale[1])
        .then(&Affine2::translate(base.offset[0] - pivot[0], base.offset[1] - pivot[1]))
        .then(&Affine2::scale(sx * squash, sy / squash))
        .then(&Affine2::rotate_degrees(vis.float(schema::ROTATION)))
        .then(&Affine2::translate(pivot[0] + pos[0] * canvas.width as f64, pivot[1] + pos[1] * canvas.height as f64))
}

/// Volume-preserving squash: x × f, y ÷ f.
pub fn squash_factor(squash: f64) -> f64 {
    1.0 + squash.max(-0.95)
}

/// One visible layer at one instant, in canvas pixels of one format variant.
#[derive(Clone, Debug)]
pub struct Placement {
    pub item: ItemId,
    pub track: TrackId,
    /// Content size in the layer's own pixels (media native size, canvas for solids).
    pub native: [f64; 2],
    /// Where the source lands before the user transform (fit/fill + focus).
    pub reframe: Reframe,
    /// Layer px → canvas px.
    pub to_canvas: Affine2,
    /// The transform's pivot (the anchor point) in canvas px.
    pub pivot: [f64; 2],
    /// Built-in visual params evaluated at this instant (overrides applied).
    pub values: Evaluated,
    pub opacity: f64,
}

impl Placement {
    /// The share of each edge cropped away: left, right, top, bottom.
    pub fn crop(&self) -> [f64; 4] {
        schema::CROPS.map(|c| self.values.float(c).clamp(0.0, 1.0))
    }

    /// The visible part of the layer (inside its crop), in layer px: x0, y0, x1, y1.
    pub fn visible_box(&self) -> [f64; 4] {
        let [w, h] = self.native;
        let [l, r, t, b] = self.crop();
        let (x0, y0) = (l * w, t * h);
        [x0, y0, ((1.0 - r) * w).max(x0), ((1.0 - b) * h).max(y0)]
    }

    /// Corners of what's visible (the crop shrinks it) in canvas px: top-left,
    /// top-right, bottom-right, bottom-left. Selection, handles and clicks use these.
    pub fn corners(&self) -> [[f64; 2]; 4] {
        let [x0, y0, x1, y1] = self.visible_box();
        [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(|p| self.to_canvas.apply(p))
    }

    /// Corners of the whole, uncropped layer in canvas px.
    pub fn full_corners(&self) -> [[f64; 2]; 4] {
        let [w, h] = self.native;
        [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]].map(|p| self.to_canvas.apply(p))
    }

    /// Canvas px → layer px (`None` if the layer is scaled to nothing).
    pub fn to_layer(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        self.to_canvas.invert().map(|inv| inv.apply(p))
    }

    /// Canvas px → fraction of the layer (0..1 inside).
    pub fn to_layer_fraction(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        self.to_layer(p).map(|[x, y]| [x / self.native[0], y / self.native[1]])
    }

    /// Does the layer's visible (cropped) rectangle cover canvas point `p`?
    pub fn contains(&self, p: [f64; 2]) -> bool {
        let [x0, y0, x1, y1] = self.visible_box();
        self.to_layer(p).is_some_and(|[x, y]| (x0..=x1).contains(&x) && (y0..=y1).contains(&y))
    }
}

/// Placement of `item` at timeline time `t` in `variant`, or why it has none. Invisible
/// layers (opacity 0) still get a placement so they can be selected and moved.
pub fn placement(project: &Project, track: TrackId, item: &Item, variant: &FormatVariant, t: Time) -> Result<Placement, Unplaceable> {
    let ctx = item.eval_context(t);
    let values = item.params.eval(schema::visual(), variant.overrides.get(&item.id), &ctx);
    let (native, _) = layer_native(project, item, variant, &ctx, true)?;
    let reframe = reframe(item, &values, native, variant);
    let to_canvas = user_transform(&values, native, &reframe, variant);
    let anchor = values.vec2(schema::ANCHOR);
    let pivot = to_canvas.apply([anchor[0] * native[0], anchor[1] * native[1]]);
    let opacity = values.float(schema::OPACITY).clamp(0.0, 1.0);
    Ok(Placement { item: item.id, track, native, reframe, to_canvas, pivot, values, opacity })
}

/// Every placeable layer showing at `t`, bottom to top — the order they composite in.
pub fn layers_at(project: &Project, seq: SeqId, variant: &FormatVariant, t: Time) -> Vec<Placement> {
    let Some(s) = project.sequence(seq) else { return Vec::new() };
    s.tracks
        .iter()
        .filter(|tr| tr.enabled && tr.kind == TrackKind::Video)
        .filter_map(|tr| {
            let item = tr.item_at(t).filter(|i| i.enabled)?;
            placement(project, tr.id, item, variant, t).ok()
        })
        .collect()
}

/// The topmost visible layer under canvas point `p` at `t`: what a click in the viewer
/// selects. Fully transparent layers are skipped so they don't block what's beneath.
pub fn hit_test(project: &Project, seq: SeqId, variant: &FormatVariant, t: Time, p: [f64; 2]) -> Option<ItemId> {
    layers_at(project, seq, variant, t)
        .into_iter()
        .rev()
        .find(|l| l.opacity > 0.0 && l.contains(p))
        .map(|l| l.item)
}
