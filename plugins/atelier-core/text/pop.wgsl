fn oa_text_pop(g_in: Glyph, base: u32) -> Glyph {
    // Letter by letter, each grows from nothing with a little overshoot.
    var g = g_in;
    let p = letter_progress(g, u(base));
    g.scale *= vec2f(max(oa_ease_back(p, u(base + 1u)), 0.0));
    g.color.a *= clamp(p * 4.0, 0.0, 1.0);
    return g;
}
