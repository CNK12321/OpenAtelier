fn oa_color_temperature(c: vec4f, base: u32) -> vec4f {
    // Warmer (towards orange) or cooler (towards blue), and a green or
    // magenta tint — the white balance, keeping the brightness.
    let warm = clamp(u(base), -1.0, 1.0);
    let tint = clamp(u(base + 1u), -1.0, 1.0);
    let gain = vec3f(1.0 + 0.35 * warm, 1.0 + 0.05 * warm - 0.25 * tint, 1.0 - 0.35 * warm);
    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    var rgb = c.rgb * gain;
    let after = dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
    rgb = rgb * select(1.0, luma / after, after > 1e-5);
    return vec4f(max(rgb, vec3f(0.0)), c.a);
}
