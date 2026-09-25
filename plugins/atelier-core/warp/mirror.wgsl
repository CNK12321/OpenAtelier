fn oa_warp_mirror(pos: vec2f, base: u32) -> vec4f {
    // Everything past the line is the reflection of what's before it.
    let size = max(in_size(), vec2f(1.0));
    let vertical = u(base) < 0.5;
    let at = in_origin() + size * clamp(u(base + 1u), 0.0, 1.0);
    let flip = u(base + 2u) > 0.5;
    var p = pos;
    if (vertical) {
        let past = select(pos.x > at.x, pos.x < at.x, flip);
        if (past) { p.x = 2.0 * at.x - pos.x; }
    } else {
        let past = select(pos.y > at.y, pos.y < at.y, flip);
        if (past) { p.y = 2.0 * at.y - pos.y; }
    }
    return sample_input(p);
}
