fn oa_color_chromatic(pos: vec2f, base: u32) -> vec4f {
    // Red and blue pulled apart along `direction`, more towards the edges.
    let size = max(in_size(), vec2f(1.0));
    let mid = in_origin() + size * 0.5;
    let a = radians(u(base + 1u));
    let dir = vec2f(cos(a), sin(a)) * u(base);
    let falloff = mix(1.0, length((pos - mid) / size) * 2.0, clamp(u(base + 2u), 0.0, 1.0));
    let shift = dir * falloff;
    let r = sample_input(pos + shift);
    let g = sample_input(pos);
    let b = sample_input(pos - shift);
    return vec4f(r.r, g.g, b.b, max(g.a, max(r.a, b.a)));
}
