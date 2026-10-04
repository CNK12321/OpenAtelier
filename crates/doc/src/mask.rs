//! Masks: areas of a clip drawn by hand (rectangles, ellipses, brush strokes, magic
//! selections, fills, imported pictures) that properties and effects can be limited to.
//!
//! * A mask lives on its clip ([`crate::Item::masks`]), in the clip's own picture
//!   space: every coordinate is a fraction of the layer (0..1 across, 0..1 down), so
//!   the mask moves, scales and turns with the clip.
//! * What was drawn is plain data ([`MaskShape`]s, applied in order: each adds to the
//!   mask or erases from it). It's turned into coverage on the CPU once per edit
//!   ([`Mask::rasterize`]); the planner caches that and the GPU does the rest.
//! * What changes over time is keyframable, in the clip's params under
//!   `mask.<id>.*` ([`params`]): where the mask sits (`center`, an offset from where it
//!   was drawn), its `scale` and `rotation`, how soft its edge is (`softness`, layer
//!   px) and how strongly it applies (`harshness`: 1 = fully, 0 = not at all).
//! * A clip property can take a second value inside the mask (`<param>#mask`,
//!   [`on_mask`]); an effect can run only inside a mask ([`EFFECT_USE`]).
//! * Shapes each grow or shrink (`expand`) and fade (`feather`) on their own; bezier
//!   paths ([`MaskShape::Path`]) keyframe point by point — the ground rotoscoping
//!   stands on. Masks used together combine by their [`MaskMode`].

use oa_params::{ParamSchema, Unit, Value};
use oa_time::Time;
use serde::{Deserialize, Serialize};

/// One mask on a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    /// Unique within the project (from `Project::alloc_id`); keys its params.
    pub id: u64,
    pub name: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Everything outside what was drawn instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    /// How it joins the masks above it when several are used together.
    #[serde(default, skip_serializing_if = "MaskMode::is_add")]
    pub mode: MaskMode,
    /// What was drawn, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shapes: Vec<MaskShape>,
    /// How the drawing is stretched over the layer around its middle — `[1, 1]` as
    /// drawn; set when a mask is copied to a clip of another shape ("fit" or "crop"),
    /// so its shapes keep their proportions.
    #[serde(default = "unit_frame", skip_serializing_if = "is_unit_frame")]
    pub frame: [f64; 2],
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn unit_frame() -> [f64; 2] {
    [1.0, 1.0]
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// How a mask joins the ones above it (in the clip's list) when several are used
/// together. The first mask with nothing above it: subtracting takes it out of the whole
/// clip; the others start from it alone.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
    /// Together: either one (`a + b − ab`).
    #[default]
    Add,
    /// Takes this one away (`a × (1 − b)`).
    Subtract,
    /// Only where both are (`a × b`).
    Intersect,
    /// Where one is but not both (`a + b − 2ab`).
    Difference,
}

impl MaskMode {
    pub const ALL: [MaskMode; 4] = [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect, MaskMode::Difference];

    pub fn is_add(&self) -> bool {
        *self == MaskMode::Add
    }

    pub fn name(self) -> &'static str {
        match self {
            MaskMode::Add => "Add",
            MaskMode::Subtract => "Subtract",
            MaskMode::Intersect => "Intersect",
            MaskMode::Difference => "Difference",
        }
    }

    /// Its number in the combining shader (`MASK_COMBINE`).
    pub fn index(self) -> u8 {
        self as u8
    }
}

/// One point of a bezier path at one instant: where it is and its two handles (offsets
/// from it, drawing fractions; zero for a corner).
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathKey {
    /// Clip time of this key.
    pub t: Time,
    pub at: [f64; 2],
    #[serde(default, rename = "in", skip_serializing_if = "is_origin")]
    pub handle_in: [f64; 2],
    #[serde(default, rename = "out", skip_serializing_if = "is_origin")]
    pub handle_out: [f64; 2],
}

fn is_origin(p: &[f64; 2]) -> bool {
    *p == [0.0, 0.0]
}

/// A point of a bezier path, keyframed on its own: one key holds it still; more move it
/// (eased from key to key, held before the first and after the last). Moving one point
/// keys only that point — what rotoscoping a moving outline needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathPoint {
    pub keys: Vec<PathKey>,
}

impl PathPoint {
    /// A point with no handles, still, at `at`.
    pub fn new(at: [f64; 2]) -> PathPoint {
        PathPoint { keys: vec![PathKey { t: Time::ZERO, at, ..Default::default() }] }
    }

    pub fn animated(&self) -> bool {
        self.keys.len() > 1
    }

    /// Where it is at clip time `t`.
    pub fn at(&self, t: Time) -> PathKey {
        let Some(first) = self.keys.first() else { return PathKey::default() };
        if self.keys.len() == 1 || t <= first.t {
            return PathKey { t, ..*first };
        }
        let i = self.keys.partition_point(|k| k.t <= t);
        let Some(b) = self.keys.get(i) else { return PathKey { t, ..*self.keys.last().expect("not empty") } };
        let a = &self.keys[i - 1];
        let f = (t - a.t).as_seconds_f64() / (b.t - a.t).as_seconds_f64().max(1e-12);
        let f = f * f * (3.0 - 2.0 * f);
        let mix = |p: [f64; 2], q: [f64; 2]| [p[0] + (q[0] - p[0]) * f, p[1] + (q[1] - p[1]) * f];
        PathKey { t, at: mix(a.at, b.at), handle_in: mix(a.handle_in, b.handle_in), handle_out: mix(a.handle_out, b.handle_out) }
    }

    /// Sets it at clip time `t`: a key there (the one within `near` of it, or a new one)
    /// when it's animated or `animate` asks; otherwise its one key changes.
    pub fn set(&mut self, t: Time, key: PathKey, animate: bool, near: Time) {
        let key = PathKey { t, ..key };
        if !animate && self.keys.len() <= 1 {
            self.keys = vec![PathKey { t: self.keys.first().map_or(Time::ZERO, |k| k.t), ..key }];
            return;
        }
        match self.keys.iter_mut().find(|k| (k.t - t).0.abs() <= near.0) {
            Some(k) => *k = PathKey { t: k.t, ..key },
            None => {
                let i = self.keys.partition_point(|k| k.t < t);
                self.keys.insert(i, key);
            }
        }
    }

    /// Takes away its key at `t` (within `near`), keeping at least one.
    pub fn remove_key(&mut self, t: Time, near: Time) -> bool {
        if self.keys.len() <= 1 {
            return false;
        }
        let (before, here) = (self.keys.len(), self.at(t));
        self.keys.retain(|k| (k.t - t).0.abs() > near.0);
        if self.keys.is_empty() {
            self.keys.push(here);
        }
        self.keys.len() != before
    }
}

fn is_unit_frame(f: &[f64; 2]) -> bool {
    *f == [1.0, 1.0]
}

/// One thing drawn into a mask. Positions and sizes are fractions of the layer (x of its
/// width, y of its height); `erase` takes it away from the mask instead of adding it.
/// Every shape can grow (`expand` > 0) or shrink (< 0) and fade over `feather`, both as
/// a fraction of the layer's height.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaskShape {
    Rect {
        center: [f64; 2],
        size: [f64; 2],
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    Ellipse {
        center: [f64; 2],
        size: [f64; 2],
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    /// A brush stroke through `points`: `radius` as a fraction of the layer's height,
    /// `softness` the share of the radius that fades out (0 = a hard edge).
    Stroke {
        points: Vec<[f32; 2]>,
        radius: f64,
        #[serde(default)]
        softness: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    /// A bezier outline through `points`, each keyframed on its own; filled once
    /// `closed`.
    Path {
        points: Vec<PathPoint>,
        #[serde(default)]
        closed: bool,
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    /// Coverage made pixel by pixel (a magic selection, a fill, an imported picture),
    /// stretched over the layer: see [`Bitmap`].
    Bitmap {
        #[serde(flatten)]
        bitmap: Bitmap,
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
    /// Coverage that changes frame by frame (automatic rotoscoping, SAM 2): a bitmap for
    /// each moment, at clip times in order; each shows until the next one. Stretched over
    /// the layer like [`MaskShape::Bitmap`].
    Matte {
        frames: Vec<MatteFrame>,
        #[serde(default, skip_serializing_if = "is_zero")]
        expand: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        erase: bool,
    },
}

/// One frame of a [`MaskShape::Matte`]: from clip time `t` on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatteFrame {
    pub t: Time,
    #[serde(flatten)]
    pub bitmap: Bitmap,
}

/// The frame of `frames` (in time order) showing at clip time `t`: the last at or before
/// it, or the first before them all.
pub fn matte_at(frames: &[MatteFrame], t: Time) -> Option<&MatteFrame> {
    let i = frames.partition_point(|f| f.t <= t);
    frames.get(i.saturating_sub(1))
}

impl MaskShape {
    pub fn erases(&self) -> bool {
        match self {
            MaskShape::Rect { erase, .. } | MaskShape::Ellipse { erase, .. } | MaskShape::Stroke { erase, .. } | MaskShape::Path { erase, .. } | MaskShape::Bitmap { erase, .. } | MaskShape::Matte { erase, .. } => *erase,
        }
    }

    /// Its expand and feather (fractions of the layer's height).
    pub fn edge(&self) -> (f64, f64) {
        match self {
            MaskShape::Rect { expand, feather, .. }
            | MaskShape::Ellipse { expand, feather, .. }
            | MaskShape::Stroke { expand, feather, .. }
            | MaskShape::Path { expand, feather, .. }
            | MaskShape::Bitmap { expand, feather, .. }
            | MaskShape::Matte { expand, feather, .. } => (*expand, *feather),
        }
    }

    pub fn edge_mut(&mut self) -> (&mut f64, &mut f64) {
        match self {
            MaskShape::Rect { expand, feather, .. }
            | MaskShape::Ellipse { expand, feather, .. }
            | MaskShape::Stroke { expand, feather, .. }
            | MaskShape::Path { expand, feather, .. }
            | MaskShape::Bitmap { expand, feather, .. }
            | MaskShape::Matte { expand, feather, .. } => (expand, feather),
        }
    }

    pub fn erase_mut(&mut self) -> &mut bool {
        match self {
            MaskShape::Rect { erase, .. } | MaskShape::Ellipse { erase, .. } | MaskShape::Stroke { erase, .. } | MaskShape::Path { erase, .. } | MaskShape::Bitmap { erase, .. } | MaskShape::Matte { erase, .. } => erase,
        }
    }

    /// A rectangle's or ellipse's box, from `a` to `b` (drawing fractions).
    pub fn boxed(ellipse: bool, a: [f64; 2], b: [f64; 2], erase: bool) -> MaskShape {
        let center = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let size = [(b[0] - a[0]).abs(), (b[1] - a[1]).abs()];
        if ellipse {
            MaskShape::Ellipse { center, size, expand: 0.0, feather: 0.0, erase }
        } else {
            MaskShape::Rect { center, size, expand: 0.0, feather: 0.0, erase }
        }
    }

    /// What it is, for lists.
    pub fn kind_name(&self) -> &'static str {
        match self {
            MaskShape::Rect { .. } => "Rectangle",
            MaskShape::Ellipse { .. } => "Ellipse",
            MaskShape::Stroke { erase: true, .. } => "Eraser stroke",
            MaskShape::Stroke { .. } => "Brush stroke",
            MaskShape::Path { .. } => "Path",
            MaskShape::Bitmap { .. } => "Pixels",
            MaskShape::Matte { .. } => "Rotoscoped",
        }
    }

    /// A path's outline at clip time `t`, flattened: points in drawing fractions (the
    /// curves cut into short straight pieces).
    pub fn path_outline(points: &[PathPoint], closed: bool, t: Time) -> Vec<[f64; 2]> {
        let keys: Vec<PathKey> = points.iter().map(|p| p.at(t)).collect();
        let n = keys.len();
        let mut out = Vec::with_capacity(n * PATH_STEPS + 1);
        let segments = if closed { n } else { n.saturating_sub(1) };
        if let Some(k) = keys.first() {
            out.push(k.at);
        }
        for i in 0..segments {
            let (a, b) = (keys[i], keys[(i + 1) % n]);
            let p0 = a.at;
            let p1 = [a.at[0] + a.handle_out[0], a.at[1] + a.handle_out[1]];
            let p2 = [b.at[0] + b.handle_in[0], b.at[1] + b.handle_in[1]];
            let p3 = b.at;
            let straight = a.handle_out == [0.0, 0.0] && b.handle_in == [0.0, 0.0];
            let steps = if straight { 1 } else { PATH_STEPS };
            for s in 1..=steps {
                let u = s as f64 / steps as f64;
                let v = 1.0 - u;
                let (c0, c1, c2, c3) = (v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u);
                out.push([c0 * p0[0] + c1 * p1[0] + c2 * p2[0] + c3 * p3[0], c0 * p0[1] + c1 * p1[1] + c2 * p2[1] + c3 * p3[1]]);
            }
        }
        out
    }
}

/// Straight pieces each curve of a path is cut into.
const PATH_STEPS: usize = 24;

/// Coverage, one byte per pixel (0 = none, 255 = full), `size` px, stored run-length
/// encoded and in base64 so it stays small in the project file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bitmap {
    pub size: [u32; 2],
    pub data: String,
}

impl Bitmap {
    /// From `size[0] × size[1]` bytes of coverage.
    pub fn encode(size: [u32; 2], coverage: &[u8]) -> Bitmap {
        debug_assert_eq!(coverage.len(), size[0] as usize * size[1] as usize);
        // Runs of (count − 1, value); a run is at most 256 long.
        let mut rle = Vec::with_capacity(coverage.len() / 8 + 2);
        let mut i = 0;
        while i < coverage.len() {
            let v = coverage[i];
            let mut n = 1;
            while n < 256 && i + n < coverage.len() && coverage[i + n] == v {
                n += 1;
            }
            rle.push((n - 1) as u8);
            rle.push(v);
            i += n;
        }
        Bitmap { size, data: base64_encode(&rle) }
    }

    /// Its coverage bytes, or `None` if the data is damaged.
    pub fn decode(&self) -> Option<Vec<u8>> {
        let rle = base64_decode(&self.data)?;
        let want = self.size[0] as usize * self.size[1] as usize;
        let mut out = Vec::with_capacity(want);
        for run in rle.as_chunks::<2>().0 {
            out.extend(std::iter::repeat_n(run[1], run[0] as usize + 1));
        }
        (out.len() == want).then_some(out)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---- parameters ----

/// Where the mask sits: an offset from where it was drawn, as a fraction of the layer.
pub const CENTER: &str = "center";
/// Its size, × as drawn.
pub const SCALE: &str = "scale";
pub const ROTATION: &str = "rotation";
/// How far its edge fades, in the layer's own pixels.
pub const SOFTNESS: &str = "softness";
/// How strongly it applies: 1 = fully, 0 = not at all.
pub const HARSHNESS: &str = "harshness";

/// The id of one of mask `mask`'s parameters in its clip's params.
pub fn param_id(mask: u64, name: &str) -> String {
    format!("mask.{mask}.{name}")
}

/// The mask a clip parameter id belongs to (`mask.7.center` → 7).
pub fn mask_of_param(id: &str) -> Option<u64> {
    id.strip_prefix("mask.")?.split('.').next()?.parse().ok()
}

/// Mask `mask`'s keyframable parameters (ids from [`param_id`]).
pub fn params(mask: u64) -> Vec<ParamSchema> {
    vec![
        ParamSchema::new(&param_id(mask, CENTER), Value::Vec2([0.0, 0.0]), Unit::SourceFraction),
        ParamSchema::new(&param_id(mask, SCALE), Value::Float(1.0), Unit::None).range(0.05, 4.0),
        ParamSchema::new(&param_id(mask, ROTATION), Value::Float(0.0), Unit::Degrees).range(-180.0, 180.0),
        ParamSchema::new(&param_id(mask, SOFTNESS), Value::Float(0.0), Unit::None).range(0.0, 200.0),
        ParamSchema::new(&param_id(mask, HARSHNESS), Value::Float(1.0), Unit::None).range(0.0, 1.0),
    ]
}

/// Mask values at one instant.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MaskValues {
    pub center: [f64; 2],
    pub scale: f64,
    pub rotation: f64,
    pub softness: f64,
    pub harshness: f64,
}

impl MaskValues {
    pub fn eval(item: &crate::Item, mask: u64, ctx: &oa_params::EvalContext) -> MaskValues {
        let v = item.params.eval(&params(mask), None, ctx);
        MaskValues {
            center: v.vec2(&param_id(mask, CENTER)),
            scale: v.float(&param_id(mask, SCALE)).max(0.001),
            rotation: v.float(&param_id(mask, ROTATION)),
            softness: v.float(&param_id(mask, SOFTNESS)).max(0.0),
            harshness: v.float(&param_id(mask, HARSHNESS)).clamp(0.0, 1.0),
        }
    }

    /// Layer fraction (as drawn) → layer fraction (where it is now), for a layer of
    /// `native` px: scaled and turned about where the mask is centered.
    pub fn place(&self, p: [f64; 2], native: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let q = [(p[0] - 0.5) * native[0] * self.scale, (p[1] - 0.5) * native[1] * self.scale];
        let r = [q[0] * c - q[1] * s, q[0] * s + q[1] * c];
        [r[0] / native[0] + 0.5 + self.center[0], r[1] / native[1] + 0.5 + self.center[1]]
    }

    /// The inverse of [`MaskValues::place`]: where on the drawing a layer point is.
    pub fn unplace(&self, p: [f64; 2], native: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let q = [(p[0] - 0.5 - self.center[0]) * native[0], (p[1] - 0.5 - self.center[1]) * native[1]];
        let r = [q[0] * c + q[1] * s, -q[0] * s + q[1] * c];
        [r[0] / (native[0] * self.scale) + 0.5, r[1] / (native[1] * self.scale) + 0.5]
    }
}

/// A clip property's value inside a mask is stored beside it under this suffix.
pub const ON_MASK_SUFFIX: &str = "#mask";

/// The id holding `param`'s value inside the mask.
pub fn on_mask(param: &str) -> String {
    format!("{param}{ON_MASK_SUFFIX}")
}

/// The clip properties that can take their own value inside a mask.
pub const MASKABLE: [&str; 5] = [crate::schema::OPACITY, crate::schema::POSITION, crate::schema::SCALE, crate::schema::ROTATION, crate::schema::SQUASH];

/// Which mask a clip's "… on mask" properties use (in the clip's params, not
/// animated): a mask's id, or [`ALL`] for every mask on the clip together.
pub const PROPS_USE: &str = "mask.props";

/// On an effect (in its params, not animated): which mask it runs inside — a mask's id,
/// [`ALL`] for every mask on the clip together, or 0 (or nothing) for the whole clip.
pub const EFFECT_USE: &str = "mask.use";
/// On an effect: run outside the mask instead.
pub const EFFECT_INVERT: &str = "mask.invert";
/// "Every mask on the clip" (see [`PROPS_USE`], [`EFFECT_USE`]).
pub const ALL: f64 = -1.0;

/// Which masks of `masks` a choice ([`PROPS_USE`], [`EFFECT_USE`]) means: the enabled
/// ones it names (empty for none).
pub fn chosen(masks: &[Mask], choice: f64) -> Vec<&Mask> {
    masks.iter().filter(|m| m.enabled && (choice == ALL || (choice > 0.0 && m.id == choice as u64))).collect()
}

// ---- rasterizing ----

impl Mask {
    /// What was drawn, as coverage (0..=255) over `size` px of the layer — row by row,
    /// top first. Erasing shapes take away; the rest add (as a union: `a + b − ab`).
    pub fn rasterize(&self, size: [u32; 2], t: Time) -> Vec<u8> {
        let (w, h) = (size[0].max(1) as usize, size[1].max(1) as usize);
        let mut m = vec![0f32; w * h];
        // Drawing coordinates → pixel coordinates (with the frame's stretch).
        let f = self.frame;
        let px = |p: [f64; 2]| [((p[0] - 0.5) * f[0] + 0.5) * w as f64, ((p[1] - 0.5) * f[1] + 0.5) * h as f64];
        // Expand and feather in pixels (they're shares of the layer's height).
        let unit = f[1] * h as f64;
        let mut layer = vec![0f32; w * h];
        for shape in &self.shapes {
            let (expand, feather) = shape.edge();
            let (expand, feather) = (expand * unit, feather.max(0.0) * unit);
            // How wide the edge's ramp is: the feather, or a pixel of anti-aliasing.
            let ramp = feather.max(1.0);
            // This shape's own coverage, in a box (so a stroke's segments don't add up).
            let (x0, y0, x1, y1) = match shape {
                MaskShape::Rect { center, size, .. } | MaskShape::Ellipse { center, size, .. } => {
                    let c = px(*center);
                    let half = [size[0].abs() * f[0] * w as f64 / 2.0, size[1].abs() * f[1] * h as f64 / 2.0];
                    let ellipse = matches!(shape, MaskShape::Ellipse { .. });
                    let reach = expand.max(0.0) + ramp + 1.0;
                    let bx = bbox([c[0] - half[0] - reach, c[1] - half[1] - reach, c[0] + half[0] + reach, c[1] + half[1] + reach], w, h);
                    for y in bx.1..bx.3 {
                        for x in bx.0..bx.2 {
                            let p = [x as f64 + 0.5 - c[0], y as f64 + 0.5 - c[1]];
                            let d = if ellipse { ellipse_distance(p, half) } else { rect_distance(p, half) };
                            layer[y * w + x] = ((expand - d) / ramp + 0.5).clamp(0.0, 1.0) as f32;
                        }
                    }
                    bx
                }
                MaskShape::Path { points, closed, .. } => {
                    if !closed || points.len() < 3 {
                        continue;
                    }
                    let outline: Vec<[f64; 2]> = MaskShape::path_outline(points, true, t).into_iter().map(px).collect();
                    let bx = fill_polygon(&outline, w, h, &mut layer);
                    if expand != 0.0 || feather > 0.0 {
                        soften(&mut layer, w, h, expand, ramp);
                        (0, 0, w, h)
                    } else {
                        bx
                    }
                }
                MaskShape::Stroke { points, radius, softness, .. } => {
                    let r = (radius * f[1] * h as f64 + expand).max(0.5);
                    let soft = (softness.clamp(0.0, 1.0) * r).max(feather).max(0.5);
                    let pts: Vec<[f64; 2]> = points.iter().map(|p| px([p[0] as f64, p[1] as f64])).collect();
                    let Some(first) = pts.first() else { continue };
                    let (mut lo, mut hi) = (*first, *first);
                    for p in &pts {
                        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                    }
                    let bx = bbox([lo[0] - r - 1.0, lo[1] - r - 1.0, hi[0] + r + 1.0, hi[1] + r + 1.0], w, h);
                    let segments: Vec<([f64; 2], [f64; 2])> = if pts.len() == 1 { vec![(pts[0], pts[0])] } else { pts.windows(2).map(|s| (s[0], s[1])).collect() };
                    for (a, b) in segments {
                        let sb = bbox([a[0].min(b[0]) - r - 1.0, a[1].min(b[1]) - r - 1.0, a[0].max(b[0]) + r + 1.0, a[1].max(b[1]) + r + 1.0], w, h);
                        for y in sb.1..sb.3 {
                            for x in sb.0..sb.2 {
                                let d = segment_distance([x as f64 + 0.5, y as f64 + 0.5], a, b);
                                // Full inside r − soft, fading to nothing at r.
                                let c = ((r - d) / soft).clamp(0.0, 1.0) as f32;
                                let slot = &mut layer[y * w + x];
                                *slot = slot.max(c);
                            }
                        }
                    }
                    bx
                }
                MaskShape::Bitmap { .. } | MaskShape::Matte { .. } => {
                    let bitmap = match shape {
                        MaskShape::Bitmap { bitmap, .. } => bitmap,
                        MaskShape::Matte { frames, .. } => match matte_at(frames, t) {
                            Some(f) => &f.bitmap,
                            None => continue,
                        },
                        _ => unreachable!(),
                    };
                    let Some(bytes) = bitmap.decode() else { continue };
                    let bx = draw_bitmap(&bytes, bitmap.size, px([0.0, 0.0]), px([1.0, 1.0]), w, h, &mut layer);
                    if expand != 0.0 || feather > 0.0 {
                        soften(&mut layer, w, h, expand, ramp);
                        (0, 0, w, h)
                    } else {
                        bx
                    }
                }
            };
            let erase = shape.erases();
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * w + x;
                    let c = std::mem::take(&mut layer[i]);
                    m[i] = if erase { m[i] * (1.0 - c) } else { m[i] + c - m[i] * c };
                }
            }
        }
        m.into_iter().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8).collect()
    }

    /// A fingerprint of what was drawn as it is at clip time `t` (not the name, on/off,
    /// invert or mode): the same drawing at the same size rasterizes the same. A path
    /// that doesn't move hashes the same at every time.
    pub fn content_hash(&self, t: Time) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.frame.map(f64::to_bits).hash(&mut h);
        for s in &self.shapes {
            let (expand, feather) = s.edge();
            (expand.to_bits(), feather.to_bits(), s.erases()).hash(&mut h);
            match s {
                MaskShape::Rect { center, size, .. } => (0u8, center.map(f64::to_bits), size.map(f64::to_bits)).hash(&mut h),
                MaskShape::Ellipse { center, size, .. } => (1u8, center.map(f64::to_bits), size.map(f64::to_bits)).hash(&mut h),
                MaskShape::Stroke { points, radius, softness, .. } => {
                    (2u8, radius.to_bits(), softness.to_bits(), points.len()).hash(&mut h);
                    for p in points {
                        p.map(f32::to_bits).hash(&mut h);
                    }
                }
                MaskShape::Path { points, closed, .. } => {
                    (4u8, closed, points.len()).hash(&mut h);
                    for p in points {
                        let k = p.at(t);
                        (k.at.map(f64::to_bits), k.handle_in.map(f64::to_bits), k.handle_out.map(f64::to_bits)).hash(&mut h);
                    }
                }
                MaskShape::Bitmap { bitmap, .. } => (3u8, bitmap.size, &bitmap.data).hash(&mut h),
                // The frame showing now (the same frame hashes the same all the while it shows).
                MaskShape::Matte { frames, .. } => match matte_at(frames, t) {
                    Some(f) => (5u8, f.t.0, f.bitmap.size, &f.bitmap.data).hash(&mut h),
                    None => 6u8.hash(&mut h),
                },
            }
        }
        h.finish()
    }

    /// The most pixels its drawing can use, when it's only pictures (bitmaps, mattes):
    /// the largest of them — drawn bigger they'd only be stretched, and the GPU stretches
    /// it anyway. `None`: something drawn (a shape, a stroke, a path) wants the layer's
    /// own resolution. A rotoscoped matte is redrawn every frame, so this matters.
    pub fn raster_cap(&self) -> Option<[u32; 2]> {
        let mut cap: Option<[u32; 2]> = None;
        for s in &self.shapes {
            let sizes: Vec<[u32; 2]> = match s {
                MaskShape::Bitmap { bitmap, .. } => vec![bitmap.size],
                MaskShape::Matte { frames, .. } => frames.iter().map(|f| f.bitmap.size).collect(),
                _ => return None,
            };
            for size in sizes {
                let c = cap.get_or_insert([0, 0]);
                *c = [c[0].max(size[0]), c[1].max(size[1])];
            }
        }
        cap
    }

    /// Whether its drawing changes over time (a path with keyframed points, a rotoscoped
    /// matte).
    pub fn animated(&self) -> bool {
        self.shapes.iter().any(|s| match s {
            MaskShape::Path { points, .. } => points.iter().any(PathPoint::animated),
            MaskShape::Matte { frames, .. } => frames.len() > 1,
            _ => false,
        })
    }

    /// For a clip whose clock moved `delta` later (the back half of a split): its path
    /// keys keep their instants.
    pub fn shift_clip_clock(&mut self, delta: Time) {
        for s in &mut self.shapes {
            match s {
                MaskShape::Path { points, .. } => {
                    for k in points.iter_mut().flat_map(|p| &mut p.keys) {
                        k.t -= delta;
                    }
                }
                MaskShape::Matte { frames, .. } => {
                    for f in frames {
                        f.t -= delta;
                    }
                }
                _ => {}
            }
        }
    }

    /// The same drawing on a layer of another shape (`from` → `to`, width/height), its
    /// shapes keeping their proportions: `fit` shows all of it (empty at the sides or
    /// top and bottom), otherwise it covers the layer and its overflow is cut off.
    pub fn refit(&mut self, from: f64, to: f64, fit: bool) {
        if !(from > 0.0 && to > 0.0) || (from - to).abs() < 1e-9 {
            return;
        }
        // In the target's fractions, the drawing's width vs its height.
        let (sx, sy) = if (from > to) == fit { (1.0, to / from) } else { (from / to, 1.0) };
        self.frame = [self.frame[0] * sx, self.frame[1] * sy];
    }
}

/// Draws coverage `bytes` (`size`) stretched from pixel `tl` to `br` into `layer`
/// (`w` × `h`), bilinear with pixel centers on pixel centers. Returns the box it touched.
fn draw_bitmap(bytes: &[u8], size: [u32; 2], tl: [f64; 2], br: [f64; 2], w: usize, h: usize, layer: &mut [f32]) -> (usize, usize, usize, usize) {
    let (bw, bh) = (size[0].max(1) as usize, size[1].max(1) as usize);
    let bx = bbox([tl[0], tl[1], br[0], br[1]], w, h);
    let (sw, sh) = ((br[0] - tl[0]).max(1e-9), (br[1] - tl[1]).max(1e-9));
    for y in bx.1..bx.3 {
        for x in bx.0..bx.2 {
            let u = ((x as f64 + 0.5 - tl[0]) / sw * bw as f64 - 0.5).clamp(0.0, (bw - 1) as f64);
            let v = ((y as f64 + 0.5 - tl[1]) / sh * bh as f64 - 0.5).clamp(0.0, (bh - 1) as f64);
            let (x0, y0) = (u.floor() as usize, v.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(bw - 1), (y0 + 1).min(bh - 1));
            let (fx, fy) = (u - x0 as f64, v - y0 as f64);
            let at = |x: usize, y: usize| bytes[y * bw + x] as f64 / 255.0;
            let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
            let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
            layer[y * w + x] = (top * (1.0 - fy) + bottom * fy) as f32;
        }
    }
    bx
}

fn bbox(b: [f64; 4], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let cl = |v: f64, max: usize| (v.max(0.0) as usize).min(max);
    (cl(b[0].floor(), w), cl(b[1].floor(), h), cl(b[2].ceil(), w), cl(b[3].ceil(), h))
}

/// Signed distance from `p` to an axis-aligned box of half-size `half` at the origin.
fn rect_distance(p: [f64; 2], half: [f64; 2]) -> f64 {
    let d = [p[0].abs() - half[0], p[1].abs() - half[1]];
    let outside = (d[0].max(0.0).powi(2) + d[1].max(0.0).powi(2)).sqrt();
    outside + d[0].max(d[1]).min(0.0)
}

/// Approximate signed distance from `p` to an ellipse of radii `r` at the origin (good
/// to a pixel near the edge, which is all anti-aliasing needs).
fn ellipse_distance(p: [f64; 2], r: [f64; 2]) -> f64 {
    let r = [r[0].max(1e-6), r[1].max(1e-6)];
    let k0 = ((p[0] / r[0]).powi(2) + (p[1] / r[1]).powi(2)).sqrt();
    let k1 = ((p[0] / (r[0] * r[0])).powi(2) + (p[1] / (r[1] * r[1])).powi(2)).sqrt();
    if k1 < 1e-12 {
        return -r[0].min(r[1]);
    }
    k0 * (k0 - 1.0) / k1
}

fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 > 0.0 { ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let d = [ap[0] - ab[0] * t, ap[1] - ab[1] * t];
    (d[0] * d[0] + d[1] * d[1]).sqrt()
}

/// Fills the closed outline `poly` (pixel coordinates; nonzero winding) into `layer`
/// (`w` × `h`), anti-aliased: four rows of samples per pixel, spans cut exactly across.
/// Returns the box it touched.
fn fill_polygon(poly: &[[f64; 2]], w: usize, h: usize, layer: &mut [f32]) -> (usize, usize, usize, usize) {
    const ROWS: usize = 4;
    if poly.len() < 3 {
        return (0, 0, 0, 0);
    }
    let (mut lo, mut hi) = (poly[0], poly[0]);
    for p in poly {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let bx = bbox([lo[0] - 1.0, lo[1] - 1.0, hi[0] + 1.0, hi[1] + 1.0], w, h);
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    let weight = 1.0 / ROWS as f32;
    for y in bx.1..bx.3 {
        let row = &mut layer[y * w..(y + 1) * w];
        for s in 0..ROWS {
            let sy = y as f64 + (s as f64 + 0.5) / ROWS as f64;
            crossings.clear();
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                if (a[1] <= sy) != (b[1] <= sy) {
                    let x = a[0] + (sy - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
                    crossings.push((x, if b[1] > a[1] { 1 } else { -1 }));
                }
            }
            crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding != 0 {
                    add_span(row, pair[0].0, pair[1].0, weight);
                }
            }
        }
        for c in &mut row[bx.0..bx.2] {
            *c = c.min(1.0);
        }
    }
    bx
}

/// Adds `weight` × how much of each pixel lies between `x0` and `x1`.
fn add_span(row: &mut [f32], x0: f64, x1: f64, weight: f32) {
    let w = row.len() as f64;
    let (x0, x1) = (x0.clamp(0.0, w), x1.clamp(0.0, w));
    if x1 <= x0 {
        return;
    }
    let (i0, i1) = (x0.floor() as usize, x1.floor() as usize);
    if i0 == i1 {
        row[i0.min(row.len() - 1)] += (x1 - x0) as f32 * weight;
        return;
    }
    row[i0] += (i0 as f64 + 1.0 - x0) as f32 * weight;
    for c in &mut row[i0 + 1..i1] {
        *c += weight;
    }
    if i1 < row.len() {
        row[i1] += (x1 - i1 as f64) as f32 * weight;
    }
}

/// Grows (or, negative, shrinks) coverage by `expand` px and ramps its edge over `ramp`
/// px, from the exact distance to its outline (half covered is the edge).
fn soften(layer: &mut [f32], w: usize, h: usize, expand: f64, ramp: f64) {
    let inside: Vec<bool> = layer.iter().map(|c| *c >= 0.5).collect();
    let to_inside = distance_to(&inside, w, h, true);
    let to_outside = distance_to(&inside, w, h, false);
    for (i, c) in layer.iter_mut().enumerate() {
        // Signed distance to the edge: negative inside.
        let d = if inside[i] { -(to_outside[i].sqrt() - 0.5) } else { to_inside[i].sqrt() - 0.5 };
        *c = ((expand - d as f64) / ramp + 0.5).clamp(0.0, 1.0) as f32;
    }
}

/// Squared distance from each pixel to the nearest pixel where `mask` is `want`
/// (Felzenszwalb and Huttenlocher's exact transform: columns, then rows).
fn distance_to(mask: &[bool], w: usize, h: usize, want: bool) -> Vec<f32> {
    const FAR: f32 = 1e20;
    let mut grid: Vec<f32> = mask.iter().map(|m| if *m == want { 0.0 } else { FAR }).collect();
    let n = w.max(h);
    let (mut f, mut d, mut v, mut z) = (vec![0f32; n], vec![0f32; n], vec![0usize; n], vec![0f32; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        distance_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&grid[y * w..(y + 1) * w]);
        distance_1d(&f[..w], &mut d[..w], &mut v, &mut z);
        grid[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    grid
}

/// One line of [`distance_to`]: the lower envelope of parabolas.
fn distance_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: this parabola is lower everywhere.
                v[0] = q;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        *out = (q as f32 - p as f32).powi(2) + f[p];
    }
}

// ---- pixel tools ----

/// The pixels connected to `start` whose color is within `tolerance` (0..1, of the
/// largest possible difference) of its color: a magic selection. `rgba` is `size` px
/// of straight-alpha RGBA8; `contiguous` off selects every such pixel anywhere. Returns
/// coverage bytes (255 selected, 0 not).
pub fn magic_select(rgba: &[u8], size: [usize; 2], start: [usize; 2], tolerance: f64, contiguous: bool) -> Vec<u8> {
    let (w, h) = (size[0], size[1]);
    let mut out = vec![0u8; w * h];
    if w == 0 || h == 0 || start[0] >= w || start[1] >= h || rgba.len() < w * h * 4 {
        return out;
    }
    let px = |i: usize| [rgba[i * 4] as i32, rgba[i * 4 + 1] as i32, rgba[i * 4 + 2] as i32, rgba[i * 4 + 3] as i32];
    let seed = px(start[1] * w + start[0]);
    let limit = (tolerance.clamp(0.0, 1.0) * 255.0) as i32;
    let near = |i: usize| {
        let c = px(i);
        (0..4).all(|k| (c[k] - seed[k]).abs() <= limit)
    };
    if !contiguous {
        for (i, slot) in out.iter_mut().enumerate() {
            if near(i) {
                *slot = 255;
            }
        }
        return out;
    }
    flood(&mut out, w, h, start, near);
    out
}

/// The empty area around `start` in `coverage` (`size` px) — bounded by anything at
/// least half covered — as coverage bytes: a fill.
pub fn fill_region(coverage: &[u8], size: [usize; 2], start: [usize; 2]) -> Vec<u8> {
    let (w, h) = (size[0], size[1]);
    let mut out = vec![0u8; w * h];
    if w == 0 || h == 0 || start[0] >= w || start[1] >= h || coverage.len() < w * h {
        return out;
    }
    let inside = coverage[start[1] * w + start[0]] >= 128;
    flood(&mut out, w, h, start, |i| (coverage[i] >= 128) == inside);
    out
}

/// Marks (255) every pixel 4-connected to `start` for which `take` holds.
fn flood(out: &mut [u8], w: usize, h: usize, start: [usize; 2], take: impl Fn(usize) -> bool) {
    let mut stack = vec![start[1] * w + start[0]];
    while let Some(i) = stack.pop() {
        if out[i] != 0 || !take(i) {
            continue;
        }
        out[i] = 255;
        let (x, y) = (i % w, i / w);
        if x > 0 {
            stack.push(i - 1);
        }
        if x + 1 < w {
            stack.push(i + 1);
        }
        if y > 0 {
            stack.push(i - w);
        }
        if y + 1 < h {
            stack.push(i + w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(shapes: Vec<MaskShape>) -> Mask {
        Mask { id: 1, name: "Mask 1".into(), enabled: true, invert: false, mode: MaskMode::Add, shapes, frame: [1.0, 1.0] }
    }

    /// A rotoscoped matte shows each frame from its time until the next, hashes by the
    /// frame showing, keeps its instants when the clip's clock moves, and saves.
    #[test]
    fn mattes_change_frame_by_frame() {
        let left = Bitmap::encode([2, 1], &[255, 0]);
        let right = Bitmap::encode([2, 1], &[0, 255]);
        let frames = vec![MatteFrame { t: Time::from_seconds(1), bitmap: left }, MatteFrame { t: Time::from_seconds(2), bitmap: right }];
        let mut m = mask(vec![MaskShape::Matte { frames, expand: 0.0, feather: 0.0, erase: false }]);
        let at = |m: &Mask, s: f64| {
            let c = m.rasterize([4, 1], Time::from_seconds_f64(s));
            (c[0], c[3])
        };
        assert_eq!(at(&m, 0.5), (255, 0), "before the first frame: the first");
        assert_eq!(at(&m, 1.5), (255, 0));
        assert_eq!(at(&m, 2.0), (0, 255), "the second from its own time on");
        assert_eq!(at(&m, 9.0), (0, 255));
        assert!(m.animated());
        assert_eq!(m.raster_cap(), Some([2, 1]), "only pictures: drawn at their own size");
        assert_eq!(mask(vec![MaskShape::boxed(false, [0.1, 0.1], [0.5, 0.5], false)]).raster_cap(), None, "a shape wants the layer's resolution");
        assert_eq!(m.content_hash(Time::from_seconds_f64(1.2)), m.content_hash(Time::from_seconds_f64(1.8)), "one frame, one hash");
        assert_ne!(m.content_hash(Time::from_seconds_f64(1.5)), m.content_hash(Time::from_seconds_f64(2.5)));
        m.shift_clip_clock(Time::from_seconds(1));
        assert_eq!(at(&m, 1.0), (0, 255), "a clock one second later: the second frame at 1 s");
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Mask>(&json).unwrap(), m);
    }

    #[test]
    fn bitmaps_round_trip() {
        let mut bytes = vec![0u8; 300 * 7];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = if i % 97 < 40 { 255 } else { (i % 5) as u8 };
        }
        let b = Bitmap::encode([300, 7], &bytes);
        assert_eq!(b.decode().unwrap(), bytes);
        // A mostly empty mask stays small.
        let empty = Bitmap::encode([1000, 1000], &vec![0u8; 1_000_000]);
        assert!(empty.data.len() < 12_000, "{}", empty.data.len());
        assert_eq!(Bitmap { size: [2, 2], data: "!!".into() }.decode(), None);
    }

    #[test]
    fn masks_save_and_load() {
        let m = Mask {
            id: 4,
            name: "Sky".into(),
            enabled: false,
            invert: true,
            mode: MaskMode::Intersect,
            shapes: vec![
                MaskShape::Rect { center: [0.5, 0.5], size: [0.2, 0.3], expand: 0.0, feather: 0.0, erase: false },
                MaskShape::Ellipse { center: [0.1, 0.9], size: [0.2, 0.2], expand: 0.0, feather: 0.0, erase: true },
                MaskShape::Stroke { points: vec![[0.1, 0.2], [0.3, 0.4]], radius: 0.02, softness: 0.5, expand: 0.0, feather: 0.0, erase: false },
                MaskShape::Bitmap { bitmap: Bitmap::encode([3, 2], &[0, 255, 255, 0, 0, 9]), expand: 0.0, feather: 0.0, erase: true },
            ],
            frame: [1.0, 0.5],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Mask>(&json).unwrap(), m, "{json}");
        // Defaults stay out of the file and come back.
        let plain = Mask { enabled: true, invert: false, mode: MaskMode::Add, frame: [1.0, 1.0], shapes: Vec::new(), ..m };
        let json = serde_json::to_string(&plain).unwrap();
        assert_eq!(json, r#"{"id":4,"name":"Sky"}"#);
        assert_eq!(serde_json::from_str::<Mask>(&json).unwrap(), plain);
    }

    #[test]
    fn shapes_add_and_erase_in_order() {
        // Edges at 24.75 and 75.25 px: the pixels they cross are partly covered.
        let rect = MaskShape::Rect { center: [0.5, 0.5], size: [0.505, 0.505], expand: 0.0, feather: 0.0, erase: false };
        let hole = MaskShape::Ellipse { center: [0.5, 0.5], size: [0.2, 0.2], expand: 0.0, feather: 0.0, erase: true };
        let m = mask(vec![rect, hole]).rasterize([100, 100], Time::ZERO);
        let at = |x: usize, y: usize| m[y * 100 + x];
        assert_eq!(at(30, 30), 255, "inside the rectangle");
        assert_eq!(at(10, 10), 0, "outside it");
        assert_eq!(at(50, 50), 0, "the ellipse erased the middle");
        assert!(at(24, 50) > 0 && at(24, 50) < 255, "anti-aliased edge: {}", at(24, 50));
    }

    fn square_path(size: f64, keys: &[(f64, f64)]) -> MaskShape {
        // A square of `size` around (0.5, 0.5), its first corner keyed at each (seconds, x).
        let h = size / 2.0;
        let mut points: Vec<PathPoint> = [[0.5 - h, 0.5 - h], [0.5 + h, 0.5 - h], [0.5 + h, 0.5 + h], [0.5 - h, 0.5 + h]].into_iter().map(PathPoint::new).collect();
        for &(s, x) in keys {
            let t = Time::from_seconds_f64(s);
            let k = PathKey { at: [x, 0.5 - h], ..points[0].at(t) };
            points[0].set(t, k, true, Time(1));
        }
        MaskShape::Path { points, closed: true, expand: 0.0, feather: 0.0, erase: false }
    }

    #[test]
    fn paths_fill_and_animate_point_by_point() {
        let m = mask(vec![square_path(0.5, &[])]).rasterize([100, 100], Time::ZERO);
        assert_eq!(m[50 * 100 + 50], 255);
        assert_eq!(m[50 * 100 + 10], 0);
        let covered = m.iter().map(|c| *c as f64 / 255.0).sum::<f64>();
        assert!((covered - 2500.0).abs() < 30.0, "a 50 × 50 square: {covered}");
        // An open path isn't filled.
        let MaskShape::Path { points, .. } = square_path(0.5, &[]) else { unreachable!() };
        let open = mask(vec![MaskShape::Path { points, closed: false, expand: 0.0, feather: 0.0, erase: false }]).rasterize([100, 100], Time::ZERO);
        assert!(open.iter().all(|c| *c == 0));
        // One corner keyed from 0.25 to 0.05 over a second: only that corner moves.
        let moving = mask(vec![square_path(0.5, &[(0.0, 0.25), (1.0, 0.05)])]);
        assert!(moving.animated());
        let (start, end) = (moving.rasterize([100, 100], Time::ZERO), moving.rasterize([100, 100], Time::from_seconds(1)));
        assert_eq!(start[27 * 100 + 12], 0);
        assert!(end[27 * 100 + 12] > 200, "the corner went left: {}", end[27 * 100 + 12]);
        assert_eq!(start[70 * 100 + 12], end[70 * 100 + 12], "the other corners stay");
        assert_ne!(moving.content_hash(Time::ZERO), moving.content_hash(Time::from_seconds(1)));
        let still = mask(vec![square_path(0.5, &[])]);
        assert_eq!(still.content_hash(Time::ZERO), still.content_hash(Time::from_seconds(3)));
    }

    #[test]
    fn expanding_and_feathering_shapes() {
        let rect = |expand: f64, feather: f64| MaskShape::Rect { center: [0.5, 0.5], size: [0.4, 0.4], expand, feather, erase: false };
        let row = |shape: MaskShape| {
            let m = mask(vec![shape]).rasterize([100, 100], Time::ZERO);
            (0..100).map(|x| m[50 * 100 + x]).collect::<Vec<u8>>()
        };
        let plain = row(rect(0.0, 0.0));
        let grown = row(rect(0.1, 0.0));
        let shrunk = row(rect(-0.1, 0.0));
        let width = |r: &[u8]| r.iter().filter(|c| **c >= 128).count();
        assert_eq!(width(&plain), 40);
        assert_eq!(width(&grown), 60, "10 px more on each side");
        assert_eq!(width(&shrunk), 20);
        let soft = row(rect(0.0, 0.2));
        assert!(soft[30] > 90 && soft[30] < 165, "half way on the edge: {}", soft[30]);
        assert!(soft[25] > 10 && soft[25] < 128 && soft[35] > 128 && soft[35] < 250, "a ramp across it: {} {}", soft[25], soft[35]);
        // A path grows and softens the same way (from its distance field).
        let widen = |expand: f64, feather: f64| {
            let MaskShape::Path { points, closed, .. } = square_path(0.4, &[]) else { unreachable!() };
            row(MaskShape::Path { points, closed, expand, feather, erase: false })
        };
        assert_eq!(width(&widen(0.0, 0.0)), 40);
        let w = width(&widen(0.1, 0.0));
        assert!((59..=61).contains(&w), "{w}");
        let soft = widen(0.0, 0.2);
        assert!(soft[30] > 70 && soft[30] < 185, "{}", soft[30]);
    }

    #[test]
    fn keys_per_point() {
        let mut p = PathPoint::new([0.1, 0.1]);
        let key = |x: f64| PathKey { at: [x, 0.1], ..Default::default() };
        // Not animated: the one key changes wherever the playhead is.
        p.set(Time::from_seconds(2), key(0.2), false, Time(1));
        assert_eq!(p.keys.len(), 1);
        assert_eq!(p.at(Time::ZERO).at, [0.2, 0.1]);
        p.set(Time::from_seconds(2), key(0.6), true, Time(1));
        assert_eq!(p.keys.len(), 2);
        assert_eq!(p.at(Time::from_seconds(1)).at[0], 0.4, "eased half way");
        assert_eq!(p.at(Time::from_seconds(9)).at[0], 0.6, "held after");
        // Near a key: that key changes.
        p.set(Time::from_seconds(2), key(0.7), false, Time(1));
        assert_eq!(p.keys.len(), 2);
        assert!(p.remove_key(Time::from_seconds(2), Time(1)));
        assert!(!p.remove_key(Time::ZERO, Time(1)), "the last key stays");
        // The back half of a split keeps the instants.
        let mut m = mask(vec![square_path(0.4, &[(0.0, 0.3), (2.0, 0.1)])]);
        let before = m.rasterize([50, 50], Time::from_seconds(1));
        m.shift_clip_clock(Time::from_seconds(1));
        assert_eq!(m.rasterize([50, 50], Time::ZERO), before);
    }

    #[test]
    fn strokes_cover_their_path() {
        let s = MaskShape::Stroke { points: vec![[0.1, 0.5], [0.9, 0.5]], radius: 0.05, softness: 0.0, expand: 0.0, feather: 0.0, erase: false };
        let m = mask(vec![s]).rasterize([200, 100], Time::ZERO);
        assert_eq!(m[50 * 200 + 100], 255);
        assert_eq!(m[20 * 200 + 100], 0);
        // Overlapping segments don't double up past full.
        let s = MaskShape::Stroke { points: vec![[0.5, 0.5], [0.5, 0.5], [0.5, 0.5]], radius: 0.1, softness: 1.0, expand: 0.0, feather: 0.0, erase: false };
        let m = mask(vec![s]).rasterize([100, 100], Time::ZERO);
        assert!(m[50 * 100 + 50] > 200 && m[50 * 100 + 58] < 60);
    }

    #[test]
    fn placing_undoes_unplacing() {
        let v = MaskValues { center: [0.1, -0.2], scale: 1.7, rotation: 33.0, softness: 0.0, harshness: 1.0 };
        let native = [1920.0, 1080.0];
        let p = [0.3, 0.8];
        let q = v.unplace(v.place(p, native), native);
        assert!((q[0] - p[0]).abs() < 1e-9 && (q[1] - p[1]).abs() < 1e-9);
    }

    #[test]
    fn refitting_keeps_proportions() {
        // A 16:9 drawing on a 9:16 clip: fit leaves it full width, a third as tall.
        let mut m = mask(vec![]);
        m.refit(16.0 / 9.0, 9.0 / 16.0, true);
        assert!((m.frame[0] - 1.0).abs() < 1e-9 && (m.frame[1] - (81.0 / 256.0)).abs() < 1e-9);
        let mut m = mask(vec![]);
        m.refit(16.0 / 9.0, 9.0 / 16.0, false);
        assert!((m.frame[0] - 256.0 / 81.0).abs() < 1e-9 && (m.frame[1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn magic_select_and_fill() {
        // Left half red, right half blue, one blue speck on the left.
        let (w, h) = (10, 4);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let blue = x >= 5 || (x == 1 && y == 1);
                rgba[i..i + 4].copy_from_slice(if blue { &[0, 0, 255, 255] } else { &[255, 0, 0, 255] });
            }
        }
        let sel = magic_select(&rgba, [w, h], [7, 2], 0.1, true);
        assert_eq!(sel.iter().filter(|c| **c == 255).count(), 20, "the right half, not the speck");
        let all = magic_select(&rgba, [w, h], [7, 2], 0.1, false);
        assert_eq!(all.iter().filter(|c| **c == 255).count(), 21);
        let filled = fill_region(&sel, [w, h], [0, 0]);
        assert_eq!(filled.iter().filter(|c| **c == 255).count(), 20, "the empty left half");
    }
}

#[cfg(test)]
mod matte_speed {
    use super::*;

    #[test]
    #[ignore]
    fn a_soft_matte_frame() {
        let (w, h) = (768usize, 432usize);
        let cov: Vec<u8> = (0..w * h).map(|i| if (i % w) > w / 3 && (i % w) < 2 * w / 3 && (i / w) > h / 4 { 255 } else { 0 }).collect();
        let frames = vec![MatteFrame { t: Time::ZERO, bitmap: Bitmap::encode([w as u32, h as u32], &cov) }];
        for feather in [0.0, 2.0 / 1080.0] {
            let m = Mask { id: 1, name: String::new(), enabled: true, invert: false, mode: MaskMode::Add, shapes: vec![MaskShape::Matte { frames: frames.clone(), expand: 0.0, feather, erase: false }], frame: [1.0, 1.0] };
            // At the size the planner now draws it: the matte's own.
            let size = m.raster_cap().unwrap();
            let t0 = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(m.rasterize(size, Time::ZERO));
            }
            eprintln!("feather {feather:.4}: {:.1} ms a frame", t0.elapsed().as_secs_f64() * 100.0);
        }
    }
}
