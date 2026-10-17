// Echo runs in the planner, not here: the clip is drawn again at moments before and/or
// after and laid together (`Planner::echoed`). This pass is never added to a frame; it's
// here so the effect has a shader like every other.
fn oa_motion_echo(c: vec4f, base: u32) -> vec4f {
    return c;
}
