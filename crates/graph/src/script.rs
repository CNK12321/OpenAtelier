//! The parts of a picture effect written in OA script (`oa_script`) rather than WGSL:
//! how a motion effect moves its layer, where an effect may draw, how many passes its
//! shader runs. Declared in a plugin's manifest (`motion`, `bounds`, `pass_count`,
//! `pass_divisor`), so a plugin can do what Atelier Core's Fly In, Surface and Glow do.

use crate::registry::{Motion, MotionInput};
use crate::Rect;
use oa_params::{Evaluated, ParamSchema};
use oa_script::{Arg, Env, Frame, Function, Host, Intrinsic, NoHost, Section};
use std::sync::Arc;

/// A compiled script and the parameters it reads.
struct Script {
    section: Section,
    frame: Frame,
    params: Vec<ParamSchema>,
    source: Arc<str>,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Script").field("source", &self.source).finish()
    }
}

impl Script {
    fn compile(source: &str, env: Env<'_>) -> Result<Self, String> {
        let (section, frame) = oa_script::compile(source, env)?;
        Ok(Script { section, frame, params: env.params.to_vec(), source: source.into() })
    }

    fn run(&self, regs: &mut [f32], host: &mut dyn Host) {
        let mut stack = Vec::new();
        oa_script::run(&self.section.block, regs, &mut stack, host, true);
        oa_script::run(&self.section.main, regs, &mut stack, host, true);
    }
}

// ---- motion ----

const MOTION_READS: [&str; 6] = ["visibility", "progress", "seconds", "canvas_w", "canvas_h", "leaving"];
const MOTION_WRITES: [&str; 5] = ["move_x", "move_y", "zoom", "turn", "opacity"];
const NOISE: u16 = 0;
const JITTER: u16 = 1;
const MOTION_FUNCTIONS: &[Function] = &[
    Function { name: "noise", args: &[Arg::Value, Arg::Value], id: NOISE, pure: true, statement: false, intrinsic: Intrinsic::Call },
    Function { name: "jitter", args: &[Arg::Value, Arg::Value], id: JITTER, pure: true, statement: false, intrinsic: Intrinsic::Call },
];

/// Smooth value noise in [-1, 1].
fn value_noise(seed: u64, x: f64) -> f64 {
    let hash = |i: i64| {
        let mut z = seed.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    };
    let i = x.floor();
    let f = x - i;
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let (a, b) = (hash(i as i64), hash(i as i64 + 1));
    a + (b - a) * s
}

/// The effect instance's noise: `noise(x, stream)` is smooth value noise, `jitter(x,
/// stream)` two octaves of it (a jitter with some body). Each stream is its own curve,
/// and each copy of the effect has its own set.
struct MotionHost {
    seed: u64,
}

impl Host for MotionHost {
    fn call(&mut self, id: u16, _site: u32, a: &[f32]) -> f32 {
        let (x, stream) = (a[0] as f64, a[1].max(0.0) as u64);
        let seed = self.seed.wrapping_add(stream);
        (match id {
            NOISE => value_noise(seed, x),
            JITTER => (value_noise(seed, x) + 0.5 * value_noise(seed ^ 0x5bd1_e995, x * 2.0)) / 1.5,
            _ => 0.0,
        }) as f32
    }
}

/// How a motion effect moves its layer: reads its clock (`visibility`, `progress`,
/// `seconds`), `canvas_w`/`canvas_h` (px) and `leaving` (1 for an outro); writes
/// `move_x`/`move_y` (canvas px), `zoom` (× size), `turn` (degrees, clockwise) and
/// `opacity` (×). `noise(x, stream)` and `jitter(x, stream)` give it smooth randomness.
#[derive(Debug)]
pub struct MotionScript(Script);

impl MotionScript {
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Self, String> {
        let env = Env { reads: &MOTION_READS, writes: &MOTION_WRITES, params, functions: MOTION_FUNCTIONS, ..Env::default() };
        Script::compile(source, env).map(MotionScript)
    }

    pub fn eval(&self, values: &Evaluated, i: &MotionInput) -> Motion {
        let s = &self.0;
        let mut regs = vec![0.0; s.frame.registers];
        let reads = [i.visibility, i.progress, i.seconds, i.canvas[0], i.canvas[1], i.leaving as u8 as f64];
        for (r, v) in regs.iter_mut().zip(reads) {
            *r = v as f32;
        }
        // move_x, move_y, zoom, turn, opacity
        regs[6..11].copy_from_slice(&[0.0, 0.0, 1.0, 0.0, 1.0]);
        s.frame.load_params(&mut regs, &s.params, values);
        s.run(&mut regs, &mut MotionHost { seed: i.seed });
        let get = |r: usize, fallback: f64| if regs[r].is_finite() { regs[r] as f64 } else { fallback };
        Motion { offset: [get(6, 0.0), get(7, 0.0)], scale: get(8, 1.0).max(1e-4), rotation: get(9, 0.0), opacity: get(10, 1.0).clamp(0.0, 1.0) }
    }
}

// ---- bounds ----

const BOUNDS_READS: [&str; 5] = ["x0", "y0", "x1", "y1", "raster_scale"];
const BOUNDS_WRITES: [&str; 4] = ["left", "top", "right", "bottom"];
const GROW: u16 = 0;
const BOUNDS_FUNCTIONS: &[Function] = &[Function { name: "grow", args: &[Arg::Value, Arg::Value], id: GROW, pure: false, statement: true, intrinsic: Intrinsic::Call }];

/// Points `grow(x, y)` was called with.
struct BoundsHost {
    rect: Rect,
}

impl Host for BoundsHost {
    fn call(&mut self, _id: u16, _site: u32, a: &[f32]) -> f32 {
        let (x, y) = (a[0] as f64, a[1] as f64);
        if x.is_finite() && y.is_finite() {
            let r = self.rect;
            self.rect = Rect::new(r.x0.min(x), r.y0.min(y), r.x1.max(x), r.y1.max(y));
        }
        0.0
    }
}

/// Where an effect may draw, given its input's box: reads `x0`, `y0`, `x1`, `y1` (raster
/// px) and `raster_scale`; writes `left`, `top`, `right`, `bottom` (starting as the
/// input's box), and `grow(x, y);` takes in a point.
#[derive(Debug)]
pub struct BoundsScript(Script);

impl BoundsScript {
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Self, String> {
        let env = Env { reads: &BOUNDS_READS, writes: &BOUNDS_WRITES, params, functions: BOUNDS_FUNCTIONS, ..Env::default() };
        Script::compile(source, env).map(BoundsScript)
    }

    pub fn eval(&self, values: &Evaluated, raster_scale: f64, input: Rect) -> Rect {
        let s = &self.0;
        let mut regs = vec![0.0; s.frame.registers];
        let reads = [input.x0, input.y0, input.x1, input.y1, raster_scale, input.x0, input.y0, input.x1, input.y1];
        for (r, v) in regs.iter_mut().zip(reads) {
            *r = v as f32;
        }
        s.frame.load_params(&mut regs, &s.params, values);
        let mut host = BoundsHost { rect: input };
        s.run(&mut regs, &mut host);
        let written = |r: usize, fallback: f64| if regs[r].is_finite() { regs[r] as f64 } else { fallback };
        let r = host.rect;
        Rect::new(r.x0.min(written(5, input.x0)), r.y0.min(written(6, input.y0)), r.x1.max(written(7, input.x1)), r.y1.max(written(8, input.y1)))
    }
}

// ---- passes ----

/// How many passes an effect's shader runs, or how coarse one pass is, from its packed
/// uniforms: reads the parameters (as the shader gets them: layer pixels already scaled),
/// and for a divisor `pass` (from 0) and `passes`; writes `out`.
#[derive(Debug)]
pub struct PassScript {
    script: Script,
    /// Where each parameter starts in the packed uniforms.
    offsets: Vec<usize>,
}

impl PassScript {
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Self, String> {
        let env = Env { reads: &["pass", "passes"], writes: &["out"], params, ..Env::default() };
        let script = Script::compile(source, env)?;
        let mut offsets = Vec::with_capacity(params.len());
        let mut at = 0;
        for p in params {
            offsets.push(at);
            at += crate::registry::packed_len(p.ty);
        }
        Ok(PassScript { script, offsets })
    }

    fn run(&self, uniforms: &[f32], pass: u32, passes: u32) -> f32 {
        let s = &self.script;
        let mut regs = vec![0.0; s.frame.registers];
        regs[0] = pass as f32;
        regs[1] = passes as f32;
        for slot in &s.frame.params {
            regs[slot.register as usize] = uniforms.get(self.offsets[slot.param] + slot.component as usize).copied().unwrap_or(0.0);
        }
        s.run(&mut regs, &mut NoHost);
        regs[2]
    }

    /// The pass count (1 to 32).
    pub fn count(&self, uniforms: &[f32]) -> u32 {
        let n = self.run(uniforms, 0, 0);
        if n.is_finite() { (n.round() as i64).clamp(1, 32) as u32 } else { 1 }
    }

    /// By how much pass `pass` of `passes` divides its target's size (1 to 64).
    pub fn divisor(&self, uniforms: &[f32], pass: u32, passes: u32) -> u32 {
        let k = self.run(uniforms, pass, passes);
        if k.is_finite() { (k.round() as i64).clamp(1, 64) as u32 } else { 1 }
    }
}

// ---- overlays ----

const OVERLAY_READS: [&str; 10] = ["canvas_w", "canvas_h", "seconds", "duration", "playing", "selected", "sel_x0", "sel_y0", "sel_x1", "sel_y1"];
const COLOR: u16 = 0;
const LINE: u16 = 1;
const RECT: u16 = 2;
const FILL: u16 = 3;
const CIRCLE: u16 = 4;
const GRID: u16 = 5;
const DOT: u16 = 6;
const V: Arg = Arg::Value;
const OVERLAY_FUNCTIONS: &[Function] = &[
    Function { name: "color", args: &[V, V, V, V], id: COLOR, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "line", args: &[V, V, V, V, V], id: LINE, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "rect", args: &[V, V, V, V, V], id: RECT, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "fill", args: &[V, V, V, V], id: FILL, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "circle", args: &[V, V, V, V], id: CIRCLE, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "grid", args: &[V, V, V, V, V, V, V], id: GRID, pure: false, statement: true, intrinsic: Intrinsic::Call },
    Function { name: "dot", args: &[V, V, V], id: DOT, pure: false, statement: true, intrinsic: Intrinsic::Call },
];

/// Most shapes one overlay may draw in a frame (a `grid` counts its lines).
pub const OVERLAY_MAX_SHAPES: usize = 512;

/// A shape an overlay draws over the viewer, in canvas px, straight RGBA color.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayShape {
    Line { from: [f32; 2], to: [f32; 2], width: f32, color: [f32; 4] },
    /// Outlined (`width` > 0) or filled (`width` = 0).
    Rect { min: [f32; 2], max: [f32; 2], width: f32, color: [f32; 4] },
    /// Outlined (`width` > 0) or filled (`width` = 0).
    Circle { center: [f32; 2], radius: f32, width: f32, color: [f32; 4] },
}

/// What an overlay sees of the editor this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayInput {
    pub canvas: [f64; 2],
    pub seconds: f64,
    pub duration: f64,
    pub playing: bool,
    /// The selected clip's box on the canvas (x0, y0, x1, y1), if one is showing.
    pub selected: Option<[f64; 4]>,
}

struct OverlayHost {
    color: [f32; 4],
    shapes: Vec<OverlayShape>,
}

impl OverlayHost {
    fn push(&mut self, shape: OverlayShape) {
        if self.shapes.len() < OVERLAY_MAX_SHAPES {
            self.shapes.push(shape);
        }
    }
}

impl Host for OverlayHost {
    fn call(&mut self, id: u16, _site: u32, a: &[f32]) -> f32 {
        if a.iter().any(|x| !x.is_finite()) {
            return 0.0;
        }
        let color = self.color;
        match id {
            COLOR => self.color = [a[0], a[1], a[2], a[3]].map(|c| c.clamp(0.0, 1.0)),
            LINE => self.push(OverlayShape::Line { from: [a[0], a[1]], to: [a[2], a[3]], width: a[4].clamp(0.0, 64.0), color }),
            RECT => self.push(OverlayShape::Rect { min: [a[0].min(a[2]), a[1].min(a[3])], max: [a[0].max(a[2]), a[1].max(a[3])], width: a[4].clamp(0.0, 64.0).max(0.5), color }),
            FILL => self.push(OverlayShape::Rect { min: [a[0].min(a[2]), a[1].min(a[3])], max: [a[0].max(a[2]), a[1].max(a[3])], width: 0.0, color }),
            CIRCLE => self.push(OverlayShape::Circle { center: [a[0], a[1]], radius: a[2].abs(), width: a[3].clamp(0.0, 64.0), color }),
            DOT => self.push(OverlayShape::Circle { center: [a[0], a[1]], radius: a[2].abs(), width: 0.0, color }),
            // grid(x0, y0, x1, y1, columns, rows, width): the lines between cells.
            GRID => {
                let (x0, y0, x1, y1) = (a[0], a[1], a[2], a[3]);
                let (cols, rows) = (a[4].clamp(1.0, 64.0) as u32, a[5].clamp(1.0, 64.0) as u32);
                let width = a[6].clamp(0.0, 64.0);
                for i in 1..cols {
                    let x = x0 + (x1 - x0) * i as f32 / cols as f32;
                    self.push(OverlayShape::Line { from: [x, y0], to: [x, y1], width, color });
                }
                for j in 1..rows {
                    let y = y0 + (y1 - y0) * j as f32 / rows as f32;
                    self.push(OverlayShape::Line { from: [x0, y], to: [x1, y], width, color });
                }
            }
            _ => {}
        }
        0.0
    }
}

/// A plugin's overlay: drawn over the viewer each frame (guides, grids, safe areas, a
/// frame counter's marks…). Reads `canvas_w`/`canvas_h` (px), `seconds` (the playhead),
/// `duration`, `playing`, and the selected clip's box (`selected` 0/1, `sel_x0`…`sel_y1`
/// in canvas px); draws with `color(r, g, b, a)`, then `line(x0, y0, x1, y1, width)`,
/// `rect(x0, y0, x1, y1, width)`, `fill(x0, y0, x1, y1)`, `circle(x, y, radius, width)`,
/// `dot(x, y, radius)` and `grid(x0, y0, x1, y1, columns, rows, width)`. It can only
/// draw: it never sees or changes the project's contents.
#[derive(Debug)]
pub struct OverlayScript(Script);

impl OverlayScript {
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Self, String> {
        let env = Env { reads: &OVERLAY_READS, writes: &[], params, functions: OVERLAY_FUNCTIONS, ..Env::default() };
        Script::compile(source, env).map(OverlayScript)
    }

    pub fn draw(&self, values: &Evaluated, i: &OverlayInput) -> Vec<OverlayShape> {
        let s = &self.0;
        let mut regs = vec![0.0; s.frame.registers];
        let sel = i.selected.unwrap_or_default();
        let reads = [i.canvas[0], i.canvas[1], i.seconds, i.duration, i.playing as u8 as f64, i.selected.is_some() as u8 as f64, sel[0], sel[1], sel[2], sel[3]];
        for (r, v) in regs.iter_mut().zip(reads) {
            *r = v as f32;
        }
        s.frame.load_params(&mut regs, &s.params, values);
        let mut host = OverlayHost { color: [1.0, 1.0, 1.0, 1.0], shapes: Vec::new() };
        s.run(&mut regs, &mut host);
        host.shapes
    }
}

// ---- actions ----

const ACTION_READS: [&str; 5] = ["index", "count", "playhead", "canvas_w", "canvas_h"];
/// What an action may change on each clip (and reads first: it starts as the clip's own).
pub const ACTION_WRITES: [&str; 8] = ["start", "duration", "position_x", "position_y", "scale", "rotation", "opacity", "volume_db"];

/// One clip as an action sees it (and what it hands back, changed or not).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionClip {
    pub start: f64,
    pub duration: f64,
    pub position: [f64; 2],
    pub scale: f64,
    pub rotation: f64,
    pub opacity: f64,
    pub volume_db: f64,
}

/// A plugin's action: run on demand over the selected clips, each in turn (`index` of
/// `count`, in timeline order), with `playhead` and `canvas_w`/`canvas_h`. It reads and
/// may set `start`, `duration` (seconds), `position_x`/`position_y` (canvas share),
/// `scale`, `rotation` (degrees), `opacity` and `volume_db`; what it changes is one edit,
/// undone in one step. `noise(x, stream)` / `jitter(x, stream)` give it randomness.
#[derive(Debug)]
pub struct ActionScript(Script);

impl ActionScript {
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Self, String> {
        let env = Env { reads: &ACTION_READS, writes: &ACTION_WRITES, params, functions: MOTION_FUNCTIONS, ..Env::default() };
        Script::compile(source, env).map(ActionScript)
    }

    /// Clip `index` of `count`, changed (non-finite results keep the clip's own value).
    pub fn apply(&self, values: &Evaluated, clip: ActionClip, index: usize, count: usize, playhead: f64, canvas: [f64; 2]) -> ActionClip {
        let s = &self.0;
        let mut regs = vec![0.0; s.frame.registers];
        let reads = [index as f64, count as f64, playhead, canvas[0], canvas[1]];
        let own = [clip.start, clip.duration, clip.position[0], clip.position[1], clip.scale, clip.rotation, clip.opacity, clip.volume_db];
        for (r, v) in regs.iter_mut().zip(reads.iter().chain(&own)) {
            *r = *v as f32;
        }
        s.frame.load_params(&mut regs, &s.params, values);
        s.run(&mut regs, &mut MotionHost { seed: 0x0A7_1015 });
        let n = ACTION_READS.len();
        let get = |k: usize| if regs[n + k].is_finite() { regs[n + k] as f64 } else { own[k] };
        ActionClip {
            start: get(0).max(0.0),
            duration: get(1).max(1e-3),
            position: [get(2), get(3)],
            scale: get(4),
            rotation: get(5),
            opacity: get(6).clamp(0.0, 1.0),
            volume_db: get(7).clamp(-60.0, 24.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_params::{ParamId, Unit, Value};

    fn input(visibility: f64) -> MotionInput {
        MotionInput { visibility, progress: visibility, seconds: visibility, canvas: [1920.0, 1080.0], seed: 3, leaving: false }
    }

    #[test]
    fn motion_scripts_move_the_layer() {
        let params = [ParamSchema::new("distance", Value::Float(1.0), Unit::None)];
        let m = MotionScript::compile("move_x = -(1 - visibility) * canvas_w * distance; opacity = visibility; turn = jitter(seconds, 2) * 0;", &params).unwrap();
        let values = Evaluated(vec![(ParamId::new("distance"), Value::Float(0.5))]);
        let half = m.eval(&values, &input(0.5));
        assert_eq!(half.offset, [-480.0, 0.0]);
        assert_eq!((half.scale, half.opacity, half.rotation), (1.0, 0.5, 0.0));
        let a = MotionScript::compile("move_x = noise(seconds, 0);", &[]).unwrap();
        let (x, y) = (a.eval(&Evaluated(vec![]), &input(0.3)), a.eval(&Evaluated(vec![]), &input(0.3)));
        assert_eq!(x, y, "the same instance at the same moment moves the same");
    }

    #[test]
    fn bounds_scripts_grow_the_box() {
        let params = [ParamSchema::new("reach", Value::Float(10.0), Unit::LayerPixels)];
        let b = BoundsScript::compile("left = x0 - reach * raster_scale; grow(x1 + 5, y1 + 7);", &params).unwrap();
        let r = b.eval(&Evaluated(vec![]), 2.0, Rect::new(0.0, 0.0, 100.0, 50.0));
        assert_eq!(r, Rect::new(-20.0, 0.0, 105.0, 57.0));
    }

    #[test]
    fn pass_scripts_read_packed_uniforms() {
        let params = [ParamSchema::new("tint", Value::Color([0.0; 4]), Unit::None), ParamSchema::new("radius", Value::Float(0.0), Unit::LayerPixels)];
        let p = PassScript::compile("out = ceil(radius / 10) + select(pass + 1 == passes, 0, tint_a);", &params).unwrap();
        let uniforms = [0.0, 0.0, 0.0, 3.0, 25.0];
        // 3 from the radius, 3 from the color's alpha (except on the last pass).
        assert_eq!(p.count(&uniforms), 6);
        assert_eq!((p.divisor(&uniforms, 0, 4), p.divisor(&uniforms, 3, 4)), (6, 3));
    }
}
