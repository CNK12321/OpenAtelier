//! Graph rewrites. Each pass must be *invisible*: an optimized graph renders the same
//! image as the reference graph (up to resampling differences that the reference-path
//! image tests tolerate).
//!
//! Passes:
//! * **Occlusion/visibility cull** in composites: drop layers under a full-canvas
//!   opaque layer, fully transparent layers, and layers outside the canvas. A composite
//!   one of whose layers covers it opaquely counts as opaque itself, so what's under a
//!   cut clip (the blurred-content backdrop) goes too.
//! * **Composite pass-through**: a composite left with one layer that fills it exactly
//!   and opaquely is that layer — a frame that's only a cut clip is copied once, not once
//!   per level of nesting. Not for a source (it may lend a decoder's own texture, which
//!   must be copied before it's reused) or a transform (drawn by the composite).
//! * **Transform merge**: directly chained transforms become one matrix (one
//!   resample). Never merged across effects — effects run in layer space — nor into
//!   the shrink of supersampled (anti-aliased) text.
//! * **Point-op fusion**: chains of fusible point ops in the same working space become
//!   one [`NodeOp::FusedPointOps`] pass.
//! * **UV-warp fusion**: chains of fusible warps that each cover their input's area
//!   become one [`NodeOp::FusedUvWarps`] pass (positions carried back through every
//!   warp, the input read once). The executor goes further and draws a warp straight
//!   into the layer it feeds, when the layer's transform doesn't shrink it past 2×.
//! * Dead nodes disappear because the output graph is rebuilt from the output node.

use crate::{BlendMode, EffectKind, Graph, GraphBuilder, KeyContext, NodeId, NodeOp, Rect};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OptLevel {
    /// No rewrites. The ground truth for testing, and a user-facing fallback.
    Reference,
    Full,
}

pub fn optimize(graph: &Graph, level: OptLevel, ctx: KeyContext) -> Graph {
    if level == OptLevel::Reference {
        return graph.clone();
    }
    let mut r = Rewriter { old: graph, memo: vec![None; graph.nodes.len()], out: GraphBuilder::new(ctx) };
    let output = r.visit(graph.output);
    let mut g = r.out.finish(output);
    g.preroll = graph.preroll;
    g
}

struct Rewriter<'a> {
    old: &'a Graph,
    memo: Vec<Option<NodeId>>,
    out: GraphBuilder,
}

impl Rewriter<'_> {
    /// A warp that reads one input and covers exactly its area — what warps can be
    /// chained (and drawn straight into a layer) on: each one's positions are the next
    /// one's.
    fn warp_in_place(&self, id: NodeId) -> bool {
        let node = self.old.node(id);
        node.inputs.len() == 1 && same_area(node.bounds, self.old.node(node.inputs[0]).bounds)
    }

    /// Whether `id` paints every pixel of `area` fully opaque: its own flag, or a
    /// composite with an opaque background or a layer that does.
    fn covers(&self, id: NodeId, area: Rect) -> bool {
        let n = self.old.node(id);
        if !n.bounds.contains_rect(&area) {
            return false;
        }
        if n.opaque {
            return true;
        }
        match &n.op {
            NodeOp::Composite { background, layers, .. } => {
                background[3] >= 1.0 || layers.iter().zip(&n.inputs).any(|(l, &input)| l.opacity >= 1.0 && l.blend == BlendMode::Normal && self.covers(input, area))
            }
            _ => false,
        }
    }

    fn visit(&mut self, id: NodeId) -> NodeId {
        if let Some(done) = self.memo[id.0 as usize] {
            return done;
        }
        let new = self.rewrite(id);
        self.memo[id.0 as usize] = Some(new);
        new
    }

    fn copy(&mut self, id: NodeId) -> NodeId {
        let node = self.old.node(id);
        let inputs = node.inputs.iter().map(|&i| self.visit(i)).collect();
        self.out.add(node.op.clone(), inputs, node.bounds, node.opaque)
    }

    fn rewrite(&mut self, id: NodeId) -> NodeId {
        let node = self.old.node(id);
        match &node.op {
            NodeOp::Transform { matrix } => {
                let mut m = *matrix;
                let mut src = node.inputs[0];
                while let NodeOp::Transform { matrix: inner } = &self.old.node(src).op {
                    // Anti-aliased text (drawn larger, then shrunk): the shrink is a pass of
                    // its own, an even average of the larger drawing's pixels — folded
                    // into the layer's placement it would be a single, sparser resample.
                    let supersampled = matches!(self.old.node(self.old.node(src).inputs[0]).op, NodeOp::Text { .. }) && inner.max_axis_scale() < 1.0;
                    if supersampled {
                        break;
                    }
                    m = inner.then(&m);
                    src = self.old.node(src).inputs[0];
                }
                if m.is_identity() {
                    return self.visit(src);
                }
                let input = self.visit(src);
                let opaque = self.out.node(input).opaque && m.is_axis_aligned();
                self.out.add(NodeOp::Transform { matrix: m }, vec![input], node.bounds, opaque)
            }
            NodeOp::Effect { kind: EffectKind::PointOp, fusible: true, stateful: false, space, nearest, .. } => {
                let (space, nearest) = (*space, *nearest);
                let mut chain = Vec::new();
                let mut cur = id;
                loop {
                    match &self.old.node(cur).op {
                        NodeOp::Effect {
                            kind: EffectKind::PointOp,
                            fusible: true,
                            stateful: false,
                            space: s,
                            type_id,
                            version,
                            uniforms,
                            nearest: n,
                        } if *s == space && *n == nearest && self.old.node(cur).inputs.len() == 1 => {
                            chain.push((type_id.clone(), *version, uniforms.clone()));
                            cur = self.old.node(cur).inputs[0];
                        }
                        _ => break,
                    }
                }
                if chain.len() < 2 {
                    return self.copy(id);
                }
                chain.reverse(); // innermost (applied first) first
                let input = self.visit(cur);
                self.out.add(NodeOp::FusedPointOps { space, chain, nearest }, vec![input], node.bounds, node.opaque)
            }
            NodeOp::Effect { kind: EffectKind::UvWarp, fusible: true, stateful: false, nearest, .. } if self.warp_in_place(id) => {
                let nearest = *nearest;
                let mut chain = Vec::new();
                let mut cur = id;
                loop {
                    match &self.old.node(cur).op {
                        NodeOp::Effect { kind: EffectKind::UvWarp, fusible: true, stateful: false, type_id, version, uniforms, nearest: n, .. }
                            if *n == nearest && self.warp_in_place(cur) =>
                        {
                            chain.push((type_id.clone(), *version, uniforms.clone()));
                            cur = self.old.node(cur).inputs[0];
                        }
                        _ => break,
                    }
                }
                if chain.len() < 2 {
                    return self.copy(id);
                }
                chain.reverse(); // innermost (applied first) first
                let input = self.visit(cur);
                self.out.add(NodeOp::FusedUvWarps { chain, nearest }, vec![input], node.bounds, node.opaque)
            }
            NodeOp::Composite { size, background, layers } => {
                let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
                let mut keep: Vec<usize> = Vec::new();
                for (i, (l, &input)) in layers.iter().zip(&node.inputs).enumerate().rev() {
                    let n = self.old.node(input);
                    if l.opacity <= 0.0 || n.bounds.is_empty() || !n.bounds.intersects(&canvas) {
                        continue;
                    }
                    keep.push(i);
                    if l.opacity >= 1.0 && l.blend == BlendMode::Normal && self.covers(input, canvas) {
                        break; // everything below is hidden
                    }
                }
                keep.reverse();
                // One layer filling it exactly and opaquely: it is the composite.
                if let [only] = keep[..] {
                    let (l, input) = (layers[only], node.inputs[only]);
                    let n = self.old.node(input);
                    let own_texture = !matches!(n.op, NodeOp::Source { .. } | NodeOp::Transform { .. });
                    if own_texture && l.opacity >= 1.0 && l.blend == BlendMode::Normal && same_area(n.bounds, canvas) && self.covers(input, canvas) {
                        return self.visit(input);
                    }
                }
                let covered = node.opaque || keep.iter().any(|&i| layers[i].opacity >= 1.0 && layers[i].blend == BlendMode::Normal && self.covers(node.inputs[i], canvas));
                let inputs = keep.iter().map(|&i| self.visit(node.inputs[i])).collect();
                let layers = keep.iter().map(|&i| layers[i]).collect();
                let op = NodeOp::Composite { size: *size, background: *background, layers };
                self.out.add(op, inputs, node.bounds, covered)
            }
            _ => self.copy(id),
        }
    }
}

/// Two regions the same to within a hundredth of a pixel.
pub fn same_area(a: Rect, b: Rect) -> bool {
    [(a.x0, b.x0), (a.y0, b.y0), (a.x1, b.x1), (a.y1, b.y1)].iter().all(|(p, q)| (p - q).abs() < 0.01)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Affine2, LayerInfo, Representation, WorkingSpace};
    use oa_time::Time;

    fn src(b: &mut GraphBuilder, size: f64) -> NodeId {
        b.add(
            NodeOp::Source {
                media: 1,
                fingerprint: Some("fp".into()),
                source_time: Time::ZERO,
                rep: Representation::Original,
                decode_scale: 1.0,
                size: [size as u32, size as u32],
                yuv: [0, 0],
            },
            vec![],
            Rect::from_size(size, size),
            true,
        )
    }

    fn point(b: &mut GraphBuilder, input: NodeId, id: &str, space: WorkingSpace) -> NodeId {
        let bounds = b.node(input).bounds;
        b.add(
            NodeOp::Effect {
                type_id: id.into(),
                version: 1,
                kind: EffectKind::PointOp,
                space,
                fusible: true,
                stateful: false,
                uniforms: vec![1.0],
                nearest: false,
            },
            vec![input],
            bounds,
            true,
        )
    }

    fn xf(b: &mut GraphBuilder, input: NodeId, m: Affine2) -> NodeId {
        let bounds = m.map_rect(b.node(input).bounds);
        let opaque = b.node(input).opaque && m.is_axis_aligned();
        b.add(NodeOp::Transform { matrix: m }, vec![input], bounds, opaque)
    }

    fn comp(b: &mut GraphBuilder, layers: &[(NodeId, f32)]) -> NodeId {
        let infos = layers.iter().map(|&(_, o)| LayerInfo { opacity: o, blend: BlendMode::Normal, pixelated: false }).collect();
        b.add(
            NodeOp::Composite { size: [100, 100], background: [0.0; 4], layers: infos },
            layers.iter().map(|l| l.0).collect(),
            Rect::from_size(100.0, 100.0),
            false,
        )
    }

    #[test]
    fn merges_transforms_and_fuses_point_ops_but_not_across_spaces() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = src(&mut b, 200.0);
        let a = point(&mut b, s, "a", WorkingSpace::Linear);
        let c = point(&mut b, a, "c", WorkingSpace::Linear);
        let d = point(&mut b, c, "d", WorkingSpace::Display);
        let t1 = xf(&mut b, d, Affine2::scale(0.5, 0.5));
        let t2 = xf(&mut b, t1, Affine2::translate(0.0, 0.0));
        let out = comp(&mut b, &[(t2, 1.0)]);
        let g = b.finish(out);

        let o = optimize(&g, OptLevel::Full, KeyContext::default());
        let text = o.describe();
        assert!(text.contains("FusedPointOps(Linear) a → c"), "{text}");
        assert!(text.contains("Effect d"), "{text}");
        assert_eq!(text.matches("Transform").count(), 1, "{text}");
        // source, fused, d, transform, composite
        assert_eq!(o.live_count(), 5);
        assert_eq!(optimize(&g, OptLevel::Reference, KeyContext::default()), g);
    }

    fn warp(b: &mut GraphBuilder, input: NodeId, id: &str, grow: f64) -> NodeId {
        let bounds = b.node(input).bounds.expand(grow);
        let op = NodeOp::Effect { type_id: id.into(), version: 1, kind: EffectKind::UvWarp, space: WorkingSpace::Linear, fusible: true, stateful: false, uniforms: vec![1.0], nearest: false };
        b.add(op, vec![input], bounds, false)
    }

    /// Warps covering their input's area fuse into one pass, innermost first; one that
    /// grows past its input doesn't join (its positions aren't the next one's).
    #[test]
    fn fuses_uv_warps_that_stay_in_place() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = src(&mut b, 100.0);
        let a = warp(&mut b, s, "a", 0.0);
        let c = warp(&mut b, a, "c", 0.0);
        let d = warp(&mut b, c, "d", 0.0);
        let grown = warp(&mut b, d, "grown", 4.0);
        let out = comp(&mut b, &[(grown, 1.0)]);
        let o = optimize(&b.finish(out), OptLevel::Full, KeyContext::default());
        let text = o.describe();
        assert!(text.contains("FusedUvWarps a → c → d"), "{text}");
        assert!(text.contains("Effect grown"), "{text}");
        // source, fused, grown, composite
        assert_eq!(o.live_count(), 4, "{text}");
    }

    #[test]
    fn culls_occluded_transparent_and_offscreen_layers() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let bottom = src(&mut b, 100.0);
        let offscreen = {
            let s = src(&mut b, 10.0);
            xf(&mut b, s, Affine2::translate(500.0, 0.0))
        };
        let full = src(&mut b, 100.0);
        let rotated = {
            let s = src(&mut b, 300.0);
            xf(&mut b, s, Affine2::rotate_degrees(10.0).then(&Affine2::translate(-100.0, -100.0)))
        };
        let invisible = src(&mut b, 100.0);
        let out = comp(&mut b, &[(bottom, 1.0), (offscreen, 1.0), (full, 1.0), (rotated, 1.0), (invisible, 0.0)]);
        let g = b.finish(out);
        let o = optimize(&g, OptLevel::Full, KeyContext::default());
        match &o.node(o.output).op {
            // `full` hides `bottom`; the rotated layer covers the canvas's bounding box
            // but isn't axis-aligned, so it must not hide `full`.
            NodeOp::Composite { layers, .. } => assert_eq!(layers.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    /// A cut clip over the blurred-content backdrop: the clip's composite covers the
    /// frame, so the backdrop goes; the frame is then that composite (the clip copied
    /// once) — never the source itself, which may be a decoder's own texture.
    #[test]
    fn a_covered_backdrop_goes_and_nested_composites_collapse() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let backdrop = src(&mut b, 100.0);
        let backdrop = point(&mut b, backdrop, "a", WorkingSpace::Linear);
        let clip = src(&mut b, 100.0);
        let front = comp(&mut b, &[(clip, 1.0)]);
        let out = comp(&mut b, &[(backdrop, 1.0), (front, 1.0)]);
        let o = optimize(&b.finish(out), OptLevel::Full, KeyContext::default());
        assert_eq!(o.nodes.len(), 2, "{:?}", o.nodes.iter().map(|n| &n.op).collect::<Vec<_>>());
        match &o.node(o.output).op {
            NodeOp::Composite { layers, .. } => assert_eq!(layers.len(), 1),
            other => panic!("{other:?}"),
        }
        // Half see-through, the clip doesn't cover: the backdrop stays.
        let mut b = GraphBuilder::new(KeyContext::default());
        let backdrop = src(&mut b, 100.0);
        let clip = src(&mut b, 100.0);
        let front = comp(&mut b, &[(clip, 0.5)]);
        let out = comp(&mut b, &[(backdrop, 1.0), (front, 1.0)]);
        let o = optimize(&b.finish(out), OptLevel::Full, KeyContext::default());
        match &o.node(o.output).op {
            NodeOp::Composite { layers, .. } => assert_eq!(layers.len(), 2),
            other => panic!("{other:?}"),
        }
    }
}
