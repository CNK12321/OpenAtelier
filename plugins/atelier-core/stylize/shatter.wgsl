// Shatter: the picture broken into shards (Voronoi cells, one around a jittered point in
// each cell of a grid about `parts` cells big) that fly out from the center point.
// Params: seed, parts, size (how far from the center it breaks: 1 covers all of it),
// center_point (fraction of the layer), travel_distance (layer px), shards_rotate.
//
// Drawn backwards: for each pixel, each shard that could have landed on it (its flight
// passes near) is carried back to where it came from; the pixel is that shard's if the
// point it came from lies in the shard (its nearest cell point is the shard's own).

fn oa_shatter_hash(p: vec2f, seed: f32) -> vec2f {
    let q = vec2f(dot(p, vec2f(127.1, 311.7)), dot(p, vec2f(269.5, 183.3))) + seed * vec2f(12.9898, 78.233);
    return fract(sin(q) * 43758.5453);
}

// The shard point of grid cell `cell` (in layer px).
fn oa_shatter_site(cell: vec2i, grid: vec2i, cs: vec2f, seed: f32) -> vec2f {
    let h = oa_shatter_hash(vec2f(cell), seed);
    return in_origin() + (vec2f(cell) + 0.15 + 0.7 * h) * cs;
}

// The grid cell whose shard point is nearest `q`.
fn oa_shatter_owner(q: vec2f, grid: vec2i, cs: vec2f, seed: f32) -> vec2i {
    let home = vec2i(floor((q - in_origin()) / cs));
    var best = home;
    var best_d = 1e20;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let c = home + vec2i(i, j);
            if (any(c < vec2i(0)) || any(c >= grid)) { continue; }
            let d = distance(q, oa_shatter_site(c, grid, cs, seed));
            if (d < best_d) { best_d = d; best = c; }
        }
    }
    return best;
}

fn oa_stylize_shatter(pos: vec2f, base: u32) -> vec4f {
    let seed = u(base);
    let parts = clamp(u(base + 1u), 2.0, 144.0);
    let reach = max(u(base + 2u), 0.0);
    let size = max(in_size(), vec2f(1.0));
    let center = in_origin() + size * vec2f(u(base + 3u), u(base + 4u));
    let travel = max(u(base + 5u), 0.0);
    let rotate = u(base + 6u) > 0.5;
    if (travel <= 0.0) {
        return sample_input(pos);
    }
    // A grid of about `parts` roughly square cells over the layer.
    let aspect = size.x / size.y;
    let nx = clamp(i32(round(sqrt(parts * aspect))), 1, 16);
    let ny = clamp(i32(round(parts / f32(nx))), 1, 16);
    let grid = vec2i(nx, ny);
    let cs = size / vec2f(grid);
    // A shard reaches at most this far from its point (plus its flight).
    let radius = length(cs) * 1.2;
    // It breaks within `reach` × half the layer's diagonal of the center.
    let breaks = reach * 0.5 * length(size);
    for (var y = 0; y < ny; y++) {
        for (var x = 0; x < nx; x++) {
            let cell = vec2i(x, y);
            let s = oa_shatter_site(cell, grid, cs, seed);
            let h = oa_shatter_hash(vec2f(cell) + 17.0, seed);
            var flight = vec2f(0.0);
            var angle = 0.0;
            if (distance(s, center) <= breaks) {
                let away = s - center;
                let dir = select(normalize(vec2f(h.x - 0.5, h.y - 0.5) + 1e-4), normalize(away), length(away) > 1e-3);
                flight = dir * travel * (0.55 + 0.45 * h.x);
                if (rotate) {
                    // Each its own way, more the further it's flown.
                    angle = (h.y - 0.5) * 6.2831853 * min(travel / max(length(cs), 1.0), 2.0);
                }
            }
            let landed = s + flight;
            if (distance(pos, landed) > radius) { continue; }
            // Back to where this point of the shard came from.
            let r = pos - landed;
            let ca = cos(-angle);
            let sa = sin(-angle);
            let q = s + vec2f(r.x * ca - r.y * sa, r.x * sa + r.y * ca);
            if (any(q < in_origin()) || any(q >= in_origin() + size)) { continue; }
            if (all(oa_shatter_owner(q, grid, cs, seed) == cell)) {
                return sample_input(q);
            }
        }
    }
    return vec4f(0.0);
}
