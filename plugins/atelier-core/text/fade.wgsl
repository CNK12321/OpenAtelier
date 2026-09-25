fn oa_text_fade(g_in: Glyph, base: u32) -> Glyph {
    // Letter by letter, each fades in.
    var g = g_in;
    g.color.a *= letter_progress(g, u(base));
    return g;
}
