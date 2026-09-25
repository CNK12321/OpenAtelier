fn oa_warp_wobble(pos: vec2f, base: u32) -> vec4f {
    // The picture shears left and right, rocking about its middle row.
    let mid = in_origin().y + in_size().y * 0.5;
    let shear = u(base) * sin(clip_seconds() * u(base + 1u) * 6.2831853);
    let src = vec2f(pos.x - shear * (pos.y - mid), pos.y);
    return select(sample_input(src), sample_input_clamped(src), u(base + 2u) > 0.5);
}
