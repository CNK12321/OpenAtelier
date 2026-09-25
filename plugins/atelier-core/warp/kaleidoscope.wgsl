fn oa_warp_kaleidoscope(pos: vec2f, base: u32) -> vec4f {
    // The picture folded into `segments` mirrored wedges around its center.
    let size = max(in_size(), vec2f(1.0));
    let center = in_origin() + size * vec2f(u(base + 2u), u(base + 3u));
    let rel = pos - center;
    let n = max(round(u(base)), 1.0);
    let wedge = 6.2831853 / n;
    let turn = radians(u(base + 1u));
    var a = atan2(rel.y, rel.x) - turn;
    a = a - wedge * floor(a / wedge);
    if (a > 0.5 * wedge) { a = wedge - a; }
    let r = length(rel);
    return sample_input_clamped(center + vec2f(cos(a + turn), sin(a + turn)) * r);
}
