// Jump-flood steps that reach `radius`: 2^(n-1) … 1 (`glow_jumps`).
fn oa_glow_jumps(radius: f32) -> u32 {
    var n = 1u;
    var reach = 1.0;
    while (reach < radius && n < 12u) { reach *= 2.0; n++; }
    return n;
}

// Pixels per flood cell (`glow_divisor`): wide halos use a coarser grid.
fn oa_glow_cell(radius: f32) -> u32 {
    var k = 1u;
    while (f32(k * 2u) * 12.0 <= radius && k < 8u) { k *= 2u; }
    return k;
}

// A cell of the previous flood pass: (way to its seed from the cell's
// center, in layer px; found; 1). Outside the grid: nothing found.
fn oa_glow_load(cell: vec2i) -> vec4f {
    let dims = vec2i(textureDimensions(input_tex));
    if (any(cell < vec2i(0)) || any(cell >= dims)) { return vec4f(0.0); }
    return textureLoad(input_tex, cell, 0);
}

fn oa_light_glow(pos: vec2f, base: u32) -> vec4f {
    let radius = max(u(base), 0.0);
    let k = oa_glow_cell(radius);
    let kf = f32(k);
    let jumps = oa_glow_jumps(radius / kf);
    let at_pass = pass_index();
    // The flood's passes are on the coarse grid: which cell is this?
    let cell = vec2i(floor((pos - out_origin())));
    let center = out_origin() + (vec2f(cell) + 0.5) * kf;
    if (at_pass == 0u) {
        // Seeds: an opaque point of the picture inside the cell (a few
        // looked at, so thin parts aren't missed on a coarse grid).
        for (var j = 0; j < 2; j++) {
            for (var i = 0; i < 2; i++) {
                let p = center + (vec2f(f32(i), f32(j)) - 0.5) * 0.5 * kf;
                if (sample_input(p).a > 0.5) { return vec4f(p - center, 1.0, 1.0); }
            }
        }
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    if (at_pass <= jumps) {
        // Jump flood: look `step` cells away in eight directions and keep
        // whichever seed those cells know that's nearest to this one.
        let step = i32(1u << (jumps - at_pass));
        var best = oa_glow_load(cell);
        var best_d = select(1e9, length(best.xy), best.z > 0.5);
        for (var j = -1; j <= 1; j++) {
            for (var i = -1; i <= 1; i++) {
                if (i == 0 && j == 0) { continue; }
                let d = vec2i(i, j) * step;
                let s = oa_glow_load(cell + d);
                if (s.z < 0.5) { continue; }
                let way = vec2f(d) * kf + s.xy;
                let dist = length(way);
                if (dist < best_d) { best_d = dist; best = vec4f(way, 1.0, 1.0); }
            }
        }
        return best;
    }
    // Last pass, full size: the nearest edge's color, fading with the
    // distance to it, with the sharp picture (the second input) over it.
    let c = select(vec4f(0.0), sample_media(pos), has_media());
    let over = u(base + 6u) > 0.5;
    if (c.a > 0.999 && !over) { return c; }
    let home = vec2i(floor((pos - in_origin()) / kf));
    let s = oa_glow_load(home);
    if (s.z < 0.5) { return c; }
    let seed = in_origin() + (vec2f(home) + 0.5) * kf + s.xy;
    let d = length(seed - pos);
    if (d > radius) { return c; }
    let edge = sample_media(seed);
    let t = 1.0 - d / max(radius, 1e-3);
    let tint = vec4f(u(base + 2u), u(base + 3u), u(base + 4u), u(base + 5u));
    let a = clamp(t * t * u(base + 1u) * tint.a, 0.0, 1.0);
    let g = vec4f(edge.rgb / max(edge.a, 1e-4) * tint.rgb * a, a);
    if (over) { return vec4f(c.rgb + g.rgb, max(c.a, g.a)); }
    return c + g * (1.0 - c.a);
}
