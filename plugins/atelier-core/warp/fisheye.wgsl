fn oa_warp_fisheye(pos: vec2f, base: u32) -> vec4f {
    // Positive bulges out of the center, negative pinches into it.
    let size = max(in_size(), vec2f(1.0));
    let mid = in_origin() + size * vec2f(u(base + 2u), u(base + 3u));
    let rel = (pos - mid) / (min(size.x, size.y) * 0.5);
    let r = length(rel);
    let amount = clamp(u(base), -4.0, 4.0);
    let zoom = max(u(base + 1u), 0.05);
    // r' = r * (1 + k r^2), inverted by sampling where the pixel came from.
    let k = amount * 0.5;
    let scale = 1.0 / (1.0 + k * r * r) / zoom;
    return sample_input(mid + rel * scale * (min(size.x, size.y) * 0.5));
}
