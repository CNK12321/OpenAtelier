fn oa_text_rise(g_in: Glyph, base: u32) -> Glyph {
    // Letter by letter, each rises into place (and fades in).
    var g = g_in;
    let p = oa_ease_out(letter_progress(g, u(base + 1u)));
    g.offset.y += (1.0 - p) * u(base) * g.em;
    if (u(base + 2u) > 0.5) { g.color.a *= p; }
    return g;
}
