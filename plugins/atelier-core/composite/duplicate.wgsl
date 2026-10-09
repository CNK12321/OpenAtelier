// Duplicate runs in the planner, not here: the clip is drawn a second time with the
// duplicate's own transform and effects (`Planner::layer` in oa-plan). This pass is never
// added to a frame; it's here so the effect has a shader like every other.
fn oa_composite_duplicate(c: vec4f, base: u32) -> vec4f {
    return c;
}
