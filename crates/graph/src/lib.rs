//! Per-frame render graph: the pure, internal description of one frame.
//!
//! * [`NodeOp`] is internal and may change at any time (plugins use [`registry`]).
//! * Every node gets a [`CacheKey`] from an explicit contract: only the fields listed
//!   in [`GraphBuilder::add`] plus [`KeyContext`] feed the key. A node whose output
//!   isn't reproducible (stateful effect, media without a fingerprint) gets `None`,
//!   and so does everything downstream of it.
//! * [`optimize`] rewrites a graph; [`OptLevel::Reference`] skips all rewrites and is
//!   the ground truth optimized output is tested against.

mod geom;
mod optimize;
pub mod plugin;
pub mod registry;

pub use geom::{Affine2, Rect};
pub use optimize::{optimize, OptLevel};
pub use registry::{EffectKind, EffectShader, Statefulness, WorkingSpace};

use oa_time::Time;
use std::fmt::Write as _;
use std::sync::Arc;
use xxhash_rust::xxh3::Xxh3;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(pub u128);

impl std::fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:08x}", (self.0 >> 96) as u32)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Representation {
    Original,
    Proxy,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
    /// The darker of the layer and what's under it, per channel.
    Darken,
    /// The lighter of the layer and what's under it, per channel.
    Lighten,
}

impl BlendMode {
    pub const ALL: [BlendMode; 6] = [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen, BlendMode::Darken, BlendMode::Lighten];

    /// The name stored in documents (`schema::BLEND`).
    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Add => "add",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
        }
    }

    /// From a stored name; anything unknown is `Normal`.
    pub fn from_name(name: &str) -> BlendMode {
        BlendMode::ALL.into_iter().find(|b| b.name() == name).unwrap_or(BlendMode::Normal)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LayerInfo {
    pub opacity: f32,
    pub blend: BlendMode,
    /// Sample the layer with nearest-neighbor filtering (crisp pixel art) instead of
    /// smoothly.
    pub pixelated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeOp {
    /// Decoded media at `decode_scale` of its native size.
    Source {
        media: u64,
        fingerprint: Option<Arc<str>>,
        source_time: Time,
        rep: Representation,
        decode_scale: f64,
        size: [u32; 2],
        /// YCbCr → RGB overrides for video: `[matrix, range]`, 0 = the file's own tags
        /// (matrix 1 = BT.601, 2 = BT.709, 3 = BT.2020; range 1 = limited, 2 = full).
        yuv: [u8; 2],
    },
    Solid { color: [f32; 4], size: [f64; 2] },
    Effect {
        type_id: Arc<str>,
        version: u32,
        kind: EffectKind,
        space: WorkingSpace,
        fusible: bool,
        stateful: bool,
        uniforms: Vec<f32>,
        /// Read the input without smoothing: the layer is pixel art, so a warp or a
        /// blur works on whole picture pixels rather than blends between them.
        nearest: bool,
    },
    /// Created by the optimizer: several point ops in one shader pass.
    FusedPointOps { space: WorkingSpace, chain: Vec<(Arc<str>, u32, Vec<f32>)>, nearest: bool },
    Transform { matrix: Affine2 },
    /// Inputs are layers, bottom to top, one [`LayerInfo`] each.
    Composite { size: [u32; 2], background: [f32; 4], layers: Vec<LayerInfo> },
    /// Mixes two canvas-sized inputs (`[from, to]`) with a transition shader at
    /// `progress` (0 = all `from`, 1 = all `to`).
    Transition { type_id: Arc<str>, version: u32, uniforms: Vec<f32>, progress: f32 },
    /// A text layer's glyphs, drawn on the GPU from distance fields into the node's
    /// bounds. `spec` is laid out at canvas size and drawn `scale` times larger; `style`
    /// is the fill/outline block (see `oa-gpu`'s text pass); `chain` the per-letter and
    /// per-pixel text effects in order, each (type, version, uniforms incl. clock).
    /// A text layer drawn in one pass (see `oa_gpu::text`). The word being spoken
    /// (`spoken_word`, −1 for none) is drawn with `spoken_style` and each effect's
    /// `spoken` uniforms — "highlight when spoken".
    Text { spec: Arc<oa_text::TextSpec>, scale: f64, style: Vec<f32>, spoken_style: Vec<f32>, spoken_word: f32, chain: Vec<TextEffect> },
}

/// One text effect in a text pass: its packed params (and clock), and the ones it uses on
/// the word being spoken (empty: the same).
#[derive(Clone, Debug, PartialEq)]
pub struct TextEffect {
    pub type_id: Arc<str>,
    pub version: u32,
    pub uniforms: Vec<f32>,
    pub spoken: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub op: NodeOp,
    pub inputs: Vec<NodeId>,
    /// Region with possibly non-transparent pixels, in this node's pixel space.
    pub bounds: Rect,
    /// Every pixel inside `bounds` is fully opaque.
    pub opaque: bool,
    pub key: Option<CacheKey>,
}

/// Inputs that affect every node's output but aren't stored in the node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct KeyContext {
    /// Identifies the color pipeline (working space, OCIO config, display transform).
    pub color_config: u64,
    /// Bumped whenever a rendering-affecting engine change ships.
    pub engine_revision: u32,
}

impl Default for KeyContext {
    fn default() -> Self {
        KeyContext { color_config: 0, engine_revision: 1 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub output: NodeId,
    /// How much earlier than the requested time rendering must start (stateful nodes).
    pub preroll: Time,
}

impl Graph {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// Nodes reachable from the output.
    pub fn live_count(&self) -> usize {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![self.output];
        let mut n = 0;
        while let Some(id) = stack.pop() {
            if std::mem::replace(&mut seen[id.0 as usize], true) {
                continue;
            }
            n += 1;
            stack.extend(&self.node(id).inputs);
        }
        n
    }

    /// Indented tree view for debugging and the CLI.
    pub fn describe(&self) -> String {
        let mut s = String::new();
        self.describe_node(self.output, 0, &mut s);
        s
    }

    fn describe_node(&self, id: NodeId, depth: usize, s: &mut String) {
        let n = self.node(id);
        let pad = "  ".repeat(depth);
        let op = match &n.op {
            NodeOp::Source { media, source_time, rep, decode_scale, size, .. } => {
                format!("Source media#{media} @{source_time:?} {rep:?} {}x{} decode×{decode_scale}", size[0], size[1])
            }
            NodeOp::Solid { color, size } => format!("Solid {color:?} {}x{}", size[0], size[1]),
            NodeOp::Effect { type_id, uniforms, stateful, .. } => {
                format!("Effect {type_id} {uniforms:?}{}", if *stateful { " [stateful]" } else { "" })
            }
            NodeOp::FusedPointOps { chain, space, .. } => {
                let names: Vec<_> = chain.iter().map(|c| &*c.0).collect();
                format!("FusedPointOps({space:?}) {}", names.join(" → "))
            }
            NodeOp::Transform { matrix } => {
                let m = matrix.m.map(|x| (x * 1000.0).round() / 1000.0);
                format!("Transform {m:?}")
            }
            NodeOp::Composite { size, layers, .. } => format!("Composite {}x{} ({} layers)", size[0], size[1], layers.len()),
            NodeOp::Transition { type_id, progress, .. } => format!("Transition {type_id} @{progress:.3}"),
            NodeOp::Text { spec, scale, chain, .. } => {
                let names: Vec<_> = chain.iter().map(|c| &*c.type_id).collect();
                format!("Text {:?} {}px×{scale} [{}]", spec.content, spec.size, names.join(" → "))
            }
        };
        let b = n.bounds;
        let key = n.key.map_or("uncached".to_string(), |k| format!("{k:?}"));
        let _ = writeln!(
            s,
            "{pad}{op}  bounds=[{:.0},{:.0} {:.0}x{:.0}]{} key={key}",
            b.x0,
            b.y0,
            b.x1 - b.x0,
            b.y1 - b.y0,
            if n.opaque { " opaque" } else { "" }
        );
        for &i in &n.inputs {
            self.describe_node(i, depth + 1, s);
        }
    }
}

pub struct GraphBuilder {
    nodes: Vec<Node>,
    ctx: KeyContext,
    preroll: Time,
}

impl GraphBuilder {
    pub fn new(ctx: KeyContext) -> Self {
        GraphBuilder { nodes: Vec::new(), ctx, preroll: Time::ZERO }
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn require_preroll(&mut self, preroll: Time) {
        self.preroll = self.preroll.max(preroll);
    }

    /// Adds a node. Inputs must already exist, so graphs are always acyclic.
    pub fn add(&mut self, op: NodeOp, inputs: Vec<NodeId>, bounds: Rect, opaque: bool) -> NodeId {
        let key = self.key_for(&op, &inputs);
        self.nodes.push(Node { op, inputs, bounds, opaque, key });
        NodeId(self.nodes.len() as u32 - 1)
    }

    pub fn finish(self, output: NodeId) -> Graph {
        Graph { nodes: self.nodes, output, preroll: self.preroll }
    }

    /// The cache-key contract. Adding a field to a `NodeOp` without adding it here is
    /// a stale-cache bug; `tests::every_field_changes_the_key` guards the common ones.
    fn key_for(&self, op: &NodeOp, inputs: &[NodeId]) -> Option<CacheKey> {
        let mut h = Xxh3::new();
        let f64s = |h: &mut Xxh3, xs: &[f64]| {
            for x in xs {
                let bits = if *x == 0.0 { 0 } else if x.is_nan() { f64::NAN.to_bits() } else { x.to_bits() };
                h.update(&bits.to_le_bytes());
            }
        };
        let f32s = |h: &mut Xxh3, xs: &[f32]| {
            for x in xs {
                let bits = if *x == 0.0 { 0 } else if x.is_nan() { f32::NAN.to_bits() } else { x.to_bits() };
                h.update(&bits.to_le_bytes());
            }
        };
        let text = |h: &mut Xxh3, s: &str| {
            h.update(&(s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        };
        h.update(&self.ctx.color_config.to_le_bytes());
        h.update(&self.ctx.engine_revision.to_le_bytes());
        match op {
            NodeOp::Source { fingerprint, source_time, rep, decode_scale, size, yuv, .. } => {
                // Keyed by content fingerprint, never by path or media id.
                text(&mut h, fingerprint.as_ref()?);
                h.update(&[0]);
                h.update(yuv);
                h.update(&source_time.0.to_le_bytes());
                h.update(&[*rep as u8]);
                f64s(&mut h, &[*decode_scale]);
                h.update(&size[0].to_le_bytes());
                h.update(&size[1].to_le_bytes());
            }
            NodeOp::Solid { color, size } => {
                h.update(&[1]);
                f32s(&mut h, color);
                f64s(&mut h, size);
            }
            NodeOp::Effect { type_id, version, stateful, uniforms, space, nearest, .. } => {
                if *stateful {
                    return None;
                }
                h.update(&[2]);
                text(&mut h, type_id);
                h.update(&version.to_le_bytes());
                h.update(&[*space as u8, *nearest as u8]);
                f32s(&mut h, uniforms);
            }
            NodeOp::FusedPointOps { space, chain, nearest } => {
                h.update(&[3, *space as u8, *nearest as u8]);
                for (id, version, u) in chain {
                    text(&mut h, id);
                    h.update(&version.to_le_bytes());
                    h.update(&(u.len() as u64).to_le_bytes());
                    f32s(&mut h, u);
                }
            }
            NodeOp::Transform { matrix } => {
                h.update(&[4]);
                f64s(&mut h, &matrix.m);
            }
            NodeOp::Composite { size, background, layers } => {
                h.update(&[5]);
                h.update(&size[0].to_le_bytes());
                h.update(&size[1].to_le_bytes());
                f32s(&mut h, background);
                for l in layers {
                    f32s(&mut h, &[l.opacity]);
                    h.update(&[l.blend as u8]);
                    h.update(&[l.pixelated as u8]);
                }
            }
            NodeOp::Transition { type_id, version, uniforms, progress } => {
                h.update(&[6]);
                text(&mut h, type_id);
                h.update(&version.to_le_bytes());
                f32s(&mut h, uniforms);
                f32s(&mut h, &[*progress]);
            }
            NodeOp::Text { spec, scale, style, spoken_style, spoken_word, chain } => {
                h.update(&[7]);
                f32s(&mut h, &[*spoken_word]);
                h.update(&(spoken_style.len() as u64).to_le_bytes());
                f32s(&mut h, spoken_style);
                text(&mut h, &spec.content);
                text(&mut h, &spec.family);
                h.update(&[spec.bold as u8, spec.italic as u8, spec.align as u8]);
                f64s(&mut h, &[spec.size, spec.tracking, spec.line_height, *scale]);
                h.update(&(style.len() as u64).to_le_bytes());
                f32s(&mut h, style);
                for fx in chain {
                    text(&mut h, &fx.type_id);
                    h.update(&fx.version.to_le_bytes());
                    h.update(&(fx.uniforms.len() as u64).to_le_bytes());
                    f32s(&mut h, &fx.uniforms);
                    h.update(&(fx.spoken.len() as u64).to_le_bytes());
                    f32s(&mut h, &fx.spoken);
                }
            }
        }
        for i in inputs {
            h.update(&self.node(*i).key?.0.to_le_bytes());
        }
        Some(CacheKey(h.digest128()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(b: &mut GraphBuilder, fp: Option<&str>, t: i64) -> NodeId {
        b.add(
            NodeOp::Source {
                media: 1,
                fingerprint: fp.map(Into::into),
                source_time: Time(t),
                rep: Representation::Original,
                decode_scale: 1.0,
                size: [100, 100],
                yuv: [0, 0],
            },
            vec![],
            Rect::from_size(100.0, 100.0),
            true,
        )
    }

    fn effect(b: &mut GraphBuilder, input: NodeId, stateful: bool, u: f32) -> NodeId {
        b.add(
            NodeOp::Effect {
                type_id: "x".into(),
                version: 1,
                kind: EffectKind::PointOp,
                space: WorkingSpace::Linear,
                fusible: true,
                stateful,
                uniforms: vec![u],
                nearest: false,
            },
            vec![input],
            Rect::from_size(100.0, 100.0),
            false,
        )
    }

    #[test]
    fn every_field_changes_the_key() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s1 = source(&mut b, Some("abc"), 0);
        let s1b = source(&mut b, Some("abc"), 0);
        let s2 = source(&mut b, Some("abc"), 1);
        let s3 = source(&mut b, Some("abd"), 0);
        let k = |b: &GraphBuilder, n| b.node(n).key.unwrap();
        assert_eq!(k(&b, s1), k(&b, s1b));
        assert_ne!(k(&b, s1), k(&b, s2));
        assert_ne!(k(&b, s1), k(&b, s3));
        let e1 = effect(&mut b, s1, false, 0.0);
        let e1neg = effect(&mut b, s1, false, -0.0);
        let e2 = effect(&mut b, s1, false, 1.0);
        let e3 = effect(&mut b, s2, false, 0.0);
        assert_eq!(k(&b, e1), k(&b, e1neg));
        assert_ne!(k(&b, e1), k(&b, e2));
        assert_ne!(k(&b, e1), k(&b, e3));

        let mut other = GraphBuilder::new(KeyContext { color_config: 7, ..Default::default() });
        let o = source(&mut other, Some("abc"), 0);
        assert_ne!(k(&b, s1), other.node(o).key.unwrap());
    }

    #[test]
    fn uncacheable_nodes_poison_downstream() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let no_fp = source(&mut b, None, 0);
        assert!(b.node(no_fp).key.is_none());
        let downstream = effect(&mut b, no_fp, false, 1.0);
        assert!(b.node(downstream).key.is_none());
        let s = source(&mut b, Some("abc"), 0);
        let st = effect(&mut b, s, true, 1.0);
        let after = effect(&mut b, st, false, 1.0);
        assert!(b.node(st).key.is_none() && b.node(after).key.is_none());
    }
}
