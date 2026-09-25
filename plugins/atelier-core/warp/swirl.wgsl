fn oa_warp_swirl(pos: vec2f, base: u32) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    let mid = in_origin() + size * vec2f(u(base + 2u), u(base + 3u));
    let rel = pos - mid;
    let radius = max(u(base + 1u), 0.001) * min(size.x, size.y) * 0.5;
    let fade = clamp(1.0 - length(rel) / radius, 0.0, 1.0);
    let a = radians(u(base)) * fade * fade;
    let turned = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
    return sample_input(mid + turned);
}
