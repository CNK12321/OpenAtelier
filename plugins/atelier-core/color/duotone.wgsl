fn oa_color_duotone(c: vec4f, base: u32) -> vec4f {
    // Brightness mapped onto two colours: shadows to one, highlights to
    // the other.
    let luma = clamp(dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    let shaped = pow(luma, max(u(base + 8u), 0.05));
    let dark = vec3f(u(base), u(base + 1u), u(base + 2u));
    let light = vec3f(u(base + 4u), u(base + 5u), u(base + 6u));
    return vec4f(mix(c.rgb, mix(dark, light, shaped), clamp(u(base + 9u), 0.0, 1.0)), c.a);
}
