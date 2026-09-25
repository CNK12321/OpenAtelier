fn oa_anim_reveal(pos: vec2f, base: u32) -> vec4f {
    // A soft edge sweeps across the layer along `angle`, uncovering it.
    let a = radians(u(base));
    let dir = vec2f(cos(a), sin(a));
    let soft = max(u(base + 1u), 0.001);
    let lo = in_origin();
    let hi = in_origin() + in_size();
    let start = min(dir.x * lo.x, dir.x * hi.x) + min(dir.y * lo.y, dir.y * hi.y) - soft;
    let reach = abs(dir.x) * in_size().x + abs(dir.y) * in_size().y + 2.0 * soft;
    let edge = start + visibility() * reach;
    let shown = 1.0 - smoothstep(edge - soft, edge + soft, dot(pos, dir));
    return sample_input(pos) * shown;
}
