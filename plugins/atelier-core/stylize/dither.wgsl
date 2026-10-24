// Dither: the picture in a few levels per channel (or of brightness), the in-between
// shades made of a pattern of the levels either side — an ordered (Bayer) grid, noise
// or lines — `size` px a cell. In color, in black and white, or between two colors
// (`dark` and `light`). `amount` mixes it with the picture (0: the picture as it is);
// the pattern can drift (`direction`, `speed` px a second).

// The threshold at a cell of an 8 × 8 Bayer grid (0 → 1): its bits interleaved.
fn oa_dither_bayer(cell: vec2i) -> f32 {
    let x = u32(cell.x) & 7u;
    let y = u32(cell.y) & 7u;
    let xy = x ^ y;
    let m = ((xy & 1u) << 5u) | ((y & 1u) << 4u) | ((xy & 2u) << 2u) | ((y & 2u) << 1u) | ((xy & 4u) >> 1u) | ((y & 4u) >> 2u);
    return (f32(m) + 0.5) / 64.0;
}

fn oa_dither_hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(12.9898, 78.233))) * 43758.5453);
}

// `v` (0 → 1) in `levels` steps, rounded up where it's further past a step than the
// threshold `t` is.
fn oa_dither_step(v: vec3f, levels: f32, t: f32) -> vec3f {
    let n = levels - 1.0;
    return clamp(floor(clamp(v, vec3f(0.0), vec3f(1.0)) * n + t), vec3f(0.0), vec3f(n)) / n;
}

fn oa_stylize_dither(c: vec4f, base: u32) -> vec4f {
    let amount = clamp(u(base), 0.0, 1.0);
    if (amount <= 0.0) { return c; }
    let levels = clamp(round(u(base + 1u)), 2.0, 64.0);
    let pattern = u32(round(u(base + 2u)));
    let size = max(u(base + 3u), 1.0);
    let palette = u32(round(u(base + 4u)));
    let dark = vec3f(u(base + 5u), u(base + 6u), u(base + 7u));
    let light = vec3f(u(base + 9u), u(base + 10u), u(base + 11u));
    // The pattern drifting towards `direction` (0 right, 90 down) at `speed` px a second.
    let way = radians(u(base + 13u));
    let drift = vec2f(cos(way), sin(way)) * max(u(base + 14u), 0.0) * clip_seconds();
    let cell = vec2i(floor((layer_pos() - in_origin() - drift) / size));
    var t = oa_dither_bayer(cell);
    if (pattern == 1u) {
        t = oa_dither_hash(vec2f(cell) + 0.5);
    } else if (pattern == 2u) {
        // Lines across: four to a repeat.
        t = (f32(u32(cell.y) & 3u) + 0.5) / 4.0;
    }
    var out = oa_dither_step(c.rgb, levels, t);
    if (palette != 0u) {
        // Of its brightness: in grays, or from `dark` to `light`.
        let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
        let q = oa_dither_step(vec3f(luma), levels, t).x;
        out = select(vec3f(q), mix(dark, light, q), palette == 2u);
    }
    return vec4f(mix(c.rgb, out, amount), c.a);
}
