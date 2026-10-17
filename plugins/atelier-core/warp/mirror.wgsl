// Mirror: everything past a line is the reflection of what's before it. The line runs
// through `point` (a fraction of the layer) at `angle` (degrees, 0 = across to the
// right, 90 = down: a line straight down mirrors left onto right). `flip` mirrors the
// other side onto this one instead.
fn oa_warp_mirror(pos: vec2f, base: u32) -> vec2f {
    let size = max(in_size(), vec2f(1.0));
    let center = in_origin() + size * vec2f(u(base), u(base + 1u));
    let a = radians(u(base + 2u));
    let normal = vec2f(-sin(a), cos(a));
    let flip = u(base + 3u) > 0.5;
    let d = dot(pos - center, normal);
    let past = select((d < 0.0), (d > 0.0), flip);
    if (past) {
        return pos - 2.0 * d * normal;
    }
    return pos;
}
