fn oa_warp_tile(pos: vec2f, base: u32) -> vec4f {
    // The whole picture, shrunk into each cell of a grid over the layer.
    let size = max(in_size(), vec2f(1.0));
    let counts = max(vec2f(u(base), u(base + 1u)), vec2f(1.0));
    let cell = (pos - in_origin()) / size * counts;
    let index = floor(cell);
    var uv = cell - index;
    // A gap around each picture, as a share of its cell.
    let gap = clamp(u(base + 2u), 0.0, 0.9) * 0.5;
    if (any(uv < vec2f(gap)) || any(uv > vec2f(1.0 - gap))) { return vec4f(0.0); }
    uv = (uv - gap) / (1.0 - 2.0 * gap);
    // Mirrored: every other cell flipped, so neighbors meet edge to edge.
    if (u(base + 3u) > 0.5) {
        let odd = abs(index % 2.0) > vec2f(0.5);
        uv = select(uv, 1.0 - uv, odd);
    }
    return sample_input(in_origin() + uv * size);
}
