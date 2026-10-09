fn oa_color_posterize(c: vec4f, base: u32) -> vec4f {
    // At the top of its range it's off: the picture as it is (its off state, so an
    // intro or outro ends without a step).
    if (u(base) >= 256.0) {
        return c;
    }
    let steps = max(u(base) - 1.0, 1.0);
    return vec4f(round(clamp(c.rgb, vec3f(0.0), vec3f(1.0)) * steps) / steps, c.a);
}
