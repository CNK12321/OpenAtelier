fn oa_text_typewriter(g_in: Glyph, base: u32) -> Glyph {
    // Letters appear one at a time.
    var g = g_in;
    let shown = visibility() * g.count;
    g.color.a *= clamp((shown - g.index) * 3.0, 0.0, 1.0);
    return g;
}
