// Invert: flips the chosen channels. Params: amount, channels (a bitmask of the options
// red 1, green 2, blue 4, magenta 8, cyan 16, yellow 32, alpha 64, brightness 128,
// hue 256, saturation 512). Magenta, cyan and yellow flip the two channels each is made
// of; a channel chosen twice flips once. Brightness, hue and saturation are flipped in
// HSL, after the color channels: brightness is lightness turned over (the colors kept),
// hue a half turn of the color wheel (the light and dark kept), saturation vivid and
// muted swapped (pure grays, which have no hue to make vivid, stay gray).
fn oa_invert_any(bits: u32, mask: u32) -> f32 {
    return select(0.0, 1.0, (bits & mask) != 0u);
}

fn oa_invert_to_hsl(c: vec3f) -> vec3f {
    let hi = max(c.r, max(c.g, c.b));
    let lo = min(c.r, min(c.g, c.b));
    let l = (hi + lo) * 0.5;
    let d = hi - lo;
    if (d <= 1e-6) {
        return vec3f(0.0, 0.0, l);
    }
    let s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-6);
    var h: f32;
    if (hi == c.r) {
        h = (c.g - c.b) / d + select(0.0, 6.0, c.g < c.b);
    } else if (hi == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3f(h / 6.0, s, l);
}

fn oa_invert_from_hsl(hsl: vec3f) -> vec3f {
    let a = hsl.y * min(hsl.z, 1.0 - hsl.z);
    let k = (vec3f(0.0, 8.0, 4.0) + hsl.x * 12.0) % 12.0;
    return hsl.z - a * max(vec3f(-1.0), min(k - 3.0, min(9.0 - k, vec3f(1.0))));
}

fn oa_color_invert(c: vec4f, base: u32) -> vec4f {
    let amount = clamp(u(base), 0.0, 1.0);
    let bits = u32(u(base + 1u) + 0.5);
    let flip = vec4f(
        oa_invert_any(bits, 1u | 8u | 32u),
        oa_invert_any(bits, 2u | 16u | 32u),
        oa_invert_any(bits, 4u | 8u | 16u),
        oa_invert_any(bits, 64u),
    );
    var out = mix(c, vec4f(1.0) - c, flip);
    if ((bits & (128u | 256u | 512u)) != 0u) {
        var hsl = oa_invert_to_hsl(clamp(out.rgb, vec3f(0.0), vec3f(1.0)));
        if ((bits & 128u) != 0u) {
            hsl.z = 1.0 - hsl.z;
        }
        if ((bits & 256u) != 0u) {
            hsl.x = fract(hsl.x + 0.5);
        }
        if ((bits & 512u) != 0u && hsl.y > 0.0) {
            hsl.y = 1.0 - hsl.y;
        }
        out = vec4f(oa_invert_from_hsl(hsl), out.a);
    }
    return mix(c, out, amount);
}
