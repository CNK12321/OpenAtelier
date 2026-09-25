fn oa_blur_smear(pos: vec2f, base: u32) -> vec4f {
    // A directional (motion) blur: the picture smeared along `direction`,
    // either both ways or trailing behind it.
    let len = u(base);
    if (len < 0.5) { return sample_input(pos); }
    let a = radians(u(base + 1u));
    let dir = vec2f(cos(a), sin(a));
    let trail = u(base + 2u) > 0.5;
    let n = i32(clamp(ceil(len / 1.5), 4.0, 64.0));
    var sum = vec4f(0.0);
    for (var i = 0; i <= n; i++) {
        let f = f32(i) / f32(n);
        let x = select(f - 0.5, -f, trail) * len;
        sum += sample_input_clamped(pos + dir * x);
    }
    return sum / f32(n + 1);
}
