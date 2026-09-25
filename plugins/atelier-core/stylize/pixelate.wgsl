fn oa_stylize_pixelate(pos: vec2f, base: u32) -> vec4f {
    // Snap to the middle of a block, so each block takes one sample.
    let size = max(u(base), 1.0);
    let lo = in_origin();
    let cell = floor((pos - lo) / size) * size + size * 0.5;
    let blend = clamp(u(base + 1u), 0.0, 1.0);
    return mix(sample_input(lo + cell), sample_input(pos), blend);
}
