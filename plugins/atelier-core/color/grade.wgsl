// A primary grade, as a colorist's wheels and sliders set it. Runs on display-encoded
// values (the space grading tools work in); exposure and white balance first, in
// linear light. Parameters (from `base`):
//   0 exposure (stops)   1 temperature   2 tint
//   3–5 lift RGB, 6 lift    7–9 gamma RGB, 10 gamma
//   11–13 gain RGB, 14 gain  15–17 offset RGB, 18 offset
//   19 contrast  20 pivot  21 highlights  22 shadows  23 whites  24 blacks
//   25 saturation  26 vibrance  27 hue (degrees)

fn oa_grade_luma(c: vec3f) -> f32 {
    return dot(c, vec3f(0.2126, 0.7152, 0.0722));
}

fn oa_grade_rgb(base: u32) -> vec3f {
    return vec3f(u(base), u(base + 1u), u(base + 2u)) + vec3f(u(base + 3u));
}

fn oa_color_grade(c: vec4f, base: u32) -> vec4f {
    // Exposure and white balance: in light, brightness kept.
    var lin = max(srgb_decode(c.rgb), vec3f(0.0)) * exp2(u(base));
    let temp = u(base + 1u);
    let tint = u(base + 2u);
    let balance = vec3f(1.0 + 0.3 * temp + 0.1 * tint, 1.0 - 0.3 * tint, 1.0 - 0.3 * temp + 0.1 * tint);
    lin = lin * balance / max(oa_grade_luma(balance), 1e-4);
    var rgb = srgb_encode(lin);

    // The wheels: lift moves the blacks, gain the whites, gamma the middle, offset all.
    let lift = oa_grade_rgb(base + 3u);
    let gamma = oa_grade_rgb(base + 7u);
    let gain = vec3f(1.0) + oa_grade_rgb(base + 11u);
    let offset = oa_grade_rgb(base + 15u);
    rgb = rgb * gain + lift * (vec3f(1.0) - rgb);
    rgb = sign(rgb) * pow(abs(rgb), vec3f(1.0) / max(vec3f(1.0) + gamma, vec3f(0.05)));
    rgb = rgb + offset;

    // Contrast around the pivot.
    let pivot = u(base + 20u);
    rgb = (rgb - vec3f(pivot)) * u(base + 19u) + vec3f(pivot);

    // Tone: highlights and shadows lift or pull their half; whites and blacks the ends.
    let l = oa_grade_luma(rgb);
    let in_shadows = 1.0 - smoothstep(0.0, 0.55, l);
    let in_highlights = smoothstep(0.45, 1.0, l);
    rgb = rgb + vec3f(0.25 * u(base + 21u) * in_highlights + 0.25 * u(base + 22u) * in_shadows);
    rgb = rgb * (1.0 + 0.2 * u(base + 23u)) + vec3f(0.1 * u(base + 24u)) * (vec3f(1.0) - rgb);

    // Saturation; vibrance adds most where the color is least saturated.
    let grey = vec3f(oa_grade_luma(rgb));
    rgb = mix(grey, rgb, u(base + 25u));
    let chroma = max(rgb.r, max(rgb.g, rgb.b)) - min(rgb.r, min(rgb.g, rgb.b));
    rgb = mix(grey, rgb, 1.0 + u(base + 26u) * (1.0 - clamp(chroma, 0.0, 1.0)));

    // Hue: every color turned around the grey axis.
    let a = radians(u(base + 27u));
    let k = vec3f(0.57735027);
    rgb = rgb * cos(a) + cross(k, rgb) * sin(a) + k * dot(k, rgb) * (1.0 - cos(a));
    return vec4f(rgb, c.a);
}
