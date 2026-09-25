fn oa_glitch_hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

fn oa_stylize_glitch(pos: vec2f, base: u32) -> vec4f {
    // Bands of rows jump sideways for a moment, the colors pull apart.
    let amount = clamp(u(base), 0.0, 1.0);
    let tick = floor(clip_seconds() * max(u(base + 1u), 0.0));
    let size = max(in_size(), vec2f(1.0));
    let y = (pos.y - in_origin().y) / size.y;
    let band = floor(y * mix(4.0, 24.0, oa_glitch_hash(vec2f(tick, 1.0))));
    let hit = oa_glitch_hash(vec2f(band, tick));
    var shift = 0.0;
    if (hit < amount * 0.6) {
        shift = (oa_glitch_hash(vec2f(band, tick + 7.0)) - 0.5) * amount * 0.3 * size.x;
    }
    let split = u(base + 2u) * (0.3 + amount) * select(1.0, 2.5, hit < amount * 0.3);
    let p = pos + vec2f(shift, 0.0);
    let r = sample_input(p + vec2f(split, 0.0));
    let g = sample_input(p);
    let b = sample_input(p - vec2f(split, 0.0));
    return vec4f(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}
