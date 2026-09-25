fn oa_transition_dip(pos: vec2f, progress: f32, base: u32) -> vec4f {
    let c = vec4f(u(base), u(base + 1u), u(base + 2u), u(base + 3u));
    let color = vec4f(c.rgb * c.a, c.a);
    if (progress < 0.5) { return mix(sample_a(pos), color, progress * 2.0); }
    return mix(color, sample_b(pos), progress * 2.0 - 1.0);
}
