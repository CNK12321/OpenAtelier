fn oa_key_chroma(c: vec4f, base: u32) -> vec4f {
    // Pixels near the key color (by chroma, so shadows on a green screen
    // key too) become transparent; spill pulls the key color out of edges.
    let key = srgb_encode(vec3f(u(base), u(base + 1u), u(base + 2u)));
    let chroma = mat3x2f(vec2f(-0.1146, 0.5), vec2f(-0.3854, -0.4542), vec2f(0.5, -0.0458));
    // Chroma divided by brightness: the same green in shadow keys the same.
    let luma = vec3f(0.2126, 0.7152, 0.0722);
    let d = distance(chroma * c.rgb / (dot(c.rgb, luma) + 0.15), chroma * key / (dot(key, luma) + 0.15));
    let tol = u(base + 4u);
    let soft = max(u(base + 5u), 1e-4);
    let keep = smoothstep(tol, tol + soft, d);
    // Spill: near the key, cap the key's dominant channel at the others' level.
    let near = 1.0 - smoothstep(tol, tol + soft * 3.0, d);
    var rgb = c.rgb;
    if (key.g >= key.r && key.g >= key.b) {
        rgb.g = mix(rgb.g, min(rgb.g, max(rgb.r, rgb.b)), u(base + 6u) * near);
    } else if (key.b >= key.r) {
        rgb.b = mix(rgb.b, min(rgb.b, max(rgb.r, rgb.g)), u(base + 6u) * near);
    } else {
        rgb.r = mix(rgb.r, min(rgb.r, max(rgb.g, rgb.b)), u(base + 6u) * near);
    }
    return vec4f(rgb, c.a * keep);
}
