//! WGSL prelude and code generation.
//!
//! Every pass uses the same bind group: a 64-float uniform block, one input texture
//! and a linear clamp sampler. Uniform slots:
//!
//! | slots  | meaning                                                   |
//! |--------|-----------------------------------------------------------|
//! | 0–1    | output origin (layer space of target texel (0,0))         |
//! | 2–3    | output size (px)                                          |
//! | 4–5    | input origin                                              |
//! | 6–7    | input size                                                |
//! | 8–13   | affine a b c d e f (layer draws)                          |
//! | 14     | opacity (layer draws)                                     |
//! | 15     | pass index                                                |
//! | 16–63  | effect parameters                                         |

use oa_graph::{EffectShader, WorkingSpace};
use std::fmt::Write as _;

pub const PARAM_BASE: usize = 16;
pub const UNIFORM_SLOTS: usize = 64;
pub use oa_graph::registry::CLOCK_SLOTS;

pub const PRELUDE: &str = r#"
struct Pass { h: array<vec4f, 16> }
@group(0) @binding(0) var<uniform> P: Pass;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var input_samp: sampler;

fn u(i: u32) -> f32 { return P.h[i / 4u][i % 4u]; }
fn out_origin() -> vec2f { return vec2f(u(0u), u(1u)); }
fn out_size() -> vec2f { return vec2f(u(2u), u(3u)); }
fn in_origin() -> vec2f { return vec2f(u(4u), u(5u)); }
fn in_size() -> vec2f { return vec2f(u(6u), u(7u)); }
fn pass_index() -> u32 { return u32(u(15u)); }

// The running effect's clock, loaded before each effect function is called (every
// effect's parameter block ends with these three values).
var<private> oa_clock: vec3f;
fn oa_set_clock(i: u32) { oa_clock = vec3f(u(i), u(i + 1u), u(i + 2u)); }
// How present the clip is: 0 → 1 over a transition in, 1 → 0 over a transition out, 1
// for a passive effect. One shader using this works as both an in and an out.
fn visibility() -> f32 { return oa_clock.x; }
// 0 → 1 across the effect's window (for passive effects, across the clip).
fn progress() -> f32 { return oa_clock.y; }
// Seconds since the effect's window began.
fn clip_seconds() -> f32 { return oa_clock.z; }

// The layer-space position of the pixel being shaded (set before effect functions run,
// so point ops can make gradients and sweeps).
var<private> oa_pos: vec2f;
fn layer_pos() -> vec2f { return oa_pos; }

// A directional gradient param packed at `base` (angle in degrees — 0 = right, 90 =
// down —, stop count, then up to 6 × (pos, r, g, b, a)), at layer position `pos` over
// the box at `lo` of `size`: 0 where the direction enters the box, 1 where it leaves.
// Straight RGBA.
fn oa_gradient(base: u32, pos: vec2f, lo: vec2f, size: vec2f) -> vec4f {
    let a = radians(u(base));
    let dir = vec2f(cos(a), sin(a));
    let half = 0.5 * (abs(dir.x) * size.x + abs(dir.y) * size.y);
    let t = clamp(dot(pos - (lo + 0.5 * size), dir) / max(half, 1e-5) * 0.5 + 0.5, 0.0, 1.0);
    let n = u32(clamp(u(base + 1u), 1.0, 6.0));
    let s = base + 2u;
    var prev = vec4f(u(s + 1u), u(s + 2u), u(s + 3u), u(s + 4u));
    if (t <= u(s)) { return prev; }
    for (var i = 1u; i < n; i++) {
        let b = s + 5u * i;
        let next = vec4f(u(b + 1u), u(b + 2u), u(b + 3u), u(b + 4u));
        let p0 = u(b - 5u);
        let p1 = u(b);
        if (t <= p1) { return mix(prev, next, clamp((t - p0) / max(p1 - p0, 1e-5), 0.0, 1.0)); }
        prev = next;
    }
    return prev;
}

// Premultiplied input at a layer-space position; transparent outside the image.
fn sample_input(pos: vec2f) -> vec4f {
    let uv = (pos - in_origin()) / in_size();
    let c = textureSampleLevel(input_tex, input_samp, uv, 0.0);
    let inside = all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0));
    return select(vec4f(0.0), c, inside);
}

// Like sample_input, but pixels outside the image repeat the nearest edge pixel. Used by
// blurs on full-frame layers, where fading to transparent would darken the canvas border.
fn sample_input_clamped(pos: vec2f) -> vec4f {
    let uv = clamp((pos - in_origin()) / in_size(), vec2f(0.0), vec2f(1.0));
    return textureSampleLevel(input_tex, input_samp, uv, 0.0);
}

fn srgb_encode(c: vec3f) -> vec3f {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3f(0.0)), vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3f(0.0031308));
}

fn srgb_decode(c: vec3f) -> vec3f {
    let lo = c / 12.92;
    let hi = pow(max((c + 0.055) / 1.055, vec3f(0.0)), vec3f(2.4));
    return select(hi, lo, c <= vec3f(0.04045));
}

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
}
"#;

pub fn solid() -> String {
    format!(
        "{PRELUDE}
@fragment
fn fs_main() -> @location(0) vec4f {{
    let c = vec4f(u(16u), u(17u), u(18u), u(19u));
    return vec4f(c.rgb * c.a, c.a);
}}"
    )
}

/// The layer shader. `over_white` makes it hand the blend its premultiplied color laid
/// over white (for Darken, a minimum, where transparent must mean "no change" rather than
/// black).
pub fn layer(over_white: bool) -> String {
    let out = if over_white { "let c = textureSampleLevel(input_tex, input_samp, v.uv, 0.0) * u(14u);\n    return vec4f(c.rgb + vec3f(1.0 - c.a), c.a);" } else { "return textureSampleLevel(input_tex, input_samp, v.uv, 0.0) * u(14u);" };
    format!(
        "{PRELUDE}
struct VOut {{ @builtin(position) pos: vec4f, @location(0) uv: vec2f }}

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> VOut {{
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(1.0, 0.0), vec2f(1.0, 1.0), vec2f(0.0, 1.0),
    );
    let c = corners[i];
    let p = in_origin() + c * in_size();
    let x = u(8u) * p.x + u(10u) * p.y + u(12u);
    let y = u(9u) * p.x + u(11u) * p.y + u(13u);
    let ndc = vec2f(x / out_size().x * 2.0 - 1.0, 1.0 - y / out_size().y * 2.0);
    return VOut(vec4f(ndc, 0.0, 1.0), c);
}}

@fragment
fn fs_layer(v: VOut) -> @location(0) vec4f {{
    {out}
}}"
    )
}

/// Linear premultiplied → display sRGB bytes, encoded here (into a plain `Rgba8Unorm`
/// target: every backend can render to it and hand it to a UI as it is — OpenGL can't
/// view one texture as both sRGB and not).
pub fn display() -> String {
    format!(
        "{PRELUDE}
@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {{
    let c = sample_input(in_origin() + fc.xy);
    return vec4f(srgb_encode(clamp(c.rgb, vec3f(0.0), vec3f(1.0))), clamp(c.a, 0.0, 1.0));
}}"
    )
}

/// Straight-alpha 8-bit sRGB image → premultiplied linear working format. The sampler
/// does the sRGB→linear part; this scales to the target size.
pub fn blit_srgb() -> String {
    format!(
        "{PRELUDE}
@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {{
    let c = textureSampleLevel(input_tex, input_samp, fc.xy / out_size(), 0.0);
    return vec4f(c.rgb * c.a, c.a);
}}"
    )
}

/// Working format → BT.709 limited-range Y (full resolution) and CbCr (half), the two
/// planes of NV12. Encoders take this directly, and it is 1.5 bytes per pixel instead of
/// the 8 a float RGBA readback would cost.
pub fn to_nv12(plane: Nv12Plane) -> String {
    let body = match plane {
        Nv12Plane::Luma => {
            "@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    let c = srgb_encode(sample_at(fc.xy / out_size()));
    let y = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    return vec4f(16.0 / 255.0 + y * (219.0 / 255.0), 0.0, 0.0, 1.0);
}"
        }
        Nv12Plane::Chroma => {
            // One chroma sample covers a 2x2 block of luma.
            "@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    let uv = fc.xy / out_size();
    let d = 0.25 / out_size();
    var c = vec3f(0.0);
    c += srgb_encode(sample_at(uv + vec2f(-d.x, -d.y)));
    c += srgb_encode(sample_at(uv + vec2f(d.x, -d.y)));
    c += srgb_encode(sample_at(uv + vec2f(-d.x, d.y)));
    c += srgb_encode(sample_at(uv + vec2f(d.x, d.y)));
    c = c * 0.25;
    let y = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec4f(0.5 + cb * (224.0 / 255.0), 0.5 + cr * (224.0 / 255.0), 0.0, 1.0);
}"
        }
    };
    format!(
        "{PRELUDE}
// Premultiplied linear input over black, as straight display-referred color.
fn sample_at(uv: vec2f) -> vec3f {{
    let c = textureSampleLevel(input_tex, input_samp, uv, 0.0);
    return clamp(c.rgb, vec3f(0.0), vec3f(1.0));
}}
{body}"
    )
}

/// Working format → straight-alpha color for an `Rgba8UnormSrgb` target (the target does
/// the sRGB curve): what formats keeping transparency are fed.
pub fn to_rgba8_straight() -> String {
    format!(
        "{PRELUDE}
@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {{
    let c = textureSampleLevel(input_tex, input_samp, fc.xy / out_size(), 0.0);
    let a = clamp(c.a, 0.0, 1.0);
    let rgb = select(vec3f(0.0), c.rgb / max(c.a, 1e-6), c.a > 1e-6);
    return vec4f(clamp(rgb, vec3f(0.0), vec3f(1.0)), a);
}}"
    )
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Nv12Plane {
    Luma,
    Chroma,
}

/// One pass running a chain of point ops (a chain of one is the unfused path).
/// `chain` is (shader, number of uniform floats) in application order.
pub fn point_chain(chain: &[(&EffectShader, usize)], space: WorkingSpace) -> String {
    let mut s = String::from(PRELUDE);
    let mut seen = Vec::new();
    for (shader, _) in chain {
        if !seen.contains(&shader.entry.as_str()) {
            seen.push(&shader.entry);
            s.push_str(&shader.source);
            s.push('\n');
        }
    }
    s.push_str(
        "@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {
    oa_pos = out_origin() + fc.xy;
    let pm = sample_input(oa_pos);
    var c = vec4f(select(vec3f(0.0), pm.rgb / pm.a, pm.a > 0.0), pm.a);
",
    );
    if space == WorkingSpace::Display {
        s.push_str("    c = vec4f(srgb_encode(c.rgb), c.a);\n");
    }
    let mut base = PARAM_BASE;
    for (shader, len) in chain {
        let clock = base + len.saturating_sub(CLOCK_SLOTS);
        let _ = writeln!(s, "    oa_set_clock({clock}u);\n    c = {}(c, {base}u);", shader.entry);
        base += len;
    }
    if space == WorkingSpace::Display {
        s.push_str("    c = vec4f(srgb_decode(c.rgb), c.a);\n");
    }
    s.push_str("    return vec4f(c.rgb * c.a, c.a);\n}\n");
    s
}

/// A neighborhood effect pass (spatial or UV warp) with `len` uniform floats. With
/// `media`, the effect also reads a second picture — a media parameter's (a mask's
/// matte, say) — through `sample_media(pos)`, stretched over the layer.
pub fn spatial(shader: &EffectShader, len: usize, media: bool) -> String {
    let clock = PARAM_BASE + len.saturating_sub(CLOCK_SLOTS);
    let prelude = if media { two_input_prelude() } else { PRELUDE.to_string() };
    let media_fn = if media {
        "// The media input (premultiplied), stretched over the layer; transparent outside.
fn sample_media(pos: vec2f) -> vec4f {
    let uv = (pos - in2_origin()) / in2_size();
    let c = textureSampleLevel(input2_tex, input_samp, uv, 0.0);
    let inside = all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0));
    return select(vec4f(0.0), c, inside);
}
fn has_media() -> bool { return true; }"
    } else {
        "fn sample_media(pos: vec2f) -> vec4f { return vec4f(0.0); }
fn has_media() -> bool { return false; }"
    };
    format!(
        "{prelude}
{media_fn}
{}
@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {{
    oa_set_clock({clock}u);
    oa_pos = out_origin() + fc.xy;
    return {}(oa_pos, {PARAM_BASE}u);
}}",
        shader.source, shader.entry
    )
}

/// The prelude with a second input texture at binding 2 (sampler moved to 3); uniform
/// slots 8–11 hold that input's origin and size.
fn two_input_prelude() -> String {
    let bindings = "@group(0) @binding(2) var input_samp: sampler;";
    assert!(PRELUDE.contains(bindings), "prelude binding layout changed");
    let prelude = PRELUDE.replace(
        bindings,
        "@group(0) @binding(2) var input2_tex: texture_2d<f32>;\n@group(0) @binding(3) var input_samp: sampler;",
    );
    format!(
        "{prelude}
fn in2_origin() -> vec2f {{ return vec2f(u(8u), u(9u)); }}
fn in2_size() -> vec2f {{ return vec2f(u(10u), u(11u)); }}"
    )
}

/// A transition pass: two inputs (`[from, to]`, the second at binding 2, the sampler moved
/// to 3). Uniform slots 8–11 hold the second input's origin and size, 14 the progress.
pub fn transition(shader: &EffectShader) -> String {
    let prelude = two_input_prelude();
    format!(
        "{prelude}
// The outgoing picture (premultiplied), transparent outside it.
fn sample_a(pos: vec2f) -> vec4f {{ return sample_input(pos); }}
// The incoming picture (premultiplied), transparent outside it.
fn sample_b(pos: vec2f) -> vec4f {{
    let uv = (pos - in2_origin()) / in2_size();
    let c = textureSampleLevel(input2_tex, input_samp, uv, 0.0);
    let inside = all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0));
    return select(vec4f(0.0), c, inside);
}}

{source}

@fragment
fn fs_main(@builtin(position) fc: vec4f) -> @location(0) vec4f {{
    return {entry}(out_origin() + fc.xy, u(14u), {base}u);
}}",
        source = shader.source,
        entry = shader.entry,
        base = PARAM_BASE,
    )
}
