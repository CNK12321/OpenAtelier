// Liquify: the picture flowed through swirling, marbled ribbons — "domain warping":
// smooth noise read at places moved by more noise, twice over, so the swirls fold into
// each other the way stirred paint does. Each pixel reads from where the flow carried
// it from.
// Params: amount (how far it flows, layer px), size (of the big swirls, layer px),
// detail (octaves of smaller swirls), swirl (how much the noise bends itself), flow
// (how fast it moves), seed, edges (clamp / transparent).

// A random unit direction for a grid corner.
fn oa_liquify_dir(p: vec2f) -> vec2f {
    let q = fract(p * vec2f(0.1031, 0.1030));
    let r = q + dot(q, q.yx + 33.33);
    let a = fract((r.x + r.y) * r.x) * 6.2831853;
    return vec2f(cos(a), sin(a));
}

// Smooth gradient noise in 0..1 (no grid-aligned blocks, unlike value noise).
fn oa_liquify_noise(p: vec2f) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = dot(oa_liquify_dir(i), f);
    let b = dot(oa_liquify_dir(i + vec2f(1.0, 0.0)), f - vec2f(1.0, 0.0));
    let c = dot(oa_liquify_dir(i + vec2f(0.0, 1.0)), f - vec2f(0.0, 1.0));
    let d = dot(oa_liquify_dir(i + vec2f(1.0, 1.0)), f - vec2f(1.0, 1.0));
    return 0.5 + 0.7 * mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

// Octaves of it, each smaller and turned, adding up to 0..1.
fn oa_liquify_fbm(p0: vec2f, octaves: i32) -> f32 {
    var p = p0;
    var sum = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    for (var o = 0; o < 6; o++) {
        if (o >= octaves) { break; }
        sum += amp * oa_liquify_noise(p);
        norm += amp;
        p = vec2f(1.6 * p.x - 1.2 * p.y, 1.2 * p.x + 1.6 * p.y) + 17.3;
        amp *= 0.5;
    }
    return sum / max(norm, 1e-4);
}

fn oa_warp_liquify(pos: vec2f, base: u32) -> vec2f {
    let amount = u(base);
    let size = max(u(base + 1u), 1.0);
    let octaves = i32(clamp(round(u(base + 2u)), 1.0, 6.0));
    let swirl = max(u(base + 3u), 0.0) * 4.0;
    let t = clip_seconds() * u(base + 4u);
    let seed = vec2f(u(base + 5u) * 7.13, u(base + 5u) * 3.71);
    let p = (pos - in_origin()) / size + seed;
    // The noise bent by itself, twice.
    let q = vec2f(oa_liquify_fbm(p + vec2f(0.0, 0.0) + t * 0.10, octaves), oa_liquify_fbm(p + vec2f(5.2, 1.3) - t * 0.08, octaves));
    let r = vec2f(oa_liquify_fbm(p + swirl * q + vec2f(1.7, 9.2) + t * 0.15, octaves), oa_liquify_fbm(p + swirl * q + vec2f(8.3, 2.8) - t * 0.126, octaves));
    let flow = vec2f(oa_liquify_fbm(p + swirl * r, octaves), oa_liquify_fbm(p + swirl * r + vec2f(3.1, 7.7), octaves));
    let src = pos + (flow - 0.5) * 2.0 * amount;
    return select(clamp_to_input(src), src, u(base + 6u) > 0.5);
}
