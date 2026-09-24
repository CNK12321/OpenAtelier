//! Concurrency diagnostic: N threads, each with its own device, render + read back in a
//! loop. Prints a heartbeat so a hang shows which stage stalled.
//!
//!   cargo run -p oa-gpu --example stress -- <threads> <iterations> [shared]

use oa_gpu::readback::read_linear;
use oa_gpu::{FusionMode, GpuContext, RenderOptions, Renderer, TestPatternSource};
use oa_graph::registry::Registry;
use oa_graph::*;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let threads: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(4);
    let iterations: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let shared = args.iter().any(|a| a == "shared");
    let shared_ctx = shared.then(|| Arc::new(GpuContext::new_headless().unwrap()));
    let info = shared_ctx.as_ref().map(|c| c.info.clone()).unwrap_or_else(|| GpuContext::new_headless().unwrap().info);
    println!("{} ({:?}), {threads} threads, shared device: {shared}", info.name, info.backend);

    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let shared_ctx = shared_ctx.clone();
            std::thread::spawn(move || {
                let ctx = shared_ctx.unwrap_or_else(|| Arc::new(GpuContext::new_headless().unwrap()));
                let registry = Registry::with_builtins();
                let mut r = Renderer::new(ctx.clone(), RenderOptions { fusion: FusionMode::Blocking, cache_budget: 0, ..Default::default() });
                for i in 0..iterations {
                    let mut b = GraphBuilder::new(KeyContext::default());
                    let s = b.add(NodeOp::Solid { color: [0.5, 0.2, 0.1, 1.0], size: [64.0, 64.0] }, vec![], Rect::from_size(64.0, 64.0), true);
                    let out = b.add(
                        NodeOp::Composite { size: [64, 64], background: [0.0, 0.0, 0.0, 1.0], layers: vec![LayerInfo { opacity: 1.0, blend: BlendMode::Normal, pixelated: false }] },
                        vec![s],
                        Rect::from_size(64.0, 64.0),
                        true,
                    );
                    eprintln!("t{t} i{i} render");
                    let img = r.render(&b.finish(out), &registry, &mut TestPatternSource::default()).unwrap();
                    eprintln!("t{t} i{i} readback");
                    let px = read_linear(&ctx, &img).unwrap();
                    assert!((px[0][0] - 0.5).abs() < 1e-3);
                }
                eprintln!("t{t} done");
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    println!("ok");
}
