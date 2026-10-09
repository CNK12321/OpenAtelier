// Slice: the picture cut along a line through `point` (a fraction of the layer) at
// `angle` (degrees, 0 = across to the right, 90 = down), the two sides pushed
// `separation` px apart (half each, away from the cut) and slid `slide` px along it in
// opposite directions.
//
// Drawn backwards: each pixel, on one side of the cut, reads from where that side's
// picture came from; a pixel whose source lies across the cut is in the gap (clear),
// with a pixel of soft edge.

fn oa_stylize_slice(pos: vec2f, base: u32) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    let center = in_origin() + size * vec2f(u(base), u(base + 1u));
    let a = radians(u(base + 2u));
    let along = vec2f(cos(a), sin(a));
    let normal = vec2f(-along.y, along.x);
    let gap = max(u(base + 3u), 0.0) * 0.5;
    let slide = u(base + 4u) * 0.5;
    let side = select(-1.0, 1.0, dot(pos - center, normal) >= 0.0);
    let src = pos - normal * side * gap - along * side * slide;
    // How far the source is inside its own side: past the cut, it's the gap.
    let inside = dot(src - center, normal) * side;
    let edge = clamp(inside + 0.5, 0.0, 1.0);
    if (edge <= 0.0) {
        return vec4f(0.0);
    }
    return sample_input(src) * edge;
}
