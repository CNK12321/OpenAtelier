//! Graph rewrites. Each pass must be *invisible*: an optimized graph renders the same
//! image as the reference graph (up to resampling differences that the reference-path
//! image tests tolerate).
//!
//! Passes:
//! * **Occlusion/visibility cull** in composites: drop layers under a full-canvas
//!   opaque layer, fully transparent layers, and layers outside the canvas.
//! * **Transform merge**: directly chained transforms become one matrix (one
//!   resample). Never merged across effects — effects run in layer space.
//! * **Point-op fusion**: chains of fusible point ops in the same working space become
//!   one [`NodeOp::FusedPointOps`] pass.
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
            NodeOp::Composite { size, background, layers } => {
                let canvas = Rect::from_size(size[0] as f64, size[1] as f64);
                let mut keep: Vec<usize> = Vec::new();
                for (i, (l, &input)) in layers.iter().zip(&node.inputs).enumerate().rev() {
                    let n = self.old.node(input);
                    if l.opacity <= 0.0 || n.bounds.is_empty() || !n.bounds.intersects(&canvas) {
                        continue;
                    }
                    keep.push(i);
                    if n.opaque && l.opacity >= 1.0 && l.blend == BlendMode::Normal && n.bounds.contains_rect(&canvas) {
                        break; // everything below is hidden
                    }
                }
                keep.reverse();
                let inputs = keep.iter().map(|&i| self.visit(node.inputs[i])).collect();
                let layers = keep.iter().map(|&i| layers[i]).collect();
                let op = NodeOp::Composite { size: *size, background: *background, layers };
                self.out.add(op, inputs, node.bounds, node.opaque)
            }
            _ => self.copy(id),
        }
    }
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
}
