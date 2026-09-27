// Curves: master, then red, green and blue, each eight points spread evenly across the
// input (0, 1/7 … 1) joined by a smooth curve that never overshoots them (monotone
// cubic); then saturation by hue, eight points around the color wheel from red (a
// smooth Catmull–Rom loop). Display-encoded values. Parameters:
//   0–7 master   8–15 red   16–23 green   24–31 blue   32–39 saturation by hue

// The curve's slope at point k (per step between points): monotone (Fritsch–Carlson), so
// it never swings past its neighbors — raising one point never darkens the next stretch.
fn oa_curves_slope(base: u32, k: u32) -> f32 {
    if (k == 0u) { return u(base + 1u) - u(base); }
    if (k == 7u) { return u(base + 7u) - u(base + 6u); }
    let a = u(base + k) - u(base + k - 1u);
    let b = u(base + k + 1u) - u(base + k);
    if (a * b <= 0.0) { return 0.0; }
    return 2.0 / (1.0 / a + 1.0 / b);
}

fn oa_curves_at(base: u32, x: f32) -> f32 {
    // Past the ends: carried on in a straight line.
    if (x <= 0.0) { return u(base) + x * 7.0 * (u(base + 1u) - u(base)); }
    if (x >= 1.0) { return u(base + 7u) + (x - 1.0) * 7.0 * (u(base + 7u) - u(base + 6u)); }
    let t = x * 7.0;
    let k = min(u32(floor(t)), 6u);
    let f = t - f32(k);
    let p0 = u(base + k);
    let p1 = u(base + k + 1u);
    let m0 = oa_curves_slope(base, k);
    let m1 = oa_curves_slope(base, k + 1u);
    let f2 = f * f;
    let f3 = f2 * f;
    return (2.0 * f3 - 3.0 * f2 + 1.0) * p0 + (f3 - 2.0 * f2 + f) * m0 + (-2.0 * f3 + 3.0 * f2) * p1 + (f3 - f2) * m1;
}

// The same, around a circle (hue): point 8 is point 0 again.
fn oa_curves_around(base: u32, h: f32) -> f32 {
    let t = fract(h) * 8.0;
    let i = i32(floor(t));
    let f = t - f32(i);
    let p0 = u(base + u32((i + 7) % 8));
    let p1 = u(base + u32(i % 8));
    let p2 = u(base + u32((i + 1) % 8));
    let p3 = u(base + u32((i + 2) % 8));
    return 0.5 * (2.0 * p1 + (p2 - p0) * f + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * f * f + (3.0 * p1 - p0 - 3.0 * p2 + p3) * f * f * f);
}

fn oa_curves_hue(c: vec3f) -> f32 {
    let hi = max(c.r, max(c.g, c.b));
    let d = hi - min(c.r, min(c.g, c.b));
    if (d <= 1e-5) { return 0.0; }
    var h: f32;
    if (hi == c.r) { h = (c.g - c.b) / d; } else if (hi == c.g) { h = 2.0 + (c.b - c.r) / d; } else { h = 4.0 + (c.r - c.g) / d; }
    return fract(h / 6.0);
}

fn oa_color_curves(c: vec4f, base: u32) -> vec4f {
    var rgb = vec3f(oa_curves_at(base, c.r), oa_curves_at(base, c.g), oa_curves_at(base, c.b));
    rgb = vec3f(oa_curves_at(base + 8u, rgb.r), oa_curves_at(base + 16u, rgb.g), oa_curves_at(base + 24u, rgb.b));
    let grey = vec3f(dot(rgb, vec3f(0.2126, 0.7152, 0.0722)));
    rgb = mix(grey, rgb, max(oa_curves_around(base + 32u, oa_curves_hue(rgb)), 0.0));
    return vec4f(rgb, c.a);
}
