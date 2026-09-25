fn oa_text_scatter(g_in: Glyph, base: u32) -> Glyph {
    // Letters fly together from random directions, spinning into place.
    var g = g_in;
    let p = oa_ease_out(letter_progress(g, u(base + 2u)));
    let a = oa_hash(g.index * 1.7 + 0.3) * 6.2831853;
    let r = (0.4 + 0.6 * oa_hash(g.index * 3.1 + 1.1)) * u(base) * g.em;
    g.offset += vec2f(cos(a), sin(a)) * r * (1.0 - p);
    g.rotation += (oa_hash(g.index * 5.3) * 2.0 - 1.0) * u(base + 1u) * (1.0 - p);
    g.color.a *= p;
    return g;
}
