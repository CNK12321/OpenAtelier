fn oa_light_shadow(pos: vec2f, base: u32) -> vec4f {
    // The picture's silhouette, moved `distance` px towards `direction`,
    // softened over `softness` px, in `color`, behind the picture.
    let color = vec4f(u(base), u(base + 1u), u(base + 2u), u(base + 3u));
    let a = radians(u(base + 5u));
    let source = pos - vec2f(cos(a), sin(a)) * u(base + 6u);
    let soft = max(u(base + 7u), 0.0);
    var cover = sample_input(source).a;
    var weight = 1.0;
    // Two rings of eight around it, weighted like a Gaussian.
    for (var ring = 1; ring <= 2; ring++) {
        let r = soft * f32(ring) * 0.5;
        let w = exp(-f32(ring * ring) * 0.5);
        for (var k = 0; k < 8; k++) {
            let t = f32(k) * 0.7853982 + f32(ring) * 0.39;
            cover += sample_input(source + vec2f(cos(t), sin(t)) * r).a * w;
            weight += w;
        }
    }
    let alpha = cover / weight * color.a * clamp(u(base + 4u), 0.0, 1.0);
    let c = sample_input(pos);
    return c + vec4f(color.rgb * alpha, alpha) * (1.0 - c.a);
}
