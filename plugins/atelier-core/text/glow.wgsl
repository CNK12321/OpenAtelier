fn oa_text_glow(c: vec4f, p: TextPixel, base: u32) -> vec4f {
    // A soft halo around the letters, from the distance to their outline.
    // Its color is a directional gradient across the text box.
    let col = oa_gradient(base, p.pos, vec2f(0.0), p.box_size);
    let r = max(u(base + 32u) * p.em, 0.5);
    let out = max(-p.dist, 0.0);
    let edge = 1.0 - smoothstep(0.6 * p.spread, p.spread, out);
    let g = exp(-2.5 * out / r) * col.a * u(base + 33u) * edge;
    let a = c.a + g * (1.0 - c.a);
    let rgb = (c.rgb * c.a + col.rgb * g * (1.0 - c.a)) / max(a, 1e-5);
    return vec4f(rgb, a);
}
