// Motion Blur runs in the planner, not here: the clip is drawn at moments across the
// shutter and averaged (`Planner::motion_blurred`). This pass is never added to a frame;
// it's here so the effect has a shader like every other.
fn oa_motion_blur(c: vec4f, base: u32) -> vec4f {
    return c;
}
