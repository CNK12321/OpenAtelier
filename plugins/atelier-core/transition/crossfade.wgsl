fn oa_transition_crossfade(pos: vec2f, progress: f32, base: u32) -> vec4f {
    return mix(sample_a(pos), sample_b(pos), progress);
}
