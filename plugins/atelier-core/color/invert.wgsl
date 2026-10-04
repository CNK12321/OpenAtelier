// Invert: flips the chosen channels. Params: amount, channels (a bitmask of the options
// red 1, green 2, blue 4, magenta 8, cyan 16, yellow 32, alpha 64). Magenta, cyan and
// yellow flip the two channels each is made of; a channel chosen twice flips once.
fn oa_invert_any(bits: u32, mask: u32) -> f32 {
    return select(0.0, 1.0, (bits & mask) != 0u);
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
    return mix(c, vec4f(1.0) - c, flip * amount);
}
