fn oa_stylize_scanlines(c: vec4f, base: u32) -> vec4f {
    // Dark lines every `spacing` layer pixels, travelling with `roll`.
    let spacing = max(u(base + 1u), 1.0);
    let y = (layer_pos().y - in_origin().y) / spacing + u(base + 3u) * clip_seconds();
    let wave = 0.5 + 0.5 * cos(6.2831853 * y);
    let line = pow(wave, max(u(base + 2u), 0.01));
    return vec4f(c.rgb * (1.0 - clamp(u(base), 0.0, 4.0) * line), c.a);
}
