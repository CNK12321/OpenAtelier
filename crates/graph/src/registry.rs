//! The **plugin-facing** description of effects.
//!
//! Plugins (and built-ins, which use the exact same path) describe *what* an effect is
//! with an [`EffectDescriptor`]. The planner lowers descriptors into internal
//! [`NodeOp`](crate::NodeOp)s. Plugins never see `NodeOp`, so the internal graph and
//! optimizer can change freely without breaking plugins; only [`PLUGIN_API_VERSION`]
//! is a compatibility promise.

use oa_params::{Evaluated, Gradient, ParamId, ParamSchema, Unit, Value};
use crate::Rect;
use oa_time::Time;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const PLUGIN_API_VERSION: u32 = 1;

/// Floats appended after an effect's packed params: its clock (visibility, progress,
/// seconds), which shaders read through `visibility()`, `progress()`, `clip_seconds()`.
pub const CLOCK_SLOTS: usize = 3;

/// The clock for an effect that doesn't vary over time (a stable cache key).
pub const STILL_CLOCK: [f32; CLOCK_SLOTS] = [1.0, 0.0, 0.0];

#[derive(Clone, Debug, PartialEq)]
pub enum EffectKind {
    /// Per-pixel color function with no neighborhood. Fusible.
    PointOp,
    /// Coordinate remap (`uv → uv`). Fusible with transforms when `fusible`.
    UvWarp,
    /// Reads a neighborhood. `expand` names a `LayerPixels` param by which the output
    /// bounds grow (e.g. blur radius).
    Spatial { expand: Option<ParamId> },
    /// Needs other frames of its input.
    Temporal { frames_before: u32, frames_after: u32 },
    /// Runs on the CPU; the optimizer groups these to minimize GPU↔CPU transfers.
    Cpu,
    /// Mixes two inputs over a transition (dissolve, wipe, push…).
    Transition,
    /// Moves, scales, rotates or fades the whole layer (fly in, camera shake): computed
    /// by [`EffectDescriptor::motion`] and folded into the layer's transform, so the
    /// layer can travel anywhere on the canvas — a pixel shader only sees the pixels
    /// inside the layer.
    Motion,
    /// Text only, **per letter**: moves, scales, rotates and colors each glyph (wave,
    /// staggered rise, typewriter). Runs in the text pass's vertex stage, once per
    /// glyph. `expand` names a param, in ems, by which letters may leave the text box.
    Glyph { expand: Option<ParamId> },
    /// Text only, **per pixel** inside the text pass: sees each pixel's glyph, its
    /// distance to the outline and its place in the box (glow).
    GlyphPixel,
    /// Text only, **behind the letters**: the text pass draws a quad around the whole
    /// text, each line and each word, and runs this for their pixels (a rounded
    /// background box). `fn <entry>(b: TextBox, base: u32) -> vec4f` — straight color;
    /// `b.kind` says which of the three the pixel belongs to (0 text, 1 line, 2 word),
    /// `b.pos`/`b.center`/`b.size` are in output px, `b.em` is the text size in px.
    TextBox,
    /// A sound effect: on clips like any other effect (same list, roles and params), but
    /// run by the audio mixer on the clip's samples, never rendered. Its `shader` is a
    /// sound shader (`oa_audio::shader`) — or `None` for one native to the host.
    Sound,
}

impl EffectKind {
    /// Effects that only mean something on text layers.
    pub fn text_only(&self) -> bool {
        matches!(self, EffectKind::Glyph { .. } | EffectKind::GlyphPixel | EffectKind::TextBox)
    }
}

/// What an effect is for, which decides where the UI offers it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EffectUsage {
    /// Runs for the whole clip (color, blur, camera shake, wiggle, masks).
    Passive,
    /// A clip transition, placed as the clip's intro or outro (fade in/out, fly in/out).
    /// Written against `visibility()` (shaders) / [`MotionInput::visibility`], so the
    /// same effect plays forwards as an intro and backwards as an outro.
    InOut,
    /// Mixes two clips across a cut (`EffectKind::Transition`: dissolve, wipe, push).
    Cut,
}

/// What a motion effect sees at one instant.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MotionInput {
    /// 0 → 1 over an intro, 1 → 0 over an outro, 1 when passive.
    pub visibility: f64,
    pub progress: f64,
    pub seconds: f64,
    /// Canvas size in px (the output of the move is in canvas px).
    pub canvas: [f64; 2],
    /// Stable per effect instance, for procedural motion (shake).
    pub seed: u64,
    /// Running as an outro (the clip is leaving), so a direction means "away to".
    pub leaving: bool,
}

/// A change to a layer's placement, applied around its anchor after its own transform.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Motion {
    /// Canvas px.
    pub offset: [f64; 2],
    pub scale: f64,
    /// Degrees, clockwise.
    pub rotation: f64,
    /// Multiplies the layer's opacity.
    pub opacity: f64,
}

impl Motion {
    pub const NONE: Motion = Motion { offset: [0.0, 0.0], scale: 1.0, rotation: 0.0, opacity: 1.0 };

    /// `self` then `next`.
    pub fn then(self, next: Motion) -> Motion {
        Motion {
            offset: [self.offset[0] + next.offset[0], self.offset[1] + next.offset[1]],
            scale: self.scale * next.scale,
            rotation: self.rotation + next.rotation,
            opacity: self.opacity * next.opacity,
        }
    }
}

/// A motion effect's function: deterministic in its inputs, so frames stay cacheable
/// and render identically in preview and export.
pub type MotionFn = fn(&Evaluated, &MotionInput) -> Motion;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Statefulness {
    /// Output depends only on (params, time, inputs, quality). Cacheable.
    Pure,
    /// Depends on previously processed frames (feedback, simulation). Never cached;
    /// rendering at time t starts `preroll` earlier to warm the state up.
    Stateful { preroll: Time },
}

/// The color encoding an effect's math expects. Effects in different spaces are never
/// fused together; the executor converts at the boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WorkingSpace {
    /// Scene-linear, premultiplied (default: correct for light, blur, glow).
    Linear,
    /// Display-referred sRGB-like 0..1 values (classic "look" effects, some blends).
    Display,
}

#[derive(Clone, Debug)]
pub struct EffectDescriptor {
    pub type_id: Arc<str>,
    pub version: u32,
    pub api_version: u32,
    pub name: String,
    pub kind: EffectKind,
    pub state: Statefulness,
    pub space: WorkingSpace,
    /// The plugin promises it may be fused with neighbors of the same kind and space.
    pub fusible: bool,
    /// Opaque input stays opaque (enables occlusion culling below it).
    pub preserves_opacity: bool,
    /// The shader reads `progress()` or `clip_seconds()` even as a passive effect (it
    /// animates on its own). Otherwise a passive effect gets a still clock, so its
    /// output — and cache key — only change when its params do. In/out effects always
    /// get the live clock.
    pub time_varying: bool,
    /// Where it's offered: passive effect, clip intro/outro, or cut transition.
    pub usage: EffectUsage,
    /// For `EffectKind::Motion`: how the layer moves.
    pub motion: Option<MotionFn>,
    /// How many passes the shader runs, from its packed uniforms (params, then the
    /// clock) — for effects whose work grows with a setting (Glow's jump flood, one pass
    /// per halving of its radius). `None`: the shader's fixed `passes`.
    pub pass_count: Option<fn(&[f32]) -> u32>,
    /// For a pass (by index) that may run at a fraction of the output's resolution: by
    /// how much its target is divided (1 = full size). The shader finds its cell from
    /// `pos - out_origin()` and reads a smaller input with `textureLoad`.
    pub pass_divisor: Option<fn(&[f32], u32) -> u32>,
    pub params: Vec<ParamSchema>,
    /// WGSL implementing the effect (see `oa-gpu` shader contract). `None` means the
    /// effect has no GPU implementation on this host (it renders as a passthrough and
    /// is reported).
    ///
    /// * `PointOp`: `fn <entry>(c: vec4f, base: u32) -> vec4f` — straight (not
    ///   premultiplied) color in the declared working space; params via `u(base + i)`.
    ///   `layer_pos()` is the pixel's layer position (for gradients and sweeps).
    /// * Any stage: a `Gradient` param packs 32 floats; `oa_gradient(base, pos, lo, size)`
    ///   gives its straight color at `pos` across the box at `lo` of `size`.
    /// * `Spatial`/`UvWarp`: `fn <entry>(pos: vec2f, base: u32) -> vec4f` — `pos` is in
    ///   layer pixels; read the input with `sample_input(pos)` (premultiplied);
    ///   `pass_index()` gives the current pass for multi-pass effects.
    /// * `Transition`: `fn <entry>(pos: vec2f, progress: f32, base: u32) -> vec4f` — `pos`
    ///   in canvas pixels; read the outgoing picture with `sample_a(pos)` and the incoming
    ///   one with `sample_b(pos)` (both premultiplied); `progress` runs 0 → 1.
    /// * `Glyph`: `fn <entry>(g: Glyph, base: u32) -> Glyph` — change `g.offset` (px),
    ///   `g.scale`, `g.rotation` (degrees) and `g.color` (straight RGBA multiplier; alpha
    ///   is the letter's opacity). Reads `g.index`/`g.count`, `g.line`, `g.word`,
    ///   `g.center` and `g.em`; `letter_progress(g, stagger)` staggers `visibility()`.
    /// * `GlyphPixel`: `fn <entry>(c: vec4f, p: TextPixel, base: u32) -> vec4f` — `c` is
    ///   the pixel's straight color (fill over outline, alpha = coverage); `p.pos` layer
    ///   px, `p.box_size`, `p.dist` px to the outline (positive inside), `p.index`.
    pub shader: Option<EffectShader>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectShader {
    /// Function name inside `source`; must be unique across effects (prefix it).
    pub entry: String,
    pub source: Arc<str>,
    /// Number of passes (e.g. 2 for a separable blur).
    pub passes: u32,
}

impl EffectDescriptor {
    /// Packs evaluated params in schema order into a flat uniform block. `LayerPixels`
    /// values are multiplied by `raster_scale`, so an effect rendered at reduced
    /// resolution covers the same visual extent.
    pub fn pack_uniforms(&self, values: &Evaluated, raster_scale: f64) -> Vec<f32> {
        let mut out = Vec::new();
        for schema in &self.params {
            let k = if schema.unit == Unit::LayerPixels { raster_scale } else { 1.0 };
            match values.get(schema.id.as_str()) {
                Some(Value::Float(x)) => out.push((x * k) as f32),
                Some(Value::Int(i)) => out.push(*i as f32),
                Some(Value::Bool(b)) => out.push(*b as u8 as f32),
                Some(Value::Vec2(v)) => out.extend(v.iter().map(|x| (x * k) as f32)),
                Some(Value::Vec3(v)) => out.extend(v.iter().map(|x| *x as f32)),
                Some(Value::Color(v)) => out.extend(v.iter().map(|x| *x as f32)),
                Some(v @ Value::Enum(_)) => out.push(schema.option_index(v).unwrap_or(0) as f32),
                // Whether a media input is connected; the picture itself arrives as the
                // effect's second input (`sample_media`).
                Some(Value::Media(m)) => out.push(m.is_some() as u8 as f32),
                Some(Value::Gradient(g)) => out.extend(g.pack()),
                Some(Value::Text(_)) | None => {}
            }
        }
        out
    }

    /// Whether this host can render it (stateful effects and plugins without a GPU
    /// shader currently can't).
    pub fn renders(&self) -> bool {
        match self.kind {
            EffectKind::Motion => self.motion.is_some(),
            // Heard, not rendered.
            EffectKind::Sound => false,
            _ => self.shader.is_some() && self.state == Statefulness::Pure,
        }
    }

    /// Floats in this effect's uniform block: packed params plus the clock.
    pub fn uniform_len(&self) -> usize {
        use oa_params::ParamType as T;
        let params: usize = self
            .params
            .iter()
            .map(|p| match p.ty {
                T::Float | T::Int | T::Bool | T::Enum | T::Media => 1,
                T::Vec2 => 2,
                T::Vec3 => 3,
                T::Color => 4,
                T::Text => 0,
                T::Gradient => oa_params::Gradient::PACKED_LEN,
            })
            .sum();
        params + CLOCK_SLOTS
    }

    /// The first media parameter, if the effect takes a media input (masks, displacement
    /// maps…). Only one media input per effect is wired up.
    pub fn media_param(&self) -> Option<&ParamSchema> {
        self.params.iter().find(|p| p.ty == oa_params::ParamType::Media)
    }

    /// How far (in raster pixels) the effect grows its input's bounds.
    pub fn expand_px(&self, values: &Evaluated, raster_scale: f64) -> f64 {
        match &self.kind {
            EffectKind::Spatial { expand: Some(id) } => values.float(id.as_str()).abs() * raster_scale,
            _ => 0.0,
        }
    }

    /// Where the effect can draw, given its input's bounds (raster px): grown by its
    /// `expand` param, or — for a surface with points pulled outside, or a sheet turned
    /// towards the camera — around the shape it makes, so nothing is cut off.
    pub fn grown_bounds(&self, values: &Evaluated, raster_scale: f64, input: Rect) -> Rect {
        let grown = match self.type_id.as_ref() {
            SURFACE => surface_bounds(values, input),
            DEPTH => depth_bounds(values, input),
            SHADOW => input.expand((values.float("distance").abs() + values.float("softness").abs()) * raster_scale),
            _ => return input.expand(self.expand_px(values, raster_scale)),
        };
        // Never more than a few times the layer's own size (a point dragged far away).
        let (w, h) = ((input.x1 - input.x0).max(1.0), (input.y1 - input.y0).max(1.0));
        let reach = 2.0 * w.max(h);
        Rect::new(
            (grown.x0.max(input.x0 - reach) + 1e-3).floor(),
            (grown.y0.max(input.y0 - reach) + 1e-3).floor(),
            (grown.x1.min(input.x1 + reach) - 1e-3).ceil(),
            (grown.y1.min(input.y1 + reach) - 1e-3).ceil(),
        )
    }
}

/// **Surface**: the layer stretched over a grid of points, each placed on its own (in
/// the viewer, where they replace the layer's handles). Params: `grid` (one of
/// [`SURFACE_GRIDS`]: 2 × 2 to 4 × 4 points), then one `Vec2` per point of a 4 × 4 grid
/// ([`surface_point_id`]) — its offset from where it rests, in fractions of the layer.
pub const SURFACE: &str = "oa.warp.surface";
pub const SURFACE_GRIDS: [&str; 3] = ["Corners", "3 × 3", "4 × 4"];
/// **Tile**: the whole picture in each cell of a grid (`columns`, `rows`, `gap`, `mirror`).
pub const TILE_EFFECT: &str = "oa.warp.tile";
/// **Scroll**: the picture sliding towards `direction` at `speed` px a second, wrapping
/// round at the layer's edges — so a tiled picture scrolls without a seam.
pub const SCROLL: &str = "oa.warp.scroll";
/// **Glow**: light leaking out around the picture: each pixel outside takes the color of
/// the nearest edge pixel, fading with the distance to it (a jump flood finds those).
/// The planner hands it the unblurred picture as its second input.
pub const GLOW: &str = "oa.light.glow";
/// **Drop Shadow**: the silhouette offset and softened behind the picture; it grows by
/// `distance + softness` (`grown_bounds`).
pub const SHADOW: &str = "oa.light.shadow";

/// How many jump-flood steps reach `radius` px: steps 2^(n-1) … 1 (the shader counts
/// the same way, by doubling, so the two always agree).
pub fn glow_jumps(radius: f32) -> u32 {
    let (mut n, mut reach) = (1u32, 1.0f32);
    while reach < radius && n < 12 {
        reach *= 2.0;
        n += 1;
    }
    n
}

/// How coarsely Glow finds its nearest edges: 1 (every pixel) up to 8 (one cell per 8 × 8
/// pixels) — a wide halo doesn't need pixel-exact distances, and each halving of the
/// resolution quarters the flood's work. Counted by doubling, as the shader does.
pub fn glow_divisor(radius: f32) -> u32 {
    let mut k = 1u32;
    while (k * 2) as f32 * 12.0 <= radius && k < 8 {
        k *= 2;
    }
    k
}

/// Glow's passes: find the seeds, one per jump (at the coarser resolution), then draw.
fn glow_passes(uniforms: &[f32]) -> u32 {
    let radius = uniforms.first().copied().unwrap_or(0.0).max(0.0);
    glow_jumps(radius / glow_divisor(radius) as f32) + 2
}

/// Glow's flood runs on the coarse grid; only the last pass, which draws, is full size.
fn glow_pass_divisor(uniforms: &[f32], pass: u32) -> u32 {
    if pass + 1 == glow_passes(uniforms) { 1 } else { glow_divisor(uniforms.first().copied().unwrap_or(0.0).max(0.0)) }
}
/// **Depth**: the picture as a sheet with thickness, turned in 3D.
pub const DEPTH: &str = "oa.depth.slab";

/// The param holding the offset of a surface's point (row and column from the top left).
pub fn surface_point_id(row: usize, col: usize) -> String {
    format!("p{row}{col}")
}

/// How many points a side a surface has (2 to 4).
pub fn surface_grid(values: &Evaluated) -> usize {
    let key = values.get("grid").and_then(Value::as_enum).unwrap_or("");
    SURFACE_GRIDS.iter().position(|g| *g == key).unwrap_or(0) + 2
}

/// Where a surface's points are, in fractions of the layer (0..1 at rest), row by row
/// from the top left: `(points a side, points)`.
pub fn surface_points(values: &Evaluated) -> (usize, Vec<[f64; 2]>) {
    let n = surface_grid(values);
    let side = (n - 1) as f64;
    let points = (0..n * n)
        .map(|i| {
            let (r, c) = (i / n, i % n);
            let o = values.get(&surface_point_id(r, c)).and_then(Value::as_vec2).unwrap_or([0.0; 2]);
            [c as f64 / side + o[0], r as f64 / side + o[1]]
        })
        .collect();
    (n, points)
}

fn surface_bounds(values: &Evaluated, input: Rect) -> Rect {
    let (w, h) = (input.x1 - input.x0, input.y1 - input.y0);
    let (_, points) = surface_points(values);
    points.iter().fold(input, |r, p| {
        let (x, y) = (input.x0 + p[0] * w, input.y0 + p[1] * h);
        Rect::new(r.x0.min(x), r.y0.min(y), r.x1.max(x), r.y1.max(y))
    })
}

/// The box the sheet is cut from, turned and seen through the same pinhole camera as
/// the shader's: its eight corners' outline on the film.
fn depth_bounds(values: &Evaluated, input: Rect) -> Rect {
    let (w, h) = (input.x1 - input.x0, input.y1 - input.y0);
    let center = [(input.x0 + input.x1) / 2.0, (input.y0 + input.y1) / 2.0];
    let thick = (values.float("depth") * 0.01 * w).max(0.001);
    let (yaw, pitch) = (values.float("yaw").to_radians(), values.float("pitch").to_radians());
    let pov = values.float("pov").clamp(0.0, 1.0);
    let dist = w * (1.2 + 40.0 * (1.0 - pov));
    let mut out = input;
    for i in 0..8 {
        let sign = |bit: usize| if i & bit == 0 { -1.0 } else { 1.0 };
        let (x, y, z) = (sign(1) * w / 2.0, sign(2) * h / 2.0, sign(4) * thick / 2.0);
        // Pitch, then yaw (the shader turns its rays the other way).
        let (y, z) = (pitch.cos() * y - pitch.sin() * z, pitch.sin() * y + pitch.cos() * z);
        let (x, z) = (yaw.cos() * x + yaw.sin() * z, -yaw.sin() * x + yaw.cos() * z);
        // Behind the camera: as far as the layer may grow.
        let k = if z + dist > 1.0 { dist / (z + dist) } else { 1e6 };
        let (px, py) = (center[0] + x * k, center[1] + y * k);
        out = Rect::new(out.x0.min(px), out.y0.min(py), out.x1.max(px), out.y1.max(py));
    }
    out
}

/// Atelier Core's sound effects (`oa.audio.*`), known even when no registry is at hand.
/// Plugins' sound effects are [`EffectKind::Sound`] descriptors: ask
/// [`Registry::is_sound`].
pub fn is_audio_effect(type_id: &str) -> bool {
    type_id.starts_with("oa.audio.")
}

/// A sound effect's descriptor: offered, stored and keyframed like any effect, run by
/// the audio mixer. `shader` is its sound shader, or `None` when the host runs it natively.
pub fn sound(type_id: &str, name: &str, usage: EffectUsage, params: Vec<ParamSchema>, shader: Option<Arc<str>>) -> EffectDescriptor {
    let mut d = descriptor(type_id, name, EffectKind::Sound, params);
    d.usage = usage;
    d.fusible = false;
    d.shader = shader.map(|source| EffectShader { entry: String::new(), source, passes: 1 });
    d
}

/// The built-in crop the planner adds for a clip's crop settings.
pub const CROP: &str = "oa.internal.crop";
/// Sequence backgrounds: a gradient over the input's box, a tile repeated over the
/// node's bounds, darkening.
pub const FILL: &str = "oa.internal.fill";
pub const TILE: &str = "oa.internal.tile";
pub const DIM: &str = "oa.internal.dim";
/// A file's input transform (DESIGN.md §9), right after it's decoded: uniforms are the
/// transfer curve's index (`oa_doc::color::Transfer::shader_index`), the row-major 3×3
/// gamut matrix to Rec.709, then the exposure gain.
pub const INPUT: &str = "oa.internal.input";
/// The output transform on the finished frame: tone map index
/// (`oa_doc::color::ToneMap::shader_index`), then the exposure gain.
pub const OUTPUT: &str = "oa.internal.output";
/// Linear ↔ display encoding around effects that declare `WorkingSpace::Display` but
/// aren't point ops (point ops convert inside their own pass).
pub const TO_DISPLAY: &str = "oa.internal.to_display";
pub const TO_LINEAR: &str = "oa.internal.to_linear";

/// Effects the host uses itself, never offered in the effects lists.
pub fn is_internal_effect(type_id: &str) -> bool {
    type_id.starts_with("oa.internal.")
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::IncompatibleApi { type_id, api_version } => {
                write!(f, "{type_id} was written for plugin API {api_version}, this host speaks {PLUGIN_API_VERSION}")
            }
            RegisterError::Duplicate(type_id) => write!(f, "another effect already uses the id {type_id}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    IncompatibleApi { type_id: String, api_version: u32 },
    Duplicate(String),
}

#[derive(Default)]
pub struct Registry {
    effects: BTreeMap<Arc<str>, Arc<EffectDescriptor>>,
    /// Which plugin each effect came from.
    origin: BTreeMap<Arc<str>, Arc<str>>,
}

impl Registry {
    pub fn with_builtins() -> Self {
        let mut r = Registry::default();
        for d in builtins() {
            r.register_from(crate::plugin::CORE_ID, d).expect("built-in effect registration");
        }
        r
    }

    /// The effects of every plugin given, in order (a later plugin can't take an id an
    /// earlier one used). Problems are returned rather than dropped.
    pub fn from_plugins<'a>(plugins: impl IntoIterator<Item = &'a crate::plugin::Plugin>) -> (Self, Vec<String>) {
        let mut r = Registry::default();
        let mut issues = Vec::new();
        for p in plugins {
            for d in &p.effects {
                let type_id = d.type_id.clone();
                if let Err(e) = r.register_from(&p.id, d.clone()) {
                    issues.push(format!("{}: {type_id}: {e}", p.name));
                }
            }
        }
        (r, issues)
    }

    /// The id of the plugin an effect came from.
    pub fn plugin_of(&self, type_id: &str) -> Option<&Arc<str>> {
        self.origin.get(type_id)
    }

    pub fn register(&mut self, d: EffectDescriptor) -> Result<(), RegisterError> {
        self.register_from(crate::plugin::CORE_ID, d)
    }

    pub fn register_from(&mut self, plugin: &str, d: EffectDescriptor) -> Result<(), RegisterError> {
        if d.api_version != PLUGIN_API_VERSION {
            return Err(RegisterError::IncompatibleApi { type_id: d.type_id.to_string(), api_version: d.api_version });
        }
        if self.effects.contains_key(&d.type_id) {
            return Err(RegisterError::Duplicate(d.type_id.to_string()));
        }
        self.origin.insert(d.type_id.clone(), plugin.into());
        self.effects.insert(d.type_id.clone(), Arc::new(d));
        Ok(())
    }

    pub fn effect(&self, type_id: &str) -> Option<&Arc<EffectDescriptor>> {
        self.effects.get(type_id)
    }

    pub fn effects(&self) -> impl Iterator<Item = &Arc<EffectDescriptor>> {
        self.effects.values()
    }

    /// Whether `type_id` is a sound effect (the audio mixer's, never rendered).
    pub fn is_sound(&self, type_id: &str) -> bool {
        is_audio_effect(type_id) || self.effects.get(type_id).is_some_and(|d| d.kind == EffectKind::Sound)
    }

    /// Sound effects for pickers, by name.
    pub fn sounds(&self) -> Vec<&Arc<EffectDescriptor>> {
        let mut list: Vec<_> = self.effects.values().filter(|d| d.kind == EffectKind::Sound).collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Transition types, for pickers.
    pub fn transitions(&self) -> impl Iterator<Item = &Arc<EffectDescriptor>> {
        self.effects.values().filter(|d| d.kind == EffectKind::Transition)
    }

    /// Effects offered for one use on a picture clip, sorted by name — only ones this
    /// host can actually render (a shader or a motion function), so a menu never offers
    /// a no-op.
    pub fn offered(&self, usage: EffectUsage) -> Vec<&Arc<EffectDescriptor>> {
        self.offered_for(usage, false)
    }

    /// Like [`Registry::offered`], including text-only (per-letter, per-pixel text)
    /// effects when `text`. Text effects come first.
    pub fn offered_for(&self, usage: EffectUsage, text: bool) -> Vec<&Arc<EffectDescriptor>> {
        let mut list: Vec<_> =
            self.effects.values().filter(|d| d.usage == usage && d.renders() && !is_internal_effect(&d.type_id) && (text || !d.kind.text_only())).collect();
        list.sort_by(|a, b| (!a.kind.text_only(), &a.name).cmp(&(!b.kind.text_only(), &b.name)));
        list
    }
}

fn descriptor(type_id: &str, name: &str, kind: EffectKind, params: Vec<ParamSchema>) -> EffectDescriptor {
    let pointish = matches!(kind, EffectKind::PointOp | EffectKind::UvWarp);
    EffectDescriptor {
        type_id: type_id.into(),
        version: 1,
        api_version: PLUGIN_API_VERSION,
        name: name.into(),
        preserves_opacity: kind == EffectKind::PointOp,
        time_varying: false,
        usage: if kind == EffectKind::Transition { EffectUsage::Cut } else { EffectUsage::Passive },
        motion: None,
        pass_count: None,
        pass_divisor: None,
        fusible: pointish,
        kind,
        state: Statefulness::Pure,
        space: WorkingSpace::Linear,
        params,
        shader: None,
    }
}

fn transition(type_id: &str, name: &str, params: Vec<ParamSchema>) -> EffectDescriptor {
    EffectDescriptor { preserves_opacity: true, fusible: false, ..descriptor(type_id, name, EffectKind::Transition, params) }
}

/// The input transform. The decoder hands over values already decoded with the sRGB
/// curve (right for ordinary video and pictures), so this first re-encodes them to the
/// file's code values, then applies the file's real curve, gamut matrix and exposure.
/// Curves map to scene-linear with 1.0 = SDR white (HDR: 203 nits, BT.2408) and log
/// curves' 18% gray at 0.18.
const INPUT_WGSL: &str = "
fn oa_pow10(x: vec3f) -> vec3f { return pow(vec3f(10.0), x); }

fn oa_input_curve(x: vec3f, t: u32) -> vec3f {
    switch t {
        case 1u: { return pow(max(x, vec3f(0.0)), vec3f(2.4)); }
        case 2u: { return pow(max(x, vec3f(0.0)), vec3f(2.2)); }
        case 3u: { return pow(max(x, vec3f(0.0)), vec3f(2.6)); }
        case 4u: { return x; }
        case 5u: {
            // PQ (SMPTE ST 2084): code → nits, then 203 nits → 1.0.
            let p = pow(max(x, vec3f(0.0)), vec3f(1.0 / 78.84375));
            let y = pow(max(p - 0.8359375, vec3f(0.0)) / (18.8515625 - 18.6875 * p), vec3f(1.0 / 0.1593017578125));
            return y * (10000.0 / 203.0);
        }
        case 6u: {
            // HLG: inverse OETF, then the OOTF of a 1000-nit display (system gamma 1.2).
            let e = select((exp((x - 0.55991073) / 0.17883277) + 0.28466892) / 12.0, x * x / 3.0, x <= vec3f(0.5));
            let ys = max(dot(e, vec3f(0.2627, 0.6780, 0.0593)), 1e-6);
            return 1000.0 * pow(ys, 0.2) * e / 203.0;
        }
        case 7u: {
            // ARRI LogC3, EI 800.
            return select((oa_pow10((x - 0.385537) / 0.247190) - 0.052272) / 5.555556, (x - 0.092809) / 5.367655, x <= vec3f(0.149658));
        }
        case 8u: {
            // Sony S-Log3.
            let cv = x * 1023.0;
            return select(oa_pow10((cv - 420.0) / 261.5) * 0.19 - 0.01, (cv - 95.0) * 0.01125 / (171.2102946929 - 95.0), cv < vec3f(171.2102946929));
        }
        case 9u: {
            // Panasonic V-Log.
            return select(oa_pow10((x - 0.598206) / 0.241514) - 0.00873, (x - 0.125) / 5.6, x < vec3f(0.181));
        }
        case 10u: {
            // Fujifilm F-Log.
            return select((oa_pow10((x - 0.790453) / 0.344676) - 0.009468) / 0.555556, (x - 0.092864) / 8.735631, x < vec3f(0.100537775));
        }
        case 11u: {
            // Canon Log 3 (with its 0.9 reflection normalization).
            let lo = -(oa_pow10((0.12783901 - x) / 0.36726845) - 1.0) / 14.98325;
            let mid = (x - 0.12512219) / 1.9754798;
            let hi = (oa_pow10((x - 0.12240537) / 0.36726845) - 1.0) / 14.98325;
            return 0.9 * select(select(hi, mid, x <= vec3f(0.15277891)), lo, x < vec3f(0.097465473));
        }
        case 12u: {
            // Apple Log.
            let hi = exp2((x - 0.69336945) / 0.08550479) - 0.00964052;
            let lo = sqrt(max(x, vec3f(0.0)) / 47.28711236) - 0.05641088;
            return select(hi, lo, x < vec3f(0.2085565));
        }
        default: { return srgb_decode(x); }
    }
}

fn oa_internal_input(c: vec4f, base: u32) -> vec4f {
    let code = srgb_encode(c.rgb);
    let l = oa_input_curve(code, u32(u(base)));
    // Row-major in the uniforms; WGSL matrices are built from columns.
    let m = mat3x3f(
        vec3f(u(base + 1u), u(base + 4u), u(base + 7u)),
        vec3f(u(base + 2u), u(base + 5u), u(base + 8u)),
        vec3f(u(base + 3u), u(base + 6u), u(base + 9u)),
    );
    return vec4f(m * l * u(base + 10u), c.a);
}
";

fn wgsl(entry: &str, passes: u32, source: &str) -> Option<EffectShader> {
    Some(EffectShader { entry: entry.into(), source: source.into(), passes })
}

pub(crate) fn builtins() -> Vec<EffectDescriptor> {
    vec![
        EffectDescriptor {
            shader: wgsl(
                "oa_color_exposure",
                1,
                "fn oa_color_exposure(c: vec4f, base: u32) -> vec4f {
                    return vec4f(c.rgb * pow(2.0, u(base)), c.a);
                }",
            ),
            ..descriptor(
                "oa.color.exposure",
                "Exposure",
                EffectKind::PointOp,
                vec![ParamSchema::new("stops", Value::Float(0.0), Unit::None).range(-10.0, 10.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_saturation",
                1,
                "fn oa_color_saturation(c: vec4f, base: u32) -> vec4f {
                    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    return vec4f(mix(vec3f(luma), c.rgb, u(base)), c.a);
                }",
            ),
            ..descriptor(
                "oa.color.saturation",
                "Saturation",
                EffectKind::PointOp,
                vec![ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 4.0)],
            )
        },
        EffectDescriptor {
            space: WorkingSpace::Display,
            shader: wgsl(
                "oa_color_posterize",
                1,
                "fn oa_color_posterize(c: vec4f, base: u32) -> vec4f {
                    let steps = max(u(base) - 1.0, 1.0);
                    return vec4f(round(clamp(c.rgb, vec3f(0.0), vec3f(1.0)) * steps) / steps, c.a);
                }",
            ),
            ..descriptor(
                "oa.color.posterize",
                "Posterize",
                EffectKind::PointOp,
                vec![ParamSchema::new("levels", Value::Float(8.0), Unit::None).range(2.0, 256.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_blur_gaussian",
                2,
                "fn oa_blur_gaussian(pos: vec2f, base: u32) -> vec4f {
                    let radius = u(base);
                    let clamp_edges = u(base + 1u) > 0.5;
                    if (radius < 0.5) { return sample_input(pos); }
                    let dir = select(vec2f(0.0, 1.0), vec2f(1.0, 0.0), pass_index() == 0u);
                    let sigma = radius / 2.5;
                    let step = max(1.0, radius / 24.0);
                    let n = i32(ceil(radius / step));
                    var sum = vec4f(0.0);
                    var wsum = 0.0;
                    for (var i = -n; i <= n; i++) {
                        let x = f32(i) * step;
                        let w = exp(-0.5 * x * x / (sigma * sigma));
                        let p = pos + dir * x;
                        sum += select(sample_input(p), sample_input_clamped(p), clamp_edges) * w;
                        wsum += w;
                    }
                    return sum / wsum;
                }",
            ),
            ..descriptor(
                "oa.blur.gaussian",
                "Blur",
                // No expansion: the blurred clip keeps its size, edges repeat inward.
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("radius", Value::Float(8.0), Unit::LayerPixels).range(0.0, 2000.0),
                    ParamSchema::new("edges", Value::Enum("clamp".into()), Unit::None)
                        .options(&["transparent", "clamp"])
                        .static_only(),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_blur_smear",
                1,
                "fn oa_blur_smear(pos: vec2f, base: u32) -> vec4f {
                    // A directional (motion) blur: the picture smeared along `direction`,
                    // either both ways or trailing behind it.
                    let len = u(base);
                    if (len < 0.5) { return sample_input(pos); }
                    let a = radians(u(base + 1u));
                    let dir = vec2f(cos(a), sin(a));
                    let trail = u(base + 2u) > 0.5;
                    let n = i32(clamp(ceil(len / 1.5), 4.0, 64.0));
                    var sum = vec4f(0.0);
                    for (var i = 0; i <= n; i++) {
                        let f = f32(i) / f32(n);
                        let x = select(f - 0.5, -f, trail) * len;
                        sum += sample_input_clamped(pos + dir * x);
                    }
                    return sum / f32(n + 1);
                }",
            ),
            ..descriptor(
                "oa.blur.smear",
                "Smear Blur",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("length", Value::Float(40.0), Unit::LayerPixels).range(0.0, 2000.0),
                    ParamSchema::new("direction", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("trail", Value::Bool(false), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_warp_wobble",
                1,
                "fn oa_warp_wobble(pos: vec2f, base: u32) -> vec4f {
                    // The picture shears left and right, rocking about its middle row.
                    let mid = in_origin().y + in_size().y * 0.5;
                    let shear = u(base) * sin(clip_seconds() * u(base + 1u) * 6.2831853);
                    let src = vec2f(pos.x - shear * (pos.y - mid), pos.y);
                    return select(sample_input(src), sample_input_clamped(src), u(base + 2u) > 0.5);
                }",
            ),
            ..descriptor(
                "oa.warp.wobble",
                "Wobble",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(0.15), Unit::None).range(0.0, 4.0),
                    ParamSchema::new("speed", Value::Float(2.0), Unit::None).range(0.0, 40.0),
                    ParamSchema::new("edges", Value::Enum("transparent".into()), Unit::None).options(&["transparent", "clamp"]).static_only(),
                ],
            )
        },
        // ---- stylize: looks that change the picture's texture ----
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_pixelate",
                1,
                "fn oa_stylize_pixelate(pos: vec2f, base: u32) -> vec4f {
                    // Snap to the middle of a block, so each block takes one sample.
                    let size = max(u(base), 1.0);
                    let lo = in_origin();
                    let cell = floor((pos - lo) / size) * size + size * 0.5;
                    let blend = clamp(u(base + 1u), 0.0, 1.0);
                    return mix(sample_input(lo + cell), sample_input(pos), blend);
                }",
            ),
            ..descriptor(
                "oa.stylize.pixelate",
                "Pixelate",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("size", Value::Float(16.0), Unit::LayerPixels).range(1.0, 512.0),
                    ParamSchema::new("blend", Value::Float(0.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            preserves_opacity: false,
            shader: wgsl(
                "oa_stylize_scanlines",
                1,
                "fn oa_stylize_scanlines(c: vec4f, base: u32) -> vec4f {
                    // Dark lines every `spacing` layer pixels, travelling with `roll`.
                    let spacing = max(u(base + 1u), 1.0);
                    let y = (layer_pos().y - in_origin().y) / spacing + u(base + 3u) * clip_seconds();
                    let wave = 0.5 + 0.5 * cos(6.2831853 * y);
                    let line = pow(wave, max(u(base + 2u), 0.01));
                    return vec4f(c.rgb * (1.0 - clamp(u(base), 0.0, 4.0) * line), c.a);
                }",
            ),
            time_varying: true,
            ..descriptor(
                "oa.stylize.scanlines",
                "Scanlines",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("amount", Value::Float(0.35), Unit::None).range(0.0, 2.0),
                    ParamSchema::new("spacing", Value::Float(4.0), Unit::LayerPixels).range(2.0, 200.0),
                    ParamSchema::new("sharpness", Value::Float(1.0), Unit::None).range(0.2, 8.0),
                    ParamSchema::new("roll", Value::Float(0.0), Unit::None).range(-20.0, 20.0),
                ],
            )
        },
        EffectDescriptor {
            preserves_opacity: false,
            shader: wgsl(
                "oa_stylize_vignette",
                1,
                "fn oa_stylize_vignette(c: vec4f, base: u32) -> vec4f {
                    // Distance from the middle, in units of half the layer, so a wide
                    // layer darkens at its own corners rather than a circle's.
                    let size = max(in_size(), vec2f(1.0));
                    let rel = (layer_pos() - in_origin()) / size - 0.5;
                    let round = clamp(u(base + 3u), 0.0, 1.0);
                    let square = max(abs(rel.x), abs(rel.y)) * 2.0;
                    let circle = length(rel * vec2f(max(size.x / size.y, 1.0), max(size.y / size.x, 1.0))) * 2.0;
                    let d = mix(square, circle, round) / max(u(base + 1u), 0.05);
                    let soft = max(u(base + 2u), 0.001);
                    let k = clamp(u(base), 0.0, 4.0) * smoothstep(1.0 - soft, 1.0 + soft, d);
                    let tint = vec3f(u(base + 4u), u(base + 5u), u(base + 6u));
                    let alpha = u(base + 7u);
                    // A coloured vignette mixes towards the colour; a black one just darkens.
                    return vec4f(mix(c.rgb, tint, clamp(k * alpha, 0.0, 1.0)) * (1.0 - k * (1.0 - alpha)), c.a);
                }",
            ),
            ..descriptor(
                "oa.stylize.vignette",
                "Vignette",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("amount", Value::Float(0.6), Unit::None).range(0.0, 2.0),
                    ParamSchema::new("size", Value::Float(0.75), Unit::None).range(0.05, 2.0),
                    ParamSchema::new("softness", Value::Float(0.45), Unit::None).range(0.0, 2.0),
                    ParamSchema::new("roundness", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("color", Value::Color([0.0, 0.0, 0.0, 1.0]), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_grain",
                1,
                "fn oa_stylize_grain(c: vec4f, base: u32) -> vec4f {
                    // Value noise on a grid of `size` px, shifted every second so it
                    // crawls the way film grain does.
                    let cell = max(u(base + 1u), 1.0);
                    let p = floor((layer_pos() - in_origin()) / cell) + floor(clip_seconds() * max(u(base + 2u), 0.0)) * 37.0;
                    let n = fract(sin(dot(p, vec2f(12.9898, 78.233))) * 43758.5453);
                    let amount = clamp(u(base), 0.0, 2.0);
                    // Grain shows most in the midtones, as it does on film.
                    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    let shape = 4.0 * luma * (1.0 - clamp(luma, 0.0, 1.0));
                    return vec4f(c.rgb + (n - 0.5) * amount * mix(1.0, shape, 0.75), c.a);
                }",
            ),
            time_varying: true,
            ..descriptor(
                "oa.stylize.grain",
                "Film Grain",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("amount", Value::Float(0.12), Unit::None).range(0.0, 1.5),
                    ParamSchema::new("size", Value::Float(2.0), Unit::LayerPixels).range(1.0, 64.0),
                    ParamSchema::new("speed", Value::Float(24.0), Unit::None).range(0.0, 60.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_halftone",
                1,
                "fn oa_stylize_halftone(pos: vec2f, base: u32) -> vec4f {
                    // Dots on a rotated grid, each as big as the ink under it.
                    let size = max(u(base), 2.0);
                    let a = radians(u(base + 1u));
                    let rel = pos - in_origin();
                    let rot = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
                    let cell = floor(rot / size) * size + size * 0.5;
                    let back = vec2f(cell.x * cos(-a) - cell.y * sin(-a), cell.x * sin(-a) + cell.y * cos(-a));
                    let c = sample_input(in_origin() + back);
                    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    let radius = sqrt(clamp(1.0 - luma, 0.0, 1.0)) * size * 0.72;
                    let d = length(rot - cell);
                    let ink = 1.0 - smoothstep(radius - 1.0, radius + 1.0, d);
                    let paper = vec3f(u(base + 2u), u(base + 3u), u(base + 4u));
                    let dot_color = vec3f(u(base + 6u), u(base + 7u), u(base + 8u));
                    return vec4f(mix(paper, dot_color, ink), c.a);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.stylize.halftone",
                "Halftone",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("size", Value::Float(8.0), Unit::LayerPixels).range(2.0, 128.0),
                    ParamSchema::new("angle", Value::Float(15.0), Unit::Degrees).range(-90.0, 90.0),
                    ParamSchema::new("paper", Value::Color([1.0, 1.0, 1.0, 1.0]), Unit::None),
                    ParamSchema::new("ink", Value::Color([0.0, 0.0, 0.0, 1.0]), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_sharpen",
                1,
                "fn oa_stylize_sharpen(pos: vec2f, base: u32) -> vec4f {
                    // Unsharp mask: the picture, plus what a small blur took away.
                    let r = max(u(base + 1u), 0.5);
                    let c = sample_input(pos);
                    var blur = vec4f(0.0);
                    blur += sample_input(pos + vec2f(-r, -r)) + sample_input(pos + vec2f(r, -r));
                    blur += sample_input(pos + vec2f(-r, r)) + sample_input(pos + vec2f(r, r));
                    blur += sample_input(pos + vec2f(-r, 0.0)) + sample_input(pos + vec2f(r, 0.0));
                    blur += sample_input(pos + vec2f(0.0, -r)) + sample_input(pos + vec2f(0.0, r));
                    blur = blur / 8.0;
                    return c + (c - blur) * clamp(u(base), 0.0, 8.0);
                }",
            ),
            ..descriptor(
                "oa.stylize.sharpen",
                "Sharpen",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(0.6), Unit::None).range(0.0, 5.0),
                    ParamSchema::new("radius", Value::Float(1.5), Unit::LayerPixels).range(0.5, 32.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_edges",
                1,
                "fn oa_stylize_edges(pos: vec2f, base: u32) -> vec4f {
                    // Sobel on brightness.
                    let r = max(u(base + 1u), 0.5);
                    var gx = vec3f(0.0);
                    var gy = vec3f(0.0);
                    gx += (sample_input(pos + vec2f(r, -r)) + 2.0 * sample_input(pos + vec2f(r, 0.0)) + sample_input(pos + vec2f(r, r))).rgb;
                    gx -= (sample_input(pos + vec2f(-r, -r)) + 2.0 * sample_input(pos + vec2f(-r, 0.0)) + sample_input(pos + vec2f(-r, r))).rgb;
                    gy += (sample_input(pos + vec2f(-r, r)) + 2.0 * sample_input(pos + vec2f(0.0, r)) + sample_input(pos + vec2f(r, r))).rgb;
                    gy -= (sample_input(pos + vec2f(-r, -r)) + 2.0 * sample_input(pos + vec2f(0.0, -r)) + sample_input(pos + vec2f(r, -r))).rgb;
                    let edge = clamp(length(vec2f(length(gx), length(gy))) * u(base), 0.0, 8.0);
                    let c = sample_input(pos);
                    let tint = vec3f(u(base + 3u), u(base + 4u), u(base + 5u));
                    // `blend` 1 shows the edges alone, 0 draws them over the picture.
                    return vec4f(mix(c.rgb + tint * edge, tint * edge, clamp(u(base + 2u), 0.0, 1.0)), c.a);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.stylize.edges",
                "Edges",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 8.0),
                    ParamSchema::new("radius", Value::Float(1.0), Unit::LayerPixels).range(0.5, 16.0),
                    ParamSchema::new("blend", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("color", Value::Color([1.0, 1.0, 1.0, 1.0]), Unit::None),
                ],
            )
        },
        // ---- warps: the picture bends ----
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_fisheye",
                1,
                "fn oa_warp_fisheye(pos: vec2f, base: u32) -> vec4f {
                    // Positive bulges out of the middle, negative pinches into it.
                    let size = max(in_size(), vec2f(1.0));
                    let mid = in_origin() + size * 0.5;
                    let rel = (pos - mid) / (min(size.x, size.y) * 0.5);
                    let r = length(rel);
                    let amount = clamp(u(base), -4.0, 4.0);
                    let zoom = max(u(base + 1u), 0.05);
                    // r' = r * (1 + k r^2), inverted by sampling where the pixel came from.
                    let k = amount * 0.5;
                    let scale = 1.0 / (1.0 + k * r * r) / zoom;
                    return sample_input(mid + rel * scale * (min(size.x, size.y) * 0.5));
                }",
            ),
            ..descriptor(
                "oa.warp.fisheye",
                "Fisheye",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(0.6), Unit::None).range(-3.0, 3.0),
                    ParamSchema::new("zoom", Value::Float(1.0), Unit::None).range(0.2, 4.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_swirl",
                1,
                "fn oa_warp_swirl(pos: vec2f, base: u32) -> vec4f {
                    let size = max(in_size(), vec2f(1.0));
                    let mid = in_origin() + size * 0.5;
                    let rel = pos - mid;
                    let radius = max(u(base + 1u), 0.001) * min(size.x, size.y) * 0.5;
                    let fade = clamp(1.0 - length(rel) / radius, 0.0, 1.0);
                    let a = radians(u(base)) * fade * fade;
                    let turned = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
                    return sample_input(mid + turned);
                }",
            ),
            ..descriptor(
                "oa.warp.swirl",
                "Swirl",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("angle", Value::Float(90.0), Unit::Degrees).range(-1080.0, 1080.0),
                    ParamSchema::new("radius", Value::Float(0.8), Unit::None).range(0.05, 3.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_mirror",
                1,
                "fn oa_warp_mirror(pos: vec2f, base: u32) -> vec4f {
                    // Everything past the line is the reflection of what's before it.
                    let size = max(in_size(), vec2f(1.0));
                    let vertical = u(base) < 0.5;
                    let at = in_origin() + size * clamp(u(base + 1u), 0.0, 1.0);
                    let flip = u(base + 2u) > 0.5;
                    var p = pos;
                    if (vertical) {
                        let past = select(pos.x > at.x, pos.x < at.x, flip);
                        if (past) { p.x = 2.0 * at.x - pos.x; }
                    } else {
                        let past = select(pos.y > at.y, pos.y < at.y, flip);
                        if (past) { p.y = 2.0 * at.y - pos.y; }
                    }
                    return sample_input(p);
                }",
            ),
            ..descriptor(
                "oa.warp.mirror",
                "Mirror",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("axis", Value::Enum("vertical".into()), Unit::None).options(&["vertical", "horizontal"]).static_only(),
                    ParamSchema::new("position", Value::Float(0.5), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("flip", Value::Bool(false), Unit::None).static_only(),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_light_glow",
                1,
                "// Jump-flood steps that reach `radius`: 2^(n-1) … 1 (`glow_jumps`).
                fn oa_glow_jumps(radius: f32) -> u32 {
                    var n = 1u;
                    var reach = 1.0;
                    while (reach < radius && n < 12u) { reach *= 2.0; n++; }
                    return n;
                }

                // Pixels per flood cell (`glow_divisor`): wide halos use a coarser grid.
                fn oa_glow_cell(radius: f32) -> u32 {
                    var k = 1u;
                    while (f32(k * 2u) * 12.0 <= radius && k < 8u) { k *= 2u; }
                    return k;
                }

                // A cell of the previous flood pass: (way to its seed from the cell's
                // center, in layer px; found; 1). Outside the grid: nothing found.
                fn oa_glow_load(cell: vec2i) -> vec4f {
                    let dims = vec2i(textureDimensions(input_tex));
                    if (any(cell < vec2i(0)) || any(cell >= dims)) { return vec4f(0.0); }
                    return textureLoad(input_tex, cell, 0);
                }

                fn oa_light_glow(pos: vec2f, base: u32) -> vec4f {
                    let radius = max(u(base), 0.0);
                    let k = oa_glow_cell(radius);
                    let kf = f32(k);
                    let jumps = oa_glow_jumps(radius / kf);
                    let at_pass = pass_index();
                    // The flood's passes are on the coarse grid: which cell is this?
                    let cell = vec2i(floor((pos - out_origin())));
                    let center = out_origin() + (vec2f(cell) + 0.5) * kf;
                    if (at_pass == 0u) {
                        // Seeds: an opaque point of the picture inside the cell (a few
                        // looked at, so thin parts aren't missed on a coarse grid).
                        for (var j = 0; j < 2; j++) {
                            for (var i = 0; i < 2; i++) {
                                let p = center + (vec2f(f32(i), f32(j)) - 0.5) * 0.5 * kf;
                                if (sample_input(p).a > 0.5) { return vec4f(p - center, 1.0, 1.0); }
                            }
                        }
                        return vec4f(0.0, 0.0, 0.0, 1.0);
                    }
                    if (at_pass <= jumps) {
                        // Jump flood: look `step` cells away in eight directions and keep
                        // whichever seed those cells know that's nearest to this one.
                        let step = i32(1u << (jumps - at_pass));
                        var best = oa_glow_load(cell);
                        var best_d = select(1e9, length(best.xy), best.z > 0.5);
                        for (var j = -1; j <= 1; j++) {
                            for (var i = -1; i <= 1; i++) {
                                if (i == 0 && j == 0) { continue; }
                                let d = vec2i(i, j) * step;
                                let s = oa_glow_load(cell + d);
                                if (s.z < 0.5) { continue; }
                                let way = vec2f(d) * kf + s.xy;
                                let dist = length(way);
                                if (dist < best_d) { best_d = dist; best = vec4f(way, 1.0, 1.0); }
                            }
                        }
                        return best;
                    }
                    // Last pass, full size: the nearest edge's color, fading with the
                    // distance to it, with the sharp picture (the second input) over it.
                    let c = select(vec4f(0.0), sample_media(pos), has_media());
                    let over = u(base + 6u) > 0.5;
                    if (c.a > 0.999 && !over) { return c; }
                    let home = vec2i(floor((pos - in_origin()) / kf));
                    let s = oa_glow_load(home);
                    if (s.z < 0.5) { return c; }
                    let seed = in_origin() + (vec2f(home) + 0.5) * kf + s.xy;
                    let d = length(seed - pos);
                    if (d > radius) { return c; }
                    let edge = sample_media(seed);
                    let t = 1.0 - d / max(radius, 1e-3);
                    let tint = vec4f(u(base + 2u), u(base + 3u), u(base + 4u), u(base + 5u));
                    let a = clamp(t * t * u(base + 1u) * tint.a, 0.0, 1.0);
                    let g = vec4f(edge.rgb / max(edge.a, 1e-4) * tint.rgb * a, a);
                    if (over) { return vec4f(c.rgb + g.rgb, max(c.a, g.a)); }
                    return c + g * (1.0 - c.a);
                }",
            ),
            preserves_opacity: false,
            pass_count: Some(glow_passes),
            pass_divisor: Some(glow_pass_divisor),
            ..descriptor(
                GLOW,
                "Glow",
                EffectKind::Spatial { expand: Some(ParamId::new("radius")) },
                vec![
                    ParamSchema::new("radius", Value::Float(30.0), Unit::LayerPixels).range(0.0, 400.0),
                    ParamSchema::new("strength", Value::Float(0.8), Unit::None).range(0.0, 4.0),
                    ParamSchema::new("color", Value::Color([1.0, 1.0, 1.0, 1.0]), Unit::None),
                    ParamSchema::new("mode", Value::Enum("behind".into()), Unit::None).options(&["behind", "over"]).static_only(),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_light_shadow",
                1,
                "fn oa_light_shadow(pos: vec2f, base: u32) -> vec4f {
                    // The picture's silhouette, moved `distance` px towards `direction`,
                    // softened over `softness` px, in `color`, behind the picture.
                    let color = vec4f(u(base), u(base + 1u), u(base + 2u), u(base + 3u));
                    let a = radians(u(base + 5u));
                    let source = pos - vec2f(cos(a), sin(a)) * u(base + 6u);
                    let soft = max(u(base + 7u), 0.0);
                    var cover = sample_input(source).a;
                    var weight = 1.0;
                    // Two rings of eight around it, weighted like a Gaussian.
                    for (var ring = 1; ring <= 2; ring++) {
                        let r = soft * f32(ring) * 0.5;
                        let w = exp(-f32(ring * ring) * 0.5);
                        for (var k = 0; k < 8; k++) {
                            let t = f32(k) * 0.7853982 + f32(ring) * 0.39;
                            cover += sample_input(source + vec2f(cos(t), sin(t)) * r).a * w;
                            weight += w;
                        }
                    }
                    let alpha = cover / weight * color.a * clamp(u(base + 4u), 0.0, 1.0);
                    let c = sample_input(pos);
                    return c + vec4f(color.rgb * alpha, alpha) * (1.0 - c.a);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                SHADOW,
                "Drop Shadow",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("color", Value::Color([0.0, 0.0, 0.0, 1.0]), Unit::None),
                    ParamSchema::new("opacity", Value::Float(0.6), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("direction", Value::Float(45.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("distance", Value::Float(14.0), Unit::LayerPixels).range(0.0, 400.0),
                    ParamSchema::new("softness", Value::Float(12.0), Unit::LayerPixels).range(0.0, 200.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_stylize_stroke",
                1,
                "fn oa_stylize_stroke(pos: vec2f, base: u32) -> vec4f {
                    // An outline `width` px wide around whatever is opaque: the most cover
                    // found on two rings of sixteen around the pixel, behind the picture.
                    let w = max(u(base), 0.0);
                    let color = vec4f(u(base + 1u), u(base + 2u), u(base + 3u), u(base + 4u));
                    let c = sample_input(pos);
                    if (w < 0.5 || c.a > 0.999) { return c; }
                    var cover = 0.0;
                    for (var k = 0; k < 16; k++) {
                        let t = f32(k) * 0.3926991;
                        let d = vec2f(cos(t), sin(t));
                        cover = max(cover, max(sample_input(pos + d * w).a, sample_input(pos + d * w * 0.5).a));
                    }
                    let alpha = cover * color.a;
                    return c + vec4f(color.rgb * alpha, alpha) * (1.0 - c.a);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.stylize.stroke",
                "Stroke",
                EffectKind::Spatial { expand: Some(ParamId::new("width")) },
                vec![
                    ParamSchema::new("width", Value::Float(6.0), Unit::LayerPixels).range(0.0, 100.0),
                    ParamSchema::new("color", Value::Color([1.0, 1.0, 1.0, 1.0]), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_stylize_glitch",
                1,
                "fn oa_glitch_hash(p: vec2f) -> f32 {
                    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
                }

                fn oa_stylize_glitch(pos: vec2f, base: u32) -> vec4f {
                    // Bands of rows jump sideways for a moment, the colors pull apart.
                    let amount = clamp(u(base), 0.0, 1.0);
                    let tick = floor(clip_seconds() * max(u(base + 1u), 0.0));
                    let size = max(in_size(), vec2f(1.0));
                    let y = (pos.y - in_origin().y) / size.y;
                    let band = floor(y * mix(4.0, 24.0, oa_glitch_hash(vec2f(tick, 1.0))));
                    let hit = oa_glitch_hash(vec2f(band, tick));
                    var shift = 0.0;
                    if (hit < amount * 0.6) {
                        shift = (oa_glitch_hash(vec2f(band, tick + 7.0)) - 0.5) * amount * 0.3 * size.x;
                    }
                    let split = u(base + 2u) * (0.3 + amount) * select(1.0, 2.5, hit < amount * 0.3);
                    let p = pos + vec2f(shift, 0.0);
                    let r = sample_input(p + vec2f(split, 0.0));
                    let g = sample_input(p);
                    let b = sample_input(p - vec2f(split, 0.0));
                    return vec4f(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.stylize.glitch",
                "Glitch",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(0.5), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("speed", Value::Float(8.0), Unit::None).range(0.0, 30.0),
                    ParamSchema::new("split", Value::Float(6.0), Unit::LayerPixels).range(0.0, 100.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_kaleidoscope",
                1,
                "fn oa_warp_kaleidoscope(pos: vec2f, base: u32) -> vec4f {
                    // The picture folded into `segments` mirrored wedges around its middle.
                    let size = max(in_size(), vec2f(1.0));
                    let center = in_origin() + 0.5 * size;
                    let rel = pos - center;
                    let n = max(round(u(base)), 1.0);
                    let wedge = 6.2831853 / n;
                    let turn = radians(u(base + 1u));
                    var a = atan2(rel.y, rel.x) - turn;
                    a = a - wedge * floor(a / wedge);
                    if (a > 0.5 * wedge) { a = wedge - a; }
                    let r = length(rel);
                    return sample_input_clamped(center + vec2f(cos(a + turn), sin(a + turn)) * r);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.warp.kaleidoscope",
                "Kaleidoscope",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("segments", Value::Float(6.0), Unit::None).range(2.0, 24.0),
                    ParamSchema::new("rotation", Value::Float(0.0), Unit::Degrees).range(-180.0, 180.0),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_warp_ripple",
                1,
                "fn oa_warp_ripple(pos: vec2f, base: u32) -> vec4f {
                    // Rings running out from the middle, like a stone dropped in water.
                    let size = max(in_size(), vec2f(1.0));
                    let rel = pos - (in_origin() + 0.5 * size);
                    let d = length(rel);
                    let wave = sin((d / max(u(base + 1u), 1.0) - clip_seconds() * u(base + 2u)) * 6.2831853);
                    let dir = select(vec2f(0.0), rel / d, d > 0.001);
                    return sample_input_clamped(pos + dir * wave * u(base));
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.warp.ripple",
                "Ripple",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amplitude", Value::Float(8.0), Unit::LayerPixels).range(0.0, 100.0),
                    ParamSchema::new("wavelength", Value::Float(60.0), Unit::LayerPixels).range(4.0, 1000.0),
                    ParamSchema::new("speed", Value::Float(1.0), Unit::None).range(-10.0, 10.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_temperature",
                1,
                "fn oa_color_temperature(c: vec4f, base: u32) -> vec4f {
                    // Warmer (towards orange) or cooler (towards blue), and a green or
                    // magenta tint — the white balance, keeping the brightness.
                    let warm = clamp(u(base), -1.0, 1.0);
                    let tint = clamp(u(base + 1u), -1.0, 1.0);
                    let gain = vec3f(1.0 + 0.35 * warm, 1.0 + 0.05 * warm - 0.25 * tint, 1.0 - 0.35 * warm);
                    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    var rgb = c.rgb * gain;
                    let after = dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
                    rgb = rgb * select(1.0, luma / after, after > 1e-5);
                    return vec4f(max(rgb, vec3f(0.0)), c.a);
                }",
            ),
            ..descriptor(
                "oa.color.temperature",
                "Temperature",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("temperature", Value::Float(0.0), Unit::None).range(-1.0, 1.0),
                    ParamSchema::new("tint", Value::Float(0.0), Unit::None).range(-1.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_tile",
                1,
                "fn oa_warp_tile(pos: vec2f, base: u32) -> vec4f {
                    // The whole picture, shrunk into each cell of a grid over the layer.
                    let size = max(in_size(), vec2f(1.0));
                    let counts = max(vec2f(u(base), u(base + 1u)), vec2f(1.0));
                    let cell = (pos - in_origin()) / size * counts;
                    let index = floor(cell);
                    var uv = cell - index;
                    // A gap around each picture, as a share of its cell.
                    let gap = clamp(u(base + 2u), 0.0, 0.9) * 0.5;
                    if (any(uv < vec2f(gap)) || any(uv > vec2f(1.0 - gap))) { return vec4f(0.0); }
                    uv = (uv - gap) / (1.0 - 2.0 * gap);
                    // Mirrored: every other cell flipped, so neighbors meet edge to edge.
                    if (u(base + 3u) > 0.5) {
                        let odd = abs(index % 2.0) > vec2f(0.5);
                        uv = select(uv, 1.0 - uv, odd);
                    }
                    return sample_input(in_origin() + uv * size);
                }",
            ),
            ..descriptor(
                TILE_EFFECT,
                "Tile",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("columns", Value::Float(3.0), Unit::None).range(1.0, 20.0),
                    ParamSchema::new("rows", Value::Float(3.0), Unit::None).range(1.0, 20.0),
                    ParamSchema::new("gap", Value::Float(0.0), Unit::None).range(0.0, 0.5),
                    ParamSchema::new("mirror", Value::Bool(false), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_warp_scroll",
                1,
                "// The input at `p` (layer px from its corner) repeated forever in both
                // directions: bilinear over four texels, each wrapped round, so there's
                // no seam where the picture meets its own other edge.
                fn oa_scroll_wrapped(p: vec2f) -> vec4f {
                    let size = max(in_size(), vec2f(1.0));
                    let q = p - 0.5;
                    let i0 = floor(q);
                    let f = q - i0;
                    let a = (i0 % size + size) % size;
                    let b = ((i0 + 1.0) % size + size) % size;
                    let o = in_origin() + 0.5;
                    let top = mix(sample_input(o + vec2f(a.x, a.y)), sample_input(o + vec2f(b.x, a.y)), f.x);
                    let bottom = mix(sample_input(o + vec2f(a.x, b.y)), sample_input(o + vec2f(b.x, b.y)), f.x);
                    return mix(top, bottom, f.y);
                }

                fn oa_warp_scroll(pos: vec2f, base: u32) -> vec4f {
                    // Moving `speed` px a second towards `direction` (0 = right, 90 = down).
                    let a = radians(u(base));
                    let shift = vec2f(cos(a), sin(a)) * u(base + 1u) * clip_seconds();
                    let p = pos - shift;
                    if (u(base + 2u) > 0.5) { return sample_input(p); }
                    return oa_scroll_wrapped(p - in_origin());
                }",
            ),
            ..descriptor(
                SCROLL,
                "Scroll",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("direction", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("speed", Value::Float(120.0), Unit::LayerPixels).range(-2000.0, 2000.0),
                    ParamSchema::new("edges", Value::Enum("wrap".into()), Unit::None).options(&["wrap", "leave"]).static_only(),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_blur_zoom",
                1,
                "fn oa_blur_zoom(pos: vec2f, base: u32) -> vec4f {
                    // Smear along the line from the middle: a zoom (or spin) blur.
                    let size = max(in_size(), vec2f(1.0));
                    let mid = in_origin() + size * vec2f(u(base + 2u), u(base + 3u));
                    let rel = pos - mid;
                    let zoom = clamp(u(base), -2.0, 2.0);
                    let spin = radians(u(base + 1u));
                    let steps = 24;
                    var sum = vec4f(0.0);
                    for (var i = 0; i < steps; i++) {
                        let t = f32(i) / f32(steps - 1) - 0.5;
                        let scale = 1.0 + zoom * t * 0.25;
                        let a = spin * t;
                        let turned = vec2f(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
                        sum += sample_input_clamped(mid + turned * scale);
                    }
                    return sum / f32(steps);
                }",
            ),
            ..descriptor(
                "oa.blur.zoom",
                "Zoom Blur",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("zoom", Value::Float(0.4), Unit::None).range(-2.0, 2.0),
                    ParamSchema::new("spin", Value::Float(0.0), Unit::Degrees).range(-180.0, 180.0),
                    ParamSchema::new("center x", Value::Float(0.5), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("center y", Value::Float(0.5), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        // ---- color ----
        EffectDescriptor {
            shader: wgsl(
                "oa_color_chromatic",
                1,
                "fn oa_color_chromatic(pos: vec2f, base: u32) -> vec4f {
                    // Red and blue pulled apart along `direction`, more towards the edges.
                    let size = max(in_size(), vec2f(1.0));
                    let mid = in_origin() + size * 0.5;
                    let a = radians(u(base + 1u));
                    let dir = vec2f(cos(a), sin(a)) * u(base);
                    let falloff = mix(1.0, length((pos - mid) / size) * 2.0, clamp(u(base + 2u), 0.0, 1.0));
                    let shift = dir * falloff;
                    let r = sample_input(pos + shift);
                    let g = sample_input(pos);
                    let b = sample_input(pos - shift);
                    return vec4f(r.r, g.g, b.b, max(g.a, max(r.a, b.a)));
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.color.chromatic",
                "Chromatic Aberration",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("amount", Value::Float(3.0), Unit::LayerPixels).range(0.0, 200.0),
                    ParamSchema::new("direction", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("edges", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_hue",
                1,
                "fn oa_color_hue(c: vec4f, base: u32) -> vec4f {
                    // Turn the colour wheel: a rotation about the grey axis.
                    let a = radians(u(base));
                    let k = vec3f(0.57735, 0.57735, 0.57735);
                    let cosa = cos(a);
                    let rgb = c.rgb * cosa + cross(k, c.rgb) * sin(a) + k * dot(k, c.rgb) * (1.0 - cosa);
                    return vec4f(mix(c.rgb, rgb, clamp(u(base + 1u), 0.0, 1.0)), c.a);
                }",
            ),
            ..descriptor(
                "oa.color.hue",
                "Hue Shift",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("angle", Value::Float(30.0), Unit::Degrees).range(-360.0, 360.0),
                    ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_contrast",
                1,
                "fn oa_color_contrast(c: vec4f, base: u32) -> vec4f {
                    // Contrast about a pivot, then lift, then a gamma curve.
                    let pivot = u(base + 2u);
                    var rgb = (c.rgb - pivot) * max(u(base), 0.0) + pivot + u(base + 1u);
                    rgb = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / max(u(base + 3u), 0.01)));
                    return vec4f(rgb, c.a);
                }",
            ),
            ..descriptor(
                "oa.color.contrast",
                "Contrast",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("contrast", Value::Float(1.2), Unit::None).range(0.0, 8.0),
                    ParamSchema::new("brightness", Value::Float(0.0), Unit::None).range(-2.0, 2.0),
                    ParamSchema::new("pivot", Value::Float(0.18), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("gamma", Value::Float(1.0), Unit::None).range(0.1, 4.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_invert",
                1,
                "fn oa_color_invert(c: vec4f, base: u32) -> vec4f {
                    return vec4f(mix(c.rgb, vec3f(1.0) - c.rgb, clamp(u(base), 0.0, 1.0)), c.a);
                }",
            ),
            space: WorkingSpace::Display,
            ..descriptor(
                "oa.color.invert",
                "Invert",
                EffectKind::PointOp,
                vec![ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 1.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_color_duotone",
                1,
                "fn oa_color_duotone(c: vec4f, base: u32) -> vec4f {
                    // Brightness mapped onto two colours: shadows to one, highlights to
                    // the other.
                    let luma = clamp(dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
                    let shaped = pow(luma, max(u(base + 8u), 0.05));
                    let dark = vec3f(u(base), u(base + 1u), u(base + 2u));
                    let light = vec3f(u(base + 4u), u(base + 5u), u(base + 6u));
                    return vec4f(mix(c.rgb, mix(dark, light, shaped), clamp(u(base + 9u), 0.0, 1.0)), c.a);
                }",
            ),
            space: WorkingSpace::Display,
            ..descriptor(
                "oa.color.duotone",
                "Duotone",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("shadows", Value::Color([0.1, 0.1, 0.4, 1.0]), Unit::None),
                    ParamSchema::new("highlights", Value::Color([1.0, 0.85, 0.4, 1.0]), Unit::None),
                    ParamSchema::new("balance", Value::Float(1.0), Unit::None).range(0.1, 4.0),
                    ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        // A clip's crop (its Transform section, not the effects list): the edges outside
        // the kept rectangle become transparent, with a one-pixel soft edge.
        EffectDescriptor {
            preserves_opacity: false,
            shader: wgsl(
                "oa_internal_crop",
                1,
                "fn oa_internal_crop(c: vec4f, base: u32) -> vec4f {
                    let size = vec2f(u(base + 4u), u(base + 5u));
                    let lo = vec2f(u(base), u(base + 2u)) * size;
                    let hi = (vec2f(1.0) - vec2f(u(base + 1u), u(base + 3u))) * size;
                    let p = layer_pos();
                    let k = clamp(p - lo + 0.5, vec2f(0.0), vec2f(1.0)) * clamp(hi - p + 0.5, vec2f(0.0), vec2f(1.0));
                    return vec4f(c.rgb, c.a * k.x * k.y);
                }",
            ),
            ..descriptor(
                CROP,
                "Crop",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("left", Value::Float(0.0), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("right", Value::Float(0.0), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("top", Value::Float(0.0), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("bottom", Value::Float(0.0), Unit::None).range(0.0, 1.0),
                    // The picture's size (native px; the planner fills it in).
                    ParamSchema::new("size", Value::Vec2([1.0, 1.0]), Unit::LayerPixels),
                ],
            )
        },
        // Sequence backgrounds: a gradient fill, a tiled picture, a darkened blur.
        EffectDescriptor {
            preserves_opacity: false,
            shader: wgsl(
                "oa_internal_fill",
                1,
                "fn oa_internal_fill(c: vec4f, base: u32) -> vec4f {
                    return oa_gradient(base, layer_pos(), in_origin(), in_size());
                }",
            ),
            ..descriptor(FILL, "Fill", EffectKind::PointOp, vec![ParamSchema::new("color", Value::Gradient(Gradient::solid([0.0, 0.0, 0.0, 1.0])), Unit::None)])
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_internal_tile",
                1,
                "fn oa_internal_tile(pos: vec2f, base: u32) -> vec4f {
                    // The input is one tile; the output (larger) repeats it.
                    let s = max(in_size(), vec2f(1.0));
                    let d = pos - in_origin();
                    let q = in_origin() + d - floor(d / s) * s;
                    return sample_input(clamp(q, in_origin() + 0.5, in_origin() + s - 0.5));
                }",
            ),
            ..descriptor(TILE, "Tile", EffectKind::Spatial { expand: None }, vec![])
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_internal_dim",
                1,
                "fn oa_internal_dim(c: vec4f, base: u32) -> vec4f {
                    return vec4f(c.rgb * (1.0 - clamp(u(base), 0.0, 1.0)), c.a);
                }",
            ),
            ..descriptor(DIM, "Dim", EffectKind::PointOp, vec![ParamSchema::new("amount", Value::Float(0.0), Unit::None).range(0.0, 1.0)])
        },
        // Color management (DESIGN.md §9).
        EffectDescriptor {
            shader: wgsl("oa_internal_input", 1, INPUT_WGSL),
            ..descriptor(
                INPUT,
                "Input Color",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("transfer", Value::Float(0.0), Unit::None),
                    ParamSchema::new("matrix", Value::Vec3([1.0, 0.0, 0.0]), Unit::None),
                    ParamSchema::new("matrix_g", Value::Vec3([0.0, 1.0, 0.0]), Unit::None),
                    ParamSchema::new("matrix_b", Value::Vec3([0.0, 0.0, 1.0]), Unit::None),
                    ParamSchema::new("gain", Value::Float(1.0), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_internal_output",
                1,
                "fn oa_internal_output(c: vec4f, base: u32) -> vec4f {
                    var rgb = max(c.rgb * u(base + 1u), vec3f(0.0));
                    let mode = u32(u(base));
                    if (mode == 1u) {
                        // Soft: untouched below the knee; above it the brightest channel
                        // eases towards white and the others follow (hue kept). The
                        // rational tail meets the knee at slope 1 and keeps very bright
                        // highlights apart (an exponential would flatten them to white).
                        let knee = 0.8;
                        let m = max(rgb.r, max(rgb.g, rgb.b));
                        if (m > knee) {
                            let d = m - knee;
                            let y = knee + (1.0 - knee) * d / (d + (1.0 - knee));
                            rgb = rgb * (y / m);
                        }
                    } else if (mode == 2u) {
                        // Filmic: an ACES-like S-curve (Narkowicz's fit).
                        let x = rgb * 0.6;
                        rgb = (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
                    }
                    return vec4f(clamp(rgb, vec3f(0.0), vec3f(1.0)), c.a);
                }",
            ),
            ..descriptor(
                OUTPUT,
                "Output Color",
                EffectKind::PointOp,
                vec![ParamSchema::new("tone_map", Value::Float(0.0), Unit::None), ParamSchema::new("gain", Value::Float(1.0), Unit::None)],
            )
        },
        EffectDescriptor {
            shader: wgsl("oa_internal_to_display", 1, "fn oa_internal_to_display(c: vec4f, base: u32) -> vec4f { return vec4f(srgb_encode(c.rgb), c.a); }"),
            ..descriptor(TO_DISPLAY, "To Display", EffectKind::PointOp, vec![])
        },
        EffectDescriptor {
            shader: wgsl("oa_internal_to_linear", 1, "fn oa_internal_to_linear(c: vec4f, base: u32) -> vec4f { return vec4f(srgb_decode(c.rgb), c.a); }"),
            ..descriptor(TO_LINEAR, "To Linear", EffectKind::PointOp, vec![])
        },
        EffectDescriptor {
            space: WorkingSpace::Display,
            preserves_opacity: false,
            shader: wgsl(
                "oa_key_chroma",
                1,
                "fn oa_key_chroma(c: vec4f, base: u32) -> vec4f {
                    // Pixels near the key color (by chroma, so shadows on a green screen
                    // key too) become transparent; spill pulls the key color out of edges.
                    let key = srgb_encode(vec3f(u(base), u(base + 1u), u(base + 2u)));
                    let chroma = mat3x2f(vec2f(-0.1146, 0.5), vec2f(-0.3854, -0.4542), vec2f(0.5, -0.0458));
                    // Chroma divided by brightness: the same green in shadow keys the same.
                    let luma = vec3f(0.2126, 0.7152, 0.0722);
                    let d = distance(chroma * c.rgb / (dot(c.rgb, luma) + 0.15), chroma * key / (dot(key, luma) + 0.15));
                    let tol = u(base + 4u);
                    let soft = max(u(base + 5u), 1e-4);
                    let keep = smoothstep(tol, tol + soft, d);
                    // Spill: near the key, cap the key's dominant channel at the others' level.
                    let near = 1.0 - smoothstep(tol, tol + soft * 3.0, d);
                    var rgb = c.rgb;
                    if (key.g >= key.r && key.g >= key.b) {
                        rgb.g = mix(rgb.g, min(rgb.g, max(rgb.r, rgb.b)), u(base + 6u) * near);
                    } else if (key.b >= key.r) {
                        rgb.b = mix(rgb.b, min(rgb.b, max(rgb.r, rgb.g)), u(base + 6u) * near);
                    } else {
                        rgb.r = mix(rgb.r, min(rgb.r, max(rgb.g, rgb.b)), u(base + 6u) * near);
                    }
                    return vec4f(rgb, c.a * keep);
                }",
            ),
            ..descriptor(
                "oa.key.chroma",
                "Chroma Key",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("key", Value::Color([0.0, 1.0, 0.0, 1.0]), Unit::None),
                    ParamSchema::new("tolerance", Value::Float(0.12), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("softness", Value::Float(0.08), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("spill", Value::Float(0.7), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        // ---- passive motion: the layer moves on its own ----
        EffectDescriptor {
            time_varying: true,
            motion: Some(motion::shake),
            ..descriptor(
                "oa.motion.shake",
                "Camera Shake",
                EffectKind::Motion,
                vec![
                    ParamSchema::new("intensity", Value::Float(0.012), Unit::None).range(0.0, 0.5),
                    ParamSchema::new("frequency", Value::Float(12.0), Unit::None).range(0.1, 120.0),
                    ParamSchema::new("rotation", Value::Float(0.6), Unit::Degrees).range(0.0, 90.0),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            motion: Some(motion::wiggle),
            ..descriptor(
                "oa.motion.wiggle",
                "Wiggle",
                EffectKind::Motion,
                vec![
                    ParamSchema::new("amount", Value::Float(0.02), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("speed", Value::Float(1.5), Unit::None).range(0.1, 10.0),
                    ParamSchema::new("rotation", Value::Float(3.0), Unit::Degrees).range(0.0, 180.0),
                ],
            )
        },
        // ---- clip intros/outros: written against visibility, so each one plays
        // forwards as an intro and backwards as an outro ----
        EffectDescriptor {
            usage: EffectUsage::InOut,
            motion: Some(motion::fade),
            ..descriptor("oa.anim.fade", "Fade", EffectKind::Motion, vec![])
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            motion: Some(motion::fly),
            ..descriptor(
                "oa.anim.fly",
                "Fly",
                EffectKind::Motion,
                vec![
                    // 0° = moving right (in from the left edge).
                    ParamSchema::new("direction", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("distance", Value::Float(1.0), Unit::None).range(0.05, 1.5),
                    ParamSchema::new("fade", Value::Bool(false), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            motion: Some(motion::zoom),
            ..descriptor(
                "oa.anim.zoom",
                "Zoom",
                EffectKind::Motion,
                vec![
                    ParamSchema::new("scale", Value::Float(0.6), Unit::None).range(0.0, 4.0),
                    ParamSchema::new("fade", Value::Bool(true), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_anim_blur",
                2,
                "fn oa_anim_blur(pos: vec2f, base: u32) -> vec4f {
                    // Fully blurred (by `radius`) when hidden, sharp when visible.
                    let radius = u(base) * (1.0 - visibility());
                    if (radius < 0.5) { return sample_input(pos); }
                    let dir = select(vec2f(0.0, 1.0), vec2f(1.0, 0.0), pass_index() == 0u);
                    let sigma = radius / 2.5;
                    let step = max(1.0, radius / 24.0);
                    let n = i32(ceil(radius / step));
                    var sum = vec4f(0.0);
                    var wsum = 0.0;
                    for (var i = -n; i <= n; i++) {
                        let x = f32(i) * step;
                        let w = exp(-0.5 * x * x / (sigma * sigma));
                        sum += sample_input(pos + dir * x) * w;
                        wsum += w;
                    }
                    return sum / wsum;
                }",
            ),
            ..descriptor(
                "oa.anim.blur",
                "Defocus",
                EffectKind::Spatial { expand: None },
                vec![ParamSchema::new("radius", Value::Float(40.0), Unit::LayerPixels).range(0.0, 500.0)],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_anim_reveal",
                1,
                "fn oa_anim_reveal(pos: vec2f, base: u32) -> vec4f {
                    // A soft edge sweeps across the layer along `angle`, uncovering it.
                    let a = radians(u(base));
                    let dir = vec2f(cos(a), sin(a));
                    let soft = max(u(base + 1u), 0.001);
                    let lo = in_origin();
                    let hi = in_origin() + in_size();
                    let start = min(dir.x * lo.x, dir.x * hi.x) + min(dir.y * lo.y, dir.y * hi.y) - soft;
                    let reach = abs(dir.x) * in_size().x + abs(dir.y) * in_size().y + 2.0 * soft;
                    let edge = start + visibility() * reach;
                    let shown = 1.0 - smoothstep(edge - soft, edge + soft, dot(pos, dir));
                    return sample_input(pos) * shown;
                }",
            ),
            ..descriptor(
                "oa.anim.reveal",
                "Wipe",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("angle", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("softness", Value::Float(30.0), Unit::LayerPixels).range(0.0, 500.0),
                ],
            )
        },
        // ---- text, per letter: run once per glyph in the text pass ----
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_text_wiggle",
                1,
                "fn oa_text_wiggle(g_in: Glyph, base: u32) -> Glyph {
                    // Each letter wanders on its own smooth noise path.
                    var g = g_in;
                    let t = clip_seconds() * u(base + 1u);
                    let i = g.index * 7.31;
                    g.offset += vec2f(oa_noise(i, t), oa_noise(i + 3.7, t)) * u(base) * g.em;
                    g.rotation += oa_noise(i + 9.1, t) * u(base + 2u);
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.wiggle",
                "Letter Wiggle",
                EffectKind::Glyph { expand: Some(ParamId::new("amount")) },
                vec![
                    ParamSchema::new("amount", Value::Float(0.06), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("speed", Value::Float(2.0), Unit::None).range(0.0, 40.0),
                    ParamSchema::new("rotation", Value::Float(8.0), Unit::Degrees).range(0.0, 90.0),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_text_wave",
                1,
                "fn oa_text_wave(g_in: Glyph, base: u32) -> Glyph {
                    // A sine wave travelling along the letters.
                    var g = g_in;
                    let phase = (g.index / max(u(base + 2u), 1.0) - clip_seconds() * u(base + 1u)) * 6.2831853;
                    g.offset.y += sin(phase) * u(base) * g.em;
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.wave",
                "Wave",
                EffectKind::Glyph { expand: Some(ParamId::new("height")) },
                vec![
                    ParamSchema::new("height", Value::Float(0.2), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("speed", Value::Float(1.0), Unit::None).range(0.0, 5.0),
                    ParamSchema::new("length", Value::Float(8.0), Unit::None).range(1.0, 40.0),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_text_rainbow",
                1,
                "fn oa_text_rainbow(g_in: Glyph, base: u32) -> Glyph {
                    // Each letter a hue, cycling over time.
                    var g = g_in;
                    let h = fract(g.index * u(base + 1u) + clip_seconds() * u(base));
                    let rgb = srgb_decode(oa_hsv(h, 0.8, 1.0));
                    g.color = vec4f(mix(g.color.rgb, g.color.rgb * rgb, u(base + 2u)), g.color.a);
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.rainbow",
                "Rainbow Letters",
                EffectKind::Glyph { expand: None },
                vec![
                    ParamSchema::new("speed", Value::Float(0.3), Unit::None).range(0.0, 4.0),
                    ParamSchema::new("spread", Value::Float(0.08), Unit::None).range(0.0, 0.5),
                    ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_text_typewriter",
                1,
                "fn oa_text_typewriter(g_in: Glyph, base: u32) -> Glyph {
                    // Letters appear one at a time.
                    var g = g_in;
                    let shown = visibility() * g.count;
                    g.color.a *= clamp((shown - g.index) * 3.0, 0.0, 1.0);
                    return g;
                }",
            ),
            ..descriptor("oa.text.typewriter", "Typewriter", EffectKind::Glyph { expand: None }, vec![])
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_text_rise",
                1,
                "fn oa_text_rise(g_in: Glyph, base: u32) -> Glyph {
                    // Letter by letter, each rises into place (and fades in).
                    var g = g_in;
                    let p = oa_ease_out(letter_progress(g, u(base + 1u)));
                    g.offset.y += (1.0 - p) * u(base) * g.em;
                    if (u(base + 2u) > 0.5) { g.color.a *= p; }
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.rise",
                "Letters Rise",
                EffectKind::Glyph { expand: Some(ParamId::new("distance")) },
                vec![
                    ParamSchema::new("distance", Value::Float(0.6), Unit::None).range(-3.0, 3.0),
                    ParamSchema::new("stagger", Value::Float(0.6), Unit::None).range(0.0, 0.95),
                    ParamSchema::new("fade", Value::Bool(true), Unit::None),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_text_pop",
                1,
                "fn oa_text_pop(g_in: Glyph, base: u32) -> Glyph {
                    // Letter by letter, each grows from nothing with a little overshoot.
                    var g = g_in;
                    let p = letter_progress(g, u(base));
                    g.scale *= vec2f(max(oa_ease_back(p, u(base + 1u)), 0.0));
                    g.color.a *= clamp(p * 4.0, 0.0, 1.0);
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.pop",
                "Letters Pop",
                EffectKind::Glyph { expand: None },
                vec![
                    ParamSchema::new("stagger", Value::Float(0.6), Unit::None).range(0.0, 0.95),
                    ParamSchema::new("overshoot", Value::Float(1.7), Unit::None).range(0.0, 4.0),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_text_scatter",
                1,
                "fn oa_text_scatter(g_in: Glyph, base: u32) -> Glyph {
                    // Letters fly together from random directions, spinning into place.
                    var g = g_in;
                    let p = oa_ease_out(letter_progress(g, u(base + 2u)));
                    let a = oa_hash(g.index * 1.7 + 0.3) * 6.2831853;
                    let r = (0.4 + 0.6 * oa_hash(g.index * 3.1 + 1.1)) * u(base) * g.em;
                    g.offset += vec2f(cos(a), sin(a)) * r * (1.0 - p);
                    g.rotation += (oa_hash(g.index * 5.3) * 2.0 - 1.0) * u(base + 1u) * (1.0 - p);
                    g.color.a *= p;
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.scatter",
                "Letters Scatter",
                EffectKind::Glyph { expand: Some(ParamId::new("distance")) },
                vec![
                    ParamSchema::new("distance", Value::Float(2.0), Unit::None).range(0.0, 6.0),
                    ParamSchema::new("spin", Value::Float(180.0), Unit::Degrees).range(0.0, 720.0),
                    ParamSchema::new("stagger", Value::Float(0.3), Unit::None).range(0.0, 0.95),
                ],
            )
        },
        EffectDescriptor {
            usage: EffectUsage::InOut,
            shader: wgsl(
                "oa_text_fade",
                1,
                "fn oa_text_fade(g_in: Glyph, base: u32) -> Glyph {
                    // Letter by letter, each fades in.
                    var g = g_in;
                    g.color.a *= letter_progress(g, u(base));
                    return g;
                }",
            ),
            ..descriptor(
                "oa.text.fade",
                "Letter Fade",
                EffectKind::Glyph { expand: None },
                vec![ParamSchema::new("stagger", Value::Float(0.7), Unit::None).range(0.0, 0.95)],
            )
        },
        // ---- text, per pixel: run for every pixel of the text pass ----
        EffectDescriptor {
            shader: wgsl(
                "oa_text_glow",
                1,
                "fn oa_text_glow(c: vec4f, p: TextPixel, base: u32) -> vec4f {
                    // A soft halo around the letters, from the distance to their outline.
                    // Its color is a directional gradient across the text box.
                    let col = oa_gradient(base, p.pos, vec2f(0.0), p.box_size);
                    let r = max(u(base + 32u) * p.em, 0.5);
                    let out = max(-p.dist, 0.0);
                    let edge = 1.0 - smoothstep(0.6 * p.spread, p.spread, out);
                    let g = exp(-2.5 * out / r) * col.a * u(base + 33u) * edge;
                    let a = c.a + g * (1.0 - c.a);
                    let rgb = (c.rgb * c.a + col.rgb * g * (1.0 - c.a)) / max(a, 1e-5);
                    return vec4f(rgb, a);
                }",
            ),
            ..descriptor(
                "oa.text.glow",
                "Glow",
                EffectKind::GlyphPixel,
                vec![
                    ParamSchema::new("color", Value::Gradient(Gradient::solid([1.0, 0.8, 0.3, 1.0])), Unit::None),
                    ParamSchema::new("size", Value::Float(0.1), Unit::None).range(0.0, 0.2),
                    ParamSchema::new("strength", Value::Float(1.0), Unit::None).range(0.0, 2.0),
                ],
            )
        },
        // ---- text, behind the letters ----
        EffectDescriptor {
            shader: wgsl(
                "oa_text_background",
                1,
                "fn oa_text_background(b: TextBox, base: u32) -> vec4f {
                    // A rounded box behind the whole text, each line or each word
                    // (`shape`), padded by a share of the text size.
                    if (abs(b.kind - u(base)) > 0.5) { return vec4f(0.0); }
                    let color = vec4f(u(base + 1u), u(base + 2u), u(base + 3u), u(base + 4u));
                    let half = b.size * 0.5 + vec2f(u(base + 5u), u(base + 5u) * 0.5) * b.em;
                    let r = clamp(u(base + 6u) * b.em, 0.0, min(half.x, half.y));
                    let q = abs(b.pos - b.center) - half + vec2f(r);
                    let d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
                    let cover = clamp(0.5 - d, 0.0, 1.0);
                    return vec4f(color.rgb, color.a * cover);
                }",
            ),
            ..descriptor(
                "oa.text.background",
                "Background",
                EffectKind::TextBox,
                vec![
                    ParamSchema::new("shape", Value::Enum("lines".into()), Unit::None).options(&["text", "lines", "words"]),
                    ParamSchema::new("color", Value::Color([0.0, 0.0, 0.0, 0.75]), Unit::None),
                    ParamSchema::new("padding", Value::Float(0.3), Unit::None).range(0.0, 1.5),
                    ParamSchema::new("roundness", Value::Float(0.25), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        // ---- effects with non-numeric properties ----
        EffectDescriptor {
            shader: wgsl(
                "oa_color_tint",
                1,
                "fn oa_color_tint(c: vec4f, base: u32) -> vec4f {
                    // The color (a directional gradient across the layer) takes over the
                    // picture's hue, keeping its brightness.
                    let g = oa_gradient(base, layer_pos(), in_origin(), in_size());
                    let luma = dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    return vec4f(mix(c.rgb, g.rgb * luma, u(base + 32u) * g.a), c.a);
                }",
            ),
            ..descriptor(
                "oa.color.tint",
                "Tint",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("color", Value::Gradient(Gradient::solid([1.0, 0.6, 0.2, 1.0])), Unit::None),
                    ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            time_varying: true,
            shader: wgsl(
                "oa_light_shimmer",
                1,
                "fn oa_light_shimmer(c: vec4f, base: u32) -> vec4f {
                    // A band of light sweeps across the layer along `direction`, again and
                    // again. Where the layer is transparent there's nothing to light.
                    let r = radians(u(base + 2u));
                    let dir = vec2f(cos(r), sin(r));
                    let rel = (layer_pos() - in_origin()) / max(in_size(), vec2f(1.0)) - 0.5;
                    let x = dot(rel, dir) / max(abs(dir.x) + abs(dir.y), 1e-4) + 0.5;
                    let t = fract(clip_seconds() * u(base)) * 1.8 - 0.4;
                    let band = 1.0 - smoothstep(0.0, max(u(base + 1u), 0.001), abs(x - t));
                    let light = vec3f(u(base + 3u), u(base + 4u), u(base + 5u)) * u(base + 6u);
                    return vec4f(c.rgb + light * band * u(base + 7u), c.a);
                }",
            ),
            ..descriptor(
                "oa.light.shimmer",
                "Shimmer",
                EffectKind::PointOp,
                vec![
                    ParamSchema::new("speed", Value::Float(0.6), Unit::None).range(0.0, 4.0),
                    ParamSchema::new("width", Value::Float(0.12), Unit::None).range(0.01, 1.0),
                    ParamSchema::new("direction", Value::Float(20.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("color", Value::Color([1.0, 1.0, 1.0, 1.0]), Unit::None),
                    ParamSchema::new("strength", Value::Float(0.8), Unit::None).range(0.0, 8.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_depth_slab",
                1,
                "// How thick the sheet is at a point, as a fraction of the full thickness:
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
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                DEPTH,
                "Depth",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("depth", Value::Float(8.0), Unit::None).range(0.0, 200.0),
                    // All the way round, either way; the output grows to fit the turned
                    // sheet (`grown_bounds`).
                    ParamSchema::new("yaw", Value::Float(25.0), Unit::Degrees).range(-180.0, 180.0),
                    ParamSchema::new("pitch", Value::Float(0.0), Unit::Degrees).range(-180.0, 180.0),
                    ParamSchema::new("pov", Value::Float(0.35), Unit::None).range(0.0, 1.0),
                    ParamSchema::new("map", Value::Media(None), Unit::None).static_only(),
                    ParamSchema::new("back", Value::Float(0.45), Unit::None).range(0.0, 1.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_warp_surface",
                1,
                "fn oa_surface_cross(a: vec2f, b: vec2f) -> f32 { return a.x * b.y - a.y * b.x; }

                // u from v, where h = u (e + g v) + f v; solved on the steadier axis.
                fn oa_surface_u(h: vec2f, e: vec2f, f: vec2f, g: vec2f, v: f32) -> f32 {
                    let den = e + g * v;
                    if (abs(den.x) >= abs(den.y)) {
                        if (abs(den.x) < 1e-9) { return -1.0; }
                        return (h.x - f.x * v) / den.x;
                    }
                    return (h.y - f.y * v) / den.y;
                }

                // Where `p` is in the quad a b c d (clockwise from the top left), as
                // (u, v) — both in 0..1 when it's inside. Bilinear interpolation run
                // backwards (after Inigo Quilez).
                fn oa_surface_unmap(p: vec2f, a: vec2f, b: vec2f, c: vec2f, d: vec2f) -> vec2f {
                    let e = b - a;
                    let f = d - a;
                    let g = a - b + c - d;
                    let h = p - a;
                    let k2 = oa_surface_cross(g, f);
                    let k1 = oa_surface_cross(e, f) + oa_surface_cross(h, g);
                    let k0 = oa_surface_cross(h, e);
                    if (abs(k2) < 1e-7) {
                        if (abs(k1) < 1e-9) { return vec2f(-1.0); }
                        let v = -k0 / k1;
                        return vec2f(oa_surface_u(h, e, f, g, v), v);
                    }
                    let w = k1 * k1 - 4.0 * k0 * k2;
                    if (w < 0.0) { return vec2f(-1.0); }
                    let s = sqrt(w);
                    var v = (-k1 - s) / (2.0 * k2);
                    var u = oa_surface_u(h, e, f, g, v);
                    if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {
                        v = (-k1 + s) / (2.0 * k2);
                        u = oa_surface_u(h, e, f, g, v);
                    }
                    return vec2f(u, v);
                }

                // A point of the n × n grid, in fractions of the layer: where it rests
                // plus its offset (params are stored for a 4 × 4 grid).
                fn oa_surface_point(base: u32, n: u32, r: u32, c: u32) -> vec2f {
                    let k = base + 1u + 2u * (r * 4u + c);
                    return vec2f(f32(c), f32(r)) / f32(n - 1u) + vec2f(u(k), u(k + 1u));
                }

                // The picture at `p` (fractions of the layer): found in whichever cell
                // of the grid covers it.
                fn oa_surface_at(p: vec2f, base: u32, n: u32) -> vec4f {
                    let size = max(in_size(), vec2f(1.0));
                    for (var r = 0u; r + 1u < n; r++) {
                        for (var c = 0u; c + 1u < n; c++) {
                            let uv = oa_surface_unmap(
                                p,
                                oa_surface_point(base, n, r, c),
                                oa_surface_point(base, n, r, c + 1u),
                                oa_surface_point(base, n, r + 1u, c + 1u),
                                oa_surface_point(base, n, r + 1u, c),
                            );
                            if (all(uv >= vec2f(-1e-4)) && all(uv <= vec2f(1.0 + 1e-4))) {
                                let src = (vec2f(f32(c), f32(r)) + clamp(uv, vec2f(0.0), vec2f(1.0))) / f32(n - 1u);
                                return sample_input(in_origin() + src * size);
                            }
                        }
                    }
                    return vec4f(0.0);
                }

                fn oa_warp_surface(pos: vec2f, base: u32) -> vec4f {
                    let size = max(in_size(), vec2f(1.0));
                    let n = u32(clamp(u(base), 0.0, 2.0)) + 2u;
                    // Four samples a pixel, so the stretched edges stay smooth.
                    var sum = vec4f(0.0);
                    for (var i = 0u; i < 4u; i++) {
                        let offset = vec2f(f32(i % 2u), f32(i / 2u)) * 0.5 - 0.25;
                        sum += oa_surface_at((pos + offset - in_origin()) / size, base, n);
                    }
                    return sum * 0.25;
                }",
            ),
            preserves_opacity: false,
            ..descriptor(SURFACE, "Surface", EffectKind::Spatial { expand: None }, {
                let mut params = vec![ParamSchema::new("grid", Value::Enum(SURFACE_GRIDS[0].into()), Unit::None).options(&SURFACE_GRIDS).static_only()];
                for i in 0..16 {
                    params.push(ParamSchema::new(&surface_point_id(i / 4, i % 4), Value::Vec2([0.0, 0.0]), Unit::SourceFraction));
                }
                params
            })
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_mask_media",
                1,
                "fn oa_mask_media(pos: vec2f, base: u32) -> vec4f {
                    // The matte comes from another file in the pool, stretched over the
                    // layer: its luminance (or alpha) decides what shows.
                    let c = sample_input(pos);
                    if (u(base) < 0.5) { return c; }
                    let m = sample_media(pos);
                    let luma = dot(m.rgb, vec3f(0.2126, 0.7152, 0.0722));
                    var k = select(luma, m.a, u(base + 1u) > 0.5);
                    if (u(base + 2u) > 0.5) { k = 1.0 - k; }
                    return c * clamp(k, 0.0, 1.0);
                }",
            ),
            preserves_opacity: false,
            ..descriptor(
                "oa.mask.media",
                "Mask",
                EffectKind::Spatial { expand: None },
                vec![
                    ParamSchema::new("matte", Value::Media(None), Unit::None).static_only(),
                    ParamSchema::new("use", Value::Enum("luma".into()), Unit::None).options(&["luma", "alpha"]).static_only(),
                    ParamSchema::new("invert", Value::Bool(false), Unit::None).static_only(),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_crossfade",
                1,
                "fn oa_transition_crossfade(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    return mix(sample_a(pos), sample_b(pos), progress);
                }",
            ),
            ..transition("oa.transition.crossfade", "Cross Dissolve", vec![])
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_dip",
                1,
                "fn oa_transition_dip(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    let c = vec4f(u(base), u(base + 1u), u(base + 2u), u(base + 3u));
                    let color = vec4f(c.rgb * c.a, c.a);
                    if (progress < 0.5) { return mix(sample_a(pos), color, progress * 2.0); }
                    return mix(color, sample_b(pos), progress * 2.0 - 1.0);
                }",
            ),
            ..transition(
                "oa.transition.dip",
                "Dip to Color",
                vec![ParamSchema::new("color", Value::Color([0.0, 0.0, 0.0, 1.0]), Unit::None)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_wipe",
                1,
                "fn oa_transition_wipe(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // A soft edge sweeps across the canvas along `angle` (0 = left to right).
                    let a = radians(u(base));
                    let dir = vec2f(cos(a), sin(a));
                    let size = out_size();
                    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
                    let soft = max(u(base + 1u), 0.001);
                    let start = min(0.0, dir.x * size.x) + min(0.0, dir.y * size.y) - soft;
                    let edge = start + progress * (reach + 2.0 * soft);
                    let t = smoothstep(edge - soft, edge + soft, dot(pos, dir));
                    return mix(sample_b(pos), sample_a(pos), t);
                }",
            ),
            ..transition(
                "oa.transition.wipe",
                "Wipe",
                vec![
                    ParamSchema::new("angle", Value::Float(0.0), Unit::Direction).range(0.0, 360.0),
                    ParamSchema::new("softness", Value::Float(40.0), Unit::LayerPixels).range(0.0, 500.0),
                ],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_push",
                1,
                "fn oa_transition_push(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // The incoming picture pushes the outgoing one off the canvas, both
                    // moving along `direction` (0 = right, 90 = down).
                    let r = radians(u(base));
                    let dir = vec2f(cos(r), sin(r));
                    // A full canvas along dir, stretched for diagonals so both axes clear.
                    let size = out_size();
                    let reach = (abs(dir.x) * size.x + abs(dir.y) * size.y) / max(max(abs(dir.x), abs(dir.y)), 1e-6);
                    let e = progress * progress * (3.0 - 2.0 * progress);
                    let shift = dir * reach * e;
                    let a = sample_a(pos - shift);
                    let b = sample_b(pos - shift + dir * reach);
                    return b + a * (1.0 - b.a);
                }",
            ),
            ..transition(
                "oa.transition.push",
                "Push",
                // 180° = the pictures move left.
                vec![ParamSchema::new("direction", Value::Float(180.0), Unit::Direction).range(0.0, 360.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_iris",
                1,
                "fn oa_transition_iris(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // A circle opens from the middle, the incoming picture inside it.
                    let size = out_size();
                    let soft = max(u(base), 0.001);
                    let reach = 0.5 * length(size) + soft;
                    let r = progress * (reach + soft) - soft;
                    let t = smoothstep(r - soft, r + soft, length(pos - 0.5 * size));
                    return mix(sample_b(pos), sample_a(pos), t);
                }",
            ),
            ..transition(
                "oa.transition.iris",
                "Iris",
                vec![ParamSchema::new("softness", Value::Float(24.0), Unit::LayerPixels).range(0.0, 400.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_zoom",
                1,
                "fn oa_transition_zoom(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // The outgoing picture rushes towards you as the incoming one settles
                    // in from close up, crossing over in the middle.
                    let c = 0.5 * out_size();
                    let k = max(u(base), 0.0);
                    let a = sample_a(c + (pos - c) / (1.0 + progress * k));
                    let b = sample_b(c + (pos - c) / (1.0 + (1.0 - progress) * k));
                    return mix(a, b, smoothstep(0.3, 0.7, progress));
                }",
            ),
            ..transition(
                "oa.transition.zoom",
                "Zoom",
                vec![ParamSchema::new("amount", Value::Float(1.0), Unit::None).range(0.0, 4.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_slide",
                1,
                "fn oa_transition_slide(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // The incoming picture slides in over the outgoing one, travelling
                    // towards `direction` (0 = right, 90 = down), easing to a stop.
                    let a = radians(u(base));
                    let dir = vec2f(cos(a), sin(a));
                    let size = out_size();
                    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
                    let e = 1.0 - pow(1.0 - progress, 3.0);
                    let b = sample_b(pos + dir * (1.0 - e) * reach);
                    return b + sample_a(pos) * (1.0 - b.a);
                }",
            ),
            ..transition(
                "oa.transition.slide",
                "Slide",
                vec![ParamSchema::new("direction", Value::Float(0.0), Unit::Direction).range(0.0, 360.0)],
            )
        },
        EffectDescriptor {
            shader: wgsl(
                "oa_transition_blur",
                1,
                "fn oa_transition_blur(pos: vec2f, progress: f32, base: u32) -> vec4f {
                    // Both pictures go out of focus towards the middle, dissolving there.
                    let radius = max(u(base), 0.0) * sin(progress * 3.1415927);
                    var a = sample_a(pos);
                    var b = sample_b(pos);
                    var weight = 1.0;
                    for (var ring = 1; ring <= 2; ring++) {
                        let r = radius * f32(ring) * 0.5;
                        for (var k = 0; k < 8; k++) {
                            let t = f32(k) * 0.7853982 + f32(ring) * 0.39;
                            let p = pos + vec2f(cos(t), sin(t)) * r;
                            a += sample_a(p);
                            b += sample_b(p);
                            weight += 1.0;
                        }
                    }
                    return mix(a, b, smoothstep(0.2, 0.8, progress)) / weight;
                }",
            ),
            ..transition(
                "oa.transition.blur",
                "Blur Dissolve",
                vec![ParamSchema::new("radius", Value::Float(40.0), Unit::LayerPixels).range(0.0, 400.0)],
            )
        },
        EffectDescriptor {
            state: Statefulness::Stateful { preroll: Time::from_seconds(2) },
            fusible: false,
            ..descriptor(
                "oa.time.feedback-trail",
                "Feedback Trail",
                EffectKind::Temporal { frames_before: 1, frames_after: 0 },
                vec![ParamSchema::new("decay", Value::Float(0.85), Unit::None).range(0.0, 1.0)],
            )
        },
    ]
}

/// The built-in motion effects.
mod motion {
    use super::{Motion, MotionInput};
    use oa_params::{Evaluated, Value};

    /// Ease-out cubic: fast at first, settling gently (intros land softly; outros,
    /// running backwards, leave slowly then quickly).
    fn ease(v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        1.0 - (1.0 - v).powi(3)
    }

    fn flag(v: &Evaluated, id: &str) -> bool {
        matches!(v.get(id), Some(Value::Bool(true)))
    }

    /// Smooth value noise in [-1, 1].
    fn noise(seed: u64, x: f64) -> f64 {
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

    /// Two octaves: a jitter with some body.
    fn jitter(seed: u64, x: f64) -> f64 {
        (noise(seed, x) + 0.5 * noise(seed ^ 0x5bd1_e995, x * 2.0)) / 1.5
    }

    pub fn fade(_: &Evaluated, i: &MotionInput) -> Motion {
        Motion { opacity: i.visibility.clamp(0.0, 1.0), ..Motion::NONE }
    }

    /// Travels along `direction` (degrees, 0 = right, 90 = down): an intro arrives
    /// moving that way (from the opposite side), an outro leaves moving that way.
    /// `distance` 1 takes the layer a full canvas off, whatever the angle.
    pub fn fly(v: &Evaluated, i: &MotionInput) -> Motion {
        let a = v.float("direction").to_radians();
        let dir = [a.cos(), a.sin()];
        // Far enough along `dir` to clear the canvas: its extent along that direction,
        // stretched for diagonals so both axes clear.
        let reach = (dir[0].abs() * i.canvas[0] + dir[1].abs() * i.canvas[1]) / dir[0].abs().max(dir[1].abs()).max(1e-6);
        let away = (1.0 - ease(i.visibility)) * reach * v.float("distance") * if i.leaving { 1.0 } else { -1.0 };
        Motion {
            offset: [dir[0] * away, dir[1] * away],
            opacity: if flag(v, "fade") { ease(i.visibility) } else { 1.0 },
            ..Motion::NONE
        }
    }

    /// Grows from (or shrinks to) `scale` times the layer's size.
    pub fn zoom(v: &Evaluated, i: &MotionInput) -> Motion {
        let e = ease(i.visibility);
        let from = v.float("scale").max(0.0);
        Motion {
            scale: (from + (1.0 - from) * e).max(1e-4),
            opacity: if flag(v, "fade") { e } else { 1.0 },
            ..Motion::NONE
        }
    }

    /// Quick, jittery handheld-camera movement.
    pub fn shake(v: &Evaluated, i: &MotionInput) -> Motion {
        let x = i.seconds * v.float("frequency");
        let amp = v.float("intensity") * i.canvas[0].min(i.canvas[1]);
        Motion {
            offset: [jitter(i.seed, x) * amp, jitter(i.seed.wrapping_add(1), x) * amp],
            rotation: jitter(i.seed.wrapping_add(2), x) * v.float("rotation"),
            // A touch of zoom hides the edges the shake would otherwise expose.
            scale: 1.0 + 2.0 * v.float("intensity"),
            ..Motion::NONE
        }
    }

    /// Slow, smooth wandering (single octave): playful drift for titles and stickers.
    pub fn wiggle(v: &Evaluated, i: &MotionInput) -> Motion {
        let x = i.seconds * v.float("speed");
        let amp = v.float("amount") * i.canvas[0].min(i.canvas[1]);
        Motion {
            offset: [noise(i.seed, x) * amp, noise(i.seed.wrapping_add(1), x) * amp],
            rotation: noise(i.seed.wrapping_add(2), x) * v.float("rotation"),
            ..Motion::NONE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_params::{EvalContext, ParamSet, ParamSource};

    /// A surface point pulled outside, or a sheet turned towards the camera, gets room
    /// to draw; at rest, both keep the layer's box.
    #[test]
    fn surface_and_depth_grow_to_fit_their_shape() {
        let r = Registry::with_builtins();
        let at = EvalContext::at(Time::ZERO, Time::ZERO);
        let layer = Rect::from_size(100.0, 50.0);

        let surface = r.effect(SURFACE).unwrap();
        let mut set = ParamSet::default();
        assert_eq!(surface.grown_bounds(&set.eval(&surface.params, None, &at), 1.0, layer), layer);
        set.set(&surface_point_id(1, 1), ParamSource::Static(Value::Vec2([0.5, -0.2])));
        let v = set.eval(&surface.params, None, &at);
        assert_eq!(surface_points(&v).1[3], [1.5, 0.8]);
        assert_eq!(surface.grown_bounds(&v, 1.0, layer), Rect::new(0.0, 0.0, 150.0, 50.0));

        let depth = r.effect(DEPTH).unwrap();
        let mut set = ParamSet::default();
        set.set("yaw", ParamSource::Static(Value::Float(0.0)));
        set.set("depth", ParamSource::Static(Value::Float(0.0)));
        let flat = depth.grown_bounds(&set.eval(&depth.params, None, &at), 1.0, layer);
        assert!((flat.x0 - 0.0).abs() < 1.0 && (flat.x1 - 100.0).abs() < 1.0, "{flat:?}");
        set.set("yaw", ParamSource::Static(Value::Float(60.0)));
        set.set("pov", ParamSource::Static(Value::Float(1.0)));
        let turned = depth.grown_bounds(&set.eval(&depth.params, None, &at), 1.0, layer);
        assert!(turned.y0 < -1.0 && turned.y1 > 51.0, "turned towards the camera, it reaches past the layer: {turned:?}");
    }

    /// Glow runs one flood pass per halving of its radius (plus finding the seeds and
    /// drawing): a small glow is cheap, a wide one costs a few passes more.
    #[test]
    fn glow_passes_grow_with_the_log_of_its_radius() {
        assert_eq!(glow_jumps(0.0), 1);
        assert_eq!(glow_jumps(1.0), 1);
        assert_eq!(glow_jumps(2.0), 2);
        assert_eq!(glow_jumps(30.0), 6, "steps 32, 16, 8, 4, 2, 1");
        assert_eq!(glow_jumps(32.0), 6);
        assert_eq!(glow_jumps(1e9), 12, "capped");
        let glow = Registry::with_builtins().effect(GLOW).unwrap().clone();
        // 30 px: a 2 × 2-pixel grid, so 15 cells of flood: 5 jumps.
        assert_eq!(glow_divisor(30.0), 2);
        assert_eq!(glow_divisor(10.0), 1);
        assert_eq!(glow_divisor(400.0), 8, "capped");
        assert_eq!((glow.pass_count.unwrap())(&[30.0, 1.0]), 7);
        let divisor = glow.pass_divisor.unwrap();
        assert_eq!((divisor(&[30.0], 0), divisor(&[30.0], 5), divisor(&[30.0], 6)), (2, 2, 1), "the last pass draws at full size");
    }

    #[test]
    fn layer_pixel_params_scale_with_raster() {
        let r = Registry::with_builtins();
        let blur = r.effect("oa.blur.gaussian").unwrap();
        let mut set = ParamSet::default();
        set.set("radius", ParamSource::Static(Value::Float(10.0)));
        let v = set.eval(&blur.params, None, &EvalContext::at(Time::ZERO, Time::ZERO));
        // radius scales with the raster; the edge mode packs as its option index.
        assert_eq!(blur.pack_uniforms(&v, 0.5), vec![5.0, 1.0]);
        assert_eq!(blur.expand_px(&v, 0.25), 0.0, "blur keeps the clip's size");
        set.set("edges", ParamSource::Static(Value::Enum("transparent".into())));
        let v = set.eval(&blur.params, None, &EvalContext::at(Time::ZERO, Time::ZERO));
        assert_eq!(blur.pack_uniforms(&v, 1.0), vec![10.0, 0.0]);
    }

    #[test]
    fn rejects_incompatible_and_duplicate_plugins() {
        let mut r = Registry::with_builtins();
        let mut d = r.effect("oa.color.exposure").unwrap().as_ref().clone();
        assert_eq!(r.register(d.clone()), Err(RegisterError::Duplicate("oa.color.exposure".into())));
        d.type_id = "com.example.x".into();
        d.api_version = 99;
        assert!(matches!(r.register(d), Err(RegisterError::IncompatibleApi { .. })));
    }
}
