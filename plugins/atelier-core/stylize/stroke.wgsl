fn oa_stylize_stroke(pos: vec2f, base: u32) -> vec4f {
    // An outline `width` px wide around whatever is opaque: the most cover
    // found on two rings of sixteen around the pixel, behind the picture.
    let w = max(u(base), 0.0);
    let color = vec4f(u(base + 1u), u(base + 2u), u(base + 3u), u(base + 4u));
    let c = sample_input(pos);
    if (w < 0.5 || c.a > 0.999) { return c; }
    var cover = 0.0;
    for (var k = 0; k < 16; k++) {
        let t = f32(k) * 0.3926991;
        let d = vec2f(cos(t), sin(t));
        cover = max(cover, max(sample_input(pos + d * w).a, sample_input(pos + d * w * 0.5).a));
    }
    let alpha = cover * color.a;
    return c + vec4f(color.rgb * alpha, alpha) * (1.0 - c.a);
}
