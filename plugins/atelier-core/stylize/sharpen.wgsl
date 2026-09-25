fn oa_stylize_sharpen(pos: vec2f, base: u32) -> vec4f {
    // Unsharp mask: the picture, plus what a small blur took away.
    let r = max(u(base + 1u), 0.5);
    let c = sample_input(pos);
    var blur = vec4f(0.0);
    blur += sample_input(pos + vec2f(-r, -r)) + sample_input(pos + vec2f(r, -r));
    blur += sample_input(pos + vec2f(-r, r)) + sample_input(pos + vec2f(r, r));
    blur += sample_input(pos + vec2f(-r, 0.0)) + sample_input(pos + vec2f(r, 0.0));
    blur += sample_input(pos + vec2f(0.0, -r)) + sample_input(pos + vec2f(0.0, r));
    blur = blur / 8.0;
    return c + (c - blur) * clamp(u(base), 0.0, 8.0);
}
