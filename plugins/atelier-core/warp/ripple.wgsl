fn oa_warp_ripple(pos: vec2f, base: u32) -> vec4f {
    // Rings running out from the center, like a stone dropped in water.
    let size = max(in_size(), vec2f(1.0));
    let rel = pos - (in_origin() + size * vec2f(u(base + 3u), u(base + 4u)));
    let d = length(rel);
    let wave = sin((d / max(u(base + 1u), 1.0) - clip_seconds() * u(base + 2u)) * 6.2831853);
    let dir = select(vec2f(0.0), rel / d, d > 0.001);
    return sample_input_clamped(pos + dir * wave * u(base));
}
