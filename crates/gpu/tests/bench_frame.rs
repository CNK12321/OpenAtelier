//! Where a frame's CPU time goes on a big project: planning, optimizing, recording the
//! GPU work. Run by hand:
//!
//!   cargo test -p oa-gpu --release --test bench_frame -- --ignored --nocapture

use oa_doc::*;
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer, TestPatternSource};
use oa_graph::registry::Registry;
use oa_graph::*;
use oa_params::{ParamSet, ParamSource, Value};
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// `layers` tracks of solids, each with a few color and warp effects, moving.
pub fn big_project(layers: u64) -> (Project, SeqId) {
    let seq_id = SeqId(1);
    let mut p = Project::new("bench");
    let variant = FormatVariant { id: VariantId(2), name: "v".into(), size: CanvasSize::new(640, 360), overrides: BTreeMap::new() };
    let mut seq = Sequence::new(seq_id, "Main", FrameRate::FPS_30, variant);
    for l in 0..layers {
        let mut track = Track::new(TrackId(100 + l), "V", TrackKind::Video);
        let mut item = Item::new(ItemId(1000 + l), "solid", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(60)));
        item.params.set(schema::SOLID_COLOR, ParamSource::Static(Value::Color([0.2 + l as f64 * 0.01, 0.4, 0.6, 1.0])));
        item.params.set(schema::SCALE, ParamSource::Static(Value::Vec2([0.3, 0.3])));
        item.params.set(schema::OPACITY, ParamSource::Static(Value::Float(0.8)));
        for (k, (id, param, v)) in [("oa.color.exposure", "stops", 0.3), ("oa.color.saturation", "amount", 1.2), ("oa.warp.swirl", "angle", 40.0), ("oa.color.hue", "degrees", 20.0)].into_iter().enumerate() {
            let mut params = ParamSet::default();
            params.set(param, ParamSource::Static(Value::Float(v)));
            item.effects.push(EffectInstance { id: EffectId(l * 10 + k as u64), type_id: id.into(), type_version: 1, enabled: true, params, role: Default::default(), on_duplicate: None });
        }
        track.items.push(item);
        seq.tracks.push(Arc::new(track));
    }
    p.sequences.insert(seq_id, Arc::new(seq));
    (p, seq_id)
}

#[test]
#[ignore]
fn where_a_frame_goes() {
    let Ok(ctx) = GpuContext::new_headless() else { return };
    let ctx = Arc::new(ctx);
    let registry = Registry::with_builtins();
    let (p, seq) = big_project(60);
    let mut r = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, cache_budget: 0, ..Default::default() });
    let mut source = TestPatternSource::default();
    let frames = 300;
    let (mut plan, mut opt, mut record) = (0.0, 0.0, 0.0);
    for f in 0..frames + 10 {
        let t = FrameRate::FPS_30.frame_start(f);
        let clock = Instant::now();
        let g = plan_frame(&p, seq, t, &PlanOptions::default(), &registry).unwrap().graph;
        let a = clock.elapsed().as_secs_f64();
        let clock = Instant::now();
        let g = optimize(&g, OptLevel::Full, KeyContext::default());
        let b = clock.elapsed().as_secs_f64();
        let clock = Instant::now();
        let _img = r.render(&g, &registry, &mut source).unwrap();
        let c = clock.elapsed().as_secs_f64();
        ctx.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        if f >= 10 {
            (plan, opt, record) = (plan + a, opt + b, record + c);
        }
    }
    let ms = |s: f64| s / frames as f64 * 1000.0;
    eprintln!("per frame: plan {:.3} ms · optimize {:.3} ms · record {:.3} ms ({} passes)", ms(plan), ms(opt), ms(record), r.stats.passes);
}
