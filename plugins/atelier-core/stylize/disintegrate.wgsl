// Disintegrate: the picture breaking up and blowing away. It's cut into blocks
// (`block` px), and each has its own moment, somewhere along `amount`, when it goes:
// from then it slides towards `direction` (up to `spread` px), its colors fringe apart
// (`fringe` px) and it crumbles into specks (`grain` px) until nothing of it is left.
// At 0 the picture is whole; at 1 it's gone. `speed` makes the blocks jitter that many
// times a second; `seed` picks another break-up.

fn oa_disintegrate_hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

fn oa_stylize_disintegrate(pos: vec2f, base: u32) -> vec4f {
    let amount = clamp(u(base), 0.0, 1.0);
    if (amount <= 0.0) { return sample_input(pos); }
    let block = max(u(base + 1u), 2.0);
    let spread = max(u(base + 2u), 0.0);
    let way = radians(u(base + 3u));
    let grain = max(u(base + 4u), 0.5);
    let fringe = max(u(base + 5u), 0.0);
    let speed = max(u(base + 6u), 0.0);
    let seed = u(base + 7u);
    let tick = floor(clip_seconds() * speed);
    let along = vec2f(cos(way), sin(way));
    let across = vec2f(-along.y, along.x);

    let q = pos - in_origin();
    let cell = floor(q / block);
    // When this block starts to go (0 → 0.8 of the way), and how far it's got: all
    // of them gone by the end.
    let moment = oa_disintegrate_hash(cell + vec2f(seed * 17.3, seed * 5.1)) * 0.8;
    let gone = clamp((amount - moment) / 0.2, 0.0, 1.0);
    // Its slide: along the way, some further than others, and a little sideways;
    // jittering `speed` times a second.
    let jitter = oa_disintegrate_hash(cell + vec2f(tick * 3.1 + seed, 11.7));
    let wobble = oa_disintegrate_hash(cell + vec2f(5.3, tick * 1.7 + seed)) - 0.5;
    let offset = along * spread * gone * (0.4 + 0.6 * jitter) + across * block * 0.35 * gone * wobble;
    let at = pos - offset;
    var c = sample_input(at);
    // Its colors pulling apart as it goes.
    let split = along * fringe * gone;
    if (gone > 0.0 && fringe > 0.0) {
        let r = sample_input(at + split);
        let b = sample_input(at - split);
        c = vec4f(r.r, c.g, b.b, max(c.a, max(r.a, b.a)));
    }
    // Crumbling: specks of it fall away as it goes, all of it by the end — and a fine
    // dust everywhere once the break-up has begun.
    let speck = oa_disintegrate_hash(floor(q / grain) + vec2f(tick * 7.7 + seed * 3.3, seed));
    let keep = step(gone, speck) * step(amount * 0.12, speck);
    return c * keep;
}
