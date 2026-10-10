// Hue Anchor: every hue measured from the anchor's, round the color wheel, and that
// distance scaled. Params: anchor (a color: only its hue counts), distance, amount.
// Distance 1 leaves the picture as it is; above 1 the hues spread out and wrap round the
// wheel (colors repeat, each over a narrower range); below 1 they gather toward the
// anchor (none repeat, each over a wider range); 0 makes everything the anchor's hue.
// Saturation and lightness stay; grays have no hue and stay gray.

fn oa_hue_anchor_to_hsl(c: vec3f) -> vec3f {
    let hi = max(c.r, max(c.g, c.b));
    let lo = min(c.r, min(c.g, c.b));
    let l = (hi + lo) * 0.5;
    let d = hi - lo;
    if (d <= 1e-6) {
        return vec3f(0.0, 0.0, l);
    }
    let s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-6);
    var h: f32;
    if (hi == c.r) {
        h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b);
    } else if (hi == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3f(h / 6.0, s, l);
}

fn oa_hue_anchor_from_hsl(hsl: vec3f) -> vec3f {
    let a = hsl.y * min(hsl.z, 1.0 - hsl.z);
    let k = (vec3f(0.0, 8.0, 4.0) + hsl.x * 12.0) % 12.0;
    return hsl.z - a * max(vec3f(-1.0), min(k - 3.0, min(9.0 - k, vec3f(1.0))));
}

fn oa_color_hue_anchor(c: vec4f, base: u32) -> vec4f {
    let anchor = oa_hue_anchor_to_hsl(clamp(vec3f(u(base), u(base + 1u), u(base + 2u)), vec3f(0.0), vec3f(1.0))).x;
    let distance = u(base + 4u);
    let amount = clamp(u(base + 5u), 0.0, 1.0);
    let hsl = oa_hue_anchor_to_hsl(clamp(c.rgb, vec3f(0.0), vec3f(1.0)));
    if (hsl.y <= 0.0) {
        return c;
    }
    // The hue's distance from the anchor's the short way round (−½ to ½ a turn), scaled.
    let gap = fract(hsl.x - anchor + 0.5) - 0.5;
    let hue = fract(anchor + gap * distance);
    let rgb = oa_hue_anchor_from_hsl(vec3f(hue, hsl.y, hsl.z));
    return vec4f(mix(c.rgb, rgb, amount), c.a);
}
