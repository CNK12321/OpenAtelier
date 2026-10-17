// Bloom: the picture's bright parts, blurred and added back over it — light spilling
// out of the highlights. Four passes (bloom.passes, bloom.divisor): the bright parts
// picked out on a grid coarser for wider blooms, blurred across, then down, and at full
// size added over the picture as it came in (the second input).

// Pixels per cell of the coarse passes (`bloom.divisor` works it out the same way).
fn oa_bloom_cell(radius: f32) -> f32 {
    var k = 1.0;
    while (k * 32.0 <= radius && k < 8.0) { k *= 2.0; }
    return k;
}

// The previous (coarse) pass at a layer position, read smoothly; nothing outside it.
fn oa_bloom_read(p: vec2f, k: f32) -> vec4f {
    let uv = (p - in_origin()) / (in_size() * k);
    let inside = all(uv >= vec2f(0.0)) && all(uv <= vec2f(1.0));
    return select(vec4f(0.0), textureSampleLevel(input_tex, input_samp, uv, 0.0), inside);
}

// How much of a (premultiplied) color is bright enough to bloom: what's over the
// threshold, eased in over a knee as wide as `softness` of it, so it doesn't cut in.
fn oa_bloom_bright(c: vec4f, threshold: f32, softness: f32) -> vec4f {
    if (c.a <= 1e-4) { return vec4f(0.0); }
    let s = c.rgb / c.a;
    let b = max(s.r, max(s.g, s.b));
    let knee = max(threshold * softness, 1e-4);
    var soft = clamp(b - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee);
    let w = max(soft, b - threshold) / max(b, 1e-4);
    return vec4f(c.rgb * w, c.a * clamp(w, 0.0, 1.0));
}

fn oa_light_bloom(pos: vec2f, base: u32) -> vec4f {
    let threshold = max(u(base), 0.0);
    let softness = clamp(u(base + 1u), 0.0, 1.0);
    let radius = max(u(base + 2u), 0.0);
    let intensity = max(u(base + 3u), 0.0);
    let k = oa_bloom_cell(radius);
    let at_pass = pass_index();
    // The coarse passes' cell for this pixel, and its middle in layer px.
    let cell = floor(pos - out_origin());
    let center = out_origin() + (cell + 0.5) * k;
    if (at_pass == 0u) {
        // The bright parts, averaged over the cell (up to 4×4 looks, so small
        // highlights still count on a coarse grid).
        let n = min(k, 4.0);
        let lo = out_origin() + cell * k;
        var sum = vec4f(0.0);
        for (var j = 0; j < i32(n); j++) {
            for (var i = 0; i < i32(n); i++) {
                let p = lo + (vec2f(f32(i), f32(j)) + 0.5) / n * k;
                sum += oa_bloom_bright(sample_input(p), threshold, softness);
            }
        }
        return sum / (n * n);
    }
    if (at_pass <= 2u) {
        // A Gaussian blur on the coarse grid: across, then down.
        let r = radius / k;
        if (r < 0.5) { return oa_bloom_read(center, k); }
        let dir = select(vec2f(0.0, 1.0), vec2f(1.0, 0.0), at_pass == 1u);
        let sigma = r / 2.5;
        let step = max(1.0, r / 24.0);
        let n = i32(ceil(r / step));
        var sum = vec4f(0.0);
        var wsum = 0.0;
        for (var i = -n; i <= n; i++) {
            let x = f32(i) * step;
            let w = exp(-0.5 * x * x / (sigma * sigma));
            sum += oa_bloom_read(center + dir * x * k, k) * w;
            wsum += w;
        }
        return sum / wsum;
    }
    // Full size: the blurred light, tinted, added over the picture. A wide blur alone
    // spreads a small highlight's light so thin it all but vanishes; a tight core read
    // from the picture itself (three rings round this point) keeps it shining where it
    // is, with the wide spread around it.
    let c = select(vec4f(0.0), sample_media(pos), has_media());
    let rc = clamp(radius * 0.15, 1.0, 16.0);
    var core = oa_bloom_bright(c, threshold, softness);
    var cw = 1.0;
    for (var ring = 1; ring <= 3; ring++) {
        let d = rc * f32(ring) / 3.0;
        let w = exp(-2.0 * f32(ring * ring) / 9.0);
        for (var i = 0; i < 8; i++) {
            let angle = (f32(i) + 0.5 * f32(ring % 2)) * 0.7853982;
            core += oa_bloom_bright(sample_media(pos + vec2f(cos(angle), sin(angle)) * d), threshold, softness) * w;
            cw += w;
        }
    }
    let g = (oa_bloom_read(pos, k) + core / cw) * 0.5 * intensity;
    let tint = vec4f(u(base + 4u), u(base + 5u), u(base + 6u), u(base + 7u));
    let light = g.rgb * tint.rgb * tint.a;
    return vec4f(c.rgb + light, max(c.a, clamp(g.a * tint.a, 0.0, 1.0)));
}
