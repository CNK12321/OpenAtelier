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
