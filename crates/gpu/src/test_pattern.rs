use crate::renderer::Uniforms;
use crate::shaders::PRELUDE;
use crate::{FrameSource, GpuImage, GpuServices, RenderError, SourceRequest};

/// A GPU-generated stand-in for decoded media: color bars tinted per media id, a
/// checkerboard strip, and a square that moves with source time. Resolution-independent,
/// so the same frame at different decode scales matches after resampling.
#[derive(Default)]
pub struct TestPatternSource {
    pub frames_generated: u64,
}

const PATTERN: &str = r#"
@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    let uv = fc.xy / out_size();
    let t = u(17u);
    let tint = fract(u(16u) * 0.6180339);
    var bars = array<vec3f, 7>(
        vec3f(0.75, 0.75, 0.75), vec3f(0.75, 0.75, 0.0), vec3f(0.0, 0.75, 0.75), vec3f(0.0, 0.75, 0.0),
        vec3f(0.75, 0.0, 0.75), vec3f(0.75, 0.0, 0.0), vec3f(0.0, 0.0, 0.75),
    );
    var c = bars[min(u32(uv.x * 7.0), 6u)] * (0.6 + 0.4 * vec3f(tint, 1.0 - tint, 0.5));
    if (uv.y > 0.8) {
        let cell = floor(uv * vec2f(16.0, 9.0));
        c = vec3f(select(0.05, 0.6, (u32(cell.x + cell.y) & 1u) == 1u));
    }
    let box_center = vec2f(0.5 + 0.35 * sin(t * 1.3), 0.4 + 0.25 * cos(t * 0.9));
    let d = abs(uv - box_center) * vec2f(out_size().x / out_size().y, 1.0);
    if (max(d.x, d.y) < 0.08) {
        c = vec3f(1.0);
    }
    return vec4f(c, 1.0);
}
"#;

impl FrameSource for TestPatternSource {
    fn frame(&mut self, gpu: &mut GpuServices<'_>, req: &SourceRequest) -> Result<GpuImage, RenderError> {
        let pipeline = gpu.working_pipeline("test-pattern", || format!("{PRELUDE}{PATTERN}"))?;
        let target = gpu.target(req.size)?;
        let u = Uniforms::default()
            .output([0.0; 2], req.size)
            .params(0, &[req.media as f32, req.source_time.as_seconds_f64() as f32])?;
        gpu.fullscreen_pass(&pipeline, &target, &u, None);
        self.frames_generated += 1;
        Ok(GpuImage { tex: target, origin: [0.0; 2], size: req.size })
    }

    fn status(&self) -> Option<String> {
        Some(format!("test pattern: {} frames generated", self.frames_generated))
    }
}
