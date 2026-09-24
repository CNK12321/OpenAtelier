// Dark horizontal lines, `spacing` layer pixels apart (the host scales that with the
// raster, so they look the same at any preview resolution).
fn example_scanlines(c: vec4f, base: u32) -> vec4f {
    let y = layer_pos().y / max(u(base + 1u), 1.0);
    let line = 0.5 + 0.5 * cos(6.28318 * y);
    return vec4f(c.rgb * (1.0 - u(base) * line), c.a);
}
