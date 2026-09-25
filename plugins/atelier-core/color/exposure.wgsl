fn oa_color_exposure(c: vec4f, base: u32) -> vec4f {
    return vec4f(c.rgb * pow(2.0, u(base)), c.a);
}
