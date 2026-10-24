// Distress: noise eating into the picture — worn print, chipped paint, old film. It
// eats most where the picture meets nothing (within `reach` px of its edges, so text
// and cut-outs crumble at their outlines; with no reach, it eats evenly all over),
// in clumps `grain` px across; `scratches` are thin broken streaks at `angle`. What's
// eaten goes see-through, or turns `hole color`. It can flicker (`speed`, changes a
// second) and `seed` picks another pattern.

fn oa_distress_hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

// Smooth value noise.
fn oa_distress_noise(p: vec2f) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    let a = oa_distress_hash(i);
    let b = oa_distress_hash(i + vec2f(1.0, 0.0));
    let c = oa_distress_hash(i + vec2f(0.0, 1.0));
    let d = oa_distress_hash(i + vec2f(1.0, 1.0));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

fn oa_stylize_distress(pos: vec2f, base: u32) -> vec4f {
    let c = sample_input(pos);
    let amount = clamp(u(base), 0.0, 1.0);
    let grain = max(u(base + 1u), 0.5);
    let reach = max(u(base + 2u), 0.0);
    let scratches = clamp(u(base + 3u), 0.0, 1.0);
    let angle = radians(u(base + 4u));
    let speed = max(u(base + 5u), 0.0);
    let seed = u(base + 6u);
    let holes = u32(round(u(base + 7u)));
    let hole = vec4f(u(base + 8u), u(base + 9u), u(base + 10u), u(base + 11u));
    if (c.a <= 0.0 || (amount <= 0.0 && scratches <= 0.0)) { return c; }

    // A new pattern `speed` times a second (still at 0).
    let tick = floor(clip_seconds() * speed);
    let q = pos - in_origin() + vec2f(seed * 131.7 + tick * 71.3, seed * 77.1 + tick * 19.9);

    // How near an edge: of eight points `reach` away, how many show nothing.
    var edge = 1.0;
    if (reach >= 0.5) {
        var open = 0.0;
        for (var i = 0; i < 8; i++) {
            let a = f32(i) * 0.7853982;
            open += 1.0 - sample_input(pos + vec2f(cos(a), sin(a)) * reach).a;
        }
        edge = clamp(open / 8.0 * 2.5, 0.0, 1.0);
    }
    // Clumps, finer bits on them, and a few single specks.
    let p = q / grain;
    let n = 0.55 * oa_distress_noise(p) + 0.3 * oa_distress_noise(p * 3.1 + 17.0) + 0.15 * oa_distress_hash(floor(p * 4.0));
    // Eaten where the noise is under the threshold: a little everywhere in reach of
    // nothing… mostly at the edges.
    let threshold = amount * mix(0.18, 0.95, edge);
    var keep = smoothstep(threshold - 0.03, threshold + 0.03, n);

    // Scratches: thin streaks across the picture at `angle`, each broken along its way.
    if (scratches > 0.0) {
        let along = vec2f(cos(angle), sin(angle));
        let across = dot(q, vec2f(-along.y, along.x));
        let lane = floor(across / 1.5);
        let lit = oa_distress_hash(vec2f(lane, seed + tick)) > 1.0 - scratches * 0.12;
        let broken = oa_distress_noise(vec2f(dot(q, along) / (grain * 25.0), lane * 3.7)) > 0.45;
        if (lit && broken) { keep = 0.0; }
    }
    if (holes == 1u) {
        // The hole color, inside the picture's shape.
        let fill = vec4f(hole.rgb * hole.a, hole.a) * c.a;
        return mix(fill, c, keep);
    }
    return c * keep;
}
