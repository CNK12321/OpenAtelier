fn oa_anim_blur(pos: vec2f, base: u32) -> vec4f {
    // Fully blurred (by `radius`) when hidden, sharp when visible.
    let radius = u(base) * (1.0 - visibility());
    if (radius < 0.5) { return sample_input(pos); }
    let dir = select(vec2f(0.0, 1.0), vec2f(1.0, 0.0), pass_index() == 0u);
    let sigma = radius / 2.5;
    let step = max(1.0, radius / 24.0);
    let n = i32(ceil(radius / step));
    var sum = vec4f(0.0);
    var wsum = 0.0;
    for (var i = -n; i <= n; i++) {
        let x = f32(i) * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        sum += sample_input(pos + dir * x) * w;
        wsum += w;
    }
    return sum / wsum;
}
