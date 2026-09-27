//! Region-of-interest propagation: which part of each node's output the frame actually
//! uses, worked out from the output back.
//!
//! A node's `bounds` is everything it *could* draw; often only part of that is seen — a
//! clip scaled up past the canvas, one sliding in from off screen, a compound clip shown
//! cropped. The executor renders just the needed part of the nodes it can (composites,
//! point ops) and everything downstream reads the same pixels it would have.
//!
//! Needs travel through the ops whose reach is known exactly:
//! * **Composite**: each layer needs what of it lands inside the composite's own need
//!   (through its placement when it's a transform), plus a pixel for filtering.
//! * **Transform** (drawn on its own): the same, through its matrix.
//! * **Point ops** (fused or not): each pixel reads only itself, so the input's need is
//!   the node's.
//!
//! Everything else — neighborhood effects, warps, transitions, text, anything with a
//! second input — asks for all of its inputs: their reach isn't declared precisely
//! enough to cut them short safely.

use crate::{EffectKind, Graph, NodeId, NodeOp, Rect};

/// The region of each node's output (by index) the frame reads, in that node's pixel
/// space; `None` for nodes nothing reads. The output needs all of its bounds.
pub fn regions_needed(graph: &Graph) -> Vec<Option<Rect>> {
    let mut need: Vec<Option<Rect>> = vec![None; graph.nodes.len()];
    need[graph.output.0 as usize] = Some(graph.node(graph.output).bounds);
    // Inputs always come before the nodes using them: walking back from the end sees
    // every consumer of a node before the node itself.
    for i in (0..graph.nodes.len()).rev() {
        let Some(wanted) = need[i] else { continue };
        let node = &graph.nodes[i];
        let mut ask = |input: NodeId, region: Rect| {
            let bounds = graph.node(input).bounds;
            let region = region.intersect(&bounds);
            // Nothing of it is seen: still something to draw (1 px), never a zero-size
            // target; the optimizer usually culls these first.
            let region = if region.is_empty() { Rect::new(bounds.x0, bounds.y0, bounds.x0 + 1.0, bounds.y0 + 1.0).intersect(&bounds) } else { region };
            let slot = &mut need[input.0 as usize];
            *slot = Some(match *slot {
                Some(already) => already.union(&region),
                None => region,
            });
        };
        match &node.op {
            NodeOp::Composite { .. } => {
                for &layer in &node.inputs {
                    match &graph.node(layer).op {
                        // Drawn straight from the transform's input (the executor places it).
                        NodeOp::Transform { matrix } => {
                            let below = graph.node(layer).inputs[0];
                            match matrix.invert() {
                                Some(back) => ask(below, back.map_rect(wanted).expand(1.0)),
                                None => ask(below, graph.node(below).bounds),
                            }
                        }
                        _ => ask(layer, wanted.expand(1.0)),
                    }
                }
            }
            NodeOp::Transform { matrix } => match matrix.invert() {
                Some(back) => ask(node.inputs[0], back.map_rect(wanted).expand(1.0)),
                None => ask(node.inputs[0], graph.node(node.inputs[0]).bounds),
            },
            NodeOp::Effect { kind: EffectKind::PointOp, .. } | NodeOp::FusedPointOps { .. } if node.inputs.len() == 1 => ask(node.inputs[0], wanted),
            _ => {
                for &input in &node.inputs {
                    ask(input, graph.node(input).bounds);
                }
            }
        }
    }
    need
}

/// `region` snapped outward onto the pixel grid that starts at `origin`, and kept inside
/// `within` (the image being read): the part of a pass worth running, on the same pixels
/// a full pass would have.
pub fn snap_region(region: Rect, origin: [f64; 2], within: Rect) -> Rect {
    let r = region.intersect(&within);
    let snap = Rect::new(
        origin[0] + (r.x0 - origin[0]).floor(),
        origin[1] + (r.y0 - origin[1]).floor(),
        origin[0] + (r.x1 - origin[0]).ceil(),
        origin[1] + (r.y1 - origin[1]).ceil(),
    );
    snap.intersect(&within)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Affine2, BlendMode, GraphBuilder, KeyContext, LayerInfo, Representation, WorkingSpace};
    use oa_time::Time;

    fn src(b: &mut GraphBuilder, w: f64, h: f64) -> NodeId {
        b.add(
            NodeOp::Source { media: 1, fingerprint: Some("fp".into()), source_time: Time::ZERO, rep: Representation::Original, decode_scale: 1.0, size: [w as u32, h as u32], yuv: [0, 0] },
            vec![],
            Rect::from_size(w, h),
            true,
        )
    }

    fn effect(b: &mut GraphBuilder, input: NodeId, kind: EffectKind) -> NodeId {
        let bounds = b.node(input).bounds;
        let op = NodeOp::Effect { type_id: "x".into(), version: 1, kind, space: WorkingSpace::Linear, fusible: true, stateful: false, uniforms: vec![], nearest: false };
        b.add(op, vec![input], bounds, false)
    }

    fn comp(b: &mut GraphBuilder, size: f64, layers: &[NodeId]) -> NodeId {
        let infos = layers.iter().map(|_| LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }).collect();
        b.add(NodeOp::Composite { size: [size as u32, size as u32], background: [0.0; 4], layers: infos }, layers.to_vec(), Rect::from_size(size, size), false)
    }

    /// A clip scaled up 4× on a 100 px canvas shows a quarter of itself each way: its
    /// point ops only need that part (plus a pixel for filtering); a blur needs it all.
    #[test]
    fn a_zoomed_in_clip_needs_only_what_shows() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = src(&mut b, 100.0, 100.0);
        let blur = effect(&mut b, s, EffectKind::Spatial { expand: None });
        let exposure = effect(&mut b, blur, EffectKind::PointOp);
        let m = Affine2::translate(-50.0, -50.0).then(&Affine2::scale(4.0, 4.0)).then(&Affine2::translate(50.0, 50.0));
        let t = b.add(NodeOp::Transform { matrix: m }, vec![exposure], m.map_rect(Rect::from_size(100.0, 100.0)), false);
        let out = comp(&mut b, 100.0, &[t]);
        let g = b.finish(out);
        let need = regions_needed(&g);
        assert_eq!(need[out.0 as usize], Some(Rect::from_size(100.0, 100.0)));
        let seen = need[exposure.0 as usize].unwrap();
        // 37.5–62.5 shows; a pixel of the clip's own either side for filtering.
        assert!((seen.x0 - 36.5).abs() < 1e-9 && (seen.x1 - 63.5).abs() < 1e-9, "{seen:?}");
        assert_eq!(need[blur.0 as usize], Some(seen), "a point op reads only its own pixels");
        assert_eq!(need[s.0 as usize], Some(Rect::from_size(100.0, 100.0)), "a blur reads around them: all of its input");
        assert_eq!(need[t.0 as usize], None, "a transform drawn into its composite isn't a pass");
    }

    /// Two layers on one input ask for the union of what each shows.
    #[test]
    fn shared_inputs_need_the_union() {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = src(&mut b, 200.0, 100.0);
        let left = b.add(NodeOp::Transform { matrix: Affine2::IDENTITY }, vec![s], Rect::from_size(200.0, 100.0), true);
        let m = Affine2::translate(-150.0, 0.0);
        let right = b.add(NodeOp::Transform { matrix: m }, vec![s], m.map_rect(Rect::from_size(200.0, 100.0)), true);
        let out = comp(&mut b, 40.0, &[left, right]);
        let need = regions_needed(&b.finish(out));
        let r = need[s.0 as usize].unwrap();
        assert_eq!((r.x0, r.x1), (0.0, 191.0), "0–41 from one, 149–191 from the other: {r:?}");
    }

    #[test]
    fn regions_snap_onto_the_input_grid() {
        let r = snap_region(Rect::new(10.3, 5.5, 20.2, 9.0), [0.5, 0.0], Rect::new(0.5, 0.0, 100.5, 50.0));
        assert_eq!(r, Rect::new(9.5, 5.0, 20.5, 9.0));
        let clipped = snap_region(Rect::new(-10.0, -10.0, 5.0, 5.0), [0.0, 0.0], Rect::from_size(50.0, 50.0));
        assert_eq!(clipped, Rect::new(0.0, 0.0, 5.0, 5.0));
    }
}
