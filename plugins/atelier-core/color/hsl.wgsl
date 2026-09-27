// The HSL mixer: eight hue bands (red, orange, yellow, green, aqua, blue, purple,
// magenta), each with its own hue shift (degrees), saturation and luminance. A color
// between two bands gets a blend of their settings; greys are left alone.
// Display-encoded values. Parameters: band i's hue, saturation, luminance at 3i … 3i+2.

fn oa_hsl_center(i: u32) -> f32 {
    // Degrees around the wheel.
    let centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);
    return centers[i];
}

fn oa_color_hsl(c: vec4f, base: u32) -> vec4f {
    var rgb = c.rgb;
    let hi = max(rgb.r, max(rgb.g, rgb.b));
    let lo = min(rgb.r, min(rgb.g, rgb.b));
    let chroma = hi - lo;
    if (chroma <= 1e-5) { return c; }
    var h: f32;
    if (hi == rgb.r) { h = (rgb.g - rgb.b) / chroma; } else if (hi == rgb.g) { h = 2.0 + (rgb.b - rgb.r) / chroma; } else { h = 4.0 + (rgb.r - rgb.g) / chroma; }
    let deg = fract(h / 6.0) * 360.0;
    // The two bands around this hue, and how far between them it is.
    var i = 7u;
    for (var k = 0u; k < 8u; k++) {
        if (oa_hsl_center(k) <= deg) { i = k; }
    }
    let j = (i + 1u) % 8u;
    let a = oa_hsl_center(i);
    var b = oa_hsl_center(j);
    if (j == 0u) { b = 360.0; }
    let t = clamp((deg - a) / max(b - a, 1e-3), 0.0, 1.0);
    let w = t * t * (3.0 - 2.0 * t);
    let shift = mix(u(base + 3u * i), u(base + 3u * j), w);
    let sat = mix(u(base + 3u * i + 1u), u(base + 3u * j + 1u), w);
    let lum = mix(u(base + 3u * i + 2u), u(base + 3u * j + 2u), w);
    // How much it's a color at all: greys and near-greys are hardly touched.
    let amount = clamp(chroma * 4.0, 0.0, 1.0);

    // Hue: turned around the grey axis.
    let turn = radians(shift * amount);
    let k = vec3f(0.57735027);
    rgb = rgb * cos(turn) + cross(k, rgb) * sin(turn) + k * dot(k, rgb) * (1.0 - cos(turn));
    // Saturation and luminance.
    let grey = vec3f(dot(rgb, vec3f(0.2126, 0.7152, 0.0722)));
    rgb = mix(grey, rgb, 1.0 + sat * amount);
    rgb = rgb * (1.0 + 0.5 * lum * amount);
    return vec4f(rgb, c.a);
}
