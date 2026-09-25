fn oa_stylize_halftone(pos: vec2f, base: u32) -> vec4f {
    // Dots on a rotated grid, each as big as the ink under it.
    let size = max(u(base), 2.0);
    let a = radians(u(base + 1u));
    let rel = pos - in_origin();
    let rot = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
    let cell = floor(rot / size) * size + size * 0.5;
    let back = vec2f(cell.x * cos(-a) - cell.y * sin(-a), cell.x * sin(-a) + cell.y * cos(-a));
    let c = sample_input(in_origin() + back);
    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
    let radius = sqrt(clamp(1.0 - luma, 0.0, 1.0)) * size * 0.72;
    let d = length(rot - cell);
    let ink = 1.0 - smoothstep(radius - 1.0, radius + 1.0, d);
    let paper = vec3f(u(base + 2u), u(base + 3u), u(base + 4u));
    let dot_color = vec3f(u(base + 6u), u(base + 7u), u(base + 8u));
    return vec4f(mix(paper, dot_color, ink), c.a);
}
