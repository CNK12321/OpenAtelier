fn oa_stylize_grain(c: vec4f, base: u32) -> vec4f {
    // Value noise on a grid of `size` px, shifted every second so it
    // crawls the way film grain does.
    let cell = max(u(base + 1u), 1.0);
    let p = floor((layer_pos() - in_origin()) / cell) + floor(clip_seconds() * max(u(base + 2u), 0.0)) * 37.0;
    let n = fract(sin(dot(p, vec2f(12.9898, 78.233))) * 43758.5453);
    let amount = clamp(u(base), 0.0, 2.0);
    // Grain shows most in the midtones, as it does on film.
    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    let shape = 4.0 * luma * (1.0 - clamp(luma, 0.0, 1.0));
    return vec4f(c.rgb + (n - 0.5) * amount * mix(1.0, shape, 0.75), c.a);
}
