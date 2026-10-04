// Luma Key: transparent where the picture is dark (or bright), by its brightness as it
// looks (display-encoded). Params: key_out (0 darks, 1 brights), threshold, softness.
fn oa_key_luma(c: vec4f, base: u32) -> vec4f {
    let l = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    let threshold = u(base + 1u);
    let soft = max(u(base + 2u), 1e-4) * 0.5;
    // How much stays: 0 below the threshold, 1 above it, a ramp softness wide between.
    var keep = smoothstep(threshold - soft, threshold + soft, l);
    if (u(base) > 0.5) {
        keep = 1.0 - keep;
    }
    return vec4f(c.rgb, c.a * keep);
}
