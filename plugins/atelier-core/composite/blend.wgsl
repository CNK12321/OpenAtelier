// Blend runs in the compositor, not here: the planner gives the clip's layer the chosen
// mode (`blend_of` in oa-plan). This pass is never added to a frame; it's here so the
// effect has a shader like every other.
fn oa_composite_blend(c: vec4f, base: u32) -> vec4f {
    return c;
}
