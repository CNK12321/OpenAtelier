fn oa_color_hue(c: vec4f, base: u32) -> vec4f {
    // Turn the colour wheel: a rotation about the grey axis.
    let a = radians(u(base));
    let k = vec3f(0.57735, 0.57735, 0.57735);
    let cosa = cos(a);
    let rgb = c.rgb * cosa + cross(k, c.rgb) * sin(a) + k * dot(k, c.rgb) * (1.0 - cosa);
    return vec4f(mix(c.rgb, rgb, clamp(u(base + 1u), 0.0, 1.0)), c.a);
}
