fn oa_surface_cross(a: vec2f, b: vec2f) -> f32 { return a.x * b.y - a.y * b.x; }

// u from v, where h = u (e + g v) + f v; solved on the steadier axis.
fn oa_surface_u(h: vec2f, e: vec2f, f: vec2f, g: vec2f, v: f32) -> f32 {
    let den = e + g * v;
    if (abs(den.x) >= abs(den.y)) {
        if (abs(den.x) < 1e-9) { return -1.0; }
        return (h.x - f.x * v) / den.x;
    }
    return (h.y - f.y * v) / den.y;
}

// Where `p` is in the quad a b c d (clockwise from the top left), as
// (u, v) — both in 0..1 when it's inside. Bilinear interpolation run
// backwards (after Inigo Quilez).
fn oa_surface_unmap(p: vec2f, a: vec2f, b: vec2f, c: vec2f, d: vec2f) -> vec2f {
    let e = b - a;
    let f = d - a;
    let g = a - b + c - d;
    let h = p - a;
    let k2 = oa_surface_cross(g, f);
    let k1 = oa_surface_cross(e, f) + oa_surface_cross(h, g);
    let k0 = oa_surface_cross(h, e);
    if (abs(k2) < 1e-7) {
        if (abs(k1) < 1e-9) { return vec2f(-1.0); }
        let v = -k0 / k1;
        return vec2f(oa_surface_u(h, e, f, g, v), v);
    }
    let w = k1 * k1 - 4.0 * k0 * k2;
    if (w < 0.0) { return vec2f(-1.0); }
    let s = sqrt(w);
    var v = (-k1 - s) / (2.0 * k2);
    var u = oa_surface_u(h, e, f, g, v);
    if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {
        v = (-k1 + s) / (2.0 * k2);
        u = oa_surface_u(h, e, f, g, v);
    }
    return vec2f(u, v);
}

// A point of the n × n grid, in fractions of the layer: where it rests
// plus its offset (params are stored for a 4 × 4 grid).
fn oa_surface_point(base: u32, n: u32, r: u32, c: u32) -> vec2f {
    let k = base + 1u + 2u * (r * 4u + c);
    return vec2f(f32(c), f32(r)) / f32(n - 1u) + vec2f(u(k), u(k + 1u));
}

// The picture at `p` (fractions of the layer): found in whichever cell
// of the grid covers it.
fn oa_surface_at(p: vec2f, base: u32, n: u32) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    for (var r = 0u; r + 1u < n; r++) {
        for (var c = 0u; c + 1u < n; c++) {
            let uv = oa_surface_unmap(
                p,
                oa_surface_point(base, n, r, c),
                oa_surface_point(base, n, r, c + 1u),
                oa_surface_point(base, n, r + 1u, c + 1u),
                oa_surface_point(base, n, r + 1u, c),
            );
            if (all(uv >= vec2f(-1e-4)) && all(uv <= vec2f(1.0 + 1e-4))) {
                let src = (vec2f(f32(c), f32(r)) + clamp(uv, vec2f(0.0), vec2f(1.0))) / f32(n - 1u);
                return sample_input(in_origin() + src * size);
            }
        }
    }
    return vec4f(0.0);
}

fn oa_warp_surface(pos: vec2f, base: u32) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    let n = u32(clamp(u(base), 0.0, 2.0)) + 2u;
    // Four samples a pixel, so the stretched edges stay smooth.
    var sum = vec4f(0.0);
    for (var i = 0u; i < 4u; i++) {
        let offset = vec2f(f32(i % 2u), f32(i / 2u)) * 0.5 - 0.25;
        sum += oa_surface_at((pos + offset - in_origin()) / size, base, n);
    }
    return sum * 0.25;
}
