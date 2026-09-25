// The input at `p` (layer px from its corner) repeated forever in both
// directions: bilinear over four texels, each wrapped round, so there's
// no seam where the picture meets its own other edge.
fn oa_scroll_wrapped(p: vec2f) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    let q = p - 0.5;
    let i0 = floor(q);
    let f = q - i0;
    let a = (i0 % size + size) % size;
    let b = ((i0 + 1.0) % size + size) % size;
    let o = in_origin() + 0.5;
    let top = mix(sample_input(o + vec2f(a.x, a.y)), sample_input(o + vec2f(b.x, a.y)), f.x);
    let bottom = mix(sample_input(o + vec2f(a.x, b.y)), sample_input(o + vec2f(b.x, b.y)), f.x);
    return mix(top, bottom, f.y);
}

fn oa_warp_scroll(pos: vec2f, base: u32) -> vec4f {
    // Moving `speed` px a second towards `direction` (0 = right, 90 = down).
    let a = radians(u(base));
    let shift = vec2f(cos(a), sin(a)) * u(base + 1u) * clip_seconds();
    let p = pos - shift;
    if (u(base + 2u) > 0.5) { return sample_input(p); }
    return oa_scroll_wrapped(p - in_origin());
}
