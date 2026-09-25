fn oa_stylize_vignette(c: vec4f, base: u32) -> vec4f {
    // Distance from the center, in units of half the layer, so a wide
    // layer darkens at its own corners rather than a circle's.
    let size = max(in_size(), vec2f(1.0));
    let rel = (layer_pos() - in_origin()) / size - vec2f(u(base + 8u), u(base + 9u));
    let round = clamp(u(base + 3u), 0.0, 1.0);
    let square = max(abs(rel.x), abs(rel.y)) * 2.0;
    let circle = length(rel * vec2f(max(size.x / size.y, 1.0), max(size.y / size.x, 1.0))) * 2.0;
    let d = mix(square, circle, round) / max(u(base + 1u), 0.05);
    let soft = max(u(base + 2u), 0.001);
    let k = clamp(u(base), 0.0, 4.0) * smoothstep(1.0 - soft, 1.0 + soft, d);
    let tint = vec3f(u(base + 4u), u(base + 5u), u(base + 6u));
    let alpha = u(base + 7u);
    // A coloured vignette mixes towards the colour; a black one just darkens.
    return vec4f(mix(c.rgb, tint, clamp(k * alpha, 0.0, 1.0)) * (1.0 - k * (1.0 - alpha)), c.a);
}
