// Bend Along Path: the picture laid along a curve, as a band — its length (or, turned
// by `alignment`, any direction across it) running from the path's start to its end,
// its other direction across the band. The path arrives sampled evenly along its
// length (64 samples: x, y as fractions of the layer; width as a fraction of the
// layer's shorter side; offset, a sideways push, the same). Stretched, the picture runs
// the path's length once; repeated (or mirrored), it's laid end to end at its own
// shape. `start`/`end` show only part of the path, so animating `end` draws it in.

const OA_BEND_PATH: u32 = 259u;

// Sample `i`: (x, y, width, offset) as packed.
fn oa_bend_raw(base: u32, i: u32) -> vec4f {
    let b = base + 3u + i * 4u;
    return vec4f(u(b), u(b + 1u), u(b + 2u), u(b + 3u));
}

// Sample `i` on the layer (px), pushed sideways by its offset, and its width (px).
fn oa_bend_center(base: u32, i: u32, n: u32, closed: bool) -> vec3f {
    let size = in_size();
    let side = min(size.x, size.y);
    let s = oa_bend_raw(base, i);
    let p = in_origin() + s.xy * size;
    // The way the path goes here: from the sample before to the one after (a closed
    // path's ends meet, its last sample being its first).
    var prev = i;
    var next = i;
    if (i > 0u) { prev = i - 1u; } else if (closed) { prev = n - 2u; }
    if (i + 1u < n) { next = i + 1u; } else if (closed) { next = 1u; }
    let way = (oa_bend_raw(base, next).xy - oa_bend_raw(base, prev).xy) * size;
    let len = length(way);
    let t = select(vec2f(1.0, 0.0), way / len, len > 1e-6);
    let normal = vec2f(-t.y, t.x);
    return vec3f(p + normal * s.w * side, max(s.z, 0.0) * side);
}

fn oa_composite_bend(pos: vec2f, base: u32) -> vec4f {
    let n = u32(u(base));
    if (n < 2u) { return vec4f(0.0); }
    let closed = u(base + 1u) > 0.5;
    let q = base + OA_BEND_PATH;
    let fit = u32(round(u(q)));
    let turn = radians(u(q + 1u));
    let slide = u(q + 2u);
    let start = clamp(u(q + 3u), 0.0, 1.0);
    let end = clamp(u(q + 4u), 0.0, 1.0);
    let tile = max(u(q + 5u), 0.01);

    // The nearest place on the path: how far along it (px), how far to which side, and
    // the band's width there. Past an open path's ends the line runs on, so what lies
    // beyond them falls outside the path's length and isn't drawn.
    var best = 1e20;
    var along = 0.0;
    var across = 0.0;
    var width = 0.0;
    var run = 0.0;
    var area = 0.0;
    var a = oa_bend_center(base, 0u, n, closed);
    for (var i = 1u; i < n; i++) {
        let b = oa_bend_center(base, i, n, closed);
        let seg = b.xy - a.xy;
        let l2 = dot(seg, seg);
        let l = sqrt(l2);
        if (l2 > 1e-9) {
            var h = dot(pos - a.xy, seg) / l2;
            let lo = select(0.0, -1e9, i == 1u && !closed);
            let hi = select(1.0, 1e9, i + 1u == n && !closed);
            h = clamp(h, lo, hi);
            let near = a.xy + seg * h;
            let d = length(pos - near);
            if (d < best) {
                best = d;
                along = run + h * l;
                let cross = seg.x * (pos.y - a.y) - seg.y * (pos.x - a.x);
                across = select(-d, d, cross >= 0.0);
                width = mix(a.z, b.z, clamp(h, 0.0, 1.0));
            }
        }
        area += (a.z + b.z) * 0.5 * l;
        run += l;
        a = b;
    }
    let total = max(run, 1e-6);
    // Inside the band (a pixel's softness at its edges) and the shown part of the path.
    let cover_across = clamp(width * 0.5 - abs(across) + 0.5, 0.0, 1.0);
    let cover_along = clamp(along - start * total + 0.5, 0.0, 1.0) * clamp(end * total - along + 0.5, 0.0, 1.0);
    let cover = cover_across * cover_along;
    if (cover <= 0.0 || width <= 0.0) { return vec4f(0.0); }

    // Where on the picture: along the band (0 → 1) and across it (0 → 1).
    let size = in_size();
    let dir = vec2f(cos(turn), sin(turn));
    let perp = vec2f(-dir.y, dir.x);
    // The picture's extent along the turned direction, and across it.
    let span_along = abs(size.x * dir.x) + abs(size.y * dir.y);
    let span_across = abs(size.x * perp.x) + abs(size.y * perp.y);
    let v = across / max(width, 1e-3) + 0.5;
    var w = along / total - slide;
    if (fit == 0u) {
        if (w < 0.0 || w > 1.0) { return vec4f(0.0); }
    } else {
        // End to end at its own shape: as long as the band is wide (on average) times
        // the picture's length to its width, times `tile`.
        let mean = area / total;
        let piece = max(mean * span_along / max(span_across, 1e-3) * tile, 1.0);
        let x = (along - slide * total) / piece;
        let k = floor(x);
        w = x - k;
        // Mirrored: every other copy flipped, so they meet without a seam.
        if (fit == 2u && (i32(k) & 1) == 1) { w = 1.0 - w; }
    }
    let at = in_origin() + size * 0.5 + dir * (w - 0.5) * span_along + perp * (v - 0.5) * span_across;
    return sample_input(at) * cover;
}
