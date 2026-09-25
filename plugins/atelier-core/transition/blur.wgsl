fn oa_transition_blur(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // Both pictures go out of focus towards the middle, dissolving there.
    let radius = max(u(base), 0.0) * sin(progress * 3.1415927);
    var a = sample_a(pos);
    var b = sample_b(pos);
    var weight = 1.0;
    for (var ring = 1; ring <= 2; ring++) {
        let r = radius * f32(ring) * 0.5;
        for (var k = 0; k < 8; k++) {
            let t = f32(k) * 0.7853982 + f32(ring) * 0.39;
            let p = pos + vec2f(cos(t), sin(t)) * r;
            a += sample_a(p);
            b += sample_b(p);
            weight += 1.0;
        }
    }
    return mix(a, b, smoothstep(0.2, 0.8, progress)) / weight;
}
