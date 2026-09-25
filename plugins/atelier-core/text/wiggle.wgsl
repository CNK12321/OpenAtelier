fn oa_text_wiggle(g_in: Glyph, base: u32) -> Glyph {
    // Each letter wanders on its own smooth noise path.
    var g = g_in;
    let t = clip_seconds() * u(base + 1u);
    let i = g.index * 7.31;
    g.offset += vec2f(oa_noise(i, t), oa_noise(i + 3.7, t)) * u(base) * g.em;
    g.rotation += oa_noise(i + 9.1, t) * u(base + 2u);
    return g;
}
