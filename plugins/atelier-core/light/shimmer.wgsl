fn oa_light_shimmer(c: vec4f, base: u32) -> vec4f {
    // A band of light sweeps across the layer along `direction`, again and
    // again. Where the layer is transparent there's nothing to light.
    let r = radians(u(base + 2u));
    let dir = vec2f(cos(r), sin(r));
    let rel = (layer_pos() - in_origin()) / max(in_size(), vec2f(1.0)) - 0.5;
    let x = dot(rel, dir) / max(abs(dir.x) + abs(dir.y), 1e-4) + 0.5;
    let t = fract(clip_seconds() * u(base)) * 1.8 - 0.4;
    let band = 1.0 - smoothstep(0.0, max(u(base + 1u), 0.001), abs(x - t));
    let light = vec3f(u(base + 3u), u(base + 4u), u(base + 5u)) * u(base + 6u);
    return vec4f(c.rgb + light * band * u(base + 7u), c.a);
}
