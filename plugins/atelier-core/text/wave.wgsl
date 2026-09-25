fn oa_text_wave(g_in: Glyph, base: u32) -> Glyph {
    // A sine wave travelling along the letters.
    var g = g_in;
    let phase = (g.index / max(u(base + 2u), 1.0) - clip_seconds() * u(base + 1u)) * 6.2831853;
    g.offset.y += sin(phase) * u(base) * g.em;
    return g;
}
