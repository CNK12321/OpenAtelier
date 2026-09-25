fn oa_color_saturation(c: vec4f, base: u32) -> vec4f {
    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    return vec4f(mix(vec3f(luma), c.rgb, u(base)), c.a);
}
