fn oa_text_rainbow(g_in: Glyph, base: u32) -> Glyph {
    // Each letter a hue, cycling over time.
    var g = g_in;
    let h = fract(g.index * u(base + 1u) + clip_seconds() * u(base));
    let rgb = srgb_decode(oa_hsv(h, 0.8, 1.0));
    g.color = vec4f(mix(g.color.rgb, g.color.rgb * rgb, u(base + 2u)), g.color.a);
    return g;
}
