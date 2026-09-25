// How thick the sheet is at a point, as a fraction of the full thickness:
// the depth map's brightness, or the same everywhere without one.
fn oa_depth_thickness(xy: vec2f, use_map: bool) -> f32 {
    if (!use_map) { return 1.0; }
    let m = sample_media(xy);
    let a = max(m.a, 1e-4);
    return dot(m.rgb / a, vec3f(0.2126, 0.7152, 0.0722)) * step(0.01, m.a);
}

// Turns the sheet: yaw about the upright axis, then pitch. The ray is
// rotated the other way instead, which comes to the same thing.
fn oa_depth_unrotate(v: vec3f, yaw: f32, pitch: f32) -> vec3f {
    let cy = cos(-yaw); let sy = sin(-yaw);
    let cp = cos(-pitch); let sp = sin(-pitch);
    let a = vec3f(cy * v.x + sy * v.z, v.y, -sy * v.x + cy * v.z);
    return vec3f(a.x, cp * a.y - sp * a.z, sp * a.y + cp * a.z);
}

fn oa_depth_slab(pos: vec2f, base: u32) -> vec4f {
    let size = max(in_size(), vec2f(1.0));
    let center = in_origin() + 0.5 * size;
    let half = 0.5 * size;
    // The sheet's thickness, as a share of its width.
    let thick = max(u(base) * 0.01 * size.x, 0.001);
    let yaw = radians(u(base + 1u));
    let pitch = radians(u(base + 2u));
    let pov = clamp(u(base + 3u), 0.0, 1.0);
    let use_map = u(base + 4u) > 0.5 && has_media();

    // A pinhole camera looking at the sheet: the closer it is, the more
    // perspective. At POV 0 it's far enough away to be flat-on.
    let dist = size.x * (1.2 + 40.0 * (1.0 - pov));
    let film = pos - center;
    let eye = oa_depth_unrotate(vec3f(0.0, 0.0, -dist), yaw, pitch);
    let dir = normalize(oa_depth_unrotate(vec3f(film, dist), yaw, pitch));

    // Where the ray is inside the block the sheet is cut from.
    let bound = vec3f(half, 0.5 * thick);
    let inv = 1.0 / select(dir, vec3f(1e-6), abs(dir) < vec3f(1e-6));
    let t1 = (-bound - eye) * inv;
    let t2 = (bound - eye) * inv;
    let lo = max(max(min(t1.x, t2.x), min(t1.y, t2.y)), min(t1.z, t2.z));
    let hi = min(min(max(t1.x, t2.x), max(t1.y, t2.y)), max(t1.z, t2.z));
    if (hi < max(lo, 0.0)) { return vec4f(0.0); }

    // Walk through it until the drawing is under the ray and the ray is
    // inside the paper's thickness there.
    let steps = 64;
    let dt = (hi - lo) / f32(steps);
    var hit = -1.0;
    var hit_xy = vec2f(0.0);
    var cap = false;
    for (var i = 0; i < steps; i++) {
        let t = lo + dt * (f32(i) + 0.5);
        let p = eye + dir * t;
        let xy = center + p.xy;
        let c = sample_input(xy);
        if (c.a < 0.4) { continue; }
        let h = 0.5 * thick * oa_depth_thickness(xy, use_map);
        if (abs(p.z) <= h) {
            hit = t;
            hit_xy = xy;
            // A face, rather than a cut edge, when the ray came in
            // through the flat side.
            cap = abs(abs(p.z) - h) < dt * abs(dir.z) + 0.5;
            break;
        }
    }
    if (hit < 0.0) { return vec4f(0.0); }

    var c = sample_input(hit_xy);
    // The lit side is the one facing the camera; the cut edges are
    // darker. Turned all the way round, the back shows the picture
    // mirrored, at the `back` brightness.
    // (The ray runs towards +z through the front, towards -z through the back.)
    var shade = select(0.68, 1.0, cap);
    if (cap && dir.z < 0.0) { shade = clamp(u(base + 5u), 0.0, 1.0); }
    return vec4f(c.rgb * shade, c.a);
}
