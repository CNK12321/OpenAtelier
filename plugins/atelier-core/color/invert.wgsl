fn oa_color_invert(c: vec4f, base: u32) -> vec4f {
    return vec4f(mix(c.rgb, vec3f(1.0) - c.rgb, clamp(u(base), 0.0, 1.0)), c.a);
}
