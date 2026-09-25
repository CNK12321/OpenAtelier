//! GPU tests. They skip (with a message) on machines without a usable adapter.

use oa_doc::*;
use oa_gpu::readback::{read_linear, read_srgb8};
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer, TestPatternSource};
use oa_graph::registry::Registry;
use oa_graph::*;
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamSet, ParamSource, Value};
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::sync::Arc;

fn gpu() -> Option<Arc<GpuContext>> {
    match GpuContext::new_headless() {
        Ok(ctx) => Some(Arc::new(ctx)),
        Err(e) => {
            eprintln!("skipping GPU test: {e}");
            None
        }
    }
}

fn renderer(ctx: &Arc<GpuContext>, fusion: FusionMode) -> Renderer {
    Renderer::new(ctx.clone(), RenderOptions { fusion, ..Default::default() })
}

fn close(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

fn solid(b: &mut GraphBuilder, color: [f32; 4], size: f64) -> NodeId {
    b.add(NodeOp::Solid { color, size: [size, size] }, vec![], Rect::from_size(size, size), color[3] >= 1.0)
}

fn composite(b: &mut GraphBuilder, size: u32, background: [f32; 4], layers: &[(NodeId, BlendMode)]) -> NodeId {
    let infos = layers.iter().map(|&(_, blend)| LayerInfo { opacity: 1.0, blend, pixelated: false }).collect();
    b.add(
        NodeOp::Composite { size: [size, size], background, layers: infos },
        layers.iter().map(|l| l.0).collect(),
        Rect::from_size(size as f64, size as f64),
        true,
    )
}

fn effect(b: &mut GraphBuilder, registry: &Registry, input: NodeId, type_id: &str, mut uniforms: Vec<f32>, expand: f64) -> NodeId {
    let d = registry.effect(type_id).unwrap();
    uniforms.extend(oa_graph::registry::STILL_CLOCK);
    let bounds = b.node(input).bounds.expand(expand);
    let op = NodeOp::Effect {
        type_id: d.type_id.clone(),
        version: d.version,
        kind: d.kind.clone(),
        space: d.space,
        fusible: d.fusible,
        stateful: false,
        uniforms,
        nearest: false,
    };
    b.add(op, vec![input], bounds, false)
}

fn render_one(r: &mut Renderer, g: &Graph) -> Vec<[f32; 4]> {
    render_with(r, g, &Registry::with_builtins())
}

/// Renders with a registry of its own (plugins).
fn render_with(r: &mut Renderer, g: &Graph, registry: &Registry) -> Vec<[f32; 4]> {
    let img = r.render(g, registry, &mut TestPatternSource::default()).unwrap();
    read_linear(r.context(), &img).unwrap()
}

#[test]
fn solids_and_blend_modes() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let cases = [
        (BlendMode::Normal, [0.2, 0.2, 0.2, 1.0], [1.0, 0.0, 0.0, 0.5], [0.6, 0.1, 0.1, 1.0]),
        (BlendMode::Add, [0.2, 0.2, 0.2, 1.0], [0.3, 0.1, 0.0, 1.0], [0.5, 0.3, 0.2, 1.0]),
        (BlendMode::Multiply, [0.5, 0.5, 0.5, 1.0], [0.5, 1.0, 0.2, 1.0], [0.25, 0.5, 0.1, 1.0]),
        (BlendMode::Screen, [0.5, 0.5, 0.5, 1.0], [0.5, 0.0, 1.0, 1.0], [0.75, 0.5, 1.0, 1.0]),
        (BlendMode::Darken, [0.5, 0.5, 0.5, 1.0], [0.2, 0.8, 0.5, 1.0], [0.2, 0.5, 0.5, 1.0]),
        (BlendMode::Lighten, [0.5, 0.5, 0.5, 1.0], [0.2, 0.8, 0.5, 1.0], [0.5, 0.8, 0.5, 1.0]),
        // Transparent parts change nothing, whichever way the minimum or maximum goes.
        (BlendMode::Darken, [0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.5, 1.0]),
        (BlendMode::Lighten, [0.5, 0.5, 0.5, 1.0], [1.0, 1.0, 1.0, 0.0], [0.5, 0.5, 0.5, 1.0]),
    ];
    for (blend, bg, layer, expected) in cases {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, layer, 4.0);
        let out = composite(&mut b, 4, bg, &[(s, blend)]);
        let px = render_one(&mut r, &b.finish(out));
        assert!(close(px[5], expected, 2e-3), "{blend:?}: {:?} != {expected:?}", px[5]);
    }
}

#[test]
fn exposure_is_exact_in_linear_light() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [0.25, 0.1, 0.05, 1.0], 8.0);
    let e = effect(&mut b, &registry, s, "oa.color.exposure", vec![1.0], 0.0);
    let out = composite(&mut b, 8, [0.0, 0.0, 0.0, 1.0], &[(e, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    assert!(close(px[0], [0.5, 0.2, 0.1, 1.0], 1e-3), "{:?}", px[0]);
}

#[test]
fn blur_grows_past_the_layer_edge() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 32.0);
    // radius 6, edges "transparent" (0)
    let blurred = effect(&mut b, &registry, s, "oa.blur.gaussian", vec![6.0, 0.0], 6.0);
    let m = Affine2::translate(16.0, 16.0);
    let bounds = m.map_rect(b.node(blurred).bounds);
    let t = b.add(NodeOp::Transform { matrix: m }, vec![blurred], bounds, false);
    let out = composite(&mut b, 64, [0.0, 0.0, 0.0, 1.0], &[(t, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    let at = |x: usize, y: usize| px[y * 64 + x][0];
    assert!(at(32, 32) > 0.99, "center {}", at(32, 32));
    // 3px outside the original square: only reachable because the effect was padded.
    assert!(at(13, 32) > 0.02 && at(13, 32) < 0.5, "outside {}", at(13, 32));
    assert!(at(2, 2) < 1e-3);
    // Blur redistributes but conserves energy.
    let energy: f32 = px.iter().map(|p| p[0]).sum();
    assert!((energy - 32.0 * 32.0).abs() < 32.0, "{energy}");
}

/// A 4K test-pattern clip with a point-op chain split by a display-space effect.
fn pattern_project(configure: impl FnOnce(&mut Item)) -> (Project, SeqId) {
    let (seq_id, media) = (SeqId(1), MediaId(2));
    let mut p = Project::new("gpu");
    let preset = |id: u64, name: &str| {
        let pr = AspectPreset::by_id(name).unwrap();
        FormatVariant { id: VariantId(id), name: pr.name.into(), size: pr.size(360), overrides: BTreeMap::new() }
    };
    let mut seq = Sequence::new(seq_id, "Main", FrameRate::FPS_30, preset(3, "landscape-16x9"));
    seq.variants.push(preset(4, "vertical-9x16"));
    let mut track = Track::new(TrackId(5), "V1", TrackKind::Video);
    let mut clip = Item::new(ItemId(6), "clip", ItemKind::Media { media }, TimeRange::new(Time::ZERO, Time::from_seconds(10)));
    configure(&mut clip);
    track.items.push(clip);
    seq.tracks.push(Arc::new(track));
    p.sequences.insert(seq_id, Arc::new(seq));
    let info = MediaInfo { width: 1280, height: 720, duration: Time::from_seconds(60), rate: Some(FrameRate::FPS_30), has_video: true, has_audio: false, still: false, ..Default::default() };
    p.media.insert(media, Arc::new(MediaRef { id: media, path: "pattern".into(), fingerprint: Some("pattern-1".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
    (p, seq_id)
}

fn add_effect(clip: &mut Item, id: u64, type_id: &str, param: &str, value: ParamSource) {
    let mut params = ParamSet::default();
    params.set(param, value);
    clip.effects.push(EffectInstance { id: EffectId(id), type_id: type_id.into(), type_version: 1, enabled: true, params, role: Default::default() });
}

fn color_chain(clip: &mut Item) {
    add_effect(clip, 10, "oa.color.exposure", "stops", ParamSource::Static(Value::Float(0.5)));
    add_effect(clip, 11, "oa.color.saturation", "amount", ParamSource::Static(Value::Float(1.6)));
    add_effect(clip, 12, "oa.color.posterize", "levels", ParamSource::Static(Value::Float(6.0)));
    add_effect(clip, 13, "oa.color.exposure", "stops", ParamSource::Static(Value::Float(-0.25)));
    add_effect(clip, 14, "oa.color.saturation", "amount", ParamSource::Static(Value::Float(0.8)));
}

#[test]
fn fused_output_matches_reference_path() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(color_chain);
    let planned = plan_frame(&p, seq, Time::from_seconds(2), &PlanOptions::default(), &registry).unwrap().graph;

    let reference = optimize(&planned, OptLevel::Reference, KeyContext::default());
    let optimized = optimize(&planned, OptLevel::Full, KeyContext::default());
    assert!(optimized.describe().contains("FusedPointOps"), "{}", optimized.describe());

    let mut r_ref = renderer(&ctx, FusionMode::Off);
    let mut r_opt = renderer(&ctx, FusionMode::Blocking);
    let a = render_one(&mut r_ref, &reference);
    let b = render_one(&mut r_opt, &optimized);
    assert!(r_opt.stats.fused_chains >= 1 && r_opt.stats.passes < r_ref.stats.passes, "{:?} vs {:?}", r_opt.stats, r_ref.stats);
    let worst = a.iter().zip(&b).map(|(x, y)| x.iter().zip(y).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max)).fold(0.0, f32::max);
    assert!(worst < 2e-3, "max channel difference {worst}");
    // Sanity: the chain actually changed the image.
    let plain = {
        let (p2, _) = pattern_project(|_| {});
        let g = plan_frame(&p2, seq, Time::from_seconds(2), &PlanOptions::default(), &registry).unwrap().graph;
        render_one(&mut renderer(&ctx, FusionMode::Off), &g)
    };
    assert!(plain.iter().zip(&a).any(|(x, y)| (x[0] - y[0]).abs() > 0.05));
}

#[test]
fn async_fusion_falls_back_then_uses_fused_pipeline() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(color_chain);
    let g = optimize(
        &plan_frame(&p, seq, Time::from_seconds(1), &PlanOptions::default(), &registry).unwrap().graph,
        OptLevel::Full,
        KeyContext::default(),
    );
    let mut r = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Async, cache_budget: 0, ..Default::default() });
    r.warm_up(&registry).unwrap();
    let first = render_one(&mut r, &g);
    // The graph has two identical exposure+saturation chains; the background compile
    // may finish before the second one, so only the first is guaranteed to fall back.
    assert!(r.stats.fallback_chains >= 1, "{:?}", r.stats);
    r.pipelines().wait_idle();
    let second = render_one(&mut r, &g);
    assert!(r.stats.fused_chains >= 1 && r.stats.fallback_chains == 0, "{:?}", r.stats);
    let worst = first.iter().zip(&second).map(|(x, y)| (x[0] - y[0]).abs().max((x[1] - y[1]).abs())).fold(0.0, f32::max);
    assert!(worst < 2e-3, "{worst}");
    assert!(r.pipelines().failures().is_empty(), "{:?}", r.pipelines().failures());
}

#[test]
fn unchanged_frames_come_from_the_cache() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(color_chain);
    let g = plan_frame(&p, seq, Time::from_seconds(1), &PlanOptions::default(), &registry).unwrap().graph;
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut source = TestPatternSource::default();
    // Admission policy: the first sighting of a key is only remembered, the second is
    // cached. Scrubbing and playback produce a new key per frame, and caching those on
    // sight would fill VRAM with entries nothing ever reads.
    r.render(&g, &registry, &mut source).unwrap();
    assert!(r.stats.passes > 0);
    r.render(&g, &registry, &mut source).unwrap();
    assert_eq!(r.stats.cache_hits, 0, "{:?}", r.stats);
    r.render(&g, &registry, &mut source).unwrap();
    assert_eq!((r.stats.passes, r.stats.cache_hits), (0, 1), "{:?}", r.stats);
    assert_eq!(source.frames_generated, 2);

    // Filling a vertical canvas from landscape footage crops in ~1.8x, so it needs a
    // sharper decode (raster 1.0 instead of 0.5): a legitimate cache miss...
    let tall = plan_frame(&p, seq, Time::from_seconds(1), &PlanOptions { variant: Some(VariantId(4)), ..Default::default() }, &registry)
        .unwrap()
        .graph;
    r.render(&tall, &registry, &mut source).unwrap();
    assert_eq!(source.frames_generated, 3);
    // ...but flipping back to landscape is free, and so is a vertical preview at half
    // resolution, which needs the same 0.5 raster the landscape frame already decoded.
    r.render(&g, &registry, &mut source).unwrap();
    let tall_preview = PlanOptions { variant: Some(VariantId(4)), render_scale: 0.5, ..Default::default() };
    let tall_preview = plan_frame(&p, seq, Time::from_seconds(1), &tall_preview, &registry).unwrap().graph;
    r.render(&tall_preview, &registry, &mut source).unwrap();
    assert_eq!(source.frames_generated, 3, "same raster scale must reuse the decoded, color-corrected layer");
}

#[test]
fn playback_reuses_pool_textures() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(color_chain);
    let mut r = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, cache_budget: 0, ..Default::default() });
    let mut source = TestPatternSource::default();
    let mut peak = 0;
    for f in 0..60 {
        let t = FrameRate::FPS_30.frame_start(f);
        let g = optimize(&plan_frame(&p, seq, t, &PlanOptions::default(), &registry).unwrap().graph, OptLevel::Full, KeyContext::default());
        let img = r.render(&g, &registry, &mut source).unwrap();
        drop(img);
        peak = peak.max(r.stats.pool_textures);
    }
    assert!(peak <= 8, "pool grew to {peak} textures");
}

#[test]
fn keyframed_blur_changes_the_rendered_frame() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(|clip| {
        let curve = Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::linear(Time::ZERO, Value::Float(0.0)), Keyframe::linear(Time::from_seconds(4), Value::Float(60.0))],
        );
        add_effect(clip, 20, "oa.blur.gaussian", "radius", ParamSource::Animated(curve));
    });
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let sharpness = |r: &mut Renderer, secs: i64| {
        let g = plan_frame(&p, seq, Time::from_seconds(secs), &PlanOptions::default(), &registry).unwrap().graph;
        let px = render_one(r, &g);
        // Sum of horizontal gradient magnitude: drops as blur increases.
        px.windows(2).map(|w| (w[0][0] - w[1][0]).abs()).sum::<f32>()
    };
    let (s0, s2, s4) = (sharpness(&mut r, 0), sharpness(&mut r, 2), sharpness(&mut r, 4));
    assert!(s0 > s2 && s2 > s4, "{s0} {s2} {s4}");
}

#[test]
fn variants_render_at_their_own_aspect_ratio() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let (p, seq) = pattern_project(|_| {});
    let mut r = renderer(&ctx, FusionMode::Blocking);
    for (variant, size) in [(VariantId(3), [640, 360]), (VariantId(4), [360, 640])] {
        let g = plan_frame(&p, seq, Time::ZERO, &PlanOptions { variant: Some(variant), ..Default::default() }, &registry).unwrap().graph;
        let img = r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
        assert_eq!(img.size, size);
        let rgba = read_srgb8(&ctx, r.pipelines(), &img).unwrap();
        assert_eq!(rgba.len(), (size[0] * size[1] * 4) as usize);
        assert!(rgba.chunks(4).all(|px| px[3] == 255), "opaque output");
    }
}

#[test]
fn stateful_effects_are_reported_not_silently_dropped() {
    let Some(ctx) = gpu() else { return };
    // An effect that needs the frames before it (none of Atelier Core's do yet).
    let mut registry = Registry::with_builtins();
    registry
        .register(oa_graph::registry::EffectDescriptor {
            state: oa_graph::Statefulness::Stateful { preroll: Time::from_seconds(2) },
            ..oa_graph::registry::EffectDescriptor::new("test.trail", "Trail", EffectKind::Temporal { frames_before: 1, frames_after: 0 }, vec![oa_params::ParamSchema::new("decay", Value::Float(0.85), oa_params::Unit::None)])
        })
        .unwrap();
    let (p, seq) = pattern_project(|clip| add_effect(clip, 30, "test.trail", "decay", ParamSource::Static(Value::Float(0.5))));
    let g = plan_frame(&p, seq, Time::from_seconds(3), &PlanOptions::default(), &registry).unwrap().graph;
    let mut r = renderer(&ctx, FusionMode::Blocking);
    r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
    assert!(r.stats.unsupported.iter().any(|u| u.contains("test.trail")), "{:?}", r.stats.unsupported);
}

/// Transitions mix their two inputs: a dissolve averages, a wipe splits the canvas, a
/// push moves both pictures, and each runs from all-`from` to all-`to`.
#[test]
fn transitions_mix_two_inputs() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (red, blue) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]);
    let mut run = |type_id: &str, uniforms: Vec<f32>, progress: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let a = solid(&mut b, red, 8.0);
        let bb = solid(&mut b, blue, 8.0);
        let op = NodeOp::Transition { type_id: type_id.into(), version: 1, uniforms, progress };
        let t = b.add(op, vec![a, bb], Rect::from_size(8.0, 8.0), true);
        let out = composite(&mut b, 8, [0.0, 0.0, 0.0, 1.0], &[(t, BlendMode::Normal)]);
        render_one(&mut r, &b.finish(out))
    };
    let px = run("oa.transition.crossfade", vec![], 0.25);
    assert!(close(px[0], [0.75, 0.0, 0.25, 1.0], 0.01), "{:?}", px[0]);
    assert!(close(run("oa.transition.crossfade", vec![], 0.0)[0], red, 0.001));
    assert!(close(run("oa.transition.crossfade", vec![], 1.0)[0], blue, 0.001));

    // Dip to black: black in the middle.
    let px = run("oa.transition.dip", vec![0.0, 0.0, 0.0, 1.0], 0.5);
    assert!(close(px[0], [0.0, 0.0, 0.0, 1.0], 0.01), "{:?}", px[0]);

    // Left-to-right wipe halfway, hard edge: blue on the left, red on the right.
    let px = run("oa.transition.wipe", vec![0.0, 0.0], 0.5);
    let row = &px[4 * 8..5 * 8];
    assert!(close(row[0], blue, 0.01) && close(row[7], red, 0.01), "{row:?}");

    // Push left (180°) halfway: the right half is the incoming picture.
    let px = run("oa.transition.push", vec![180.0], 0.5);
    let row = &px[4 * 8..5 * 8];
    assert!(close(row[1], red, 0.01) && close(row[6], blue, 0.01), "{row:?}");
}

/// A red clip then a blue one with a 1 s cross dissolve on the cut: planned from the
/// document and rendered, the cut's midpoint is half of each, and the fade from black at
/// the start runs from the background.
#[test]
fn planned_crossfade_mixes_adjacent_clips() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let secs = Time::from_seconds_f64;
    let (seq_id, track_id) = (SeqId(1), TrackId(2));
    let p = AspectPreset::by_id("square-1x1").unwrap();
    let variant = FormatVariant { id: VariantId(3), name: p.name.into(), size: CanvasSize { width: 16, height: 16 }, overrides: BTreeMap::new() };
    let mut seq = Sequence::new(seq_id, "s", FrameRate::FPS_30, variant);
    let mut track = Track::new(track_id, "V1", TrackKind::Video);
    for (id, start, color) in [(10, 0.0, [1.0, 0.0, 0.0, 1.0]), (11, 2.0, [0.0, 0.0, 1.0, 1.0])] {
        let mut item = Item::new(ItemId(id), "solid", ItemKind::Solid, TimeRange::new(secs(start), secs(2.0)));
        item.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color(color)));
        track.items.push(item);
    }
    track.items[1].transition_in = Some(oa_doc::Transition::new("oa.transition.crossfade", secs(1.0)));
    track.items[0].transition_in = Some(oa_doc::Transition::new("oa.transition.crossfade", secs(0.5)));
    seq.tracks.push(Arc::new(track));
    let mut project = Project::new("t");
    project.sequences.insert(seq_id, Arc::new(seq));

    let registry = Registry::with_builtins();
    let mut at = |t: f64| {
        let plan = plan_frame(&project, seq_id, secs(t), &PlanOptions::default(), &registry).unwrap();
        let g = optimize(&plan.graph, OptLevel::Full, KeyContext::default());
        let img = r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
        read_linear(r.context(), &img).unwrap()[8 * 16 + 8]
    };
    assert!(close(at(2.0), [0.5, 0.0, 0.5, 1.0], 0.01), "midpoint of the dissolve: {:?}", at(2.0));
    assert!(close(at(1.0), [1.0, 0.0, 0.0, 1.0], 0.001), "before the transition");
    assert!(close(at(3.0), [0.0, 0.0, 1.0, 1.0], 0.001), "after it");
    // Fade in from nothing over the first half second: half-way is half red on black.
    assert!(close(at(0.25), [0.5, 0.0, 0.0, 1.0], 0.01), "fade in: {:?}", at(0.25));
}

/// A white solid clip with effects in each role, planned from the document: a fade in
/// is half-way at 0.25 s, gone at 1 s; a zoom out has shrunk the layer to nothing by
/// the clip's end; a passive fade does nothing.
#[test]
fn effect_roles_follow_their_windows() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let secs = Time::from_seconds_f64;
    let p = AspectPreset::by_id("square-1x1").unwrap();
    let variant = FormatVariant { id: VariantId(3), name: p.name.into(), size: CanvasSize { width: 16, height: 16 }, overrides: BTreeMap::new() };
    let project_with = |effects: Vec<EffectInstance>| {
        let mut seq = Sequence::new(SeqId(1), "s", FrameRate::FPS_30, variant.clone());
        let mut track = Track::new(TrackId(2), "V1", TrackKind::Video);
        let mut item = Item::new(ItemId(10), "white", ItemKind::Solid, TimeRange::new(Time::ZERO, secs(2.0)));
        item.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([1.0, 1.0, 1.0, 1.0])));
        item.effects = effects;
        track.items.push(item);
        seq.tracks.push(Arc::new(track));
        let mut project = Project::new("t");
        project.sequences.insert(SeqId(1), Arc::new(seq));
        project
    };
    let registry = Registry::with_builtins();
    let mut at = |project: &Project, t: f64, px: usize| {
        let plan = plan_frame(project, SeqId(1), secs(t), &PlanOptions::default(), &registry).unwrap();
        let g = optimize(&plan.graph, OptLevel::Full, KeyContext::default());
        let img = r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
        read_linear(r.context(), &img).unwrap()[px]
    };
    let center = 8 * 16 + 8;

    let mut fade_in = EffectInstance::new(EffectId(20), "oa.anim.fade");
    fade_in.role = EffectRole::In { duration: secs(0.5) };
    let p = project_with(vec![fade_in]);
    assert!(close(at(&p, 0.25, center), [0.5, 0.5, 0.5, 1.0], 0.02), "half-way in: {:?}", at(&p, 0.25, center));
    assert!(close(at(&p, 1.0, center), [1.0, 1.0, 1.0, 1.0], 0.001), "after its window it's gone");

    let mut zoom_out = EffectInstance::new(EffectId(21), "oa.anim.zoom");
    zoom_out.role = EffectRole::Out { duration: secs(1.0) };
    zoom_out.params.set("scale", ParamSource::Static(Value::Float(0.0)));
    let p = project_with(vec![zoom_out]);
    assert!(close(at(&p, 0.5, 0), [1.0, 1.0, 1.0, 1.0], 0.001), "before the out window");
    let late = at(&p, 1.97, 0);
    assert!(late[0] < 0.05, "the corner is uncovered as the layer zooms away: {late:?}");

    let p = project_with(vec![EffectInstance::new(EffectId(22), "oa.anim.fade")]);
    assert!(close(at(&p, 0.1, center), [1.0, 1.0, 1.0, 1.0], 0.001), "passive: fully visible");
}

/// A mask effect reads its matte from a second input: a 25%-gray matte leaves a quarter
/// of the layer; inverted, three quarters; with no matte connected, all of it.
#[test]
fn mask_effect_uses_its_media_input() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let registry = Registry::with_builtins();
    let d = registry.effect("oa.mask.media").unwrap().clone();
    let mut run = |connected: bool, invert: bool| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let layer = solid(&mut b, [1.0, 0.0, 0.0, 1.0], 8.0);
        let matte = solid(&mut b, [0.25, 0.25, 0.25, 1.0], 8.0);
        let mut uniforms = vec![connected as u8 as f32, 0.0, invert as u8 as f32];
        uniforms.extend(oa_graph::registry::STILL_CLOCK);
        let op = NodeOp::Effect {
            type_id: d.type_id.clone(),
            version: 1,
            kind: d.kind.clone(),
            space: d.space,
            fusible: false,
            stateful: false,
            uniforms,
            nearest: false,
        };
        let inputs = if connected { vec![layer, matte] } else { vec![layer] };
        let e = b.add(op, inputs, Rect::from_size(8.0, 8.0), false);
        let out = composite(&mut b, 8, [0.0, 0.0, 0.0, 1.0], &[(e, BlendMode::Normal)]);
        render_one(&mut r, &b.finish(out))[4 * 8 + 4]
    };
    assert!(close(run(true, false), [0.25, 0.0, 0.0, 1.0], 0.01), "{:?}", run(true, false));
    assert!(close(run(true, true), [0.75, 0.0, 0.0, 1.0], 0.01), "{:?}", run(true, true));
    assert!(close(run(false, false), [1.0, 0.0, 0.0, 1.0], 0.001));
}

/// Every effect the UI offers must visibly do something: rendered on a test-pattern clip
/// with its numbers pushed off their defaults (intros/outros half-way through), each
/// one changes the picture. Catches effects that compile but quietly do nothing.
#[test]
fn every_offered_effect_changes_the_picture() {
    use oa_graph::registry::EffectUsage;
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut render = |p: &Project, at: Time| {
        let plan = plan_frame(p, SeqId(1), at, &PlanOptions { render_scale: 0.5, ..Default::default() }, &registry).unwrap();
        let g = optimize(&plan.graph, OptLevel::Full, KeyContext::default());
        let img = r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
        read_linear(r.context(), &img).unwrap()
    };
    // Two moments: an effect that moves on its own (wobble, scanlines rolling) can sit
    // at a zero crossing at any one instant, and that isn't a fault.
    let moments = [Time::from_seconds(2), Time::from_seconds_f64(2.37)];
    let (base, _) = pattern_project(|_| {});
    let references: Vec<Vec<[f32; 4]>> = moments.iter().map(|t| render(&base, *t)).collect();
    let mut checked = Vec::new();
    for usage in [EffectUsage::Passive, EffectUsage::InOut] {
        for d in registry.offered(usage) {
            // These only draw outside the picture, and this clip fills the frame; they have
            // a test of their own (`shadow_and_stroke_draw_around_the_picture`).
            if [oa_graph::registry::SHADOW, "oa.stylize.stroke"].contains(&d.type_id.as_ref()) {
                continue;
            }
            let (p, _) = pattern_project(|clip| {
                let mut fx = EffectInstance::new(EffectId(90), &d.type_id);
                for s in &d.params {
                    let value = match (&s.default, s.range) {
                        (Value::Float(_), Some((lo, hi))) => Value::Float(lo + (hi - lo) * 0.75),
                        (Value::Float(x), None) => Value::Float(if *x == 0.0 { 45.0 } else { x * 2.0 }),
                        // The pattern clip itself as a matte.
                        (Value::Media(_), _) => Value::Media(Some(2)),
                        // Points and offsets: moved a little.
                        (Value::Vec2(_), _) => Value::Vec2([0.1, -0.05]),
                        // The clip fills the frame: a glow behind it has nowhere to show.
                        (Value::Enum(_), _) if d.type_id.as_ref() == oa_graph::registry::GLOW => Value::Enum("over".into()),
                        _ => continue,
                    };
                    fx.params.set(s.id.as_str(), ParamSource::Static(value));
                }
                if usage == EffectUsage::InOut {
                    // A 4 s intro, 2 s in: half-way.
                    fx.role = EffectRole::In { duration: Time::from_seconds(4) };
                }
                clip.effects.push(fx);
            });
            let diff = moments
                .iter()
                .zip(&references)
                .map(|(t, reference)| {
                    let out = render(&p, *t);
                    out.iter().zip(reference).map(|(a, b)| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()).sum::<f32>()
                        / out.len() as f32
                })
                .fold(0.0f32, f32::max);
            assert!(diff > 1e-3, "{} ({:?}) left the picture unchanged (mean diff {diff})", d.name, usage);
            checked.push(d.name.clone());
        }
    }
    eprintln!("checked: {checked:?}");
    assert!(checked.len() >= 12, "{checked:?}");
}

/// A 640×360 canvas with one text clip (white "HELLO", 100 px) over black.
fn text_project(configure: impl FnOnce(&mut Item)) -> (Project, SeqId) {
    let seq_id = SeqId(1);
    let mut p = Project::new("text");
    let v = FormatVariant { id: VariantId(3), name: "16:9".into(), size: CanvasSize { width: 640, height: 360 }, overrides: BTreeMap::new() };
    let mut seq = Sequence::new(seq_id, "Main", FrameRate::FPS_30, v);
    let mut track = Track::new(TrackId(5), "V1", TrackKind::Video);
    let mut clip = Item::new(ItemId(6), "title", ItemKind::Text, TimeRange::new(Time::ZERO, Time::from_seconds(10)));
    clip.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text("HELLO".into())));
    clip.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(100.0)));
    configure(&mut clip);
    track.items.push(clip);
    seq.tracks.push(Arc::new(track));
    p.sequences.insert(seq_id, Arc::new(seq));
    (p, seq_id)
}

fn render_project(r: &mut Renderer, p: &Project, at: Time) -> Vec<[f32; 4]> {
    let registry = Registry::with_builtins();
    let plan = plan_frame(p, SeqId(1), at, &PlanOptions::default(), &registry).unwrap();
    let g = optimize(&plan.graph, OptLevel::Full, KeyContext::default());
    let img = r.render(&g, &registry, &mut TestPatternSource::default()).unwrap();
    assert!(r.stats.unsupported.is_empty(), "{:?}", r.stats.unsupported);
    read_linear(r.context(), &img).unwrap()
}

/// Coverage-weighted center and total of bright pixels on a 640-wide frame.
fn ink(px: &[[f32; 4]]) -> ([f32; 2], f32) {
    let (mut sx, mut sy, mut total) = (0.0, 0.0, 0.0);
    for (i, p) in px.iter().enumerate() {
        let w = (p[0] + p[1] + p[2]) / 3.0;
        sx += (i % 640) as f32 * w;
        sy += (i / 640) as f32 * w;
        total += w;
    }
    ([sx / total.max(1e-6), sy / total.max(1e-6)], total)
}

#[test]
fn text_layers_draw_their_glyphs_centered() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (p, _) = text_project(|_| {});
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let ([cx, cy], total) = ink(&px);
    // Five 100 px capitals: a good few thousand fully covered pixels, centered.
    assert!(total > 4000.0 && total < 40000.0, "{total}");
    assert!((cx - 320.0).abs() < 12.0 && (cy - 180.0).abs() < 20.0, "center {cx},{cy}");
    // Edges are anti-aliased: some pixels are partly covered.
    assert!(px.iter().any(|p| p[0] > 0.1 && p[0] < 0.9));
    // Corners stay black.
    assert!(close(px[0], [0.0, 0.0, 0.0, 1.0], 1e-3));

    // Twice the size: about four times the ink.
    let (big, _) = text_project(|c| {
        c.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(200.0)));
    });
    let (_, big_total) = ink(&render_project(&mut r, &big, Time::from_seconds(1)));
    assert!((big_total / total - 4.0).abs() < 0.6, "{}", big_total / total);
}

/// A bounded per-letter effect only changes the letters in its range: rainbow on the
/// first half of "HELLO" tints the left letters and leaves the right ones white.
#[test]
fn bounded_text_effects_only_touch_their_letters() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (p, _) = text_project(|c| {
        let mut fx = EffectInstance::new(EffectId(40), "oa.text.rainbow");
        fx.params.set(schema::BOUNDED, ParamSource::Static(Value::Bool(true)));
        fx.params.set(schema::BOUND_START, ParamSource::Static(Value::Float(0.0)));
        fx.params.set(schema::BOUND_END, ParamSource::Static(Value::Float(50.0)));
        c.effects.push(fx);
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    // How far from grey the ink is, on each side.
    let tint = |x0: usize, x1: usize| {
        let (mut spread, mut ink) = (0.0, 0.0);
        for (i, c) in px.iter().enumerate() {
            let x = i % 640;
            if x >= x0 && x < x1 && c[3] > 0.5 {
                let (hi, lo) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
                spread += hi - lo;
                ink += hi;
            }
        }
        spread / ink.max(1e-6)
    };
    let (left, right) = (tint(0, 250), tint(420, 640));
    assert!(left > 0.3, "the first letters are tinted: {left}");
    assert!(right < 0.05, "the last ones stay white: {right}");
}

/// Any effect on a title can be bounded: Color To turns the first half of "HELLO" red
/// and leaves the rest white; a bounded blur softens only its letters.
#[test]
fn bounded_picture_effects_on_titles() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let bound = |fx: &mut EffectInstance| {
        fx.params.set(schema::BOUNDED, ParamSource::Static(Value::Bool(true)));
        fx.params.set(schema::BOUND_START, ParamSource::Static(Value::Float(0.0)));
        fx.params.set(schema::BOUND_END, ParamSource::Static(Value::Float(50.0)));
    };
    let (p, _) = text_project(|c| {
        let mut fx = EffectInstance::new(EffectId(41), "oa.color.color-to");
        fx.params.set("from", ParamSource::Static(Value::Color([1.0, 1.0, 1.0, 1.0])));
        fx.params.set("to", ParamSource::Static(Value::Color([1.0, 0.0, 0.0, 1.0])));
        fx.params.set("offset_from_original", ParamSource::Static(Value::Bool(false)));
        bound(&mut fx);
        c.effects.push(fx);
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let color = |x0: usize, x1: usize| {
        let mut sum = [0.0f32; 3];
        for (i, c) in px.iter().enumerate() {
            if (x0..x1).contains(&(i % 640)) && c[3] > 0.9 && c[0] > 0.5 {
                for k in 0..3 {
                    sum[k] += c[k];
                }
            }
        }
        sum
    };
    let (left, right) = (color(0, 250), color(420, 640));
    assert!(left[0] > 10.0 && left[1] < 0.1 * left[0], "the first letters turn red: {left:?}");
    assert!(right[0] > 10.0 && (right[1] - right[0]).abs() < 0.05 * right[0], "the last stay white: {right:?}");

    // A blur bounded to the first half: soft edges there, crisp ones on the right.
    let (p, _) = text_project(|c| {
        let mut fx = EffectInstance::new(EffectId(42), "oa.blur.gaussian");
        fx.params.set("radius", ParamSource::Static(Value::Float(12.0)));
        bound(&mut fx);
        c.effects.push(fx);
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let soft = |x0: usize, x1: usize| {
        let (mut partial, mut ink) = (0.0, 0.0);
        for (i, c) in px.iter().enumerate() {
            if (x0..x1).contains(&(i % 640)) {
                ink += c[0];
                if c[0] > 0.05 && c[0] < 0.95 {
                    partial += 1.0;
                }
            }
        }
        partial / ink.max(1.0)
    };
    let (left, right) = (soft(0, 250), soft(420, 640));
    assert!(left > 3.0 * right, "blurred on the left only: {left} vs {right}");
}

#[test]
fn text_color_gradient_and_outline() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (p, _) = text_project(|c| {
        c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Color([1.0, 0.0, 0.0, 1.0])));
        c.params.set(schema::TEXT_GRADIENT, ParamSource::Static(Value::Bool(true)));
        c.params.set(schema::TEXT_COLOR2, ParamSource::Static(Value::Color([0.0, 0.0, 1.0, 1.0])));
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let (mut top, mut bottom) = ([0.0f32; 3], [0.0f32; 3]);
    for (i, c) in px.iter().enumerate() {
        let row = if i / 640 < 180 { &mut top } else { &mut bottom };
        for k in 0..3 {
            row[k] += c[k];
        }
    }
    assert!(top[0] > top[2] && bottom[2] > bottom[0], "red on top, blue below: {top:?} {bottom:?}");

    let (plain, _) = text_project(|_| {});
    let (outlined, _) = text_project(|c| {
        c.params.set(schema::TEXT_OUTLINE, ParamSource::Static(Value::Float(8.0)));
        c.params.set(schema::TEXT_OUTLINE_COLOR, ParamSource::Static(Value::Color([0.0, 1.0, 0.0, 1.0])));
    });
    let a = render_project(&mut r, &plain, Time::from_seconds(1));
    let b = render_project(&mut r, &outlined, Time::from_seconds(1));
    let green = |px: &[[f32; 4]]| px.iter().filter(|p| p[1] > 0.5 && p[0] < 0.2).count();
    assert_eq!(green(&a), 0);
    assert!(green(&b) > 1000, "{}", green(&b));
}

#[test]
fn every_text_effect_changes_the_text() {
    use oa_graph::registry::EffectUsage;
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let at = Time::from_seconds(1);
    // Blue text, so white highlights show.
    let blue = |c: &mut Item| c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Color([0.2, 0.4, 1.0, 1.0])));
    let (base, _) = text_project(|c| {
        blue(c);
    });
    let reference = render_project(&mut r, &base, at);
    let mut checked = Vec::new();
    for usage in [EffectUsage::Passive, EffectUsage::InOut] {
        for d in registry.offered_for(usage, true).into_iter().filter(|d| d.kind.text_only()) {
            let (p, _) = text_project(|clip| {
                blue(clip);
                let mut fx = EffectInstance::new(EffectId(90), &d.type_id);
                // A black box behind the text doesn't show on the black background.
                if d.kind == oa_graph::EffectKind::TextBox {
                    fx.params.set("color", ParamSource::Static(Value::Color([1.0, 1.0, 1.0, 1.0])));
                }
                if usage == EffectUsage::InOut {
                    fx.role = EffectRole::In { duration: Time::from_seconds(2) };
                }
                clip.effects.push(fx);
            });
            let out = render_project(&mut r, &p, at);
            let diff: f32 = out.iter().zip(&reference).map(|(a, b)| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()).sum::<f32>();
            assert!(diff > 50.0, "{} ({usage:?}) left the text unchanged (total diff {diff})", d.name);
            checked.push(d.name.clone());
        }
    }
    eprintln!("checked: {checked:?}");
    assert!(checked.len() >= 9, "{checked:?}");
    assert!(registry.offered(EffectUsage::Passive).iter().all(|d| !d.kind.text_only()), "text effects stay off picture clips");
}

#[test]
fn letters_rise_one_after_another() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (p, _) = text_project(|clip| {
        let mut fx = EffectInstance::new(EffectId(90), "oa.text.rise");
        fx.params.set("fade", ParamSource::Static(Value::Bool(false)));
        fx.params.set("distance", ParamSource::Static(Value::Float(1.0)));
        fx.role = EffectRole::In { duration: Time::from_seconds(2) };
        clip.effects.push(fx);
    });
    let rest = render_project(&mut r, &p, Time::from_seconds(3));
    let mid = render_project(&mut r, &p, Time::from_seconds(1));
    let half = |px: &[[f32; 4]], left: bool| {
        let (mut sy, mut t) = (0.0, 0.0);
        for (i, p) in px.iter().enumerate() {
            if (i % 640 < 320) == left {
                sy += (i / 640) as f32 * p[0];
                t += p[0];
            }
        }
        sy / t.max(1e-6)
    };
    // Half-way through, the first letters have (nearly) landed; the last are still low.
    let (rest_l, rest_r) = (half(&rest, true), half(&rest, false));
    let (mid_l, mid_r) = (half(&mid, true), half(&mid, false));
    assert!(mid_r - rest_r > mid_l - rest_l + 10.0, "left drop {} right drop {}", mid_l - rest_l, mid_r - rest_r);
    assert!(mid_l - rest_l >= -1.0);
}

/// Red → blue along `angle` over a text clip: which halves of the canvas lean red/blue.
fn halves(px: &[[f32; 4]], vertical: bool) -> ([f32; 3], [f32; 3]) {
    let (mut a, mut b) = ([0.0f32; 3], [0.0f32; 3]);
    for (i, c) in px.iter().enumerate() {
        let first = if vertical { i / 640 < 180 } else { i % 640 < 320 };
        let side = if first { &mut a } else { &mut b };
        for k in 0..3 {
            side[k] += c[k];
        }
    }
    (a, b)
}

#[test]
fn directional_gradients_follow_their_angle() {
    use oa_params::Gradient;
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (red, blue) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]);
    let text = |angle: f64| {
        text_project(|c| {
            c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Gradient(Gradient::two(angle, red, blue))));
        })
        .0
    };
    // 0°: red on the left, blue on the right.
    let (l, rt) = halves(&render_project(&mut r, &text(0.0), Time::from_seconds(1)), false);
    assert!(l[0] > l[2] && rt[2] > rt[0], "{l:?} {rt:?}");
    // 180°: flipped.
    let (l, rt) = halves(&render_project(&mut r, &text(180.0), Time::from_seconds(1)), false);
    assert!(l[2] > l[0] && rt[0] > rt[2], "{l:?} {rt:?}");
    // Three stops: green in the middle.
    let mut g = Gradient::two(0.0, red, blue);
    g.stops.insert(1, oa_params::GradientStop { pos: 0.5, color: [0.0, 1.0, 0.0, 1.0] });
    let (p, _) = text_project(|c| {
        c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Gradient(g.clone())));
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let mid: f32 = px.iter().enumerate().filter(|(i, _)| (i % 640).abs_diff(320) < 20).map(|(_, c)| c[1] - c[0] - c[2]).sum();
    assert!(mid > 10.0, "{mid}");

    // Tint takes a gradient too, across the clip: a white solid tinted red → blue.
    let (p, _) = text_project(|c| {
        c.kind = ItemKind::Solid;
        c.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([1.0; 4])));
        let mut tint = EffectInstance::new(EffectId(80), "oa.color.tint");
        tint.params.set("color", ParamSource::Static(Value::Gradient(Gradient::two(0.0, red, blue))));
        c.effects.push(tint);
    });
    let px = render_project(&mut r, &p, Time::from_seconds(1));
    let row = &px[180 * 640..181 * 640];
    assert!(row[5][0] > 0.15 && row[5][2] < 0.02, "left edge red: {:?}", row[5]);
    assert!(row[634][2] > 0.05 && row[634][0] < 0.02, "right edge blue: {:?}", row[634]);
}

#[test]
fn shimmer_lights_pictures_as_well_as_text() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let gray = |c: &mut Item| {
        c.kind = ItemKind::Solid;
        c.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([0.2, 0.2, 0.2, 1.0])));
    };
    let (plain, _) = text_project(gray);
    let (lit, _) = text_project(|c| {
        gray(c);
        let mut s = EffectInstance::new(EffectId(81), "oa.light.shimmer");
        s.params.set("direction", ParamSource::Static(Value::Float(0.0)));
        c.effects.push(s);
    });
    // At 1 s the band is at 0.6·1.8 − 0.4 = 0.68 of the way across (left to right).
    let a = render_project(&mut r, &plain, Time::from_seconds(1));
    let b = render_project(&mut r, &lit, Time::from_seconds(1));
    let at = |px: &[[f32; 4]], x: usize| px[180 * 640 + x][0];
    assert!(at(&b, 435) > at(&a, 435) + 0.3, "lit at the band: {} vs {}", at(&b, 435), at(&a, 435));
    assert!((at(&b, 100) - at(&a, 100)).abs() < 0.01, "dark away from it");
}

#[test]
fn chroma_key_removes_the_green_and_keeps_the_rest() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut run = |color: [f32; 4]| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, color, 8.0);
        // key (green), tolerance, softness, spill
        let e = effect(&mut b, &registry, s, "oa.key.chroma", vec![0.0, 1.0, 0.0, 1.0, 0.12, 0.08, 0.7], 0.0);
        let out = composite(&mut b, 8, [0.0, 0.0, 0.0, 0.0], &[(e, BlendMode::Normal)]);
        render_one(&mut r, &b.finish(out))[0]
    };
    assert!(run([0.0, 0.8, 0.0, 1.0])[3] < 0.01, "green screen keyed out");
    assert!(run([0.0, 0.2, 0.0, 1.0])[3] < 0.05, "shadows on the screen too");
    assert!(run([0.8, 0.5, 0.4, 1.0])[3] > 0.99, "skin kept");
    assert!(run([0.1, 0.1, 0.8, 1.0])[3] > 0.99, "blue kept");
}

#[test]
fn crop_makes_the_cut_edges_transparent() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 8.0);
    // left, right, top, bottom, size
    let e = effect(&mut b, &registry, s, oa_graph::registry::CROP, vec![0.25, 0.0, 0.0, 0.5, 8.0, 8.0], 0.0);
    let out = composite(&mut b, 8, [0.0, 0.0, 0.0, 0.0], &[(e, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    let a = |x: usize, y: usize| px[y * 8 + x][3];
    assert!(a(0, 0) < 0.01 && a(1, 1) < 0.01, "left quarter cut");
    assert!(a(4, 6) < 0.01, "bottom half cut");
    assert!(a(4, 1) > 0.99 && a(7, 3) > 0.99, "the rest kept");
}

#[test]
fn background_fill_and_tile() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);

    // A left-to-right gradient, red to blue.
    let mut g = oa_params::Gradient::solid([1.0, 0.0, 0.0, 1.0]);
    g.angle = 0.0;
    g.stops.push(oa_params::GradientStop { pos: 1.0, color: [0.0, 0.0, 1.0, 1.0] });
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [1.0; 4], 8.0);
    let e = effect(&mut b, &registry, s, oa_graph::registry::FILL, g.pack(), 0.0);
    let out = composite(&mut b, 8, [0.0; 4], &[(e, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    assert!(px[3 * 8][0] > 0.8 && px[3 * 8][2] < 0.2, "red on the left: {:?}", px[3 * 8]);
    assert!(px[3 * 8 + 7][2] > 0.8 && px[3 * 8 + 7][0] < 0.2, "blue on the right: {:?}", px[3 * 8 + 7]);

    // A 4×4 tile whose left half is cut away, repeated across 8 px: columns 2, 3, 6, 7.
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [1.0; 4], 4.0);
    let half = effect(&mut b, &registry, s, oa_graph::registry::CROP, vec![0.5, 0.0, 0.0, 0.0, 4.0, 4.0], 0.0);
    let mut u = vec![];
    u.extend(oa_graph::registry::STILL_CLOCK);
    let d = registry.effect(oa_graph::registry::TILE).unwrap();
    let op = NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms: u, nearest: false };
    let tiled = b.add(op, vec![half], Rect::from_size(8.0, 8.0), false);
    let out = composite(&mut b, 8, [0.0; 4], &[(tiled, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    let alpha: Vec<bool> = (0..8).map(|x| px[5 * 8 + x][3] > 0.5).collect();
    assert_eq!(alpha, [false, false, true, true, false, false, true, true]);
}

#[test]
fn pixelated_layers_keep_hard_edges() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    // A 2×1 picture (black | white, via a crop) enlarged 4× to 8×4.
    let mut run = |pixelated: bool| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [1.0; 4], 2.0);
        let half = effect(&mut b, &registry, s, oa_graph::registry::CROP, vec![0.5, 0.0, 0.0, 0.0, 2.0, 2.0], 0.0);
        let m = Affine2::scale(4.0, 4.0);
        let t = b.add(NodeOp::Transform { matrix: m }, vec![half], m.map_rect(Rect::from_size(2.0, 2.0)), false);
        let layers = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated }];
        let out = b.add(NodeOp::Composite { size: [8, 8], background: [0.0; 4], layers }, vec![t], Rect::from_size(8.0, 8.0), false);
        let px = render_one(&mut r, &b.finish(out));
        (0..8).map(|x| px[4 * 8 + x][3]).collect::<Vec<f32>>()
    };
    let crisp = run(true);
    assert!(crisp.iter().all(|a| *a < 0.01 || *a > 0.99), "only fully on or off: {crisp:?}");
    let smooth = run(false);
    assert!(smooth.iter().any(|a| *a > 0.05 && *a < 0.95), "blended edge: {smooth:?}");
}

/// A plugin's shader goes through the same path as a built-in: the example plugin in
/// the repo darkens the corners of a white frame.
#[test]
fn plugin_effects_render() {
    let Some(ctx) = gpu() else { return };
    let manifest = std::path::Path::new("../../plugins/example-looks/plugin.json");
    let plugin = oa_graph::plugin::load(manifest).expect("example plugin loads");
    assert!(plugin.issues.is_empty(), "{:?}", plugin.issues);
    let core = oa_graph::plugin::core();
    let (registry, issues) = Registry::from_plugins([&core, &plugin]);
    assert!(issues.is_empty(), "{issues:?}");

    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 16.0);
    // amount, size
    let e = effect(&mut b, &registry, s, "com.example.vignette", vec![1.0, 0.6], 0.0);
    let out = composite(&mut b, 16, [0.0; 4], &[(e, BlendMode::Normal)]);
    let px = render_with(&mut r, &b.finish(out), &registry);
    let at = |x: usize, y: usize| px[y * 16 + x][0];
    assert!(at(8, 8) > 0.95, "middle untouched: {}", at(8, 8));
    assert!(at(0, 0) < 0.5, "corner darkened: {}", at(0, 0));
}


/// Color To turns red blue and leaves green alone; with "offset from original", a darker
/// red becomes a darker blue.
#[test]
fn color_to_replaces_a_color_and_keeps_its_shading() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut through = |color: [f32; 4], offset: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, color, 8.0);
        // from, tolerance, softness, to, offset from original
        let e = effect(&mut b, &registry, s, "oa.color.color-to", vec![1.0, 0.0, 0.0, 1.0, 0.3, 0.05, 0.0, 0.0, 1.0, 1.0, offset], 0.0);
        let out = composite(&mut b, 8, [0.0; 4], &[(e, BlendMode::Normal)]);
        render_with(&mut r, &b.finish(out), &registry)[36]
    };
    let red = through([1.0, 0.0, 0.0, 1.0], 0.0);
    assert!(red[2] > 0.95 && red[0] < 0.05, "red became blue: {red:?}");
    let green = through([0.0, 1.0, 0.0, 1.0], 0.0);
    assert!(green[1] > 0.95 && green[2] < 0.05, "green stays: {green:?}");
    // A red a little darker (in display terms): replaced, and as much darker.
    let dark = through([0.6, 0.0, 0.0, 1.0], 1.0);
    let flat = through([0.6, 0.0, 0.0, 1.0], 0.0);
    assert!(dark[2] > 0.3 && dark[2] < 0.8 && dark[0] < 0.05, "a darker blue: {dark:?}");
    assert!(flat[2] > 0.95, "without the offset, just blue: {flat:?}");
}

/// Effects on pixel art read whole picture pixels: a shader that samples half a pixel
/// off keeps the hard edge instead of blending the two colors.
#[test]
fn effects_on_pixel_art_do_not_blend_pixels() {
    let Some(ctx) = gpu() else { return };
    let mut registry = Registry::with_builtins();
    registry
        .register(oa_graph::registry::EffectDescriptor {
            shader: Some(oa_graph::EffectShader {
                entry: "test_halfpixel".into(),
                source: "fn test_halfpixel(pos: vec2f, base: u32) -> vec4f { return sample_input(pos + vec2f(0.5, 0.0)); }".into(),
                passes: 1,
            }),
            ..oa_graph::registry::EffectDescriptor::new("test.halfpixel", "Half pixel", EffectKind::Spatial { expand: None }, Vec::new())
        })
        .expect("test effect");

    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut run = |nearest: bool| {
        let mut b = GraphBuilder::new(KeyContext::default());
        // A 4×4 picture whose left half is cut away: a hard edge down the middle.
        let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 4.0);
        let half = effect(&mut b, &registry, s, oa_graph::registry::CROP, vec![0.5, 0.0, 0.0, 0.0, 4.0, 4.0], 0.0);
        let d = registry.effect("test.halfpixel").unwrap();
        let op = NodeOp::Effect {
            type_id: d.type_id.clone(),
            version: d.version,
            kind: d.kind.clone(),
            space: d.space,
            fusible: false,
            stateful: false,
            uniforms: oa_graph::registry::STILL_CLOCK.to_vec(),
            nearest,
        };
        let shifted = b.add(op, vec![half], Rect::from_size(4.0, 4.0), false);
        let out = composite(&mut b, 4, [0.0; 4], &[(shifted, BlendMode::Normal)]);
        let px = render_with(&mut r, &b.finish(out), &registry);
        (0..4).map(|x| px[2 * 4 + x][3]).collect::<Vec<f32>>()
    };
    let crisp = run(true);
    assert!(crisp.iter().all(|a| *a < 0.01 || *a > 0.99), "whole pixels only: {crisp:?}");
    let smoothed = run(false);
    assert!(smoothed.iter().any(|a| *a > 0.05 && *a < 0.95), "smoothing blends the edge: {smoothed:?}");
}

/// Depth turns a picture into a sheet with thickness: turned to the side you see its
/// cut edge (darker than the face), and nothing where the sheet no longer covers.
#[test]
fn depth_extrudes_a_sheet() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let run = |r: &mut Renderer, depth: f32, yaw: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 64.0);
        // depth %, yaw, pitch, pov, map, back
        let e = effect(&mut b, &registry, s, "oa.depth.slab", vec![depth, yaw, 0.0, 0.4, 0.0, 0.45], 0.0);
        let out = composite(&mut b, 64, [0.0; 4], &[(e, BlendMode::Normal)]);
        render_one(r, &b.finish(out))
    };
    let flat = run(&mut r, 0.0, 0.0);
    assert!(flat[32 * 64 + 32][0] > 0.9, "face-on and flat: the picture as it was");

    let turned = run(&mut r, 25.0, 45.0);
    let row: Vec<[f32; 4]> = (0..64).map(|x| turned[32 * 64 + x]).collect();
    let lit = row.iter().filter(|p| p[3] > 0.5 && p[0] > 0.9).count();
    let edge = row.iter().filter(|p| p[3] > 0.5 && p[0] < 0.8).count();
    let empty = row.iter().filter(|p| p[3] < 0.5).count();
    assert!(lit > 4, "the face is still there: {lit}");
    assert!(edge > 0, "its cut edge shows: {edge}");
    assert!(empty > 4, "turning it leaves space beside it: {empty}");

    // Turned all the way round: the back of the sheet, at the back's brightness.
    let back = run(&mut r, 8.0, 180.0);
    let c = back[32 * 64 + 32];
    assert!(c[3] > 0.9 && (c[0] - 0.45).abs() < 0.05, "the back faces the camera: {c:?}");
}


/// Surface stretches the picture over its points: a corner pulled in leaves empty
/// space behind it; pulled out (with the room `grown_bounds` gives), the picture
/// reaches past where the layer was.
#[test]
fn surface_moves_the_picture_with_its_points() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let run = |r: &mut Renderer, corner: [f32; 2], room: f64| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 64.0);
        // Corners grid; the bottom-right point (row 1, column 1) moved.
        let mut uniforms = vec![0.0; 33];
        uniforms[1 + 2 * 5] = corner[0];
        uniforms[2 + 2 * 5] = corner[1];
        let e = effect(&mut b, &registry, s, oa_graph::registry::SURFACE, uniforms, room);
        let out = composite(&mut b, 128, [0.0; 4], &[(e, BlendMode::Normal)]);
        render_one(r, &b.finish(out))
    };
    let at = |px: &[[f32; 4]], x: usize, y: usize| px[y * 128 + x][3];
    let rest = run(&mut r, [0.0, 0.0], 0.0);
    assert!(at(&rest, 60, 60) > 0.99 && at(&rest, 70, 70) < 0.01, "at rest: the layer as it was");

    let pulled_in = run(&mut r, [-0.5, -0.5], 0.0);
    assert!(at(&pulled_in, 4, 4) > 0.99, "the other corners stay");
    assert!(at(&pulled_in, 56, 56) < 0.01, "behind the moved corner is empty");
    assert!(at(&pulled_in, 50, 4) > 0.99 && at(&pulled_in, 4, 50) > 0.99, "the edges bend to it");
    assert!(at(&pulled_in, 60, 8) < 0.01, "and are cut on a slant");

    let pulled_out = run(&mut r, [0.25, 0.25], 32.0);
    assert!(at(&pulled_out, 70, 70) > 0.99, "past the layer's old corner");
    assert!(at(&pulled_out, 90, 90) < 0.01, "but not past the point");
}

/// An effect node with its clock at `seconds` (a passive effect's live clock).
fn effect_at(b: &mut GraphBuilder, registry: &Registry, input: NodeId, type_id: &str, mut uniforms: Vec<f32>, seconds: f32) -> NodeId {
    let d = registry.effect(type_id).unwrap();
    uniforms.extend([1.0, 0.0, seconds]);
    let bounds = b.node(input).bounds;
    let op = NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms, nearest: false };
    b.add(op, vec![input], bounds, false)
}

/// Tile repeats the picture in a grid; Scroll slides it and wraps it round; together,
/// scrolling a tiled picture by one cell gives back the same picture — no seam.
#[test]
fn tile_and_scroll_work_together() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    // 64 × 64: white in the top-left quarter, black elsewhere.
    let picture = |b: &mut GraphBuilder| {
        let white = solid(b, [1.0, 1.0, 1.0, 1.0], 32.0);
        composite(b, 64, [0.0, 0.0, 0.0, 1.0], &[(white, BlendMode::Normal)])
    };
    // tile: columns, rows, gap, mirror · scroll: direction, speed, edges (0 = wrap)
    let run = |r: &mut Renderer, tile: bool, shift: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let mut n = picture(&mut b);
        if tile {
            n = effect_at(&mut b, &registry, n, oa_graph::registry::TILE_EFFECT, vec![2.0, 2.0, 0.0, 0.0], 0.0);
        }
        if shift != 0.0 {
            // Right at `shift` px a second, one second in.
            n = effect_at(&mut b, &registry, n, oa_graph::registry::SCROLL, vec![0.0, shift, 0.0], 1.0);
        }
        let out = composite(&mut b, 64, [0.0; 4], &[(n, BlendMode::Normal)]);
        render_one(r, &b.finish(out))
    };
    let lum = |px: &[[f32; 4]], x: usize, y: usize| px[y * 64 + x][0];

    let tiled = run(&mut r, true, 0.0);
    for (x, white) in [(8, true), (24, false), (40, true), (56, false)] {
        assert_eq!(lum(&tiled, x, 8) > 0.9, white, "tile at x = {x}");
    }
    assert!(lum(&tiled, 40, 40) > 0.9, "and down the rows");

    let scrolled = run(&mut r, false, 16.0);
    assert!(lum(&scrolled, 40, 8) > 0.9 && lum(&scrolled, 8, 8) < 0.1, "moved right by 16 px");
    let wrapped = run(&mut r, false, 48.0);
    assert!(lum(&wrapped, 8, 8) > 0.9, "what goes off the right comes back on the left");

    let both = run(&mut r, true, 16.0);
    assert!(lum(&both, 24, 8) > 0.9 && lum(&both, 8, 8) < 0.1, "the tiles scroll");
    let one_cell = run(&mut r, true, 32.0);
    let worst = one_cell.iter().zip(&tiled).map(|(a, b)| (a[0] - b[0]).abs().max((a[3] - b[3]).abs())).fold(0.0f32, f32::max);
    assert!(worst < 0.02, "scrolling a tiled picture by one cell gives it back, seams and all ({worst})");
}

/// Glow: every pixel outside the picture takes the color of the picture's nearest edge
/// pixel, fading with the distance to it; the picture itself stays exactly as it was.
#[test]
fn glow_takes_the_nearest_edge_color_and_fades_out() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let d = registry.effect(oa_graph::registry::GLOW).unwrap();
    let mut b = GraphBuilder::new(KeyContext::default());
    // 32 × 32: red, with its top-right quarter blue.
    let red = solid(&mut b, [1.0, 0.0, 0.0, 1.0], 32.0);
    let blue = solid(&mut b, [0.0, 0.0, 1.0, 1.0], 16.0);
    let m = Affine2::translate(16.0, 0.0);
    let blue = b.add(NodeOp::Transform { matrix: m }, vec![blue], m.map_rect(b.node(blue).bounds), true);
    let infos = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }; 2];
    let picture = b.add(NodeOp::Composite { size: [32, 32], background: [0.0; 4], layers: infos }, vec![red, blue], Rect::from_size(32.0, 32.0), true);
    // radius 16, strength 1, white tint, behind; the second input is the picture itself.
    let mut uniforms = vec![16.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0];
    uniforms.extend(oa_graph::registry::STILL_CLOCK);
    let op = NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms, nearest: false };
    let glow = b.add(op, vec![picture, picture], b.node(picture).bounds.expand(16.0), false);
    let m = Affine2::translate(32.0, 32.0);
    let placed = b.add(NodeOp::Transform { matrix: m }, vec![glow], m.map_rect(b.node(glow).bounds), false);
    let out = composite(&mut b, 96, [0.0, 0.0, 0.0, 1.0], &[(placed, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    let at = |x: usize, y: usize| px[y * 96 + x];

    assert!(close(at(40, 56), [1.0, 0.0, 0.0, 1.0], 1e-3), "inside: untouched {:?}", at(40, 56));
    assert!(close(at(63, 33), [0.0, 0.0, 1.0, 1.0], 1e-3), "sharp right to the edge {:?}", at(63, 33));
    // Right of the blue corner it glows blue; right of the red part, red.
    let (by_blue, by_red) = (at(66, 40), at(66, 56));
    assert!(by_blue[2] > 0.3 && by_blue[0] < 0.01, "blue beside blue: {by_blue:?}");
    assert!(by_red[0] > 0.3 && by_red[2] < 0.01, "red beside red: {by_red:?}");
    // Fading with distance, and gone past the radius.
    let fade: Vec<f32> = [65, 70, 75, 79].iter().map(|&x| at(x, 56)[0]).collect();
    assert!(fade.windows(2).all(|w| w[0] > w[1]), "fades outwards: {fade:?}");
    assert!(at(82, 56)[0] < 1e-3 && at(4, 4)[0] < 1e-3, "nothing past the radius");
    // Round the corner it's round: about as bright diagonally as straight out at the
    // same distance.
    let (straight, diagonal) = (at(71, 56)[0], at(68, 25)[2]);
    assert!((straight - diagonal).abs() < 0.06, "a radial falloff (the blue corner, 7.9 px away, vs red 7.5 px out): {straight} vs {diagonal}");
}

/// A wide glow finds its edges on a coarse grid (4 × 4 pixels a cell at 64 px) and still
/// takes the nearest edge's color, fades smoothly and stops at the radius.
#[test]
fn a_wide_glow_on_a_coarse_grid_still_follows_the_edges() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let d = registry.effect(oa_graph::registry::GLOW).unwrap();
    assert_eq!(d.pass_divisor.as_ref().unwrap().divisor(&[64.0], 0, 99), 4);
    let mut b = GraphBuilder::new(KeyContext::default());
    let red = solid(&mut b, [1.0, 0.0, 0.0, 1.0], 32.0);
    let blue = solid(&mut b, [0.0, 0.0, 1.0, 1.0], 16.0);
    let m = Affine2::translate(16.0, 0.0);
    let blue = b.add(NodeOp::Transform { matrix: m }, vec![blue], m.map_rect(b.node(blue).bounds), true);
    let infos = vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }; 2];
    let picture = b.add(NodeOp::Composite { size: [32, 32], background: [0.0; 4], layers: infos }, vec![red, blue], Rect::from_size(32.0, 32.0), true);
    let mut uniforms = vec![64.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0];
    uniforms.extend(oa_graph::registry::STILL_CLOCK);
    let op = NodeOp::Effect { type_id: d.type_id.clone(), version: d.version, kind: d.kind.clone(), space: d.space, fusible: d.fusible, stateful: false, uniforms, nearest: false };
    let glow = b.add(op, vec![picture, picture], b.node(picture).bounds.expand(64.0), false);
    let m = Affine2::translate(80.0, 80.0);
    let placed = b.add(NodeOp::Transform { matrix: m }, vec![glow], m.map_rect(b.node(glow).bounds), false);
    let out = composite(&mut b, 192, [0.0, 0.0, 0.0, 1.0], &[(placed, BlendMode::Normal)]);
    let px = render_one(&mut r, &b.finish(out));
    let at = |x: usize, y: usize| px[y * 192 + x];
    // The picture spans 80..112; its top-right quarter is blue.
    assert!(close(at(111, 81), [0.0, 0.0, 1.0, 1.0], 1e-3) && close(at(90, 105), [1.0, 0.0, 0.0, 1.0], 1e-3), "untouched inside");
    let (by_blue, by_red) = (at(120, 86), at(120, 106));
    assert!(by_blue[2] > 0.3 && by_blue[0] < 0.01, "blue beside blue: {by_blue:?}");
    assert!(by_red[0] > 0.3 && by_red[2] < 0.01, "red beside red: {by_red:?}");
    let fade: Vec<f32> = [114, 124, 134, 144, 154, 164].iter().map(|&x| at(x, 106)[0]).collect();
    assert!(fade.windows(2).all(|w| w[0] > w[1]), "fades outwards: {fade:?}");
    assert!(at(180, 106)[0] < 1e-3, "and stops at the radius: {:?}", at(180, 106));
}

/// Drop Shadow puts the silhouette, offset and softened, behind the picture; Stroke
/// rings it with an outline. Both leave the picture itself alone.
#[test]
fn shadow_and_stroke_draw_around_the_picture() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let run = |r: &mut Renderer, type_id: &str, uniforms: Vec<f32>, room: f64| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [1.0, 1.0, 1.0, 1.0], 32.0);
        let e = effect(&mut b, &registry, s, type_id, uniforms, room);
        let m = Affine2::translate(32.0, 32.0);
        let placed = b.add(NodeOp::Transform { matrix: m }, vec![e], m.map_rect(b.node(e).bounds), false);
        let out = composite(&mut b, 96, [0.0; 4], &[(placed, BlendMode::Normal)]);
        render_one(r, &b.finish(out))
    };
    let at = |px: &[[f32; 4]], x: usize, y: usize| px[y * 96 + x];
    // Shadow: black, full opacity, towards 0° (right), 10 px, hardly soft.
    let px = run(&mut r, oa_graph::registry::SHADOW, vec![0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 10.0, 1.0], 11.0);
    assert!(close(at(&px, 48, 48), [1.0, 1.0, 1.0, 1.0], 1e-3), "the picture on top");
    assert!(at(&px, 70, 48)[3] > 0.9 && at(&px, 70, 48)[0] < 0.05, "the shadow to its right: {:?}", at(&px, 70, 48));
    assert!(at(&px, 26, 48)[3] < 0.05, "and not to its left");
    // Stroke: 4 px, red.
    let px = run(&mut r, "oa.stylize.stroke", vec![4.0, 1.0, 0.0, 0.0, 1.0], 4.0);
    assert!(close(at(&px, 48, 48), [1.0, 1.0, 1.0, 1.0], 1e-3), "the picture inside");
    assert!(close(at(&px, 30, 48), [1.0, 0.0, 0.0, 1.0], 0.05), "a red ring round it: {:?}", at(&px, 30, 48));
    assert!(at(&px, 20, 48)[3] < 0.01, "only as wide as asked");
}

/// The new cut transitions start on the outgoing picture, end on the incoming one, and
/// halfway show something of the change.
#[test]
fn new_transitions_run_from_one_picture_to_the_other() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (red, blue) = ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]);
    let mut run = |type_id: &str, uniforms: Vec<f32>, progress: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let a = solid(&mut b, red, 32.0);
        let bb = solid(&mut b, blue, 32.0);
        let op = NodeOp::Transition { type_id: type_id.into(), version: 1, uniforms, progress };
        let t = b.add(op, vec![a, bb], Rect::from_size(32.0, 32.0), true);
        let out = composite(&mut b, 32, [0.0, 0.0, 0.0, 1.0], &[(t, BlendMode::Normal)]);
        render_one(&mut r, &b.finish(out))
    };
    for (id, uniforms) in [
        ("oa.transition.iris", vec![2.0]),
        ("oa.transition.zoom", vec![1.0]),
        ("oa.transition.slide", vec![0.0]),
        ("oa.transition.blur", vec![8.0]),
    ] {
        let start = run(id, uniforms.clone(), 0.0);
        let end = run(id, uniforms.clone(), 1.0);
        assert!(close(start[16 * 32 + 16], red, 0.02), "{id} starts on the outgoing picture: {:?}", start[16 * 32 + 16]);
        assert!(close(end[16 * 32 + 16], blue, 0.02), "{id} ends on the incoming one: {:?}", end[16 * 32 + 16]);
        let middle = run(id, uniforms, 0.5);
        let blue_share = middle.iter().map(|p| p[2]).sum::<f32>() / middle.len() as f32;
        assert!(blue_share > 0.1 && blue_share < 0.9, "{id} is part way at the middle ({blue_share})");
    }
}

/// An export's readback: both NV12 planes packed one after the other are exactly what
/// reading them one at a time gives, and the staging buffers come back to be reused
/// (none allocated for the next frame).
#[test]
fn packed_readback_matches_and_reuses_its_buffers() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut b = GraphBuilder::new(KeyContext::default());
    let s = solid(&mut b, [0.8, 0.3, 0.1, 1.0], 30.0);
    let out = composite(&mut b, 30, [0.0; 4], &[(s, BlendMode::Normal)]);
    let image = r.render(&b.finish(out), &Registry::with_builtins(), &mut TestPatternSource::default()).unwrap();
    let (luma, chroma) = r.run(|gpu| (gpu.to_nv12_plane(&image, oa_gpu::Nv12Plane::Luma).unwrap(), gpu.to_nv12_plane(&image, oa_gpu::Nv12Plane::Chroma).unwrap()));
    let half = [15, 15];
    let planes = [(&luma, image.size, 1), (&chroma, half, 2)];
    let single = oa_gpu::readback::start_read(&ctx, &planes).wait(&ctx).unwrap();
    let mut spare = Vec::new();
    let mut packed = Vec::new();
    let starts = oa_gpu::readback::start_read_reusing(&ctx, &planes, &mut spare).wait_packed(&ctx, &mut packed, &mut spare).unwrap();
    assert_eq!(starts, vec![0, 30 * 30]);
    assert_eq!(&packed[..starts[1]], single[0].as_slice(), "the Y plane, unpadded");
    assert_eq!(&packed[starts[1]..], single[1].as_slice(), "then the UV rows");
    assert_eq!(spare.len(), 2, "both staging buffers handed back");
    let _next = oa_gpu::readback::start_read_reusing(&ctx, &planes, &mut spare);
    assert!(spare.is_empty(), "and taken again for the next frame");
}

/// An effect track runs its container's effects over everything below it — the picture
/// composited under it — and not over the tracks above; and only while the container
/// lasts.
#[test]
fn an_effect_track_applies_to_everything_below_it() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let preset = AspectPreset::by_id("landscape-16x9").unwrap();
    let variant = FormatVariant { id: VariantId(3), name: preset.name.into(), size: preset.size(360), overrides: BTreeMap::new() };
    let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, variant);
    let solid = |id: u64, color: [f64; 4], scale: f64| {
        let mut it = Item::new(ItemId(id), "solid", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(10)));
        it.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color(color)));
        it.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([scale, scale])));
        it
    };
    let mut v1 = Track::new(TrackId(10), "V1", TrackKind::Video);
    v1.items.push(solid(11, [1.0, 0.0, 0.0, 1.0], 1.0));
    let mut fx = Track::effects(TrackId(20), "VFX1", TrackKind::Video);
    let mut container = Item::new(ItemId(21), "Effects", ItemKind::Adjustment, TimeRange::new(Time::ZERO, Time::from_seconds(2)));
    container.effects.push(EffectInstance::new(EffectId(22), "oa.color.invert"));
    fx.items.push(container);
    let mut v2 = Track::new(TrackId(30), "V2", TrackKind::Video);
    v2.items.push(solid(31, [0.0, 0.0, 1.0, 1.0], 0.4));
    seq.tracks = vec![Arc::new(v1), Arc::new(fx), Arc::new(v2)];
    let mut p = Project::new("fx");
    p.sequences.insert(SeqId(1), Arc::new(seq));

    let (corner, middle) = (5 * 640 + 5, 180 * 640 + 320);
    let during = render_project(&mut r, &p, Time::from_seconds(1));
    assert!(close(during[corner], [0.0, 1.0, 1.0, 1.0], 0.02), "the red below, inverted: {:?}", during[corner]);
    assert!(close(during[middle], [0.0, 0.0, 1.0, 1.0], 0.02), "the blue above it, untouched: {:?}", during[middle]);
    let after = render_project(&mut r, &p, Time::from_seconds(3));
    assert!(close(after[corner], [1.0, 0.0, 0.0, 1.0], 0.02), "once the container ends, plain red: {:?}", after[corner]);

    // Half opacity: the inverted picture half over the untouched one.
    let mut half = p.clone();
    let s = Arc::make_mut(half.sequences.get_mut(&SeqId(1)).unwrap());
    let track = Arc::make_mut(&mut s.tracks[1]);
    track.items[0].params.set(schema::OPACITY, ParamSource::Static(Value::Float(0.5)));
    let mixed = render_project(&mut r, &half, Time::from_seconds(1));
    assert!(close(mixed[corner], [0.5, 0.5, 0.5, 1.0], 0.02), "half the effect: {:?}", mixed[corner]);

    // A Fade intro fades the effect in: nothing of it at the start, all of it after.
    let mut faded = p.clone();
    let s = Arc::make_mut(faded.sequences.get_mut(&SeqId(1)).unwrap());
    let track = Arc::make_mut(&mut s.tracks[1]);
    let mut fade = EffectInstance::new(EffectId(23), "oa.anim.fade");
    fade.role = oa_doc::EffectRole::In { duration: Time::from_seconds(1) };
    track.items[0].effects.push(fade);
    let start = render_project(&mut r, &faded, Time::ZERO);
    assert!(close(start[corner], [1.0, 0.0, 0.0, 1.0], 0.02), "faded out at the start: {:?}", start[corner]);
    let later = render_project(&mut r, &faded, Time::from_seconds_f64(1.5));
    assert!(close(later[corner], [0.0, 1.0, 1.0, 1.0], 0.02), "all there after the fade: {:?}", later[corner]);
}
/// The renderer works to a memory budget: as it fills, cached frames and idle textures
/// are given back rather than allocated past, and it says it is under pressure so the
/// host can render smaller.
#[test]
fn a_full_memory_budget_is_given_back_not_grown_past() {
    let Some(ctx) = gpu() else { return };
    // Room for a couple of 128×128 targets (8 bytes a texel = 128 KB each).
    let budget = 300 * 1024;
    let mut r = Renderer::new(
        ctx.clone(),
        RenderOptions { fusion: FusionMode::Blocking, vram_budget: budget, cache_budget: 1 << 30, ..Default::default() },
    );
    let frame = |r: &mut Renderer, shade: f32| {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [shade, shade, shade, 1.0], 128.0);
        let out = composite(&mut b, 128, [0.0; 4], &[(s, BlendMode::Normal)]);
        let g = b.finish(out);
        // Twice, so the node is worth caching (a key seen once never is).
        let _ = r.render(&g, &Registry::with_builtins(), &mut TestPatternSource::default()).expect("render");
        let img = r.render(&g, &Registry::with_builtins(), &mut TestPatternSource::default()).expect("render");
        drop(img);
        r.memory()
    };

    let mut pressured = false;
    let mut last = frame(&mut r, 0.0);
    for i in 1..24 {
        last = frame(&mut r, i as f32 / 24.0);
        pressured |= last.pressure != oa_gpu::Pressure::Easy;
    }
    assert!(pressured, "a budget this small gets tight: {last:?}");
    assert!(last.used() <= budget, "stays inside the budget: {} of {budget}", last.used());
    assert_eq!(last.budget, budget);

    // Rendering still works with the budget full — it just keeps less around.
    let after = frame(&mut r, 1.0);
    assert!(after.used() <= budget, "{after:?}");
    assert!(!ctx.health.is_lost() && ctx.health.errors() == 0, "no GPU complaints: {:?}", ctx.health.message());
}

fn srgb_decode(x: f32) -> f32 {
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

/// The input transform's curves hit their published reference points: each file value
/// (as the decoder hands it over, sRGB-decoded) comes out as the right scene light.
#[test]
fn input_curves_hit_their_reference_points() {
    use oa_doc::color::{Gamut, Transfer};
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let identity: Vec<f32> = Gamut::Rec709.to_rec709().iter().flatten().map(|x| *x as f32).collect();
    let mut run = |transfer: Transfer, code: f32, matrix: &[f32]| -> [f32; 4] {
        let mut b = GraphBuilder::new(KeyContext::default());
        let v = srgb_decode(code);
        let s = solid(&mut b, [v, v, v, 1.0], 4.0);
        let mut u = vec![transfer.shader_index() as f32];
        u.extend(matrix);
        u.push(1.0);
        let e = effect(&mut b, &registry, s, oa_graph::registry::INPUT, u, 0.0);
        render_one(&mut r, &b.finish(e))[5]
    };
    let cases = [
        // (curve, file value, scene light) — HDR at 203 nits = 1.0, log 18% gray = 0.18.
        (Transfer::Srgb, 0.5, srgb_decode(0.5)),
        (Transfer::Rec709, 0.5, 0.5f32.powf(2.4)),
        (Transfer::Linear, 0.25, 0.25),
        (Transfer::Pq, 0.58069, 1.0),
        (Transfer::Hlg, 0.75, 1.0),
        (Transfer::LogC3, 0.391007, 0.18),
        (Transfer::SLog3, 420.0 / 1023.0, 0.18),
        (Transfer::VLog, 0.42331, 0.18),
        (Transfer::FLog, 0.459325, 0.18),
        (Transfer::CanonLog3, 0.34339, 0.18),
        (Transfer::AppleLog, 0.48827, 0.18),
    ];
    for (t, code, want) in cases {
        let got = run(t, code, &identity)[0];
        assert!((got - want).abs() <= want * 0.01 + 1e-3, "{t:?}: {code} → {got}, want {want}");
    }
    // Rec.2020 primaries: pure red lands outside Rec.709 (more than 1 in red, negative
    // green); white stays white.
    let m: Vec<f32> = Gamut::Rec2020.to_rec709().iter().flatten().map(|x| *x as f32).collect();
    let white = run(Transfer::Linear, 1.0, &m);
    assert!(close(white, [1.0, 1.0, 1.0, 1.0], 2e-3), "{white:?}");
}

/// The output transform: `soft` leaves midtones alone and rolls highlights off below
/// white, keeping their order; `filmic` is an S-curve; `off` just clips.
#[test]
fn tone_mapping_keeps_midtones_and_rolls_off_highlights() {
    let Some(ctx) = gpu() else { return };
    let registry = Registry::with_builtins();
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let mut run = |mode: f32, v: f32| -> f32 {
        let mut b = GraphBuilder::new(KeyContext::default());
        let s = solid(&mut b, [v, v * 0.5, v * 0.25, 1.0], 4.0);
        let e = effect(&mut b, &registry, s, oa_graph::registry::OUTPUT, vec![mode, 1.0], 0.0);
        render_one(&mut r, &b.finish(e))[5][0]
    };
    assert!((run(1.0, 0.5) - 0.5).abs() < 1e-3, "midtones untouched");
    let (a, b, c) = (run(1.0, 1.0), run(1.0, 4.0), run(1.0, 40.0));
    assert!(a < b && b < c && c <= 1.0 && a > 0.85, "rolled off in order: {a} {b} {c}");
    assert!((run(0.0, 4.0) - 1.0).abs() < 1e-3, "off clips");
    let (lo, mid, hi) = (run(2.0, 0.05), run(2.0, 0.5), run(2.0, 8.0));
    assert!(lo < mid && mid < hi && hi < 1.0, "filmic: {lo} {mid} {hi}");
}

/// "Highlight when spoken": the word being spoken takes the spoken color, by the clip's
/// word times — the left word first, then the right one; and the spoken copy of an
/// effect's params applies to that word only (a word box that's only lit when spoken).
#[test]
fn the_spoken_word_is_highlighted() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let red = |px: &[[f32; 4]], left: bool| -> f32 {
        px.iter().enumerate().filter(|(i, _)| ((i % 640) < 320) == left).map(|(_, p)| (p[0] - p[2]).max(0.0)).sum()
    };
    let (p, _) = text_project(|c| {
        c.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text("AAA BBB".into())));
        c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Color([0.2, 0.4, 1.0, 1.0])));
        c.params.set(&schema::spoken(schema::TEXT_COLOR), ParamSource::Static(Value::Color([1.0, 0.1, 0.1, 1.0])));
        c.word_times = vec![TimeRange::new(Time::ZERO, Time::from_seconds(1)), TimeRange::new(Time::from_seconds(1), Time::from_seconds(1))];
    });
    let first = render_project(&mut r, &p, Time::from_seconds_f64(0.5));
    assert!(red(&first, true) > 100.0 && red(&first, false) < 1.0, "left word lit: {} / {}", red(&first, true), red(&first, false));
    let second = render_project(&mut r, &p, Time::from_seconds_f64(1.5));
    assert!(red(&second, false) > 100.0 && red(&second, true) < 1.0, "right word lit: {} / {}", red(&second, true), red(&second, false));

    // A transparent word box whose spoken copy is white: a box behind the spoken word.
    let (p, _) = text_project(|c| {
        c.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text("AAA BBB".into())));
        c.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Color([0.0, 0.0, 0.0, 1.0])));
        c.word_times = vec![TimeRange::new(Time::ZERO, Time::from_seconds(1)), TimeRange::new(Time::from_seconds(1), Time::from_seconds(1))];
        let mut fx = EffectInstance::new(EffectId(91), "oa.text.background");
        fx.params.set("shape", ParamSource::Static(Value::Enum("words".into())));
        fx.params.set("color", ParamSource::Static(Value::Color([1.0, 1.0, 1.0, 0.0])));
        fx.params.set(&schema::spoken("color"), ParamSource::Static(Value::Color([1.0, 1.0, 1.0, 1.0])));
        c.effects.push(fx);
    });
    // Well away from the middle, where the left box's padding reaches.
    let white = |px: &[[f32; 4]], left: bool| {
        px.iter().enumerate().filter(|(i, _)| if left { i % 640 < 260 } else { i % 640 > 380 }).filter(|(_, p)| p[0] > 0.9 && p[1] > 0.9).count()
    };
    let lit = render_project(&mut r, &p, Time::from_seconds_f64(0.5));
    assert!(white(&lit, true) > 500 && white(&lit, false) == 0, "box behind the left word only: {} / {}", white(&lit, true), white(&lit, false));
    let later = render_project(&mut r, &p, Time::from_seconds_f64(1.5));
    assert!(white(&later, false) > 500 && white(&later, true) == 0, "then the right: {} / {}", white(&later, true), white(&later, false));
}

/// Background boxes: one around each line, with rounded corners (the box's very corner
/// stays clear), larger than the letters themselves.
#[test]
fn text_background_boxes_are_rounded() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let (plain, _) = text_project(|_| {});
    let (boxed, _) = text_project(|c| {
        let mut fx = EffectInstance::new(EffectId(92), "oa.text.background");
        fx.params.set("color", ParamSource::Static(Value::Color([0.0, 1.0, 0.0, 1.0])));
        fx.params.set("roundness", ParamSource::Static(Value::Float(0.5)));
        c.effects.push(fx);
    });
    let a = render_project(&mut r, &plain, Time::from_seconds(1));
    let b = render_project(&mut r, &boxed, Time::from_seconds(1));
    let green: Vec<usize> = b.iter().enumerate().filter(|(_, p)| p[1] > 0.9 && p[0] < 0.1).map(|(i, _)| i).collect();
    assert!(green.len() > 2000, "{}", green.len());
    assert!(a.iter().all(|p| !(p[1] > 0.9 && p[0] < 0.1)), "no green without it");
    // The box's bounding rectangle: its corners are cut by the rounding.
    let (x0, x1) = (green.iter().map(|i| i % 640).min().unwrap(), green.iter().map(|i| i % 640).max().unwrap());
    let (y0, y1) = (green.iter().map(|i| i / 640).min().unwrap(), green.iter().map(|i| i / 640).max().unwrap());
    let at = |x: usize, y: usize| b[y * 640 + x];
    assert!(at(x0, y0)[1] < 0.5 && at(x1, y1)[1] < 0.5, "rounded corners");
    assert!(at((x0 + x1) / 2, y0 + 1)[1] > 0.9 || at((x0 + x1) / 2, y0 + 1)[0] > 0.9, "straight top edge");
}

/// A tint on the background (the sequence's own effect) renders — on its own, and with
/// clips over it.
#[test]
fn a_tinted_background_renders() {
    let Some(ctx) = gpu() else { return };
    let mut r = renderer(&ctx, FusionMode::Blocking);
    let preset = AspectPreset::by_id("landscape-16x9").unwrap();
    let variant = FormatVariant { id: VariantId(3), name: preset.name.into(), size: preset.size(360), overrides: BTreeMap::new() };
    let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, variant);
    seq.params.set(schema::BG_COLOR, ParamSource::Static(Value::Gradient(oa_params::Gradient::solid([0.5, 0.5, 0.5, 1.0]))));
    seq.background.effects.push(EffectInstance::new(EffectId(9), "oa.color.tint"));
    let mut p = Project::new("bg");
    p.sequences.insert(SeqId(1), Arc::new(seq.clone()));
    let empty = render_project(&mut r, &p, Time::from_seconds(1));
    assert!(empty[5][0] > empty[5][2], "tinted orange: {:?}", empty[5]);

    let mut v1 = Track::new(TrackId(10), "V1", TrackKind::Video);
    let mut it = Item::new(ItemId(11), "solid", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(10)));
    it.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([0.0, 0.0, 1.0, 1.0])));
    it.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([0.4, 0.4])));
    v1.items.push(it);
    seq.tracks = vec![Arc::new(v1)];
    p.sequences.insert(SeqId(1), Arc::new(seq.clone()));
    let with_clip = render_project(&mut r, &p, Time::from_seconds(1));
    assert!(with_clip[5][0] > with_clip[5][2], "tinted orange: {:?}", with_clip[5]);

    // The blurred-content background too (it was the one that failed: the blurred
    // backdrop arrived as a transform, which an effect couldn't read): the blue clip,
    // blurred behind itself, comes out orange-tinted at the edge of the frame.
    seq.params.set(schema::BG_MODE, ParamSource::Static(Value::Enum("blur".into())));
    p.sequences.insert(SeqId(1), Arc::new(seq));
    let blurred = render_project(&mut r, &p, Time::from_seconds(1));
    assert!(blurred[5][0] > blurred[5][2], "tinted orange: {:?}", blurred[5]);
}
