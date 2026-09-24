use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParamType {
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    /// Straight (non-premultiplied) RGBA in the project's working color space.
    Color,
    Enum,
    Text,
    /// A media file from the project's pool (e.g. a mask's matte).
    Media,
    /// Color stops along a direction ([`Gradient`]).
    Gradient,
}

/// One color of a [`Gradient`]: straight RGBA at `pos` (0 → 1 along the direction).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub pos: f64,
    pub color: [f64; 4],
}

/// A directional gradient: color stops laid along `angle` across whatever it's
/// applied to (a clip, a text box). One stop is a plain color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    /// Degrees on screen: 0 = towards the right, 90 = down (clockwise).
    pub angle: f64,
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    /// Most stops a shader reads.
    pub const MAX_STOPS: usize = 6;
    /// Floats [`Gradient::pack`] produces: angle, count, then (pos, r, g, b, a) per stop.
    pub const PACKED_LEN: usize = 2 + 5 * Self::MAX_STOPS;

    pub fn solid(color: [f64; 4]) -> Self {
        Gradient { angle: 90.0, stops: vec![GradientStop { pos: 0.0, color }] }
    }

    pub fn two(angle: f64, a: [f64; 4], b: [f64; 4]) -> Self {
        Gradient { angle, stops: vec![GradientStop { pos: 0.0, color: a }, GradientStop { pos: 1.0, color: b }] }
    }

    /// Stops sorted by position, at most [`Gradient::MAX_STOPS`], never empty.
    pub fn sorted_stops(&self) -> Vec<GradientStop> {
        let mut s: Vec<GradientStop> = self.stops.iter().take(Self::MAX_STOPS).cloned().collect();
        if s.is_empty() {
            s.push(GradientStop { pos: 0.0, color: [1.0; 4] });
        }
        s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        s
    }

    /// The color at `t` (0 → 1 along the direction).
    pub fn sample(&self, t: f64) -> [f64; 4] {
        let s = self.sorted_stops();
        if t <= s[0].pos {
            return s[0].color;
        }
        for w in s.windows(2) {
            if t <= w[1].pos {
                let k = ((t - w[0].pos) / (w[1].pos - w[0].pos).max(1e-9)).clamp(0.0, 1.0);
                return std::array::from_fn(|i| w[0].color[i] + (w[1].color[i] - w[0].color[i]) * k);
            }
        }
        s[s.len() - 1].color
    }

    /// The uniform layout shaders read with `oa_gradient`.
    pub fn pack(&self) -> Vec<f32> {
        let s = self.sorted_stops();
        let mut out = vec![0f32; Self::PACKED_LEN];
        out[0] = self.angle as f32;
        out[1] = s.len() as f32;
        for (i, stop) in s.iter().enumerate() {
            let b = 2 + 5 * i;
            out[b] = stop.pos as f32;
            for c in 0..4 {
                out[b + 1 + c] = stop.color[c] as f32;
            }
        }
        out
    }

    fn lerp(&self, other: &Gradient, t: f64) -> Option<Gradient> {
        if self.stops.len() != other.stops.len() {
            return None;
        }
        // The shorter way round.
        let d = (other.angle - self.angle + 540.0).rem_euclid(360.0) - 180.0;
        let stops = self
            .stops
            .iter()
            .zip(&other.stops)
            .map(|(a, b)| GradientStop {
                pos: a.pos + (b.pos - a.pos) * t,
                color: std::array::from_fn(|i| a.color[i] + (b.color[i] - a.color[i]) * t),
            })
            .collect();
        Some(Gradient { angle: (self.angle + d * t).rem_euclid(360.0), stops })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Float(f64),
    Int(i64),
    Bool(bool),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Color([f64; 4]),
    /// Index into the schema's option list; stored by stable string key.
    Enum(String),
    Text(String),
    /// A media id from the project's pool, or none chosen yet. The planner feeds the
    /// media's picture to the effect as a second input.
    Media(Option<u64>),
    Gradient(Gradient),
}

impl Value {
    pub fn ty(&self) -> ParamType {
        match self {
            Value::Float(_) => ParamType::Float,
            Value::Int(_) => ParamType::Int,
            Value::Bool(_) => ParamType::Bool,
            Value::Vec2(_) => ParamType::Vec2,
            Value::Vec3(_) => ParamType::Vec3,
            Value::Color(_) => ParamType::Color,
            Value::Enum(_) => ParamType::Enum,
            Value::Text(_) => ParamType::Text,
            Value::Media(_) => ParamType::Media,
            Value::Gradient(_) => ParamType::Gradient,
        }
    }

    pub fn as_gradient(&self) -> Option<&Gradient> {
        match self {
            Value::Gradient(g) => Some(g),
            _ => None,
        }
    }

    /// This value as type `ty` where there is an obvious conversion (a color is a
    /// one-stop gradient; a gradient read as a color is its first stop), so a
    /// parameter can change type without breaking saved projects.
    pub fn coerce(self, ty: ParamType) -> Option<Value> {
        match (self, ty) {
            (v, ty) if v.ty() == ty => Some(v),
            (Value::Color(c), ParamType::Gradient) => Some(Value::Gradient(Gradient::solid(c))),
            (Value::Gradient(g), ParamType::Color) => Some(Value::Color(g.sorted_stops()[0].color)),
            (Value::Int(i), ParamType::Float) => Some(Value::Float(i as f64)),
            _ => None,
        }
    }

    pub fn as_media(&self) -> Option<u64> {
        match *self {
            Value::Media(m) => m,
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match *self {
            Value::Float(f) => Some(f),
            Value::Int(i) => Some(i as f64),
            _ => None,
        }
    }

    pub fn as_vec2(&self) -> Option<[f64; 2]> {
        match *self {
            Value::Vec2(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Value::Enum(s) => Some(s),
            _ => None,
        }
    }

    /// False for values carrying NaN or infinity (which would poison a render).
    pub fn is_finite(&self) -> bool {
        match self {
            Value::Float(f) => f.is_finite(),
            Value::Vec2(v) => v.iter().all(|x| x.is_finite()),
            Value::Vec3(v) => v.iter().all(|x| x.is_finite()),
            Value::Color(v) => v.iter().all(|x| x.is_finite()),
            Value::Gradient(g) => g.angle.is_finite() && g.stops.iter().all(|s| s.pos.is_finite() && s.color.iter().all(|x| x.is_finite())),
            _ => true,
        }
    }

    /// `self + k·other` for numeric values of the same type; anything else is `self`.
    pub fn plus(&self, other: &Value, k: f64) -> Value {
        fn add<const N: usize>(a: &[f64; N], b: &[f64; N], k: f64) -> [f64; N] {
            std::array::from_fn(|i| a[i] + b[i] * k)
        }
        match (self, other) {
            (Value::Float(a), Value::Float(b)) => Value::Float(a + b * k),
            (Value::Vec2(a), Value::Vec2(b)) => Value::Vec2(add(a, b, k)),
            (Value::Vec3(a), Value::Vec3(b)) => Value::Vec3(add(a, b, k)),
            (Value::Color(a), Value::Color(b)) => Value::Color(add(a, b, k)),
            _ => self.clone(),
        }
    }

    /// Blend between two values of the same type. Discrete types hold `self` until
    /// `t` reaches 1.
    pub fn interpolate(&self, other: &Value, t: f64) -> Value {
        fn lerp<const N: usize>(a: &[f64; N], b: &[f64; N], t: f64) -> [f64; N] {
            std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
        }
        match (self, other) {
            (Value::Float(a), Value::Float(b)) => Value::Float(a + (b - a) * t),
            (Value::Vec2(a), Value::Vec2(b)) => Value::Vec2(lerp(a, b, t)),
            (Value::Vec3(a), Value::Vec3(b)) => Value::Vec3(lerp(a, b, t)),
            (Value::Color(a), Value::Color(b)) => Value::Color(lerp(a, b, t)),
            (Value::Gradient(a), Value::Gradient(b)) if a.stops.len() == b.stops.len() => {
                Value::Gradient(a.lerp(b, t).expect("same stop count"))
            }
            _ if t >= 1.0 => other.clone(),
            _ => self.clone(),
        }
    }

    /// Feeds a canonical byte encoding to `out`, for cache keys. `-0.0` and `0.0`
    /// encode identically, and all NaNs encode identically.
    pub fn write_canonical(&self, out: &mut dyn FnMut(&[u8])) {
        fn f(x: f64, out: &mut dyn FnMut(&[u8])) {
            let bits = if x == 0.0 {
                0u64
            } else if x.is_nan() {
                f64::NAN.to_bits()
            } else {
                x.to_bits()
            };
            out(&bits.to_le_bytes());
        }
        out(&[self.ty() as u8]);
        match self {
            Value::Float(x) => f(*x, out),
            Value::Int(i) => out(&i.to_le_bytes()),
            Value::Bool(b) => out(&[*b as u8]),
            Value::Vec2(v) => v.iter().for_each(|x| f(*x, out)),
            Value::Vec3(v) => v.iter().for_each(|x| f(*x, out)),
            Value::Color(v) => v.iter().for_each(|x| f(*x, out)),
            Value::Enum(s) | Value::Text(s) => {
                out(&(s.len() as u64).to_le_bytes());
                out(s.as_bytes());
            }
            Value::Media(m) => out(&m.map_or(0, |id| id + 1).to_le_bytes()),
            Value::Gradient(g) => {
                f(g.angle, out);
                out(&(g.stops.len() as u64).to_le_bytes());
                for s in &g.stops {
                    f(s.pos, out);
                    s.color.iter().for_each(|x| f(*x, out));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(v: &Value) -> Vec<u8> {
        let mut b = Vec::new();
        v.write_canonical(&mut |s| b.extend_from_slice(s));
        b
    }

    #[test]
    fn canonical_floats() {
        assert_eq!(bytes(&Value::Float(0.0)), bytes(&Value::Float(-0.0)));
        assert_eq!(bytes(&Value::Float(f64::NAN)), bytes(&Value::Float(-f64::NAN)));
        assert_ne!(bytes(&Value::Float(1.0)), bytes(&Value::Int(1)));
    }

    #[test]
    fn discrete_values_hold() {
        let a = Value::Bool(false);
        let b = Value::Bool(true);
        assert_eq!(a.interpolate(&b, 0.99), a);
        assert_eq!(a.interpolate(&b, 1.0), b);
    }
}
