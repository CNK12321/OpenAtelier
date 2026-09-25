fn oa_transition_zoom(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // The outgoing picture rushes towards you as the incoming one settles
    // in from close up, crossing over in the middle.
    let c = 0.5 * out_size();
    let k = max(u(base), 0.0);
    let a = sample_a(c + (pos - c) / (1.0 + progress * k));
    let b = sample_b(c + (pos - c) / (1.0 + (1.0 - progress) * k));
    return mix(a, b, smoothstep(0.3, 0.7, progress));
}
