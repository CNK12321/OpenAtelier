fn oa_blur_zoom(pos: vec2f, base: u32) -> vec4f {
    // Smear along the line from the center: a zoom (or spin) blur.
    let size = max(in_size(), vec2f(1.0));
    let mid = in_origin() + size * vec2f(u(base + 2u), u(base + 3u));
    let rel = pos - mid;
    let zoom = clamp(u(base), -2.0, 2.0);
    let spin = radians(u(base + 1u));
    let steps = 24;
    var sum = vec4f(0.0);
    for (var i = 0; i < steps; i++) {
        let t = f32(i) / f32(steps - 1) - 0.5;
        let scale = 1.0 + zoom * t * 0.25;
        let a = spin * t;
        let turned = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
        sum += sample_input_clamped(mid + turned * scale);
    }
    return sum / f32(steps);
}
