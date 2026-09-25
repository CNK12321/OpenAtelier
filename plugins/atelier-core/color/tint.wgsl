fn oa_color_tint(c: vec4f, base: u32) -> vec4f {
    // The color (a directional gradient across the layer) takes over the
    // picture's hue, keeping its brightness.
    let g = oa_gradient(base, layer_pos(), in_origin(), in_size());
    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    return vec4f(mix(c.rgb, g.rgb * luma, u(base + 32u) * g.a), c.a);
}
