fn oa_transition_slide(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // The incoming picture slides in over the outgoing one, travelling
    // towards `direction` (0 = right, 90 = down), easing to a stop.
    let a = radians(u(base));
    let dir = vec2f(cos(a), sin(a));
    let size = out_size();
    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
    let e = 1.0 - pow(1.0 - progress, 3.0);
    let b = sample_b(pos + dir * (1.0 - e) * reach);
    return b + sample_a(pos) * (1.0 - b.a);
}
