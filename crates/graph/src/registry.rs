//! The **plugin-facing** description of effects.
//!
//! Plugins (and built-ins, which use the exact same path) describe *what* an effect is
//! with an [`EffectDescriptor`]. The planner lowers descriptors into internal
//! [`NodeOp`](crate::NodeOp)s. Plugins never see `NodeOp`, so the internal graph and
//! optimizer can change freely without breaking plugins; only [`PLUGIN_API_VERSION`]
//! is a compatibility promise.

use oa_params::{Evaluated, Gradient, ParamId, ParamSchema, Unit, Value};
use crate::script::{BoundsScript, MotionScript, PassScript};
use crate::Rect;
use oa_time::Time;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const PLUGIN_API_VERSION: u32 = 1;

/// The viewer places the effect's points (a grid, `grid` plus a `Vec2` per point named
/// by [`surface_point_id`]) in place of the layer's handles.
pub const EDITOR_SURFACE: &str = "surface";
/// The sound card draws the effect's response curve and drags its bands (params
/// `low_freq`/`low_gain`, `p1_freq`/`p1_gain`/`p1_q` … `p3_*`, `high_freq`/`high_gain`).
pub const EDITOR_EQUALIZER: &str = "equalizer";
/// How far the effect is turning the sound down (what its shader writes to `reduction`).
pub const METER_REDUCTION: &str = "reduction";
/// How alike the two channels are.
pub const METER_CORRELATION: &str = "correlation";

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

/// What an effect's second input is (besides the picture it works on).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SecondInput {
    /// Its media parameter's picture, stretched over the layer (masks, displacement maps).
    #[default]
    Media,
    /// The picture as it came in, before the effect's earlier passes (Glow lays it over
    /// its halo).
    Original,
}

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
    /// One or two sentences for its tooltip.
    pub description: String,
    /// The picker's group for it ("Color", "Blur"…); `None`: the plugin's name.
    pub category: Option<String>,
    /// Settings its preview in the picker shows it with (a stronger look than its
    /// defaults, so the thumbnail says what it does).
    pub preview: Vec<(ParamId, Value)>,
    /// A host editor it uses, beyond the parameter list: [`EDITOR_SURFACE`],
    /// [`EDITOR_EQUALIZER`].
    pub editor: Option<String>,
    /// Sound: the live meter its card shows, [`METER_REDUCTION`] or [`METER_CORRELATION`].
    pub meter: Option<String>,
    /// Sound: how far behind its input it runs, in seconds (a lookahead), beyond its
    /// spectrum's window.
    pub latency: f64,
    /// Sound: a script (reading the parameters, writing `out`) saying how many seconds it
    /// keeps sounding after its clip ends — an echo's repeats, a room's ring.
    pub tail: Option<Arc<str>>,
    /// For `EffectKind::Motion`: how the layer moves (a script, deterministic in its
    /// inputs, so frames stay cacheable and render identically in preview and export).
    pub motion: Option<Arc<MotionScript>>,
    /// Where it may draw, given its input's box (a script); `None`: the box grown by its
    /// `expand` parameter.
    pub bounds: Option<Arc<BoundsScript>>,
    /// How many passes the shader runs, from its packed uniforms (a script) — for effects
    /// whose work grows with a setting (Glow's jump flood, one pass per halving of its
    /// radius). `None`: the shader's fixed `passes`.
    pub pass_count: Option<Arc<PassScript>>,
    /// For a pass that may run at a fraction of the output's resolution: by how much its
    /// target is divided (1 = full size; a script). The shader finds its cell from
    /// `pos - out_origin()` and reads a smaller input with `textureLoad`.
    pub pass_divisor: Option<Arc<PassScript>>,
    /// What `sample_media` reads.
    pub second_input: SecondInput,
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

/// How many floats a parameter of this type takes in a uniform block.
pub fn packed_len(ty: oa_params::ParamType) -> usize {
    use oa_params::ParamType as T;
    match ty {
        T::Float | T::Int | T::Bool | T::Enum | T::Media => 1,
        T::Vec2 => 2,
        T::Vec3 => 3,
        T::Color => 4,
        T::Text => 0,
        T::Gradient => oa_params::Gradient::PACKED_LEN,
    }
}

impl EffectDescriptor {
    /// An effect with its kind's usual settings and no shader yet.
    pub fn new(type_id: &str, name: &str, kind: EffectKind, params: Vec<ParamSchema>) -> Self {
        descriptor(type_id, name, kind, params)
    }

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
        self.params.iter().map(|p| packed_len(p.ty)).sum::<usize>() + CLOCK_SLOTS
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
    /// `expand` param, or — for an effect with a bounds script (a surface with points
    /// pulled outside, a sheet turned towards the camera) — around the shape it makes, so
    /// nothing is cut off.
    pub fn grown_bounds(&self, values: &Evaluated, raster_scale: f64, input: Rect) -> Rect {
        let Some(script) = &self.bounds else { return input.expand(self.expand_px(values, raster_scale)) };
        let grown = script.eval(values, raster_scale, input);
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

/// Some of Atelier Core's effects, by id (for tests and the host's own defaults).
/// **Surface**: the layer stretched over a grid of points ([`EDITOR_SURFACE`]).
pub const SURFACE: &str = "oa.warp.surface";
/// **Tile**: the whole picture in each cell of a grid (`columns`, `rows`, `gap`, `mirror`).
pub const TILE_EFFECT: &str = "oa.warp.tile";
/// **Scroll**: the picture sliding towards `direction` at `speed` px a second, wrapping
/// round at the layer's edges — so a tiled picture scrolls without a seam.
pub const SCROLL: &str = "oa.warp.scroll";
/// **Glow**: light leaking out around the picture (a jump flood finds the nearest edge).
pub const GLOW: &str = "oa.light.glow";
/// **Drop Shadow**: the silhouette offset and softened behind the picture.
pub const SHADOW: &str = "oa.light.shadow";
/// **Depth**: the picture as a sheet with thickness, turned in 3D.
pub const DEPTH: &str = "oa.depth.slab";

/// A surface editor's grid sizes (its `grid` parameter's options): 2 × 2 to 4 × 4 points.
pub const SURFACE_GRIDS: [&str; 3] = ["Corners", "3 × 3", "4 × 4"];

/// The param holding the offset of a surface's point (row and column from the top left):
/// a `Vec2`, in fractions of the layer, from where it rests.
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

/// Atelier Core's sound effects (`oa.audio.*`), known even when no registry is at hand.
/// Plugins' sound effects are [`EffectKind::Sound`] descriptors: ask
/// [`Registry::is_sound`].
pub fn is_audio_effect(type_id: &str) -> bool {
    type_id.starts_with("oa.audio.")
}

/// The id the host's own effects are registered under.
pub const HOST_ID: &str = "com.openatelier.host";

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
/// A Gaussian blur (`radius` in layer px, then 1 to clamp at the edges): a background's
/// blurred picture.
pub const BLUR: &str = "oa.internal.blur";
/// A bounded effect on a text layer, put together from three passes over the effect's
/// area, each with a second input: its result × the letters' mask (`MASK_KEEP`), the
/// picture before it × what the mask leaves (`MASK_DROP`), and the two added (`ADD`).
pub const MASK_KEEP: &str = "oa.internal.mask_keep";
pub const MASK_DROP: &str = "oa.internal.mask_drop";
pub const ADD: &str = "oa.internal.add";

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
    /// The host's own effects only.
    pub fn host() -> Self {
        let mut r = Registry::default();
        for d in host_effects() {
            r.register_from(HOST_ID, d).expect("the host's effects register");
        }
        r
    }

    /// The host's effects and Atelier Core's.
    pub fn with_builtins() -> Self {
        let (r, issues) = Registry::from_plugins([&crate::plugin::core()]);
        assert!(issues.is_empty(), "{issues:?}");
        r
    }

    /// The host's effects, then those of every plugin given, in order (a later plugin
    /// can't take an id an earlier one used). Problems are returned rather than dropped.
    pub fn from_plugins<'a>(plugins: impl IntoIterator<Item = &'a crate::plugin::Plugin>) -> (Self, Vec<String>) {
        let mut r = Registry::host();
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

pub(crate) fn descriptor(type_id: &str, name: &str, kind: EffectKind, params: Vec<ParamSchema>) -> EffectDescriptor {
    let pointish = matches!(kind, EffectKind::PointOp | EffectKind::UvWarp);
    EffectDescriptor {
        type_id: type_id.into(),
        version: 1,
        api_version: PLUGIN_API_VERSION,
        name: name.into(),
        preserves_opacity: kind == EffectKind::PointOp,
        time_varying: false,
        usage: if kind == EffectKind::Transition { EffectUsage::Cut } else { EffectUsage::Passive },
        description: String::new(),
        category: None,
        preview: Vec::new(),
        editor: None,
        meter: None,
        latency: 0.0,
        tail: None,
        motion: None,
        bounds: None,
        second_input: SecondInput::Media,
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

/// The effects the host itself uses (`oa.internal.*`): a clip's crop, backgrounds, the
/// color transforms around every frame. They're part of the renderer, not of a plugin,
/// so they're always there — even with Atelier Core turned off — and never offered.
pub(crate) fn host_effects() -> Vec<EffectDescriptor> {
    vec![
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
        // Bounded effects on text (`MASK_KEEP`…): the second input is the mask, or the
        // other half.
        EffectDescriptor {
            shader: wgsl("oa_internal_mask_keep", 1, "fn oa_internal_mask_keep(pos: vec2f, base: u32) -> vec4f { return sample_input(pos) * sample_media(pos).a; }"),
            ..descriptor(MASK_KEEP, "Mask Keep", EffectKind::Spatial { expand: None }, vec![])
        },
        EffectDescriptor {
            shader: wgsl("oa_internal_mask_drop", 1, "fn oa_internal_mask_drop(pos: vec2f, base: u32) -> vec4f { return sample_input(pos) * (1.0 - sample_media(pos).a); }"),
            ..descriptor(MASK_DROP, "Mask Drop", EffectKind::Spatial { expand: None }, vec![])
        },
        EffectDescriptor {
            shader: wgsl("oa_internal_add", 1, "fn oa_internal_add(pos: vec2f, base: u32) -> vec4f { return sample_input(pos) + sample_media(pos); }"),
            ..descriptor(ADD, "Add", EffectKind::Spatial { expand: None }, vec![])
        },
        // The blurred-picture background (a copy of Atelier Core's Blur, which may be off).
        EffectDescriptor {
            shader: wgsl(
                "oa_internal_blur",
                2,
                "fn oa_internal_blur(pos: vec2f, base: u32) -> vec4f {
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
                BLUR,
                "Blur",
                EffectKind::Spatial { expand: None },
                vec![ParamSchema::new("radius", Value::Float(8.0), Unit::LayerPixels), ParamSchema::new("clamp", Value::Float(1.0), Unit::None)],
            )
        },
    ]
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
        let glow = Registry::with_builtins().effect(GLOW).unwrap().clone();
        // Uniforms: radius, strength, color, mode.
        let at = |radius: f32| [radius, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0];
        let count = |radius: f32| glow.pass_count.as_ref().unwrap().count(&at(radius));
        let divisor = |radius: f32, pass: u32| glow.pass_divisor.as_ref().unwrap().divisor(&at(radius), pass, count(radius));
        // Seeds, the jumps, drawing.
        assert_eq!((count(0.0), count(1.0), count(2.0)), (3, 3, 4));
        assert_eq!(count(10.0), 7, "steps 16, 8, 4, 2, 1 at full size");
        // 30 px: a 2 × 2-pixel grid, so 15 cells of flood: 5 jumps.
        assert_eq!((divisor(30.0, 0), count(30.0)), (2, 7));
        assert_eq!(divisor(10.0, 0), 1);
        assert_eq!(divisor(400.0, 0), 8, "capped");
        assert_eq!(count(1e9), 14, "capped");
        assert_eq!((divisor(30.0, 0), divisor(30.0, 5), divisor(30.0, 6)), (2, 2, 1), "the last pass draws at full size");
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
