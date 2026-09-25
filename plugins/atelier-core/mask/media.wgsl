fn oa_mask_media(pos: vec2f, base: u32) -> vec4f {
    // The matte comes from another file in the pool, stretched over the
    // layer: its luminance (or alpha) decides what shows.
    let c = sample_input(pos);
    if (u(base) < 0.5) { return c; }
    let m = sample_media(pos);
    let luma = dot(m.rgb, vec3f(0.2126, 0.7152, 0.0722));
    var k = select(luma, m.a, u(base + 1u) > 0.5);
    if (u(base + 2u) > 0.5) { k = 1.0 - k; }
    return c * clamp(k, 0.0, 1.0);
}
