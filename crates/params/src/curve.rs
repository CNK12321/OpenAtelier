use crate::{EvalContext, Value};
use oa_time::Time;
use serde::{Deserialize, Serialize};

/// Which clock a curve's keyframe times are measured on.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyframeAnchor {
    /// Time since the clip starts on the timeline. Trimming the clip head shifts the
    /// animation relative to the footage (fades, pops, titles).
    #[default]
    ClipStart,
    /// Time in the source media. Trimming never moves the animation relative to the
    /// footage (masks, tracking, reframe focus).
    SourceMedia,
}

/// A cubic-bezier easing handle in normalized segment space (x = time, y = value),
/// like CSS `cubic-bezier()`. `x` is clamped to `[0, 1]` so time stays monotonic;
/// `y` may overshoot for anticipation/bounce.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ease {
    pub x: f64,
    pub y: f64,
}

/// How the segment *leaving* a keyframe interpolates.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Interp {
    Hold,
    Linear,
    /// `out` is this keyframe's handle; `into` is the handle arriving at the next one.
    Bezier { out: Ease, into: Ease },
    /// A power curve: `u^power` easing in, its mirror easing out, or both halves.
    Power { power: f64, ease: EaseDir },
}

/// Which end of a segment a [`Interp::Power`] eases.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EaseDir {
    In,
    Out,
    InOut,
}

impl Interp {
    /// The eased fraction at `u` (0..1) along the segment.
    pub fn ease(self, u: f64) -> f64 {
        match self {
            Interp::Hold => 0.0,
            Interp::Linear => u,
            Interp::Bezier { out, into } => cubic_bezier(out, into, u),
            Interp::Power { power, ease } => {
                let p = if power.is_finite() { power.clamp(0.1, 10.0) } else { 1.0 };
                match ease {
                    EaseDir::In => u.powf(p),
                    EaseDir::Out => 1.0 - (1.0 - u).powf(p),
                    EaseDir::InOut if u < 0.5 => 0.5 * (2.0 * u).powf(p),
                    EaseDir::InOut => 1.0 - 0.5 * (2.0 - 2.0 * u).powf(p),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub t: Time,
    pub value: Value,
    pub interp: Interp,
}

impl Keyframe {
    pub fn linear(t: Time, value: Value) -> Self {
        Keyframe { t, value, interp: Interp::Linear }
    }

    pub fn hold(t: Time, value: Value) -> Self {
        Keyframe { t, value, interp: Interp::Hold }
    }

    /// Standard ease-in-out.
    pub fn ease(t: Time, value: Value) -> Self {
        Keyframe {
            t,
            value,
            interp: Interp::Bezier { out: Ease { x: 0.42, y: 0.0 }, into: Ease { x: 0.58, y: 1.0 } },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub anchor: KeyframeAnchor,
    /// Sorted by `t`, no duplicate times.
    pub keys: Vec<Keyframe>,
}

impl Curve {
    pub fn new(anchor: KeyframeAnchor, mut keys: Vec<Keyframe>) -> Self {
        keys.sort_by_key(|k| k.t);
        keys.dedup_by_key(|k| k.t);
        Curve { anchor, keys }
    }

    /// Inserts or replaces the keyframe at `key.t`.
    pub fn set_key(&mut self, key: Keyframe) {
        match self.keys.binary_search_by_key(&key.t, |k| k.t) {
            Ok(i) => self.keys[i] = key,
            Err(i) => self.keys.insert(i, key),
        }
    }

    /// This curve's own time for an instant (clip or source clock).
    pub fn clock(&self, ctx: &EvalContext) -> Time {
        match self.anchor {
            KeyframeAnchor::ClipStart => ctx.clip_time,
            KeyframeAnchor::SourceMedia => ctx.source_time,
        }
    }

    /// Sets the value at `t`: replaces the value of a key there (keeping its easing), or
    /// adds a key that eases like the key before it (or after, at the very start).
    pub fn set_value_at(&mut self, t: Time, value: Value) {
        match self.keys.binary_search_by_key(&t, |k| k.t) {
            Ok(i) => self.keys[i].value = value,
            Err(i) => {
                let interp = i
                    .checked_sub(1)
                    .and_then(|p| self.keys.get(p))
                    .or_else(|| self.keys.get(i))
                    .map_or_else(crate::default_interp, |k| k.interp);
                self.keys.insert(i, Keyframe { t, value, interp });
            }
        }
    }

    /// Removes the key at exactly `t`. Returns false if there was none.
    pub fn remove_key(&mut self, t: Time) -> bool {
        match self.keys.binary_search_by_key(&t, |k| k.t) {
            Ok(i) => {
                self.keys.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// See [`crate::ParamSource::shift_clip_clock`].
    pub fn shift_clip_clock(&mut self, delta: Time) {
        if self.anchor == KeyframeAnchor::ClipStart {
            for k in &mut self.keys {
                k.t -= delta;
            }
        }
    }

    pub fn eval(&self, ctx: &EvalContext) -> Value {
        let t = match self.anchor {
            KeyframeAnchor::ClipStart => ctx.clip_time,
            KeyframeAnchor::SourceMedia => ctx.source_time,
        };
        self.eval_at(t)
    }

    /// Panics on an empty curve; an empty curve is never stored.
    pub fn eval_at(&self, t: Time) -> Value {
        let keys = &self.keys;
        assert!(!keys.is_empty(), "empty curve");
        let next = keys.partition_point(|k| k.t <= t);
        if next == 0 {
            return keys[0].value.clone();
        }
        if next == keys.len() {
            return keys[next - 1].value.clone();
        }
        let (a, b) = (&keys[next - 1], &keys[next]);
        let u = (t - a.t).0 as f64 / (b.t - a.t).0 as f64;
        if a.interp == Interp::Hold {
            return a.value.clone();
        }
        a.value.interpolate(&b.value, a.interp.ease(u))
    }
}

/// Solves y for x on the curve (0,0) → p1 → p2 → (1,1). Newton's method with a
/// bisection fallback; x(s) is monotonic because handle x is clamped to [0,1].
fn cubic_bezier(p1: Ease, p2: Ease, x: f64) -> f64 {
    let (x1, x2) = (p1.x.clamp(0.0, 1.0), p2.x.clamp(0.0, 1.0));
    let bez = |a: f64, b: f64, s: f64| {
        let inv = 1.0 - s;
        3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
    };
    let dbez = |a: f64, b: f64, s: f64| {
        let inv = 1.0 - s;
        3.0 * inv * inv * a + 6.0 * inv * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = x;
    let mut solved = false;
    for _ in 0..8 {
        let err = bez(x1, x2, s) - x;
        if err.abs() < 1e-9 {
            solved = true;
            break;
        }
        let d = dbez(x1, x2, s);
        if d.abs() < 1e-7 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    if !solved {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..60 {
            s = 0.5 * (lo + hi);
            if bez(x1, x2, s) < x {
                lo = s;
            } else {
                hi = s;
            }
        }
    }
    bez(p1.y, p2.y, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    fn f(v: Value) -> f64 {
        v.as_float().unwrap()
    }

    #[test]
    fn linear_hold_and_clamping() {
        let c = Curve::new(
            KeyframeAnchor::ClipStart,
            vec![
                Keyframe::linear(secs(0), Value::Float(0.0)),
                Keyframe::hold(secs(2), Value::Float(10.0)),
                Keyframe::linear(secs(4), Value::Float(20.0)),
            ],
        );
        assert_eq!(f(c.eval_at(secs(-5))), 0.0);
        assert!((f(c.eval_at(secs(1))) - 5.0).abs() < 1e-9);
        assert_eq!(f(c.eval_at(secs(3))), 10.0);
        assert_eq!(f(c.eval_at(secs(4))), 20.0);
        assert_eq!(f(c.eval_at(secs(99))), 20.0);
    }

    #[test]
    fn symmetric_ease_hits_midpoint_and_is_monotonic() {
        let c = Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0), Value::Float(0.0)), Keyframe::linear(secs(1), Value::Float(1.0))],
        );
        let mid = f(c.eval_at(Time(oa_time::FLICKS_PER_SECOND / 2)));
        assert!((mid - 0.5).abs() < 1e-6, "{mid}");
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = f(c.eval_at(Time(oa_time::FLICKS_PER_SECOND * i / 100)));
            assert!(v >= prev - 1e-12);
            prev = v;
        }
    }

    #[test]
    fn degenerate_handles_still_solve() {
        let v = cubic_bezier(Ease { x: 0.0, y: 0.0 }, Ease { x: 1.0, y: 1.0 }, 0.5);
        assert!((0.0..=1.0).contains(&v));
        let steep = cubic_bezier(Ease { x: 1.0, y: 0.0 }, Ease { x: 0.0, y: 1.0 }, 0.5);
        assert!((steep - 0.5).abs() < 1e-6);
    }

    #[test]
    fn set_key_replaces_same_time() {
        let mut c = Curve::new(KeyframeAnchor::ClipStart, vec![Keyframe::linear(secs(1), Value::Float(1.0))]);
        c.set_key(Keyframe::linear(secs(1), Value::Float(2.0)));
        c.set_key(Keyframe::linear(secs(0), Value::Float(0.0)));
        assert_eq!(c.keys.len(), 2);
        assert_eq!(f(c.eval_at(secs(1))), 2.0);
    }
}

#[cfg(test)]
mod power_tests {
    use super::*;

    #[test]
    fn power_ease_in_out_is_symmetric_and_anchored() {
        let e = Interp::Power { power: 1.5, ease: EaseDir::InOut };
        assert_eq!(e.ease(0.0), 0.0);
        assert!((e.ease(1.0) - 1.0).abs() < 1e-12);
        assert!((e.ease(0.5) - 0.5).abs() < 1e-12);
        assert!((e.ease(0.25) + e.ease(0.75) - 1.0).abs() < 1e-12, "mirror halves");
        assert!(e.ease(0.25) < 0.25, "slow start");
        assert!(Interp::Power { power: 2.0, ease: EaseDir::Out }.ease(0.5) > 0.5, "fast start");
    }
}
