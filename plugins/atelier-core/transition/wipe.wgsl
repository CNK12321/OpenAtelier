fn oa_transition_wipe(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // A soft edge sweeps across the canvas along `angle` (0 = left to right).
    let a = radians(u(base));
    let dir = vec2f(cos(a), sin(a));
    let size = out_size();
    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
    let soft = max(u(base + 1u), 0.001);
    let start = min(0.0, dir.x * size.x) + min(0.0, dir.y * size.y) - soft;
    let edge = start + progress * (reach + 2.0 * soft);
    let t = smoothstep(edge - soft, edge + soft, dot(pos, dir));
    return mix(sample_b(pos), sample_a(pos), t);
}
