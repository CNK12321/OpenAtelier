fn oa_text_background(b: TextBox, base: u32) -> vec4f {
    // A rounded box behind the whole text, each line or each word
    // (`shape`), padded by a share of the text size.
    if (abs(b.kind - u(base)) > 0.5) { return vec4f(0.0); }
    let color = vec4f(u(base + 1u), u(base + 2u), u(base + 3u), u(base + 4u));
    let half = b.size * 0.5 + vec2f(u(base + 5u), u(base + 5u) * 0.5) * b.em;
    let r = clamp(u(base + 6u) * b.em, 0.0, min(half.x, half.y));
    let q = abs(b.pos - b.center) - half + vec2f(r);
    let d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
    let cover = clamp(0.5 - d, 0.0, 1.0);
    return vec4f(color.rgb, color.a * cover);
}
