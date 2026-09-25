fn oa_transition_push(pos: vec2f, progress: f32, base: u32) -> vec4f {
    // The incoming picture pushes the outgoing one off the canvas, both
    // moving along `direction` (0 = right, 90 = down).
    let r = radians(u(base));
    let dir = vec2f(cos(r), sin(r));
    // A full canvas along dir, stretched for diagonals so both axes clear.
    let size = out_size();
    let reach = (abs(dir.x) * size.x + abs(dir.y) * size.y) / max(max(abs(dir.x), abs(dir.y)), 1e-6);
    let e = progress * progress * (3.0 - 2.0 * progress);
    let shift = dir * reach * e;
    let a = sample_a(pos - shift);
    let b = sample_b(pos - shift + dir * reach);
    return b + a * (1.0 - b.a);
}
