fn oa_transition_iris(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // A circle opens from the middle, the incoming picture inside it.
    let size = out_size();
    let soft = max(u(base), 0.001);
    let reach = 0.5 * length(size) + soft;
    let r = progress * (reach + soft) - soft;
    let t = smoothstep(r - soft, r + soft, length(pos - 0.5 * size));
    return mix(sample_b(pos), sample_a(pos), t);
}
