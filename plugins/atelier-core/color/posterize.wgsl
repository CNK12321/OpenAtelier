fn oa_color_posterize(c: vec4f, base: u32) -> vec4f {
    let steps = max(u(base) - 1.0, 1.0);
    return vec4f(round(clamp(c.rgb, vec3f(0.0), vec3f(1.0)) * steps) / steps, c.a);
}
