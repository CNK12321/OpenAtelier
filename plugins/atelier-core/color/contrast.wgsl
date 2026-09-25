fn oa_color_contrast(c: vec4f, base: u32) -> vec4f {
    // Contrast about a pivot, then lift, then a gamma curve.
    let pivot = u(base + 2u);
    var rgb = (c.rgb - pivot) * max(u(base), 0.0) + pivot + u(base + 1u);
    rgb = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / max(u(base + 3u), 0.01)));
    return vec4f(rgb, c.a);
}
