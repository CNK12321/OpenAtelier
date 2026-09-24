use oa_doc::*;
use oa_graph::registry::Registry;
use oa_graph::{optimize, KeyContext, NodeOp, OptLevel};
use oa_params::{Curve, Gradient, GradientStop, Keyframe, KeyframeAnchor, ParamSet, ParamSource, Value};
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::sync::Arc;

const SEQ: SeqId = SeqId(1);
const WIDE: VariantId = VariantId(2);
const TALL: VariantId = VariantId(3);
const V1: TrackId = TrackId(4);
const V2: TrackId = TrackId(5);
const MEDIA: MediaId = MediaId(6);
const CLIP: ItemId = ItemId(7);

fn secs(s: i64) -> Time {
    Time::from_seconds(s)
}

fn variant(id: VariantId, preset: &str) -> FormatVariant {
    let p = AspectPreset::by_id(preset).unwrap();
    FormatVariant { id, name: p.name.into(), size: p.size(1080), overrides: BTreeMap::new() }
}

/// 4K media on V1, 16:9 + 9:16 variants.
fn project(configure: impl FnOnce(&mut Item)) -> Project {
    let mut p = Project::new("t");
    let mut seq = Sequence::new(SEQ, "Main", FrameRate::FPS_30, variant(WIDE, "landscape-16x9"));
    seq.variants.push(variant(TALL, "vertical-9x16"));
    let mut v1 = Track::new(V1, "V1", TrackKind::Video);
    let mut clip = Item::new(CLIP, "clip", ItemKind::Media { media: MEDIA }, TimeRange::new(secs(0), secs(10)));
    configure(&mut clip);
    v1.items.push(clip);
    seq.tracks.push(Arc::new(v1));
    seq.tracks.push(Arc::new(Track::new(V2, "V2", TrackKind::Video)));
    p.sequences.insert(SEQ, Arc::new(seq));
    let info = MediaInfo { width: 3840, height: 2160, duration: secs(60), rate: Some(FrameRate::FPS_30), has_video: true, has_audio: true, still: false, ..Default::default() };
    p.media.insert(MEDIA, Arc::new(MediaRef { id: MEDIA, path: "a.mp4".into(), fingerprint: Some("fp-a".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
    p
}

fn plan(p: &Project, t: Time, opts: PlanOptions) -> oa_plan::Plan {
    plan_frame(p, SEQ, t, &opts, &Registry::with_builtins()).unwrap()
}

fn find(g: &oa_graph::Graph, pred: impl Fn(&NodeOp) -> bool) -> Vec<&oa_graph::Node> {
    g.nodes.iter().filter(|n| pred(&n.op)).collect()
}

fn source_node(g: &oa_graph::Graph) -> &oa_graph::Node {
    find(g, |op| matches!(op, NodeOp::Source { .. }))[0]
}

#[test]
fn uhd_clip_decodes_at_half_res_for_1080p_and_transform_disappears() {
    let p = project(|_| {});
    let full = plan(&p, secs(1), PlanOptions::default());
    match &source_node(&full.graph).op {
        NodeOp::Source { decode_scale, size, .. } => assert_eq!((*decode_scale, *size), (0.5, [1920, 1080])),
        _ => unreachable!(),
    }
    let o = optimize(&full.graph, OptLevel::Full, KeyContext::default());
    assert!(find(&o, |op| matches!(op, NodeOp::Transform { .. })).is_empty(), "{}", o.describe());

    // Half-res preview decodes at quarter size.
    let preview = plan(&p, secs(1), PlanOptions { render_scale: 0.5, ..Default::default() });
    match &source_node(&preview.graph).op {
        NodeOp::Source { size, .. } => assert_eq!(*size, [960, 540]),
        _ => unreachable!(),
    }
}

#[test]
fn vertical_variant_fills_and_follows_focus_override() {
    let mut p = project(|_| {});
    let seq = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    let mut ov = oa_params::ParamSet::default();
    ov.set(schema::FOCUS, ParamSource::Static(Value::Vec2([1.0, 0.5])));
    seq.variants[1].overrides.insert(CLIP, ov);

    let tall = plan(&p, secs(1), PlanOptions { variant: Some(TALL), ..Default::default() });
    let comp = tall.graph.node(tall.graph.output);
    assert!(matches!(comp.op, NodeOp::Composite { size: [1080, 1920], .. }));
    let xf = find(&tall.graph, |op| matches!(op, NodeOp::Transform { .. }))[0];
    // Fill height 1920 -> width 3413.3; focus 1.0 pins the right edge to the canvas edge.
    assert!((xf.bounds.x1 - 1080.0).abs() < 1e-6 && xf.bounds.x0 < -2000.0, "{:?}", xf.bounds);
    assert!(xf.opaque);

    // The wide variant is unaffected by the vertical override.
    let wide = plan(&p, secs(1), PlanOptions { variant: Some(WIDE), ..Default::default() });
    let xf = find(&wide.graph, |op| matches!(op, NodeOp::Transform { .. }))[0];
    assert!(xf.bounds.x0.abs() < 1e-6 && (xf.bounds.x1 - 1920.0).abs() < 1e-6);
}

#[test]
fn layer_pixel_params_and_bounds_follow_raster_scale() {
    let p = project(|clip| {
        let mut params = oa_params::ParamSet::default();
        params.set("radius", ParamSource::Static(Value::Float(20.0)));
        clip.effects.push(EffectInstance { id: EffectId(9), type_id: "oa.blur.gaussian".into(), type_version: 1, enabled: true, params, role: Default::default() });
    });
    let full = plan(&p, secs(1), PlanOptions::default());
    let blur = |g: &oa_graph::Graph| match &find(g, |op| matches!(op, NodeOp::Effect { .. }))[0].op {
        NodeOp::Effect { uniforms, .. } => uniforms[0],
        _ => unreachable!(),
    };
    // Layer space is 4K pixels; decoded at 0.5 -> radius 10 raster px.
    assert_eq!(blur(&full.graph), 10.0);
    let preview = plan(&p, secs(1), PlanOptions { render_scale: 0.5, ..Default::default() });
    assert_eq!(blur(&preview.graph), 5.0);
    // Blur keeps the clip's size: edges repeat inward rather than the layer growing.
    let fx = find(&full.graph, |op| matches!(op, NodeOp::Effect { .. }))[0];
    assert_eq!(fx.bounds.x0, 0.0);
}

#[test]
fn effect_params_are_keyframed_and_modulated_per_frame() {
    let p = project(|clip| {
        let mut params = oa_params::ParamSet::default();
        // Blur intensity ramps 0 -> 40 layer px over 4 seconds...
        params.set(
            "radius",
            ParamSource::Animated(Curve::new(
                KeyframeAnchor::ClipStart,
                vec![Keyframe::linear(secs(0), Value::Float(0.0)), Keyframe::linear(secs(4), Value::Float(40.0))],
            )),
        );
        clip.effects.push(EffectInstance { id: EffectId(9), type_id: "oa.blur.gaussian".into(), type_version: 1, enabled: true, params, role: Default::default() });
        // ...while the layer shakes.
        clip.params.set(schema::POSITION, ParamSource::Static(Value::Vec2([0.0, 0.0])).wiggle(0.01, 10.0, 1));
    });
    let radius_at = |s: i64| {
        let g = plan(&p, secs(s), PlanOptions::default()).graph;
        let r = match &find(&g, |op| matches!(op, NodeOp::Effect { .. }))[0].op {
            NodeOp::Effect { uniforms, .. } => uniforms[0],
            _ => unreachable!(),
        };
        let m = match &find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0].op {
            NodeOp::Transform { matrix } => matrix.m,
            _ => unreachable!(),
        };
        (r, m[4], m[5])
    };
    // Raster scale 0.5: 20 layer px at 2s -> 10 raster px.
    assert_eq!(radius_at(0).0, 0.0);
    assert_eq!(radius_at(2).0, 10.0);
    assert_eq!(radius_at(4).0, 20.0);
    let (a, b) = (radius_at(1), radius_at(3));
    assert!(a.1 != b.1 && a.1.abs() <= 19.2 && a.2.abs() <= 10.8, "shake offset stays within 1% of canvas");
}

#[test]
fn animated_scale_keeps_decode_raster_stable() {
    let p = project(|clip| {
        let curve = Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::linear(secs(0), Value::Vec2([0.6, 0.6])), Keyframe::linear(secs(10), Value::Vec2([0.9, 0.9]))],
        );
        clip.params.set(schema::SCALE, ParamSource::Animated(curve));
    });
    // On-screen scale animates 0.3 -> 0.45, but the decode raster stays at 0.5, so the
    // decoded frame (and its cache entry) doesn't change size every frame.
    let mut output_keys = Vec::new();
    for s in 0..10 {
        let g = plan(&p, secs(s), PlanOptions::default()).graph;
        match &source_node(&g).op {
            NodeOp::Source { size, .. } => assert_eq!(*size, [1920, 1080]),
            _ => unreachable!(),
        }
        output_keys.push(g.node(g.output).key.unwrap());
    }
    // The composited frame itself does change as the transform animates.
    assert_ne!(output_keys[0], output_keys[9]);
}

#[test]
fn squash_preserves_area() {
    let det = |squash: f64| {
        let p = project(|clip| {
            clip.params.set(schema::SQUASH, ParamSource::Static(Value::Float(squash)));
            clip.params.set(schema::FIT, ParamSource::Static(Value::Enum("none".into())));
        });
        let g = plan(&p, secs(1), PlanOptions::default()).graph;
        match &find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0].op {
            NodeOp::Transform { matrix } => {
                let [a, b, c, d, ..] = matrix.m;
                (a * d - b * c, a, d)
            }
            _ => unreachable!(),
        }
    };
    let (base, ..) = det(0.0);
    let (squashed, sx, sy) = det(0.5);
    assert!((base - squashed).abs() < 1e-9);
    assert!(sx > sy);
}

#[test]
fn opaque_solid_on_top_culls_media_below() {
    let mut p = project(|_| {});
    let seq = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    let mut solid = Item::new(ItemId(20), "bg", ItemKind::Solid, TimeRange::new(secs(0), secs(5)));
    solid.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([1.0, 0.0, 0.0, 1.0])));
    Arc::make_mut(&mut seq.tracks[1]).items.push(solid);

    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    assert_eq!(find(&g, |op| matches!(op, NodeOp::Source { .. })).len(), 1);
    let o = optimize(&g, OptLevel::Full, KeyContext::default());
    assert!(find(&o, |op| matches!(op, NodeOp::Source { .. })).is_empty(), "{}", o.describe());
    // After the solid ends, the media is back.
    let later = optimize(&plan(&p, secs(6), PlanOptions::default()).graph, OptLevel::Full, KeyContext::default());
    assert_eq!(find(&later, |op| matches!(op, NodeOp::Source { .. })).len(), 1);
}

#[test]
fn missing_and_stateful_effects() {
    let p = project(|clip| {
        for (i, id) in ["com.someone.missing", "oa.time.feedback-trail"].iter().enumerate() {
            clip.effects.push(EffectInstance { id: EffectId(30 + i as u64), type_id: (*id).into(), type_version: 1, enabled: true, params: Default::default(), role: Default::default() });
        }
    });
    let plan = plan(&p, secs(3), PlanOptions::default());
    assert_eq!(plan.report.missing_effects, vec!["com.someone.missing".to_string()]);
    assert_eq!(plan.graph.preroll, secs(2));
    let out = plan.graph.node(plan.graph.output);
    assert!(out.key.is_none(), "stateful effect must make the frame uncacheable");
}

#[test]
fn nested_sequences_render_at_raster_scale() {
    let mut p = project(|_| {});
    let inner = SeqId(40);
    let mut s = Sequence::new(inner, "Inner", FrameRate::FPS_30, variant(VariantId(41), "square-1x1"));
    let mut t = Track::new(TrackId(42), "V1", TrackKind::Video);
    t.items.push(Item::new(ItemId(43), "solid", ItemKind::Solid, TimeRange::new(secs(0), secs(100))));
    s.tracks.push(Arc::new(t));
    p.sequences.insert(inner, Arc::new(s));
    let seq = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    let mut nest = Item::new(ItemId(44), "nest", ItemKind::Nested { sequence: inner }, TimeRange::new(secs(0), secs(10)));
    nest.params.set(schema::FIT, ParamSource::Static(Value::Enum("fit".into())));
    Arc::make_mut(&mut seq.tracks[1]).items.push(nest);

    let g = plan(&p, secs(1), PlanOptions { render_scale: 0.5, ..Default::default() }).graph;
    let comps = find(&g, |op| matches!(op, NodeOp::Composite { .. }));
    let sizes: Vec<_> = comps.iter().map(|n| match n.op { NodeOp::Composite { size, .. } => size, _ => unreachable!() }).collect();
    // Inner 1080x1080 fitted into a 540-tall half-res output -> rasterized at 0.5 -> 540x540.
    assert!(sizes.contains(&[540, 540]) && sizes.contains(&[960, 540]), "{sizes:?}");
}

/// An effect with a media parameter gets that media's picture as a second input,
/// decoded at the layer's raster size; with nothing chosen it has one input.
#[test]
fn media_parameters_become_effect_inputs() {
    let matte = MediaId(40);
    let mut p = project(|clip| {
        let mut mask = EffectInstance::new(EffectId(41), "oa.mask.media");
        mask.params.set("matte", ParamSource::Static(Value::Media(Some(40))));
        clip.effects.push(mask);
    });
    let info = MediaInfo { width: 640, height: 360, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() };
    p.media.insert(matte, Arc::new(MediaRef { id: matte, path: "m.png".into(), fingerprint: Some("fp-m".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let mask = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == "oa.mask.media"))[0];
    assert_eq!(mask.inputs.len(), 2);
    match &g.node(mask.inputs[1]).op {
        NodeOp::Source { media, size, .. } => assert_eq!((*media, *size), (40, [1920, 1080])),
        other => panic!("{other:?}"),
    }

    let p = project(|clip| clip.effects.push(EffectInstance::new(EffectId(41), "oa.mask.media")));
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let mask = find(&g, |op| matches!(op, NodeOp::Effect { .. }))[0];
    assert_eq!(mask.inputs.len(), 1, "no matte chosen: no second input");
}

/// A compound clip (a sequence) can be an effect's picture: it's rendered, see-through
/// where empty, and stretched over the layer.
#[test]
fn sequences_can_be_effect_media() {
    let mut p = project(|clip| {
        let mut mask = EffectInstance::new(EffectId(41), "oa.mask.media");
        mask.params.set("matte", ParamSource::Static(Value::Media(Some(40))));
        clip.effects.push(mask);
    });
    let inner = SeqId(40);
    let mut s = Sequence::new(inner, "Group", FrameRate::FPS_30, variant(VariantId(42), "square-1x1"));
    let mut t = Track::new(TrackId(43), "V1", TrackKind::Video);
    t.items.push(Item::new(ItemId(44), "solid", ItemKind::Solid, TimeRange::new(secs(0), secs(100))));
    s.tracks.push(Arc::new(t));
    p.sequences.insert(inner, Arc::new(s));
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let mask = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == "oa.mask.media"))[0];
    assert_eq!(mask.inputs.len(), 2);
    match &g.node(mask.inputs[1]).op {
        NodeOp::Composite { size, background, .. } => assert_eq!((*size, background[3]), ([1920, 1080], 0.0)),
        other => panic!("{other:?}"),
    }
}

/// A clip's crop becomes a crop node right after its picture, before its effects.
#[test]
fn crop_cuts_the_edges_before_effects() {
    let p = project(|clip| {
        clip.params.set(schema::CROP_LEFT, ParamSource::Static(Value::Float(0.25)));
        clip.effects.push(EffectInstance::new(EffectId(41), "oa.color.exposure"));
    });
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let crop = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == oa_graph::registry::CROP))[0];
    match &crop.op {
        NodeOp::Effect { uniforms, .. } => assert_eq!(&uniforms[..6], &[0.25, 0.0, 0.0, 0.0, 1920.0, 1080.0]),
        _ => unreachable!(),
    }
    assert!(matches!(g.node(crop.inputs[0]).op, NodeOp::Source { .. }));
    let exposure = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == "oa.color.exposure"))[0];
    assert!(std::ptr::eq(g.node(exposure.inputs[0]), crop));

    let g = plan(&project(|_| {}), secs(1), PlanOptions::default()).graph;
    assert!(find(&g, |op| matches!(op, NodeOp::Effect { .. })).is_empty(), "no crop: no node");
}

/// In/out effects only appear inside their windows, carrying their clock.
#[test]
fn in_and_out_effects_run_only_in_their_windows() {
    // A shader outro (Defocus) is an effect node only inside its window.
    let p = project(|clip| {
        let mut defocus = EffectInstance::new(EffectId(41), "oa.anim.blur");
        defocus.role = EffectRole::Out { duration: secs(2) };
        clip.effects.push(defocus);
    });
    let effects = |t| {
        let g = plan(&p, t, PlanOptions::default()).graph;
        find(&g, |op| matches!(op, NodeOp::Effect { .. })).iter().map(|n| match &n.op {
            NodeOp::Effect { uniforms, .. } => uniforms.clone(),
            _ => unreachable!(),
        }).collect::<Vec<_>>()
    };
    assert!(effects(secs(5)).is_empty(), "the clip runs 0..10 s; the out starts at 8 s");
    let u = &effects(secs(9))[0];
    // [radius, visibility, progress, seconds]
    assert!((u[1] - 0.5).abs() < 1e-6 && (u[2] - 0.5).abs() < 1e-6, "visibility and progress half-way: {u:?}");

    // A motion outro (Fade) isn't a node at all: it lowers the layer's opacity.
    let p = project(|clip| {
        let mut fade = EffectInstance::new(EffectId(42), "oa.anim.fade");
        fade.role = EffectRole::Out { duration: secs(2) };
        clip.effects.push(fade);
    });
    let opacity = |t| {
        let g = plan(&p, t, PlanOptions::default()).graph;
        match &g.node(g.output).op {
            NodeOp::Composite { layers, .. } => layers[0].opacity,
            _ => unreachable!(),
        }
    };
    assert_eq!(opacity(secs(5)), 1.0);
    assert!((opacity(secs(9)) - 0.5).abs() < 1e-6);
}

/// Fly in from the left starts a whole canvas to the left and lands in place.
#[test]
fn fly_in_moves_the_layer_across_the_canvas() {
    let p = project(|clip| {
        let mut fly = EffectInstance::new(EffectId(43), "oa.anim.fly");
        fly.role = EffectRole::In { duration: secs(2) };
        clip.effects.push(fly);
    });
    let x = |t| {
        let g = plan(&p, t, PlanOptions::default()).graph;
        let t = find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0];
        match &t.op {
            NodeOp::Transform { matrix } => matrix.m[4],
            _ => unreachable!(),
        }
    };
    assert!((x(secs(0)) + 1920.0).abs() < 1.0, "starts off the left edge: {}", x(secs(0)));
    assert!(x(secs(1)) > -1920.0 && x(secs(1)) < -100.0, "on its way: {}", x(secs(1)));
    assert!(x(secs(3)).abs() < 1e-9, "landed");
}

#[test]
fn several_intros_play_together_each_over_its_own_time() {
    let p = project(|clip| {
        let mut fly = EffectInstance::new(EffectId(43), "oa.anim.fly");
        fly.role = EffectRole::In { duration: secs(2) };
        let mut fade = EffectInstance::new(EffectId(44), "oa.anim.fade");
        fade.role = EffectRole::In { duration: secs(4) };
        clip.effects.push(fly);
        clip.effects.push(fade);
    });
    let at = |t| {
        let g = plan(&p, t, PlanOptions::default()).graph;
        let x = match &find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0].op {
            NodeOp::Transform { matrix } => matrix.m[4],
            _ => unreachable!(),
        };
        let opacity = match &find(&g, |op| matches!(op, NodeOp::Composite { .. })).last().unwrap().op {
            NodeOp::Composite { layers, .. } => layers[0].opacity,
            _ => unreachable!(),
        };
        (x, opacity)
    };
    let (x1, o1) = at(secs(1));
    assert!(x1 < -100.0 && (o1 - 0.25).abs() < 0.01, "both running: x {x1}, opacity {o1}");
    let (x3, o3) = at(secs(3));
    assert!(x3.abs() < 1e-9 && (o3 - 0.75).abs() < 0.01, "fly done, fade still going: x {x3}, opacity {o3}");
    let (_, o5) = at(secs(5));
    assert_eq!(o5, 1.0);
}

/// A text clip on V2 over the media.
fn with_title(configure: impl FnOnce(&mut Item)) -> Project {
    let mut p = project(|_| {});
    let seq = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    let mut title = Item::new(ItemId(20), "Title", ItemKind::Text, TimeRange::new(secs(0), secs(5)));
    title.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text("Hello".into())));
    configure(&mut title);
    Arc::make_mut(&mut seq.tracks[1]).items.push(title);
    p
}

#[test]
fn text_is_placed_at_its_own_size_centered_and_clickable() {
    let p = with_title(|_| {});
    let g = plan(&p, secs(1), PlanOptions { render_scale: 0.5, ..Default::default() }).graph;
    let text = find(&g, |op| matches!(op, NodeOp::Text { .. }));
    assert_eq!(text.len(), 1);
    let NodeOp::Text { spec, scale, .. } = &text[0].op else { unreachable!() };
    assert_eq!(spec.content, "Hello");
    assert_eq!(spec.size, 120.0);
    assert_eq!(*scale, 0.5, "rasterized at the output scale");

    // Hit testing uses the text box: the canvas center is on the title, a corner isn't.
    let seq = p.sequence(SEQ).unwrap();
    let v = seq.variant(WIDE).unwrap();
    assert_eq!(oa_plan::scene::hit_test(&p, SEQ, v, secs(1), [960.0, 540.0]), Some(ItemId(20)));
    assert_eq!(oa_plan::scene::hit_test(&p, SEQ, v, secs(1), [100.0, 100.0]), Some(CLIP));
    let placed = oa_plan::scene::layers_at(&p, SEQ, v, secs(1)).pop().unwrap();
    let [w, h] = placed.native;
    assert!(w > 200.0 && w < 450.0 && h > 100.0 && h < 220.0, "{w}x{h}");
    let c = placed.corners();
    assert!(((c[0][0] + c[2][0]) / 2.0 - 960.0).abs() < 1e-6 && ((c[0][1] + c[2][1]) / 2.0 - 540.0).abs() < 1e-6, "centered");

    // Bigger type, bigger box.
    let big = with_title(|t| {
        t.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(240.0)));
    });
    let v = big.sequence(SEQ).unwrap().variant(WIDE).unwrap();
    let placed_big = oa_plan::scene::layers_at(&big, SEQ, v, secs(1)).pop().unwrap();
    assert!((placed_big.native[0] / w - 2.0).abs() < 0.05);
}

#[test]
fn letter_effects_go_to_the_text_pass_and_widen_its_bounds() {
    let plain = with_title(|_| {});
    let rising = with_title(|t| {
        let mut rise = EffectInstance::new(EffectId(50), "oa.text.rise");
        rise.params.set("distance", ParamSource::Static(Value::Float(2.0)));
        rise.role = EffectRole::In { duration: secs(2) };
        t.effects.push(rise);
        t.effects.push(EffectInstance::new(EffectId(51), "oa.color.saturation"));
    });
    let text = |p: &Project, t| {
        let g = plan(p, t, PlanOptions::default()).graph;
        let n = find(&g, |op| matches!(op, NodeOp::Text { .. }))[0].clone();
        let effects = find(&g, |op| matches!(op, NodeOp::Effect { .. })).len();
        (n, effects)
    };
    let (a, _) = text(&plain, secs(1));
    let (b, effects) = text(&rising, secs(1));
    let NodeOp::Text { chain, .. } = &b.op else { unreachable!() };
    assert_eq!(chain.len(), 1, "the rise runs per letter inside the text pass");
    assert_eq!(&*chain[0].type_id, "oa.text.rise");
    assert_eq!(effects, 1, "saturation stays an ordinary effect on the text layer");
    let (wa, wb) = (a.bounds.x1 - a.bounds.x0, b.bounds.x1 - b.bounds.x0);
    assert!(wb > wa + 400.0, "room for letters 2 em away: {wa} → {wb}");
    // After its window, the intro is gone from the pass.
    let (c, _) = text(&rising, secs(3));
    let NodeOp::Text { chain, .. } = &c.op else { unreachable!() };
    assert!(chain.is_empty());

    // Letter effects on a picture clip are ignored, not rendered as garbage.
    let p = project(|clip| clip.effects.push(EffectInstance::new(EffectId(52), "oa.text.wave")));
    let g = plan(&p, secs(1), PlanOptions::default());
    assert!(find(&g.graph, |op| matches!(op, NodeOp::Effect { .. } | NodeOp::Text { .. })).is_empty());
}

#[test]
fn fly_follows_any_direction_and_outros_leave_that_way() {
    let offset = |role: EffectRole, degrees: f64, t: Time| {
        let p = project(|clip| {
            let mut fly = EffectInstance::new(EffectId(43), "oa.anim.fly");
            fly.params.set("direction", ParamSource::Static(Value::Float(degrees)));
            fly.role = role;
            clip.effects.push(fly);
        });
        let g = plan(&p, t, PlanOptions::default()).graph;
        match &find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0].op {
            NodeOp::Transform { matrix } => [matrix.m[4], matrix.m[5]],
            _ => unreachable!(),
        }
    };
    let intro = EffectRole::In { duration: secs(2) };
    let outro = EffectRole::Out { duration: secs(2) };
    // Moving down (90°): an intro starts a full canvas above.
    let [x, y] = offset(intro, 90.0, secs(0));
    assert!(x.abs() < 1e-6 && (y + 1080.0).abs() < 1.0, "{x},{y}");
    // Moving up-left (225°): starts down-right, far enough to clear both edges.
    let [x, y] = offset(intro, 225.0, secs(0));
    assert!(x >= 1920.0 && y >= 1080.0, "{x},{y}");
    // An outro moving down leaves through the bottom.
    let [x, y] = offset(outro, 90.0, secs(10) - Time(1));
    assert!(x.abs() < 1e-6 && y > 1000.0, "{x},{y}");
}

#[test]
fn reverse_outro_plays_the_intro_backwards() {
    let p = project(|clip| {
        let mut fly = EffectInstance::new(EffectId(43), "oa.anim.fly");
        fly.role = EffectRole::In { duration: secs(2) };
        clip.effects.push(fly);
        // An outro of its own, which "Reverse" turns off.
        let mut zoom = EffectInstance::new(EffectId(44), "oa.anim.zoom");
        zoom.role = EffectRole::Out { duration: secs(2) };
        clip.effects.push(zoom);
        clip.outro_reverses_intro = true;
    });
    let matrix = |t: Time| {
        let g = plan(&p, t, PlanOptions::default()).graph;
        match &find(&g, |op| matches!(op, NodeOp::Transform { .. }))[0].op {
            NodeOp::Transform { matrix } => matrix.m,
            _ => unreachable!(),
        }
    };
    // Mirror images in time: 0.5 s into the intro = 0.5 s before the end.
    for (a, b) in [(Time::from_seconds_f64(0.5), secs(10) - Time::from_seconds_f64(0.5)), (secs(1), secs(9))] {
        let (ma, mb) = (matrix(a), matrix(b));
        assert!(ma.iter().zip(&mb).all(|(x, y)| (x - y).abs() < 1.0), "{ma:?} vs {mb:?}");
    }
    // It leaves the way it came (to the left), not by zooming.
    let m = matrix(secs(10) - Time(1));
    assert!(m[4] < -1800.0 && (m[0] - matrix(secs(5))[0]).abs() < 1e-9, "{m:?}");
    assert_eq!(matrix(secs(5))[4], 0.0, "untouched in the middle");
}

/// The background: a plain color clears the canvas; a gradient, the blurred picture and
/// a tiled texture are drawn under the clips.
#[test]
fn backgrounds() {
    let with = |set: &dyn Fn(&mut ParamSet)| {
        let mut p = project(|_| {});
        set(&mut Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap()).params);
        let info = MediaInfo { width: 200, height: 100, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() };
        p.media.insert(MediaId(40), Arc::new(MediaRef { id: MediaId(40), path: "t.png".into(), fingerprint: Some("fp-t".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
        plan(&p, secs(1), PlanOptions::default()).graph
    };
    let effect = |g: &oa_graph::Graph, id: &str| find(g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == id)).len();
    let top = |g: &oa_graph::Graph| match &g.node(g.output).op {
        NodeOp::Composite { background, layers, .. } => (*background, layers.len()),
        other => panic!("{other:?}"),
    };

    let g = with(&|s| {
        s.set(schema::BG_COLOR, ParamSource::Static(Value::Gradient(Gradient::solid([0.2, 0.3, 0.4, 1.0]))));
    });
    assert_eq!(top(&g), ([0.2, 0.3, 0.4, 1.0], 1), "one color: just the clear color");

    let g = with(&|s| {
        let mut two = Gradient::solid([1.0, 0.0, 0.0, 1.0]);
        two.stops.push(GradientStop { pos: 1.0, color: [0.0, 0.0, 1.0, 1.0] });
        s.set(schema::BG_COLOR, ParamSource::Static(Value::Gradient(two)));
    });
    assert_eq!((effect(&g, oa_graph::registry::FILL), top(&g).1), (1, 2), "a gradient under the clip");

    let g = with(&|s| {
        s.set(schema::BG_MODE, ParamSource::Static(Value::Enum("blur".into())));
    });
    assert_eq!((effect(&g, "oa.blur.gaussian"), effect(&g, oa_graph::registry::DIM), top(&g).1), (1, 1, 2));
    assert_eq!(find(&g, |op| matches!(op, NodeOp::Composite { size: [480, 270], .. })).len(), 1, "blurred at a quarter size");

    // The frame as placed: a clip at half size is enlarged 2× behind itself (then ¼).
    let mut p = project(|clip| {
        clip.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([0.5, 0.5])));
    });
    Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap()).params.set(schema::BG_MODE, ParamSource::Static(Value::Enum("blur".into())));
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let shrunk = find(&g, |op| matches!(op, NodeOp::Composite { size: [480, 270], .. }))[0];
    match &g.node(shrunk.inputs[0]).op {
        NodeOp::Transform { matrix } => assert!((matrix.m[0] - 0.5).abs() < 1e-9 && (matrix.m[3] - 0.5).abs() < 1e-9, "{:?}", matrix.m),
        other => panic!("{other:?}"),
    }
    assert!(matches!(g.node(g.node(shrunk.inputs[0]).inputs[0]).op, NodeOp::Composite { size: [1920, 1080], .. }), "made from the finished frame");

    let g = with(&|s| {
        s.set(schema::BG_MODE, ParamSource::Static(Value::Enum("texture".into())));
        s.set(schema::BG_TEXTURE, ParamSource::Static(Value::Media(Some(40))));
    });
    let tile = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == oa_graph::registry::TILE))[0];
    assert_eq!((tile.bounds.x1, tile.bounds.y1), (1920.0, 1080.0), "tiles cover the canvas");
    match &g.node(tile.inputs[0]).op {
        NodeOp::Source { size, .. } => assert_eq!(size[..], [540, 270], "a quarter of the height, the picture's shape"),
        other => panic!("{other:?}"),
    }
}

/// A compound clip as the background texture: its picture is tiled, and it loops, so a
/// short one still fills the background long after its own end.
#[test]
fn compound_clips_tile_the_background() {
    let mut p = project(|_| {});
    let inner = SeqId(40);
    let mut s = Sequence::new(inner, "Pattern", FrameRate::FPS_30, variant(VariantId(42), "square-1x1"));
    let mut t = Track::new(TrackId(43), "V1", TrackKind::Video);
    t.items.push(Item::new(ItemId(44), "solid", ItemKind::Solid, TimeRange::new(secs(0), secs(2))));
    s.tracks.push(Arc::new(t));
    p.sequences.insert(inner, Arc::new(s));
    let params = &mut Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap()).params;
    params.set(schema::BG_MODE, ParamSource::Static(Value::Enum("texture".into())));
    params.set(schema::BG_TEXTURE, ParamSource::Static(Value::Media(Some(40))));
    for at in [1, 7] {
        let g = plan(&p, secs(at), PlanOptions::default()).graph;
        let tile = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == oa_graph::registry::TILE));
        assert_eq!(tile.len(), 1, "at {at}s");
        // The compound's solid is inside the tile, even at 7 s (the compound is 2 s long).
        let mut stack = vec![tile[0].inputs[0]];
        let mut solid = false;
        while let Some(n) = stack.pop() {
            solid |= matches!(g.node(n).op, NodeOp::Solid { .. });
            stack.extend(g.node(n).inputs.iter().copied());
        }
        assert!(solid, "at {at}s the tile shows the compound");
    }
}

/// The background's effects run on the background only (a canvas of its color when
/// it's a plain color), under the clips; turned off, they're gone.
#[test]
fn background_effects_run_under_the_clips() {
    let mut p = project(|_| {});
    let s = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    s.params.set(schema::BG_COLOR, ParamSource::Static(Value::Gradient(Gradient::solid([0.2, 0.3, 0.4, 1.0]))));
    s.background.effects.push(EffectInstance::new(EffectId(50), "oa.blur.gaussian"));
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let blur = effect_nodes(&g, "oa.blur.gaussian");
    assert_eq!(blur.len(), 1);
    assert!(matches!(g.node(blur[0].inputs[0]).op, NodeOp::Solid { color, .. } if color == [0.2, 0.3, 0.4, 1.0]), "blurs a canvas of the color");
    match &g.node(g.output).op {
        NodeOp::Composite { layers, .. } => assert_eq!(layers.len(), 2, "background, then the clip"),
        other => panic!("{other:?}"),
    }
    let bottom = &g.node(g.node(g.output).inputs[0]).op;
    assert!(matches!(bottom, NodeOp::Effect { type_id, .. } if &**type_id == "oa.blur.gaussian"), "the blurred background is the bottom layer");

    let s = Arc::make_mut(p.sequences.get_mut(&SEQ).unwrap());
    s.background.effects[0].enabled = false;
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    assert!(effect_nodes(&g, "oa.blur.gaussian").is_empty());
}

/// Tiny pictures (pixel art) are drawn with nearest-neighbor scaling unless set to
/// smooth; anything can be set to pixel scaling.
#[test]
fn pixel_art_scales_without_smoothing() {
    let pixelated = |w: u32, h: u32, scaling: MediaScaling| {
        let mut p = project(|_| {});
        let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
        m.info = Some(MediaInfo { width: w, height: h, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() });
        m.scaling = scaling;
        let g = plan(&p, secs(1), PlanOptions::default()).graph;
        match &g.node(g.output).op {
            NodeOp::Composite { layers, .. } => layers[0].pixelated,
            other => panic!("{other:?}"),
        }
    };
    assert!(pixelated(16, 16, MediaScaling::Auto));
    assert!(pixelated(32, 24, MediaScaling::Auto));
    assert!(!pixelated(33, 16, MediaScaling::Auto));
    assert!(!pixelated(16, 16, MediaScaling::Smooth));
    assert!(pixelated(1920, 1080, MediaScaling::Pixel));

    // And its shaders run per picture pixel: the source is decoded at its own size even
    // in a half-res preview, so effects and sampling never see a smoothed-down copy.
    let mut p = project(|clip| {
        clip.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([0.25, 0.25])));
    });
    let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
    m.info = Some(MediaInfo { width: 16, height: 16, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() });
    let g = plan(&p, secs(1), PlanOptions { render_scale: 0.5, ..Default::default() }).graph;
    match &source_node(&g).op {
        NodeOp::Source { size, decode_scale, .. } => assert_eq!((size[..].to_vec(), *decode_scale), (vec![16, 16], 1.0)),
        other => panic!("{other:?}"),
    }

    // Its effects read whole picture pixels too, so a warp lands on the pixel grid
    // instead of blending neighbors.
    let flag = |scaling: MediaScaling| {
        let mut p = project(|clip| {
            clip.effects.push(EffectInstance::new(EffectId(41), "oa.warp.wobble"));
        });
        let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
        m.info = Some(MediaInfo { width: 16, height: 16, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() });
        m.scaling = scaling;
        let g = plan(&p, secs(1), PlanOptions::default()).graph;
        match &find(&g, |op| matches!(op, NodeOp::Effect { .. }))[0].op {
            NodeOp::Effect { nearest, .. } => *nearest,
            other => panic!("{other:?}"),
        }
    };
    assert!(flag(MediaScaling::Auto), "pixel art: no smoothing inside effects");
    assert!(!flag(MediaScaling::Smooth));
}

/// Pixel art is enlarged to the size it's shown at before its effects run: a 16×16
/// sprite at 10× is a 160×160 picture of crisp blocks, and the effects work on all of
/// those pixels — not on 16×16 and then stretched.
#[test]
fn pixel_art_effects_run_at_the_size_it_is_shown() {
    let mut p = project(|clip| {
        // 16×16 shown 1:1 in the canvas, then scaled 10×.
        clip.params.set(schema::FIT, ParamSource::Static(Value::Enum("none".into())));
        clip.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([10.0, 10.0])));
        clip.effects.push(EffectInstance::new(EffectId(41), "oa.blur.gaussian"));
    });
    let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
    m.info = Some(MediaInfo { width: 16, height: 16, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() });
    let g = plan(&p, secs(1), PlanOptions::default()).graph;

    // Decoded at its own size…
    match &source_node(&g).op {
        NodeOp::Source { size, decode_scale, .. } => assert_eq!((size[..].to_vec(), *decode_scale), (vec![16, 16], 1.0)),
        other => panic!("{other:?}"),
    }
    // …enlarged without smoothing…
    let grown = find(&g, |op| matches!(op, NodeOp::Composite { size: [160, 160], .. }))[0];
    match &grown.op {
        NodeOp::Composite { layers, .. } => assert!(layers[0].pixelated, "enlarged with nearest neighbor"),
        other => panic!("{other:?}"),
    }
    // …and the blur runs over all 160×160 of it.
    let blur = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == "oa.blur.gaussian"))[0];
    assert_eq!((blur.bounds.x1, blur.bounds.y1), (160.0, 160.0));
    assert!(std::ptr::eq(g.node(blur.inputs[0]), grown), "the blur reads the enlarged picture");

    // Shown small, it stays 1:1 — there's nothing to gain from rasterizing below native.
    let mut small = p.clone();
    let seq = Arc::make_mut(small.sequences.get_mut(&SEQ).unwrap());
    let track = Arc::make_mut(&mut seq.tracks[0]);
    track.items[0].params.set(schema::SCALE, ParamSource::Static(Value::Vec2([0.5, 0.5])));
    let g = plan(&small, secs(1), PlanOptions::default()).graph;
    assert!(find(&g, |op| matches!(op, NodeOp::Composite { size: [16, 16], .. })).is_empty(), "no enlargement needed");
    let blur = find(&g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == "oa.blur.gaussian"))[0];
    assert_eq!((blur.bounds.x1, blur.bounds.y1), (16.0, 16.0));
}

fn effect_nodes<'a>(g: &'a oa_graph::Graph, id: &str) -> Vec<&'a oa_graph::Node> {
    find(g, |op| matches!(op, NodeOp::Effect { type_id, .. } if &**type_id == id))
}

/// Color management (DESIGN.md §9): SDR files pass straight through; HDR and log files
/// get their input transform right after decoding, and turn on tone mapping at the end.
#[test]
fn input_and_output_color_transforms() {
    use oa_doc::color::{ColorTags, InputColor, Matrix, Range, Transfer};
    use oa_graph::registry::{INPUT, OUTPUT};

    let sdr = plan(&project(|_| {}), secs(1), PlanOptions::default()).graph;
    assert!(effect_nodes(&sdr, INPUT).is_empty() && effect_nodes(&sdr, OUTPUT).is_empty(), "{}", sdr.describe());

    // A file tagged PQ: its curve is undone after decoding, and `auto` tone maps.
    let mut p = project(|_| {});
    let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
    m.info.as_mut().unwrap().color = ColorTags { transfer: Some("smpte2084".into()), primaries: Some("bt2020".into()) };
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    let input = effect_nodes(&g, INPUT);
    assert_eq!(input.len(), 1);
    assert!(matches!(g.node(input[0].inputs[0]).op, NodeOp::Source { .. }));
    match &input[0].op {
        NodeOp::Effect { uniforms, .. } => {
            assert_eq!(uniforms[0], Transfer::Pq.shader_index() as f32);
            assert!((uniforms[1] - 1.6605).abs() < 2e-3, "Rec.2020 → Rec.709 matrix: {uniforms:?}");
        }
        _ => unreachable!(),
    }
    let out = g.node(g.output);
    assert!(matches!(&out.op, NodeOp::Effect { type_id, uniforms, .. } if &**type_id == OUTPUT && uniforms[0] == 1.0), "{}", g.describe());

    // Turning tone mapping off by hand leaves the output alone.
    let mut off = p.clone();
    Arc::make_mut(off.sequences.get_mut(&SEQ).unwrap()).params.set(schema::OUT_TONE_MAP, ParamSource::Static(Value::Enum("off".into())));
    assert!(effect_nodes(&plan(&off, secs(1), PlanOptions::default()).graph, OUTPUT).is_empty());

    // Overrides: a phone clip tagged Rec.709 that's really Apple Log, full range.
    let mut p = project(|_| {});
    let m = Arc::make_mut(p.media.get_mut(&MEDIA).unwrap());
    m.color = InputColor { transfer: Transfer::AppleLog, matrix: Matrix::Bt2020, range: Range::Full, exposure: 1.0, ..Default::default() };
    let g = plan(&p, secs(1), PlanOptions::default()).graph;
    assert!(matches!(source_node(&g).op, NodeOp::Source { yuv: [3, 2], .. }));
    match &effect_nodes(&g, INPUT)[0].op {
        NodeOp::Effect { uniforms, .. } => assert_eq!((uniforms[0], uniforms[10]), (Transfer::AppleLog.shader_index() as f32, 2.0)),
        _ => unreachable!(),
    }
    // The override takes part in the cache key.
    let before = plan(&project(|_| {}), secs(1), PlanOptions::default()).graph;
    assert_ne!(source_node(&before).key, source_node(&g).key);
}

/// A cropped clip's bounds shrink with the crop: selection, handles and clicks follow
/// what's visible, while the whole picture stays available for the crop overlay.
#[test]
fn cropped_bounds_shrink() {
    let p = project(|clip| {
        clip.params.set(schema::CROP_LEFT, ParamSource::Static(Value::Float(0.25)));
        clip.params.set(schema::CROP_BOTTOM, ParamSource::Static(Value::Float(0.5)));
    });
    let v = p.sequence(SEQ).unwrap().variant(WIDE).unwrap();
    let placed = oa_plan::scene::layers_at(&p, SEQ, v, secs(1)).pop().unwrap();
    let full = placed.full_corners();
    let seen = placed.corners();
    let width = |c: [[f64; 2]; 4]| c[1][0] - c[0][0];
    let height = |c: [[f64; 2]; 4]| c[3][1] - c[0][1];
    assert!((width(seen) - width(full) * 0.75).abs() < 1e-6, "{seen:?} vs {full:?}");
    assert!((height(seen) - height(full) * 0.5).abs() < 1e-6);
    // A click on the cut-away part misses; one on what's left hits.
    assert!(!placed.contains([full[0][0] + 1.0, full[0][1] + 1.0]));
    assert!(placed.contains([seen[0][0] + 1.0, seen[0][1] + 1.0]));
}
