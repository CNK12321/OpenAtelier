//! The frame planner — the only bridge between the document and rendering.
//!
//! Space and resolution rules (DESIGN.md §4):
//! 1. A layer's source is placed by **reframe** (fit/fill/stretch/none + focus), then
//!    the user transform (anchor, position, scale, squash, rotation), then the output
//!    render scale.
//! 2. Effects run in **layer space**, before the transform, at a **raster scale**
//!    picked from the layer's final on-screen scale: never above the source's native
//!    resolution for media, quantized to powers of two so animated scale doesn't
//!    change the raster size (and cache key) every frame.
//! 3. `LayerPixels` effect params are multiplied by the raster scale, so a 10px blur
//!    looks the same at any preview resolution.

pub mod scene;
pub mod transitions;

use oa_doc::{schema, Item, ItemId, ItemKind, Project, SeqId, TrackKind, VariantId};
use oa_graph::registry::{Motion, MotionInput, Registry, Statefulness};
use oa_graph::{
    Affine2, BlendMode, Graph, GraphBuilder, KeyContext, LayerInfo, NodeId, NodeOp, Rect, Representation,
};
use oa_params::Value;
use oa_time::Time;
use std::fmt;

pub struct PlanOptions {
    /// `None` = the sequence's active variant.
    pub variant: Option<VariantId>,
    /// Output resolution relative to the canvas (0.5 = half-res preview).
    pub render_scale: f64,
    pub use_proxies: bool,
    pub key_context: KeyContext,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions { variant: None, render_scale: 1.0, use_proxies: false, key_context: KeyContext::default() }
    }
}

/// Things the planner skipped. Never fatal: the user's data is untouched and the UI
/// can show placeholders/warnings.
#[derive(Debug, Default, PartialEq)]
pub struct PlanReport {
    pub missing_effects: Vec<String>,
    pub unsupported_items: Vec<ItemId>,
    pub media_without_info: Vec<ItemId>,
}

pub struct Plan {
    pub graph: Graph,
    pub report: PlanReport,
}

#[derive(Debug, PartialEq)]
pub enum PlanError {
    NoSuchSequence(SeqId),
    NoSuchVariant(VariantId),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NoSuchSequence(s) => write!(f, "sequence {} not found", s.0),
            PlanError::NoSuchVariant(v) => write!(f, "format variant {} not found", v.0),
        }
    }
}

impl std::error::Error for PlanError {}

const MAX_NESTING: usize = 32;

/// The longest side a pixel-art layer is enlarged to before its effects run.
const MAX_PIXEL_ART_SIDE: f64 = 4096.0;

pub fn plan_frame(
    project: &Project,
    seq: SeqId,
    t: Time,
    opts: &PlanOptions,
    registry: &Registry,
) -> Result<Plan, PlanError> {
    let sequence = project.sequence(seq).ok_or(PlanError::NoSuchSequence(seq))?;
    let variant = match opts.variant {
        Some(v) => sequence.variant(v).ok_or(PlanError::NoSuchVariant(v))?.id,
        None => sequence.active().id,
    };
    let mut p = Planner {
        project,
        registry,
        opts,
        b: GraphBuilder::new(opts.key_context),
        report: PlanReport::default(),
    };
    let out = p.sequence(seq, Some(variant), t, opts.render_scale, 0);
    let out = p.output_transform(sequence, seq, t, out);
    Ok(Plan { graph: p.b.finish(out), report: p.report })
}

/// The tone map a sequence's output uses (`schema::OUT_TONE_MAP`): `auto` turns it on
/// only when HDR or log footage is used somewhere in it, so SDR projects are untouched.
pub fn tone_map(project: &Project, seq: SeqId, setting: &str) -> oa_doc::color::ToneMap {
    use oa_doc::color::ToneMap;
    match setting {
        "off" => ToneMap::Off,
        "soft" => ToneMap::Soft,
        "filmic" => ToneMap::Filmic,
        _ if uses_high_range(project, seq, 0) => ToneMap::Soft,
        _ => ToneMap::Off,
    }
}

/// Whether `seq` (or a sequence nested in it) shows HDR or log footage.
fn uses_high_range(project: &Project, seq: SeqId, depth: usize) -> bool {
    let Some(s) = project.sequence(seq) else { return false };
    s.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| &t.items).any(|i| match i.kind {
        ItemKind::Media { media } => project.media(media).is_some_and(|m| m.input_color().0.is_high_range()),
        ItemKind::Nested { sequence } => depth < MAX_NESTING && uses_high_range(project, sequence, depth + 1),
        _ => false,
    })
}

/// A file's YCbCr overrides for [`NodeOp::Source`]'s `yuv`.
fn yuv_override(m: &oa_doc::MediaRef) -> [u8; 2] {
    use oa_doc::color::{Matrix, Range};
    let matrix = match m.color.matrix {
        Matrix::Auto => 0,
        Matrix::Bt601 => 1,
        Matrix::Bt709 => 2,
        Matrix::Bt2020 => 3,
    };
    let range = match m.color.range {
        Range::Auto => 0,
        Range::Limited => 1,
        Range::Full => 2,
    };
    [matrix, range]
}

/// "Highlight when spoken": `values` with each parameter that has a spoken value
/// (`schema::spoken(id)` in `params`) replaced by it, or `None` when none has one.
fn spoken(params: &oa_params::ParamSet, schema: &[oa_params::ParamSchema], values: &oa_params::Evaluated, ctx: &oa_params::EvalContext) -> Option<oa_params::Evaluated> {
    let mut out = values.clone();
    let mut any = false;
    for s in schema {
        let Some(src) = params.get(&schema::spoken(s.id.as_str())) else { continue };
        let v = match src.eval(ctx) {
            // A plain color where a gradient goes (as a title's own color accepts).
            Value::Color(c) if s.ty == oa_params::ParamType::Gradient => Value::Gradient(oa_params::Gradient::solid(c)),
            v if v.ty() == s.ty => v,
            _ => continue,
        };
        match out.0.iter_mut().find(|(id, _)| id == &s.id) {
            Some(slot) => slot.1 = v,
            None => out.0.push((s.id.clone(), v)),
        }
        any = true;
    }
    any.then_some(out)
}

/// Snap a scale up to the next power of two (…, 1/4, 1/2, 1, 2, …) within limits.
fn quantize_scale(k: f64, max: f64) -> f64 {
    let k = k.clamp(1.0 / 64.0, max);
    let q = 2f64.powi(k.log2().ceil() as i32);
    q.min(max)
}

struct Planner<'a> {
    project: &'a Project,
    registry: &'a Registry,
    opts: &'a PlanOptions,
    b: GraphBuilder,
    report: PlanReport,
}

impl Planner<'_> {
    /// Composite of `seq` at time `t`, `scale` × its canvas size.
    fn sequence(&mut self, seq: SeqId, variant: Option<VariantId>, t: Time, scale: f64, depth: usize) -> NodeId {
        let s = self.project.sequence(seq).expect("validated by caller or nesting checks");
        let v = variant.and_then(|v| s.variant(v)).unwrap_or_else(|| s.active());
        let size = [
            ((v.size.width as f64 * scale).round() as u32).max(1),
            ((v.size.height as f64 * scale).round() as u32).max(1),
        ];
        // The top sequence sits on its background (black by default); a nested one (a
        // compound clip, a group used as media) is see-through where it has nothing.
        let top = depth == 0;
        let (background, under) = if top { self.background(s, t, size, depth) } else { ([0.0; 4], None) };
        // The background's own effects run on what it drew — or on a canvas of its color.
        // Not for blurred content: that background is the finished frame blurred, built
        // (and given its effects) further down; a canvas here would sit under the clips,
        // opaque, and the blur would be made of it.
        let blurred = top
            && matches!(s.params.eval(schema::background(), None, &oa_params::EvalContext::at(t, t)).get(schema::BG_MODE), Some(Value::Enum(m)) if m == "blur");
        let under = if top && !blurred && self.has_background_effects(s) {
            let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
            let node = under.unwrap_or_else(|| self.b.add(NodeOp::Solid { color: background, size: [canvas.x1, canvas.y1] }, vec![], canvas, background[3] >= 1.0));
            Some(self.background_effects(s, t, scale, size, node, depth))
        } else {
            under
        };
        let mut inputs = Vec::new();
        let mut layers = Vec::new();
        if let Some(node) = under {
            inputs.push(node);
            layers.push(LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false });
        }
        let canvas_rect = Rect::from_size(size[0] as f64, size[1] as f64);
        for track in s.tracks.iter().filter(|t| t.enabled && t.kind == TrackKind::Video) {
            // An effect track: its container's effects run over everything below it —
            // flattened into one picture (the background too), then carried on up as the
            // bottom layer of what's above.
            if track.effects {
                let Some(item) = track.item_at(t).filter(|i| i.enabled && i.effects.iter().any(|e| e.enabled && !self.registry.is_sound(&e.type_id))) else { continue };
                let ctx = item.eval_context(t);
                // How much of the result shows: its opacity, times its fades (a Fade intro
                // fades the effect in over the untouched picture).
                let vis = item.params.eval(schema::visual(), v.overrides.get(&item.id), &ctx);
                let mix = (vis.float(schema::OPACITY) * self.motion(item, v, &ctx).opacity).clamp(0.0, 1.0);
                if mix <= 0.0 {
                    continue;
                }
                let below = self.b.add(
                    NodeOp::Composite { size, background: if top { background } else { [0.0; 4] }, layers: std::mem::take(&mut layers) },
                    std::mem::take(&mut inputs),
                    canvas_rect,
                    top && background[3] >= 1.0,
                );
                let (node, _) = self.effect_chain(item, &ctx, below, canvas_rect, scale, [canvas_rect.x1, canvas_rect.y1], false, depth);
                let blend = blend_of(&vis);
                if mix < 1.0 || blend != BlendMode::Normal {
                    // The untouched picture, with the result laid over it.
                    inputs.push(below);
                    layers.push(LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false });
                }
                inputs.push(node);
                layers.push(LayerInfo { opacity: mix as f32, blend, pixelated: false });
                continue;
            }
            if let Some(active) = transitions::active(track, t) {
                if let Some(node) = self.transition(&active, v, t, scale, size, depth) {
                    inputs.push(node);
                    layers.push(LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false });
                }
                continue;
            }
            let Some(item) = track.item_at(t).filter(|i| i.enabled) else { continue };
            if let Some((node, info)) = self.layer(item, v, t, scale, depth) {
                inputs.push(node);
                layers.push(info);
            }
        }
        let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
        if top && !inputs.is_empty() {
            let bg = s.params.eval(schema::background(), None, &oa_params::EvalContext::at(t, t));
            if matches!(bg.get(schema::BG_MODE), Some(Value::Enum(m)) if m == "blur") {
                // Blurred content: the frame itself (every layer, as placed), enlarged
                // behind itself until its content covers the canvas.
                let front = self.b.add(NodeOp::Composite { size, background: [0.0; 4], layers }, inputs, canvas, false);
                let content = self.content_bounds(front, canvas);
                let back = content.and_then(|c| self.blurred_backdrop(front, c, size, scale, bg.float(schema::BG_BLUR), bg.float(schema::BG_DIM)));
                let back = back.map(|b| if self.has_background_effects(s) { self.background_effects(s, t, scale, size, b, depth) } else { b });
                let (inputs, layers): (Vec<NodeId>, Vec<LayerInfo>) =
                    back.into_iter().chain([front]).map(|n| (n, LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false })).unzip();
                return self.b.add(NodeOp::Composite { size, background: [0.0, 0.0, 0.0, 1.0], layers }, inputs, canvas, true);
            }
        }
        let op = NodeOp::Composite { size, background, layers };
        self.b.add(op, inputs, canvas, top && background[3] >= 1.0)
    }

    /// Whether the sequence's background has picture effects to run.
    fn has_background_effects(&self, s: &oa_doc::Sequence) -> bool {
        s.background.effects.iter().any(|e| e.enabled && !self.registry.is_sound(&e.type_id))
    }

    /// The background's effects (`Sequence::background`, a clip as long as time) on
    /// `node`, the background at `scale` × the canvas — the same chain clips run.
    fn background_effects(&mut self, s: &oa_doc::Sequence, t: Time, scale: f64, size: [u32; 2], node: NodeId, depth: usize) -> NodeId {
        let item = &s.background;
        let ctx = item.eval_context(t);
        // The blurred-content backdrop arrives placed (a transform, only drawable into a
        // composite): flatten it onto a canvas first, so its effects see one picture the
        // size of the frame.
        let node = if matches!(self.b.node(node).op, NodeOp::Transform { .. }) {
            let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }];
            self.b.add(NodeOp::Composite { size, background: [0.0; 4], layers }, vec![node], Rect::from_size(size[0] as f64, size[1] as f64), false)
        } else {
            node
        };
        let bounds = self.b.node(node).bounds;
        let size = [bounds.x1 - bounds.x0, bounds.y1 - bounds.y0];
        self.effect_chain(item, &ctx, node, bounds, scale, size, false, depth).0
    }

    /// Where a composite's layers actually put pixels, within `canvas` (`None`: nowhere).
    fn content_bounds(&self, composite: NodeId, canvas: Rect) -> Option<Rect> {
        let mut out: Option<Rect> = None;
        for &i in &self.b.node(composite).inputs {
            let b = self.b.node(i).bounds;
            let b = Rect { x0: b.x0.max(canvas.x0), y0: b.y0.max(canvas.y0), x1: b.x1.min(canvas.x1), y1: b.y1.min(canvas.y1) };
            if b.x1 - b.x0 < 1.0 || b.y1 - b.y0 < 1.0 {
                continue;
            }
            out = Some(match out {
                Some(o) => Rect { x0: o.x0.min(b.x0), y0: o.y0.min(b.y0), x1: o.x1.max(b.x1), y1: o.y1.max(b.y1) },
                None => b,
            });
        }
        out
    }

    /// `front` scaled so `content` covers the canvas (centered), blurred by `blur` canvas
    /// px and darkened by `dim` — worked at a quarter of the size, since it's blurred.
    fn blurred_backdrop(&mut self, front: NodeId, content: Rect, size: [u32; 2], scale: f64, blur: f64, dim: f64) -> Option<NodeId> {
        let k = 0.25;
        let small = [((size[0] as f64 * k).round() as u32).max(1), ((size[1] as f64 * k).round() as u32).max(1)];
        let small_rect = Rect::from_size(small[0] as f64, small[1] as f64);
        let (w, h) = (size[0] as f64, size[1] as f64);
        let (cw, ch) = (content.x1 - content.x0, content.y1 - content.y0);
        let cover = (w / cw).max(h / ch);
        let (cx, cy) = ((content.x0 + content.x1) / 2.0, (content.y0 + content.y1) / 2.0);
        let matrix = Affine2::translate(-cx, -cy).then(&Affine2::scale(cover, cover)).then(&Affine2::translate(w / 2.0, h / 2.0)).then(&Affine2::scale(
            small[0] as f64 / w,
            small[1] as f64 / h,
        ));
        let placed = self.b.add(NodeOp::Transform { matrix }, vec![front], matrix.map_rect(Rect::from_size(w, h)), false);
        let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }];
        let shrunk = self.b.add(NodeOp::Composite { size: small, background: [0.0; 4], layers }, vec![placed], small_rect, false);
        let blurred = self.internal("oa.blur.gaussian", vec![(blur * scale * k) as f32, 1.0], shrunk, small_rect)?;
        let dimmed = self.internal(oa_graph::registry::DIM, vec![dim as f32], blurred, small_rect)?;
        let up = Affine2::scale(w / small[0] as f64, h / small[1] as f64);
        Some(self.b.add(NodeOp::Transform { matrix: up }, vec![dimmed], up.map_rect(small_rect), false))
    }

    /// What shows behind the top sequence's clips (`schema::background()`): the color
    /// the canvas is cleared to, and a layer drawn under everything (a gradient, the
    /// blurred picture, a tiled texture).
    fn background(
        &mut self,
        s: &oa_doc::Sequence,
        t: Time,
        size: [u32; 2],
        depth: usize,
    ) -> ([f32; 4], Option<NodeId>) {
        const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        let bg = s.params.eval(schema::background(), None, &oa_params::EvalContext::at(t, t));
        let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
        let mode = match bg.get(schema::BG_MODE) {
            Some(Value::Enum(m)) => m.clone(),
            _ => "solid".into(),
        };
        match mode.as_str() {
            // Blurred content needs the finished frame: `sequence` builds it.
            "blur" => (BLACK, None),
            "texture" => {
                let Some(id) = bg.get(schema::BG_TEXTURE).and_then(|v| v.as_media()) else { return (BLACK, None) };
                // Tile height is a share of the canvas; width follows the picture's shape.
                let aspect = match (self.project.media(oa_doc::MediaId(id)), self.project.sequence(SeqId(id))) {
                    (Some(m), _) => m.info.as_ref().filter(|i| i.height > 0).map_or(1.0, |i| i.width as f64 / i.height as f64),
                    (None, Some(inner)) => inner.canvas().width as f64 / inner.canvas().height.max(1) as f64,
                    _ => return (BLACK, None),
                };
                let h = (bg.float(schema::BG_TILE) * size[1] as f64).max(2.0);
                // A compound clip loops behind the whole timeline, however short it is.
                let t = match self.project.sequence(SeqId(id)).map(|inner| inner.duration()) {
                    Some(d) if d > Time::ZERO => Time(t.0.rem_euclid(d.0)),
                    _ => t,
                };
                let pixel = self.project.media(oa_doc::MediaId(id)).filter(|m| m.pixelated()).and_then(|m| m.info.as_ref()).map(|i| [i.width as f64, i.height as f64]);
                let tile = match pixel {
                    // Pixel art: decode as is, then enlarge without smoothing.
                    Some(native) => self.media_source(id, t, native, depth).map(|src| {
                        let out = [(h * aspect).round().max(1.0) as u32, h.round().max(1.0) as u32];
                        let matrix = Affine2::scale(out[0] as f64 / native[0], out[1] as f64 / native[1]);
                        let placed = self.b.add(NodeOp::Transform { matrix }, vec![src], matrix.map_rect(Rect::from_size(native[0], native[1])), false);
                        let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: true }];
                        self.b.add(NodeOp::Composite { size: out, background: [0.0; 4], layers }, vec![placed], Rect::from_size(out[0] as f64, out[1] as f64), false)
                    }),
                    None => self.media_source(id, t, [h * aspect, h], depth),
                };
                let Some(tile) = tile else { return (BLACK, None) };
                (BLACK, self.internal(oa_graph::registry::TILE, vec![], tile, canvas))
            }
            _ => {
                let g = bg.get(schema::BG_COLOR).and_then(|v| v.as_gradient()).cloned().unwrap_or_else(|| oa_params::Gradient::solid([0.0, 0.0, 0.0, 1.0]));
                if g.stops.len() <= 1 {
                    // One color: just clear to it.
                    let c = g.stops.first().map_or([0.0, 0.0, 0.0, 1.0], |s| s.color);
                    return (c.map(|x| x as f32), None);
                }
                let white = self.b.add(NodeOp::Solid { color: [1.0; 4], size: [size[0] as f64, size[1] as f64] }, vec![], canvas, true);
                (BLACK, self.internal(oa_graph::registry::FILL, g.pack(), white, canvas))
            }
        }
    }

    /// The sequence's output transform (exposure, tone mapping) on the finished frame;
    /// nothing when it would change nothing.
    fn output_transform(&mut self, s: &oa_doc::Sequence, seq: SeqId, t: Time, out: NodeId) -> NodeId {
        let v = s.params.eval(schema::output(), None, &oa_params::EvalContext::at(t, t));
        let setting = match v.get(schema::OUT_TONE_MAP) {
            Some(Value::Enum(m)) => m.clone(),
            _ => "auto".into(),
        };
        let map = tone_map(self.project, seq, &setting);
        let exposure = v.float(schema::OUT_EXPOSURE);
        if map == oa_doc::color::ToneMap::Off && exposure == 0.0 {
            return out;
        }
        let bounds = self.b.node(out).bounds;
        let opaque = self.b.node(out).opaque;
        self.internal_keeping(oa_graph::registry::OUTPUT, vec![map.shader_index() as f32, 2f64.powf(exposure) as f32], out, bounds, opaque)
            .unwrap_or(out)
    }

    /// A file's input transform on its freshly decoded picture (DESIGN.md §9); nothing
    /// for ordinary sRGB/Rec.709 material with no exposure trim.
    fn input_color(&mut self, m: &oa_doc::MediaRef, src: NodeId) -> NodeId {
        let (transfer, gamut) = m.input_color();
        let exposure = m.color.exposure;
        if transfer.shader_index() == 0 && gamut == oa_doc::color::Gamut::Rec709 && exposure == 0.0 {
            return src;
        }
        let mut uniforms = vec![transfer.shader_index() as f32];
        uniforms.extend(gamut.to_rec709().iter().flatten().map(|x| *x as f32));
        uniforms.push(2f64.powf(exposure) as f32);
        let bounds = self.b.node(src).bounds;
        let opaque = self.b.node(src).opaque;
        self.internal_keeping(oa_graph::registry::INPUT, uniforms, src, bounds, opaque).unwrap_or(src)
    }

    /// [`Planner::internal`] for effects that leave opacity alone.
    fn internal_keeping(&mut self, type_id: &str, uniforms: Vec<f32>, input: NodeId, bounds: Rect, opaque: bool) -> Option<NodeId> {
        let op = self.internal_op(type_id, uniforms)?;
        Some(self.b.add(op, vec![input], bounds, opaque))
    }

    /// One of the host's own effects (`oa.internal.*`, or a built-in by id) on `input`,
    /// with a still clock.
    fn internal(&mut self, type_id: &str, uniforms: Vec<f32>, input: NodeId, bounds: Rect) -> Option<NodeId> {
        let op = self.internal_op(type_id, uniforms)?;
        Some(self.b.add(op, vec![input], bounds, false))
    }

    fn internal_op(&self, type_id: &str, mut uniforms: Vec<f32>) -> Option<NodeOp> {
        let d = self.registry.effect(type_id)?;
        uniforms.extend(oa_graph::registry::STILL_CLOCK);
        Some(NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms, nearest: false })
    }

    /// The combined move of a clip's motion effects (fly in, shake…) at this instant.
    fn motion(&mut self, item: &Item, variant: &oa_doc::FormatVariant, ctx: &oa_params::EvalContext) -> Motion {
        let canvas = [variant.size.width as f64, variant.size.height as f64];
        let mut total = Motion::NONE;
        for fx in item.active_effects().iter().filter(|e| e.enabled) {
            let Some(d) = self.registry.effect(&fx.type_id) else { continue };
            let Some(f) = d.motion.filter(|_| d.kind == oa_graph::EffectKind::Motion) else { continue };
            let Some(clock) = fx.role.clock(ctx.clip_time, item.range.duration) else { continue };
            let values = fx.params.eval(&d.params, None, ctx);
            let input = MotionInput { visibility: clock.visibility, progress: clock.progress, seconds: clock.seconds, canvas, seed: fx.id.0, leaving: matches!(fx.role, oa_doc::EffectRole::Out { .. }) };
            total = total.then(f(&values, &input));
        }
        total
    }

    /// Runs `item`'s picture effects, in order, on `node` (its picture at `raster` ×,
    /// `bounds` in raster px): each in its window with its clock, media params as second
    /// inputs, display-space effects wrapped. Clips and the background share it.
    #[allow(clippy::too_many_arguments)]
    fn effect_chain(
        &mut self,
        item: &Item,
        ctx: &oa_params::EvalContext,
        mut node: NodeId,
        mut bounds: Rect,
        raster: f64,
        raster_size: [f64; 2],
        pixelated: bool,
        depth: usize,
    ) -> (NodeId, Rect) {
        for fx in item.active_effects().iter().filter(|e| e.enabled) {
            if self.registry.is_sound(&fx.type_id) {
                continue; // sound: the audio mixer runs it
            }
            let Some(d) = self.registry.effect(&fx.type_id).cloned() else {
                self.report.missing_effects.push(fx.type_id.clone());
                continue;
            };
            if d.kind == oa_graph::EffectKind::Motion || d.kind.text_only() {
                continue; // folded into the layer's transform above / run in the text pass
            }
            // In/out effects only run inside their windows.
            let Some(clock) = fx.role.clock(ctx.clip_time, item.range.duration) else { continue };
            let clock = if fx.role == oa_doc::EffectRole::Passive && !d.time_varying {
                oa_graph::registry::STILL_CLOCK
            } else {
                [clock.visibility as f32, clock.progress as f32, clock.seconds as f32]
            };
            let values = fx.params.eval(&d.params, None, ctx);
            // A media parameter's picture becomes the effect's second input, stretched
            // over the layer at the layer's raster size.
            // Glow's second input is the clip itself, unblurred, to lay over its halo.
            let media_input = if d.type_id.as_ref() == oa_graph::registry::GLOW {
                Some(node)
            } else {
                d.media_param()
                    .and_then(|p| values.get(p.id.as_str()))
                    .and_then(|v| v.as_media())
                    .and_then(|id| self.media_source(id, ctx.clip_time, raster_size, depth))
            };
            let stateful = match d.state {
                Statefulness::Pure => false,
                Statefulness::Stateful { preroll } => {
                    self.b.require_preroll(preroll);
                    true
                }
            };
            // A neighborhood effect written for display-encoded values gets them: encode
            // before, decode after (point ops do this inside their own pass).
            let display = d.space == oa_graph::WorkingSpace::Display && d.kind != oa_graph::EffectKind::PointOp;
            if display {
                let (b, o) = (self.b.node(node).bounds, self.b.node(node).opaque);
                node = self.internal_keeping(oa_graph::registry::TO_DISPLAY, vec![], node, b, o).unwrap_or(node);
            }
            bounds = d.grown_bounds(&values, raster, bounds);
            let opaque = d.preserves_opacity && self.b.node(node).opaque;
            let op = NodeOp::Effect {
                type_id: d.type_id.clone(),
                version: d.version,
                kind: d.kind.clone(),
                space: d.space,
                fusible: d.fusible,
                stateful,
                uniforms: {
                    let mut u = d.pack_uniforms(&values, raster);
                    u.extend(clock);
                    u
                },
                // Pixel art: effects read whole picture pixels, never a blend of two.
                nearest: pixelated,
            };
            let inputs = std::iter::once(node).chain(media_input).collect();
            node = self.b.add(op, inputs, bounds, opaque);
            if display {
                node = self.internal_keeping(oa_graph::registry::TO_LINEAR, vec![], node, bounds, opaque).unwrap_or(node);
            }
        }
        (node, bounds)
    }

    /// A pool media file as an effect input: decoded at `size`, at `t` into the file
    /// (following the clip's clock; stills ignore it). `None` if it has no picture.
    /// The id can also name a sequence (a group turned into media): it's rendered at
    /// `t`, stretched to `size`.
    fn media_source(&mut self, id: u64, t: Time, size: [f64; 2], depth: usize) -> Option<NodeId> {
        if let Some(s) = self.project.sequence(SeqId(id)) {
            if depth + 1 >= MAX_NESTING {
                return None;
            }
            let canvas = s.canvas();
            let scale = (size[0] / canvas.width.max(1) as f64).max(size[1] / canvas.height.max(1) as f64);
            let node = self.sequence(SeqId(id), None, t.max(Time::ZERO), scale, depth + 1);
            let out = [size[0].round().max(1.0) as u32, size[1].round().max(1.0) as u32];
            let got = self.b.node(node).bounds;
            let (w, h) = (got.x1 - got.x0, got.y1 - got.y0);
            if (w - out[0] as f64).abs() < 0.5 && (h - out[1] as f64).abs() < 0.5 {
                return Some(node);
            }
            // Stretch to the layer, as a file of another shape would be.
            let matrix = oa_graph::Affine2::scale(out[0] as f64 / w.max(1.0), out[1] as f64 / h.max(1.0));
            let node = self.b.add(NodeOp::Transform { matrix }, vec![node], matrix.map_rect(got), false);
            let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }];
            return Some(self.b.add(NodeOp::Composite { size: out, background: [0.0; 4], layers }, vec![node], Rect::from_size(out[0] as f64, out[1] as f64), false));
        }
        let m = self.project.media(oa_doc::MediaId(id))?;
        let info = m.info.as_ref().filter(|i| i.has_video && i.width > 0)?;
        let size = [size[0].round().max(1.0) as u32, size[1].round().max(1.0) as u32];
        let rep = if self.opts.use_proxies { Representation::Proxy } else { Representation::Original };
        let op = NodeOp::Source {
            media: id,
            fingerprint: m.fingerprint.as_deref().map(Into::into),
            source_time: t.max(Time::ZERO),
            rep,
            decode_scale: size[0] as f64 / info.width as f64,
            size,
            yuv: yuv_override(m),
        };
        let src = self.b.add(op, vec![], Rect::from_size(size[0] as f64, size[1] as f64), false);
        Some(self.input_color(m, src))
    }

    /// A transition on one track: each side is the clip composited alone onto a
    /// transparent canvas (so its own transform and effects apply), then the two are mixed.
    fn transition(
        &mut self,
        a: &transitions::Active<'_>,
        variant: &oa_doc::FormatVariant,
        t: Time,
        scale: f64,
        size: [u32; 2],
        depth: usize,
    ) -> Option<NodeId> {
        let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
        let mut side = |item: Option<&Item>| -> NodeId {
            let layer = item.and_then(|i| self.layer(i, variant, t, scale, depth));
            let opaque = layer.as_ref().is_some_and(|(n, info)| {
                let node = self.b.node(*n);
                node.opaque && info.opacity >= 1.0 && node.bounds.contains_rect(&canvas)
            });
            let (inputs, layers) = layer.map(|(n, info)| (vec![n], vec![info])).unwrap_or_default();
            self.b.add(NodeOp::Composite { size, background: [0.0; 4], layers }, inputs, canvas, opaque)
        };
        let from = side(a.from);
        let to = side(a.to);
        let Some(d) = self.registry.effect(&a.transition.type_id).cloned() else {
            // Unknown type (a plugin that isn't installed): cut at the midpoint.
            self.report.missing_effects.push(a.transition.type_id.clone());
            return Some(if a.progress < 0.5 { from } else { to });
        };
        let values = a.transition.params.eval(&d.params, None, &a.owner.eval_context(t));
        let op = NodeOp::Transition {
            type_id: d.type_id.clone(),
            version: d.version,
            uniforms: d.pack_uniforms(&values, scale),
            progress: a.progress as f32,
        };
        let opaque = self.b.node(from).opaque && self.b.node(to).opaque;
        Some(self.b.add(op, vec![from, to], canvas, opaque))
    }

    /// A text clip's glyphs at `raster` × canvas size, with its per-letter and per-pixel
    /// text effects. Returns the node and its bounds: the text box grown by the outline
    /// and by however far animated letters may travel.
    fn text(
        &mut self,
        item: &Item,
        variant: &oa_doc::FormatVariant,
        ctx: &oa_params::EvalContext,
        raster: f64,
        bounds: Rect,
    ) -> (NodeId, Rect) {
        let values = item.params.eval(schema::text(), variant.overrides.get(&item.id), ctx);
        let spec = scene::text_spec(&values);
        // "Highlight when spoken": the values the spoken word takes instead.
        let spoken_values = spoken(&item.params, schema::text(), &values, ctx);
        let style_of = |values: &oa_params::Evaluated| {
            let gradient = |id: &str| values.get(id).and_then(Value::as_gradient).cloned().unwrap_or_else(|| oa_params::Gradient::solid([1.0; 4]));
            let mut fill = gradient(schema::TEXT_COLOR);
            // Projects from before gradients: a top-to-bottom two-color switch.
            let legacy = |id: &str| item.params.get(id).map(|s| s.eval(ctx));
            if let (Some(Value::Bool(true)), Some(Value::Color(to))) = (legacy(schema::TEXT_GRADIENT), legacy(schema::TEXT_COLOR2))
                && fill.stops.len() == 1
            {
                fill = oa_params::Gradient::two(90.0, fill.stops[0].color, to);
            }
            let outline = values.float(schema::TEXT_OUTLINE).max(0.0);
            // Layout: see `oa_gpu::text::STYLE_LEN`.
            let mut style = Vec::with_capacity(2 * oa_params::Gradient::PACKED_LEN + 1);
            style.extend(fill.pack());
            style.extend(gradient(schema::TEXT_OUTLINE_COLOR).pack());
            style.push((outline * raster) as f32);
            (style, outline)
        };
        let (style, mut outline) = style_of(&values);
        let spoken_style = match &spoken_values {
            Some(v) => {
                let (style, o) = style_of(v);
                outline = outline.max(o);
                style
            }
            None => Vec::new(),
        };

        // How far letters may leave the box, in ems (a little always, for rotation/scale).
        let mut reach_em = 0.3;
        let mut any_spoken = spoken_values.is_some();
        let mut chain = Vec::new();
        for fx in item.active_effects().iter().filter(|e| e.enabled) {
            let Some(d) = self.registry.effect(&fx.type_id).cloned() else { continue };
            if !d.kind.text_only() {
                continue;
            }
            let Some(clock) = fx.role.clock(ctx.clip_time, item.range.duration) else { continue };
            let clock = if fx.role == oa_doc::EffectRole::Passive && !d.time_varying {
                oa_graph::registry::STILL_CLOCK
            } else {
                [clock.visibility as f32, clock.progress as f32, clock.seconds as f32]
            };
            let values = fx.params.eval(&d.params, None, ctx);
            let alt = spoken(&fx.params, &d.params, &values, ctx);
            for v in std::iter::once(&values).chain(alt.as_ref()) {
                match &d.kind {
                    oa_graph::EffectKind::Glyph { expand: Some(p) } => reach_em += v.float(p.as_str()).abs() * 1.1,
                    // Boxes behind the letters reach past them by their padding.
                    oa_graph::EffectKind::TextBox => reach_em += 1.0,
                    _ => {}
                }
            }
            let pack = |v: &oa_params::Evaluated| {
                let mut u = d.pack_uniforms(v, raster);
                u.extend(clock);
                u
            };
            any_spoken |= alt.is_some();
            chain.push(oa_graph::TextEffect { type_id: d.type_id.clone(), version: d.version, uniforms: pack(&values), spoken: alt.as_ref().map(pack).unwrap_or_default() });
        }
        // Which word is being spoken — only worked out when something changes for it,
        // so other titles' frames (and cache keys) don't depend on it.
        let spoken_word = if any_spoken {
            item.spoken_word(ctx.clip_time, spec.content.split_whitespace().count()).map_or(-1.0, |w| w as f32)
        } else {
            -1.0
        };
        let margin = (outline + reach_em * spec.size) * raster + 2.0;
        let bounds = bounds.expand(margin);
        let op = NodeOp::Text { spec: std::sync::Arc::new(spec), scale: raster, style, spoken_style, spoken_word, chain };
        (self.b.add(op, vec![], bounds, false), bounds)
    }

    fn layer(
        &mut self,
        item: &Item,
        variant: &oa_doc::FormatVariant,
        t: Time,
        out_scale: f64,
        depth: usize,
    ) -> Option<(NodeId, LayerInfo)> {
        let ctx = item.eval_context(t);
        let overrides = variant.overrides.get(&item.id);
        let vis = item.params.eval(schema::visual(), overrides, &ctx);
        let motion = self.motion(item, variant, &ctx);
        let opacity = (vis.float(schema::OPACITY) * motion.opacity).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return None;
        }

        // Native size and resolution limit of the layer's content.
        let (native, max_raster) = match scene::layer_native(self.project, item, variant, &ctx, depth < MAX_NESTING) {
            Ok(n) => n,
            Err(scene::Unplaceable::MediaWithoutInfo) => {
                self.report.media_without_info.push(item.id);
                return None;
            }
            Err(scene::Unplaceable::Unsupported) => {
                self.report.unsupported_items.push(item.id);
                return None;
            }
        };

        let base = scene::reframe(item, &vis, native, variant);
        let to_canvas = scene::user_transform(&vis, native, &base, variant);
        // Motion effects move the whole layer around its anchor, after its own transform.
        let to_canvas = if motion == Motion::NONE {
            to_canvas
        } else {
            let anchor = vis.vec2(schema::ANCHOR);
            let p = to_canvas.apply([anchor[0] * native[0], anchor[1] * native[1]]);
            to_canvas
                .then(&Affine2::translate(-p[0], -p[1]))
                .then(&Affine2::scale(motion.scale, motion.scale))
                .then(&Affine2::rotate_degrees(motion.rotation))
                .then(&Affine2::translate(p[0] + motion.offset[0], p[1] + motion.offset[1]))
        };
        let full = to_canvas.then(&Affine2::scale(out_scale, out_scale));

        let pixelated = match item.kind {
            ItemKind::Media { media } => self.project.media(media).is_some_and(|m| m.pixelated()),
            _ => false,
        };
        // Pixel art is blown up to the size it's shown at *before* its effects run, so a
        // 16×16 sprite at 10× is a 160×160 picture of crisp blocks and the effects work
        // on all of those pixels. The enlargement itself is nearest-neighbor, so the
        // blocks stay square; everything after it is ordinary rendering.
        let raster = if pixelated {
            // The exact scale (in quarter steps, so a slow zoom doesn't resize the
            // texture every frame), never below 1:1 and never past a sane texture size.
            let want = (full.max_axis_scale() * 4.0).round().max(4.0) / 4.0;
            let room = (MAX_PIXEL_ART_SIDE / native[0].max(native[1]).max(1.0)).max(1.0);
            want.min(room)
        } else {
            quantize_scale(full.max_axis_scale(), max_raster)
        };
        let raster_size = [native[0] * raster, native[1] * raster];
        let mut bounds = Rect::from_size(raster_size[0], raster_size[1]);

        let mut node = match &item.kind {
            ItemKind::Media { media } => {
                let m = self.project.media(*media).expect("checked above");
                let rep = if self.opts.use_proxies { Representation::Proxy } else { Representation::Original };
                // Pixel art decodes at its own size and is enlarged below; everything
                // else decodes straight to the raster size.
                let decode = if pixelated { 1.0 } else { raster };
                let size = if pixelated { native } else { raster_size };
                let op = NodeOp::Source {
                    media: media.0,
                    fingerprint: m.fingerprint.as_deref().map(Into::into),
                    source_time: ctx.source_time,
                    rep,
                    decode_scale: decode,
                    size: [size[0].round() as u32, size[1].round() as u32],
                    yuv: yuv_override(m),
                };
                let src = self.b.add(op, vec![], Rect::from_size(size[0], size[1]), true);
                let src = self.input_color(m, src);
                if !pixelated || raster <= 1.0 {
                    src
                } else {
                    let out = [raster_size[0].round().max(1.0) as u32, raster_size[1].round().max(1.0) as u32];
                    let matrix = Affine2::scale(out[0] as f64 / native[0], out[1] as f64 / native[1]);
                    let placed = self.b.add(NodeOp::Transform { matrix }, vec![src], matrix.map_rect(Rect::from_size(native[0], native[1])), true);
                    let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: true }];
                    self.b.add(NodeOp::Composite { size: out, background: [0.0; 4], layers }, vec![placed], bounds, true)
                }
            }
            ItemKind::Solid => {
                let color = match item.params.eval(schema::solid(), overrides, &ctx).get(schema::SOLID_COLOR) {
                    Some(Value::Color(c)) => c.map(|x| x as f32),
                    _ => [0.0, 0.0, 0.0, 1.0],
                };
                self.b.add(NodeOp::Solid { color, size: raster_size }, vec![], bounds, color[3] >= 1.0)
            }
            ItemKind::Nested { sequence } => self.sequence(*sequence, None, ctx.source_time, raster, depth + 1),
            ItemKind::Text => {
                let (node, grown) = self.text(item, variant, &ctx, raster, bounds);
                bounds = grown;
                node
            }
            _ => unreachable!("filtered above"),
        };

        // The clip's crop: the cut-away edges go transparent before any effect sees them.
        let crop = schema::CROPS.map(|c| vis.float(c).clamp(0.0, 1.0) as f32);
        if crop.iter().any(|c| *c > 0.0)
            && let Some(d) = self.registry.effect(oa_graph::registry::CROP).cloned()
        {
            let mut uniforms = crop.to_vec();
            uniforms.extend([raster_size[0] as f32, raster_size[1] as f32]);
            uniforms.extend(oa_graph::registry::STILL_CLOCK);
            let op = NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms, nearest: pixelated };
            node = self.b.add(op, vec![node], bounds, false);
        }

        let (node, bounds) = self.effect_chain(item, &ctx, node, bounds, raster, raster_size, pixelated, depth);

        let matrix = Affine2::scale(1.0 / raster, 1.0 / raster).then(&full);
        let opaque = self.b.node(node).opaque && matrix.is_axis_aligned();
        let node = self.b.add(NodeOp::Transform { matrix }, vec![node], matrix.map_rect(bounds), opaque);
        // Pixel art (or media set to pixel scaling) stays crisp when enlarged.
        Some((node, LayerInfo { opacity: opacity as f32, blend: blend_of(&vis), pixelated }))
    }
}

/// A clip's (or container's) blend mode, from its evaluated visual params.
fn blend_of(vis: &oa_params::Evaluated) -> BlendMode {
    vis.get(schema::BLEND).and_then(|v| v.as_enum()).map_or(BlendMode::Normal, BlendMode::from_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_quantization() {
        assert_eq!(quantize_scale(1.0, 1.0), 1.0);
        assert_eq!(quantize_scale(0.3, 1.0), 0.5);
        assert_eq!(quantize_scale(0.26, 1.0), 0.5);
        assert_eq!(quantize_scale(0.25, 1.0), 0.25);
        assert_eq!(quantize_scale(3.0, 1.0), 1.0);
        assert_eq!(quantize_scale(3.0, 4.0), 4.0);
        assert_eq!(quantize_scale(0.0, 1.0), 1.0 / 64.0);
    }
}
