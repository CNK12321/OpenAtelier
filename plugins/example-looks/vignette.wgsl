// Darkens the corners. `base` is this effect's first parameter slot: amount, then size.
fn example_vignette(c: vec4f, base: u32) -> vec4f {
    let rel = (layer_pos() - in_origin()) / max(in_size(), vec2f(1.0)) - 0.5;
    let d = length(rel) / max(u(base + 1u), 0.05);
    let shade = 1.0 - u(base) * smoothstep(0.5, 1.1, d);
    return vec4f(c.rgb * shade, c.a);
}
