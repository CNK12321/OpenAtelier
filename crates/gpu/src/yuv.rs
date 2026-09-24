//! Decoder output (YUV planes on the GPU) → working-space linear RGB.

use crate::pipelines::{pass_layout_entries, PipelineSpec, Pipelines};
use crate::pool::WORKING_FORMAT;

/// YCbCr → RGB matrix coefficients.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    fn kr_kb(self) -> (f32, f32) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// Transfer function of the encoded signal.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TransferFn {
    /// SDR video (BT.709 / BT.601 / sRGB). Decoded with the sRGB curve so an unedited clip
    /// round-trips exactly to an sRGB display; a full OCIO pipeline replaces this later.
    Sdr,
    Linear,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoColor {
    pub matrix: YuvMatrix,
    pub full_range: bool,
    pub transfer: TransferFn,
}

impl VideoColor {
    /// Common guess when a file carries no color tags: HD and larger → BT.709.
    pub fn guess(height: u32) -> Self {
        VideoColor {
            matrix: if height >= 720 { YuvMatrix::Bt709 } else { YuvMatrix::Bt601 },
            full_range: false,
            transfer: TransferFn::Sdr,
        }
    }
}

/// A decoded 8-bit 4:2:0 frame living on the GPU.
pub struct Nv12Frame<'a> {
    /// A `TextureFormat::NV12` texture (decoder surfaces are often larger than the
    /// picture, e.g. 1920×1088 for 1080p) — or, with [`Nv12Frame::chroma`], just the luma
    /// plane as an `R8Unorm` texture.
    pub texture: &'a wgpu::Texture,
    /// The chroma plane as its own `Rg8Unorm` texture (half size), for decoders that
    /// upload planes separately: works on every backend, where NV12 textures don't.
    pub chroma: Option<&'a wgpu::Texture>,
    pub coded_size: [u32; 2],
    /// Picture size inside the coded surface, before rotation.
    pub visible_size: [u32; 2],
    /// Clockwise display rotation in quarter turns (container metadata).
    pub rotation_quarter_turns: u32,
    pub color: VideoColor,
}

impl Nv12Frame<'_> {
    /// Picture size as displayed (90°/270° swap the axes).
    pub fn display_size(&self) -> [u32; 2] {
        match self.rotation_quarter_turns % 4 {
            1 | 3 => [self.visible_size[1], self.visible_size[0]],
            _ => self.visible_size,
        }
    }
}

pub(crate) fn pipelines(device: &wgpu::Device) -> Pipelines {
    Pipelines::with_layout(device, "oa-yuv", &pass_layout_entries(2))
}

pub(crate) fn spec() -> PipelineSpec {
    PipelineSpec {
        label: "yuv-nv12".into(),
        source: SHADER.into(),
        vertex_entry: "vs_full",
        fragment_entry: "fs_main",
        format: WORKING_FORMAT,
        blend: None,
    }
}

/// Uniform block (64 floats, same buffer size as other passes).
pub(crate) fn uniforms(frame: &Nv12Frame<'_>, out_size: [u32; 2]) -> [f32; 64] {
    let (kr, kb) = frame.color.matrix.kr_kb();
    let mut u = [0.0; 64];
    u[0] = out_size[0] as f32;
    u[1] = out_size[1] as f32;
    u[2] = frame.visible_size[0] as f32 / frame.coded_size[0] as f32;
    u[3] = frame.visible_size[1] as f32 / frame.coded_size[1] as f32;
    u[4] = kr;
    u[5] = kb;
    u[6] = frame.color.full_range as u8 as f32;
    u[7] = match frame.color.transfer {
        TransferFn::Sdr => 0.0,
        TransferFn::Linear => 1.0,
    };
    // Supersample when shrinking more than ~1.5x to avoid aliasing.
    let display = frame.display_size();
    u[8] = (display[0] as f32 / out_size[0] as f32).max(display[1] as f32 / out_size[1] as f32);
    u[9] = (frame.rotation_quarter_turns % 4) as f32;
    u
}

const SHADER: &str = r#"
struct Pass { h: array<vec4f, 16> }
@group(0) @binding(0) var<uniform> P: Pass;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var chroma: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
}

fn yuv_to_rgb(uv: vec2f) -> vec3f {
    var y = textureSampleLevel(luma, samp, uv, 0.0).r;
    var c = textureSampleLevel(chroma, samp, uv, 0.0).rg;
    if (P.h[1].z < 0.5) {
        y = (y - 16.0 / 255.0) * (255.0 / 219.0);
        c = (c - 16.0 / 255.0) * (255.0 / 224.0);
    }
    let cb = c.x - 0.5;
    let cr = c.y - 0.5;
    let kr = P.h[1].x;
    let kb = P.h[1].y;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    return clamp(vec3f(r, g, b), vec3f(0.0), vec3f(1.0));
}

fn linearize(c: vec3f) -> vec3f {
    if (P.h[1].w > 0.5) { return c; }
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3f(2.4));
    return select(hi, lo, c <= vec3f(0.04045));
}

// Display-space position (0..1) to source-space position, undoing display rotation.
fn unrotate(p: vec2f) -> vec2f {
    let turns = u32(P.h[2].y);
    if (turns == 1u) { return vec2f(p.y, 1.0 - p.x); }
    if (turns == 2u) { return vec2f(1.0 - p.x, 1.0 - p.y); }
    if (turns == 3u) { return vec2f(1.0 - p.y, p.x); }
    return p;
}

@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    let out_size = P.h[0].xy;
    let crop = P.h[0].zw;
    let uv = unrotate(fc.xy / out_size) * crop;
    var rgb: vec3f;
    if (P.h[2].x > 1.5) {
        // 2x2 supersampling across the output texel footprint.
        // The output footprint maps to swapped axes when the picture is rotated 90/270.
        let turns = u32(P.h[2].y);
        let axes = select(out_size, out_size.yx, turns == 1u || turns == 3u);
        let d = 0.25 / axes * crop;
        rgb = 0.25 * (linearize(yuv_to_rgb(uv + vec2f(-d.x, -d.y))) + linearize(yuv_to_rgb(uv + vec2f(d.x, -d.y)))
            + linearize(yuv_to_rgb(uv + vec2f(-d.x, d.y))) + linearize(yuv_to_rgb(uv + vec2f(d.x, d.y))));
    } else {
        rgb = linearize(yuv_to_rgb(uv));
    }
    return vec4f(rgb, 1.0);
}
"#;
