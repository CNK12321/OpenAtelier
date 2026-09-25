fn oa_stylize_edges(pos: vec2f, base: u32) -> vec4f {
    // Sobel on brightness.
    let r = max(u(base + 1u), 0.5);
    var gx = vec3f(0.0);
    var gy = vec3f(0.0);
    gx += (sample_input(pos + vec2f(r, -r)) + 2.0 * sample_input(pos + vec2f(r, 0.0)) + sample_input(pos + vec2f(r, r))).rgb;
    gx -= (sample_input(pos + vec2f(-r, -r)) + 2.0 * sample_input(pos + vec2f(-r, 0.0)) + sample_input(pos + vec2f(-r, r))).rgb;
    gy += (sample_input(pos + vec2f(-r, r)) + 2.0 * sample_input(pos + vec2f(0.0, r)) + sample_input(pos + vec2f(r, r))).rgb;
    gy -= (sample_input(pos + vec2f(-r, -r)) + 2.0 * sample_input(pos + vec2f(0.0, -r)) + sample_input(pos + vec2f(r, -r))).rgb;
    let edge = clamp(length(vec2f(length(gx), length(gy))) * u(base), 0.0, 8.0);
    let c = sample_input(pos);
    let tint = vec3f(u(base + 3u), u(base + 4u), u(base + 5u));
    // `blend` 1 shows the edges alone, 0 draws them over the picture.
    return vec4f(mix(c.rgb + tint * edge, tint * edge, clamp(u(base + 2u), 0.0, 1.0)), c.a);
}
