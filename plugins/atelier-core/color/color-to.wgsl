// Color To: pixels close to one color become another.
// Params: from (color), tolerance, softness, to (color), offset_from_original (bool).

fn oa_color_to_hsv(c: vec3f) -> vec3f {
    let top = max(c.r, max(c.g, c.b));
    let d = top - min(c.r, min(c.g, c.b));
    var h = 0.0;
    if (d > 1e-6) {
        if (top == c.r) {
            h = (c.g - c.b) / d;
        } else if (top == c.g) {
            h = 2.0 + (c.b - c.r) / d;
        } else {
            h = 4.0 + (c.r - c.g) / d;
        }
        h = fract(h / 6.0 + 1.0);
    }
    return vec3f(h, select(0.0, d / top, top > 1e-6), top);
}

fn oa_color_to_rgb(hsv: vec3f) -> vec3f {
    let p = abs(fract(vec3f(hsv.x) + vec3f(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return hsv.z * mix(vec3f(1.0), clamp(p - 1.0, vec3f(0.0), vec3f(1.0)), hsv.y);
}

fn oa_color_to(c: vec4f, base: u32) -> vec4f {
    // Compared and changed as they look (display-encoded), not as light.
    let px = srgb_encode(max(c.rgb, vec3f(0.0)));
    let key = srgb_encode(vec3f(u(base), u(base + 1u), u(base + 2u)));
    let tolerance = max(u(base + 4u), 0.0);
    let softness = max(u(base + 5u), 0.0);
    let goal = srgb_encode(vec3f(u(base + 6u), u(base + 7u), u(base + 8u)));
    let keep_offset = u(base + 10u) > 0.5;

    // How far the pixel is from `from`: 0 the same, 1 as far as black from white.
    let d = length(px - key) / sqrt(3.0);
    let near = 1.0 - smoothstep(tolerance, tolerance + softness + 1e-5, d);

    var new_color = goal;
    if (keep_offset) {
        // The pixel's shade relative to `from`, carried over to `to`: the same turn of
        // hue, the same step in saturation and brightness.
        let p = oa_color_to_hsv(px);
        let f = oa_color_to_hsv(key);
        let t = oa_color_to_hsv(goal);
        let hsv = vec3f(fract(t.x + p.x - f.x + 1.0), clamp(t.y + p.y - f.y, 0.0, 1.0), max(t.z + p.z - f.z, 0.0));
        new_color = oa_color_to_rgb(hsv);
    }
    let result = mix(px, new_color, near * u(base + 9u));
    return vec4f(srgb_decode(result), c.a);
}
