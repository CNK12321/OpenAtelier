//! The text pass: a text layer's glyphs drawn from distance fields, one instanced quad
//! per glyph.
//!
//! The three levels of text control all live in this one pass:
//! * **whole object** — the layer transform, motion effects and ordinary effects,
//!   applied to the pass's output like any layer;
//! * **per letter** — `Glyph` effects, chained in the vertex shader: each moves, scales,
//!   rotates and tints its glyph, knowing its index, line and word (staggered intros);
//! * **per pixel** — the fill (color or gradient) and outline from the distance
//!   field, then `GlyphPixel` effects, chained in the fragment shader (shimmer, glow).
//!
//! And **behind the letters**, `TextBox` effects: a quad around the whole text, each
//! line and each word, drawn before the glyphs (rounded backgrounds).
//!
//! **Highlight when spoken**: the word being spoken (`TextDraw::spoken_word`) is drawn
//! with a second style and a second copy of every effect's params. Each letter (and word
//! box) picks between the two blocks by comparing its word with the spoken one, so any
//! text effect param can change on just that word.
//!
//! Uniform block (512 floats): slots 0–3 as every pass (output origin/size), then the
//! style from the planner at 16 (`STYLE_LEN` floats: fill gradient at 16, outline
//! gradient at 48 — 32 floats each, see `oa_params::Gradient::pack` — outline width px
//! at 80), derived values at 84 (glyph count, lines, words, em px, distance-field
//! spread px, box w/h, and at 91 the spoken word or −1), the spoken word's style at 96,
//! then each effect's params from 164: its own, followed by its spoken copy.

use crate::pipelines::{PipelineSpec, Pipelines};
use crate::pool::WORKING_FORMAT;
use crate::shaders::{PRELUDE, CLOCK_SLOTS};
use crate::{GpuImage, RenderError};
use oa_graph::registry::Registry;
use oa_graph::{EffectKind, EffectShader};
use oa_text::{sdf, FontId, TextSpec};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const STYLE_LEN: usize = 2 * oa_params::Gradient::PACKED_LEN + 1;
const SLOTS: usize = 512;
const STYLE_BASE: usize = 16;
const DERIVED_BASE: usize = 84;
const SPOKEN_WORD: usize = 91;
const SPOKEN_STYLE_BASE: usize = 96;
const CHAIN_BASE: usize = 164;
/// Floats per box instance (rect x, y, w, h; kind; word; line; pad).
const BOX: usize = 8;
const ATLAS: u32 = 4096;
/// Floats per glyph instance.
const INSTANCE: usize = 16;

#[derive(Copy, Clone)]
struct AtlasEntry {
    /// Texel rect in the atlas.
    rect: [u32; 4],
    /// Bitmap top-left relative to the pen, bucket px.
    offset: [f32; 2],
}

/// Glyph distance fields, packed in shelves into one R8 texture. Filled on demand;
/// when it's full it starts over.
struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    entries: HashMap<(FontId, u16, u32), Option<AtlasEntry>>,
    shelf_y: u32,
    shelf_h: u32,
    cursor_x: u32,
}

impl Atlas {
    fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oa-glyph-atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Atlas { texture, view, entries: HashMap::new(), shelf_y: 0, shelf_h: 0, cursor_x: 0 }
    }

    fn place(&mut self, w: u32, h: u32) -> Option<[u32; 2]> {
        if w > ATLAS || h > ATLAS {
            return None;
        }
        if self.cursor_x + w > ATLAS {
            self.shelf_y += self.shelf_h + 1;
            self.shelf_h = 0;
            self.cursor_x = 0;
        }
        if self.shelf_y + h > ATLAS {
            return None;
        }
        let at = [self.cursor_x, self.shelf_y];
        self.cursor_x += w + 1;
        self.shelf_h = self.shelf_h.max(h);
        Some(at)
    }

    /// The glyph's place in the atlas, uploading it first if needed. With `wait` off,
    /// fields are made on worker threads: `Err(NotReady)` until this one is done.
    fn glyph(&mut self, queue: &wgpu::Queue, font: FontId, glyph: u16, bucket: u32, wait: bool) -> Result<Option<AtlasEntry>, RenderError> {
        if let Some(e) = self.entries.get(&(font, glyph, bucket)) {
            return Ok(*e);
        }
        let field = if wait {
            sdf::glyph_sdf(font, glyph, bucket).map(Arc::new)
        } else {
            match sdf::glyph_sdf_nonblocking(font, glyph, bucket) {
                std::task::Poll::Ready(f) => f,
                std::task::Poll::Pending => return Err(RenderError::NotReady),
            }
        };
        let entry = field.and_then(|g| {
            let at = self.place(g.size[0], g.size[1]).or_else(|| {
                // Full: start over (glyphs already drawn this frame were uploaded before
                // any draw runs, so a reset can only glitch a frame drawing > the atlas).
                self.entries.clear();
                self.shelf_y = 0;
                self.shelf_h = 0;
                self.cursor_x = 0;
                self.place(g.size[0], g.size[1])
            })?;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d { x: at[0], y: at[1], z: 0 }, aspect: wgpu::TextureAspect::All },
                &g.pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(g.size[0]), rows_per_image: None },
                wgpu::Extent3d { width: g.size[0], height: g.size[1], depth_or_array_layers: 1 },
            );
            Some(AtlasEntry { rect: [at[0], at[1], g.size[0], g.size[1]], offset: g.offset })
        });
        self.entries.insert((font, glyph, bucket), entry);
        Ok(entry)
    }
}

/// Text rendering state kept across frames: the glyph atlas and the text pipelines.
pub struct TextState {
    pipelines: Pipelines,
    sampler: wgpu::Sampler,
    atlas: Option<Atlas>,
}

impl TextState {
    pub fn new(device: &wgpu::Device) -> Self {
        let mut entries = crate::pipelines::pass_layout_entries(1);
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("oa-glyph-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        TextState { pipelines: Pipelines::with_layout(device, "oa-text", &entries), sampler, atlas: None }
    }
}

/// Which stage a text effect runs in.
/// Where in the text pass an effect runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Per letter, in the vertex shader.
    Letter,
    /// Per pixel of the letters, in the fragment shader.
    Pixel,
    /// Behind the letters, in boxes around the text, its lines and its words.
    Box,
}

pub fn stage(kind: &EffectKind) -> Option<Stage> {
    match kind {
        EffectKind::Glyph { .. } => Some(Stage::Letter),
        EffectKind::GlyphPixel => Some(Stage::Pixel),
        EffectKind::TextBox => Some(Stage::Box),
        _ => None,
    }
}

const TEXT_COMMON: &str = r#"
// One letter as per-letter effects see it: read index/count/line/word/center/size/em,
// change offset (px), scale, rotation (degrees) and color (straight RGBA multiplier).
struct Glyph {
    index: f32, count: f32, line: f32, lines: f32, word: f32, words: f32,
    center: vec2f, size: vec2f, em: f32,
    offset: vec2f, scale: vec2f, rotation: f32, color: vec4f,
}
// One pixel of the text as per-pixel text effects see it (layout px, px distances).
struct TextPixel { pos: vec2f, box_size: vec2f, dist: f32, spread: f32, em: f32, index: f32, count: f32 }
// One pixel of a box behind the text: `kind` 0 = the whole text, 1 = a line, 2 = a word
// (output px; `em` is the text size in px).
struct TextBox { pos: vec2f, center: vec2f, size: vec2f, em: f32, kind: f32, word: f32, line: f32 }

fn text_count() -> f32 { return u(84u); }
fn text_em() -> f32 { return u(87u); }
fn text_spread() -> f32 { return u(88u); }
fn text_box() -> vec2f { return vec2f(u(89u), u(90u)); }
// The word being spoken (−1: none).
fn spoken_word() -> f32 { return u(91u); }

fn oa_hash(n: f32) -> f32 { return fract(sin(n * 12.9898 + 78.233) * 43758.5453); }
// Smooth 1-D value noise in [-1, 1].
fn oa_noise(seed: f32, x: f32) -> f32 {
    let i = floor(x);
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    let a = oa_hash(seed * 17.13 + i) * 2.0 - 1.0;
    let b = oa_hash(seed * 17.13 + i + 1.0) * 2.0 - 1.0;
    return mix(a, b, s);
}
fn oa_ease_out(x: f32) -> f32 { let v = clamp(x, 0.0, 1.0); return 1.0 - pow(1.0 - v, 3.0); }
fn oa_ease_back(x: f32, k: f32) -> f32 { let v = clamp(x, 0.0, 1.0) - 1.0; return 1.0 + (k + 1.0) * v * v * v + k * v * v; }
fn oa_hsv(h: f32, s: f32, v: f32) -> vec3f {
    let p = abs(fract(vec3f(h) + vec3f(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return v * mix(vec3f(1.0), clamp(p - 1.0, vec3f(0.0), vec3f(1.0)), s);
}
// How much a bounded effect applies to letter `i` (which spans i..i+1): its range at
// slot `b` — percent? (1: of the letter count), start, end, blend — covers the letters
// whose middles are inside it, fading in and out over the blend.
fn oa_bounded(i: f32, b: u32) -> f32 {
    let k = select(1.0, text_count() / 100.0, u(b) > 0.5);
    let start = u(b + 1u) * k;
    let end = u(b + 2u) * k;
    let blend = max(u(b + 3u) * k, 0.0);
    let x = i + 0.5;
    if (blend < 1e-4) {
        return step(start, x) * step(x, end);
    }
    return clamp((x - start) / blend + 0.5, 0.0, 1.0) * clamp((end - x) / blend + 0.5, 0.0, 1.0);
}
// A per-letter effect's result `b`, blended with the letter as it was, `a`.
fn oa_bounded_glyph(a: Glyph, b: Glyph, w: f32) -> Glyph {
    var g = b;
    g.offset = mix(a.offset, b.offset, w);
    g.scale = mix(a.scale, b.scale, w);
    g.rotation = mix(a.rotation, b.rotation, w);
    g.color = mix(a.color, b.color, w);
    return g;
}
// This letter's own 0 → 1 progress through the effect's `visibility()`: letters start
// one after another, `stagger` (0..1) of the window apart from first to last.
fn letter_progress(g: Glyph, stagger: f32) -> f32 {
    let s = clamp(stagger, 0.0, 0.95);
    let start = select(0.0, s * g.index / max(g.count - 1.0, 1.0), g.count > 1.0);
    return clamp((visibility() - start) / (1.0 - s), 0.0, 1.0);
}
"#;

const GLYPH_LIB: &str = r#"
struct GlyphIn { quad: vec4f, uv: vec4f, center: vec2f, size: vec2f, index: f32, line: f32, word: f32, pad: f32 }
@group(0) @binding(3) var<storage, read> glyphs: array<GlyphIn>;

struct VOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
    @location(2) local: vec2f,
    @location(3) @interpolate(flat) info: vec4f,
}
"#;

const BOX_LIB: &str = r#"
struct BoxIn { rect: vec4f, kind: f32, word: f32, line: f32, pad: f32 }
@group(0) @binding(3) var<storage, read> boxes: array<BoxIn>;

struct BoxOut {
    @builtin(position) pos: vec4f,
    @location(0) local: vec2f,
    @location(1) @interpolate(flat) info: vec4f,
    @location(2) @interpolate(flat) rect: vec4f,
}
"#;

fn prelude() -> String {
    let mut s = PRELUDE.replace("array<vec4f, 16>", &format!("array<vec4f, {}>", SLOTS / 4));
    s.push_str(TEXT_COMMON);
    s
}

/// One effect of a text pass: its shader, stage, uniform floats, and whether it's
/// bounded to some of the letters.
pub type Link<'a> = (&'a EffectShader, Stage, usize, bool);

/// Floats of a bounded effect's range: percent?, start, end, blend.
const BOUNDS: usize = 4;

/// Where each effect's params sit: `base` for each, then their spoken copy, then (when
/// bounded) its range.
fn bases(chain: &[Link<'_>]) -> Vec<usize> {
    let mut base = CHAIN_BASE;
    chain
        .iter()
        .map(|(_, _, len, bounded)| {
            let b = base;
            base += 2 * len + if *bounded { BOUNDS } else { 0 };
            b
        })
        .collect()
}

/// WGSL for a text pass with this chain of (shader, stage, uniform floats). Box effects
/// take their slots but are drawn by their own pipelines (`box_shader`).
pub fn text_shader(chain: &[Link<'_>]) -> String {
    let mut s = prelude();
    s.push_str(GLYPH_LIB);
    let mut seen = Vec::new();
    for (shader, stage, _, _) in chain {
        if *stage != Stage::Box && !seen.contains(&shader.entry.as_str()) {
            seen.push(&shader.entry);
            s.push_str(&shader.source);
            s.push('\n');
        }
    }
    let (mut vertex, mut pixel) = (String::new(), String::new());
    for ((shader, stage, len, bounded), base) in chain.iter().zip(bases(chain)) {
        let clock = base + len.saturating_sub(CLOCK_SLOTS);
        let alt = base + len;
        // A bounded effect is blended with the letter as it was, by how far the letter is
        // inside its range.
        let range = base + 2 * len;
        match (stage, bounded) {
            (Stage::Letter, false) => {
                let _ = writeln!(vertex, "    oa_set_clock({clock}u);\n    g = {}(g, select({base}u, {alt}u, g.word == spoken_word()));", shader.entry);
            }
            (Stage::Letter, true) => {
                let _ = writeln!(
                    vertex,
                    "    oa_set_clock({clock}u);\n    {{ let before = g;\n      g = oa_bounded_glyph(before, {}(g, select({base}u, {alt}u, g.word == spoken_word())), oa_bounded(gi.index, {range}u)); }}",
                    shader.entry
                );
            }
            (Stage::Pixel, false) => {
                let _ = writeln!(pixel, "    oa_set_clock({clock}u);\n    c = {}(c, px, select({base}u, {alt}u, spoken));", shader.entry);
            }
            (Stage::Pixel, true) => {
                let _ = writeln!(pixel, "    oa_set_clock({clock}u);\n    c = mix(c, {}(c, px, select({base}u, {alt}u, spoken)), oa_bounded(v.info.x, {range}u));", shader.entry);
            }
            (Stage::Box, _) => {}
        }
    }
    let _ = write!(
        s,
        r#"
@vertex
fn vs_text(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {{
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(1.0, 0.0), vec2f(1.0, 1.0), vec2f(0.0, 1.0),
    );
    let gi = glyphs[ii];
    let c = corners[vi];
    let p = gi.quad.xy + c * gi.quad.zw;
    var g = Glyph(gi.index, u(84u), gi.line, u(85u), gi.word, u(86u), gi.center, gi.size, u(87u),
                  vec2f(0.0), vec2f(1.0), 0.0, vec4f(1.0));
{vertex}
    let r = radians(g.rotation);
    let d = (p - g.center) * g.scale;
    let q = g.center + g.offset + vec2f(d.x * cos(r) - d.y * sin(r), d.x * sin(r) + d.y * cos(r));
    let o = (q - out_origin()) / out_size();
    var out: VOut;
    out.pos = vec4f(o.x * 2.0 - 1.0, 1.0 - o.y * 2.0, 0.0, 1.0);
    out.uv = mix(gi.uv.xy, gi.uv.zw, c);
    out.color = g.color;
    out.local = p;
    out.info = vec4f(gi.index, max(max(abs(g.scale.x), abs(g.scale.y)), 1e-4), gi.word, 0.0);
    return out;
}}

@fragment
fn fs_text(v: VOut) -> @location(0) vec4f {{
    // The word being spoken takes the spoken style (and effects their spoken params).
    let spoken = v.info.z == spoken_word();
    let field = textureSampleLevel(input_tex, input_samp, v.uv, 0.0).r;
    // Distance to the outline in output px (positive inside), and the width of one
    // output pixel in the same units, for anti-aliasing at any scale.
    let dist = (field - 0.5) * 2.0 * text_spread() * v.info.y;
    let w = max(length(vec2f(dpdx(dist), dpdy(dist))), 1e-4);
    let fill_cover = clamp(dist / w + 0.5, 0.0, 1.0);
    let ow = u(select(80u, 160u, spoken)) * v.info.y;
    let line_cover = select(0.0, clamp((dist + ow) / w + 0.5, 0.0, 1.0), ow > 0.0);
    // Fill and outline are directional gradients over the text box, fixed to the
    // letters (sampled where each letter sits before per-letter motion).
    var fill = oa_gradient(select(16u, 96u, spoken), v.local, vec2f(0.0), text_box());
    fill = vec4f(fill.rgb * v.color.rgb, fill.a);
    let line = oa_gradient(select(48u, 128u, spoken), v.local, vec2f(0.0), text_box());
    // Fill over outline, as straight color.
    let fa = fill_cover * fill.a;
    let la = line_cover * line.a;
    let a = fa + la * (1.0 - fa);
    var c = vec4f((fill.rgb * fa + line.rgb * la * (1.0 - fa)) / max(a, 1e-5), a);
    let px = TextPixel(v.local, text_box(), dist, text_spread() * v.info.y, text_em(), v.info.x, text_count());
{pixel}
    let alpha = clamp(c.a * v.color.a, 0.0, 1.0);
    return vec4f(c.rgb * alpha, alpha);
}}
"#
    );
    s
}

/// WGSL drawing one box effect (params at `base`, spoken copy after them) behind the
/// letters: one quad per box, grown by 1.5 em so padding and rounding fit.
pub fn box_shader(shader: &EffectShader, base: usize, len: usize) -> String {
    let mut s = prelude();
    s.push_str(BOX_LIB);
    s.push_str(&shader.source);
    let clock = base + len.saturating_sub(CLOCK_SLOTS);
    let alt = base + len;
    let entry = &shader.entry;
    let _ = write!(
        s,
        r#"
@vertex
fn vs_box(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> BoxOut {{
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(1.0, 0.0), vec2f(1.0, 1.0), vec2f(0.0, 1.0),
    );
    let b = boxes[ii];
    let m = text_em() * 1.5;
    let p = b.rect.xy - vec2f(m) + corners[vi] * (b.rect.zw + vec2f(2.0 * m));
    let o = (p - out_origin()) / out_size();
    var out: BoxOut;
    out.pos = vec4f(o.x * 2.0 - 1.0, 1.0 - o.y * 2.0, 0.0, 1.0);
    out.local = p;
    out.info = vec4f(b.kind, b.word, b.line, 0.0);
    out.rect = b.rect;
    return out;
}}

@fragment
fn fs_box(v: BoxOut) -> @location(0) vec4f {{
    oa_set_clock({clock}u);
    let base = select({base}u, {alt}u, v.info.y == spoken_word());
    let b = TextBox(v.local, v.rect.xy + v.rect.zw * 0.5, v.rect.zw, text_em(), v.info.x, v.info.y, v.info.z);
    let c = {entry}(b, base);
    let a = clamp(c.a, 0.0, 1.0);
    return vec4f(c.rgb * a, a);
}}
"#
    );
    s
}

fn text_key(chain: &[Link<'_>]) -> String {
    let mut k = String::from("text2");
    for (s, stage, len, bounded) in chain {
        let tag = match stage {
            Stage::Letter => "g",
            Stage::Pixel => "p",
            Stage::Box => "b",
        };
        let _ = write!(k, "|{tag}{}:{len}{}", s.entry, if *bounded { "~" } else { "" });
    }
    k
}

fn box_key(shader: &EffectShader, base: usize, len: usize) -> String {
    format!("textbox|{}:{base}:{len}", shader.entry)
}

fn text_spec(key: &str, chain: &[Link<'_>]) -> PipelineSpec {
    PipelineSpec {
        label: key.to_string(),
        source: text_shader(chain),
        vertex_entry: "vs_text",
        fragment_entry: "fs_text",
        format: WORKING_FORMAT,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
    }
}

fn box_spec(key: &str, shader: &EffectShader, base: usize, len: usize) -> PipelineSpec {
    PipelineSpec {
        label: key.to_string(),
        source: box_shader(shader, base, len),
        vertex_entry: "vs_box",
        fragment_entry: "fs_box",
        format: WORKING_FORMAT,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
    }
}

pub(crate) struct TextDraw<'a> {
    pub spec: &'a TextSpec,
    pub scale: f64,
    pub style: &'a [f32],
    /// The spoken word's style (empty: the same as `style`), and the word (−1: none).
    pub spoken_style: &'a [f32],
    pub spoken_word: f32,
    pub chain: &'a [oa_graph::TextEffect],
    pub origin: [f64; 2],
    pub size: [u32; 2],
}

impl crate::renderer::GpuServices<'_> {
    /// Draws a text layer into a new image (see the module docs).
    pub(crate) fn text_pass(&mut self, state: &mut TextState, registry: &Registry, t: &TextDraw<'_>) -> Result<GpuImage, RenderError> {
        let device = &self.ctx.device;
        let atlas = state.atlas.get_or_insert_with(|| Atlas::new(device));
        let layout = oa_text::layout(t.spec);
        let em = t.spec.size * t.scale;
        let bucket = sdf::bucket_for(em);
        let k = (em / bucket as f64) as f32;

        // The effect chain: shaders, stages and uniforms (each effect's own, then its
        // spoken copy).
        let mut shaders = Vec::new();
        let mut uniforms = vec![0f32; SLOTS];
        let mut base = CHAIN_BASE;
        for fx in t.chain {
            let Some(d) = registry.effect(&fx.type_id) else { continue };
            let (Some(shader), Some(stage)) = (d.shader.clone(), stage(&d.kind)) else { continue };
            let len = fx.uniforms.len();
            let bounds = fx.bounds.filter(|_| stage != Stage::Box);
            let size = 2 * len + if bounds.is_some() { BOUNDS } else { 0 };
            if base + size > SLOTS {
                return Err(RenderError::Unsupported(format!("text effects need {} params; {} fit", base + size - CHAIN_BASE, SLOTS - CHAIN_BASE)));
            }
            uniforms[base..base + len].copy_from_slice(&fx.uniforms);
            let spoken = if fx.spoken.len() == len { &fx.spoken } else { &fx.uniforms };
            uniforms[base + len..base + 2 * len].copy_from_slice(spoken);
            if let Some(b) = bounds {
                uniforms[base + 2 * len..base + size].copy_from_slice(&b);
            }
            base += size;
            shaders.push((shader, stage, len, bounds.is_some()));
        }
        let chain: Vec<Link<'_>> = shaders.iter().map(|(s, st, l, b)| (s, *st, *l, *b)).collect();
        let key = text_key(&chain);
        let pipeline = state.pipelines.get(&key, self.wait, || text_spec(&key, &chain))?;
        let mut box_pipelines = Vec::new();
        for ((shader, stage, len, _), base) in chain.iter().zip(bases(&chain)) {
            if *stage == Stage::Box {
                let key = box_key(shader, base, *len);
                box_pipelines.push(state.pipelines.get(&key, self.wait, || box_spec(&key, shader, base, *len))?);
            }
        }

        // One instance per glyph.
        let mut instances: Vec<f32> = Vec::with_capacity(layout.glyphs.len() * INSTANCE);
        let s = t.scale;
        let mut pending = false;
        for g in &layout.glyphs {
            let e = match atlas.glyph(&self.ctx.queue, g.font, g.glyph, bucket, self.wait) {
                Ok(Some(e)) => e,
                Ok(None) => continue,
                Err(_) => {
                    // Keep asking for the rest, so they're all made at once.
                    pending = true;
                    continue;
                }
            };
            let pen = [(g.origin[0] * s) as f32, (g.origin[1] * s) as f32];
            let [ax, ay, aw, ah] = e.rect.map(|x| x as f32);
            let n = ATLAS as f32;
            instances.extend([
                pen[0] + e.offset[0] * k,
                pen[1] + e.offset[1] * k,
                aw * k,
                ah * k,
                ax / n,
                ay / n,
                (ax + aw) / n,
                (ay + ah) / n,
                ((g.ink[0] + g.ink[2]) / 2.0 * s) as f32,
                ((g.ink[1] + g.ink[3]) / 2.0 * s) as f32,
                ((g.ink[2] - g.ink[0]) * s) as f32,
                ((g.ink[3] - g.ink[1]) * s) as f32,
                g.index as f32,
                g.line as f32,
                g.word as f32,
                0.0,
            ]);
        }
        if pending {
            return Err(RenderError::NotReady);
        }
        // Boxes behind the text: the whole text, each line, each word — letters' ink
        // across, a fixed band around the baseline down (so boxes on a line match).
        let boxes = if box_pipelines.is_empty() { Vec::new() } else { text_boxes(&layout.glyphs, t.spec.size, s) };
        let count = instances.len() / INSTANCE;
        if instances.is_empty() {
            instances.resize(INSTANCE, 0.0);
        }

        uniforms[0..4].copy_from_slice(&[t.origin[0] as f32, t.origin[1] as f32, t.size[0] as f32, t.size[1] as f32]);
        let n = t.style.len().min(STYLE_LEN);
        uniforms[STYLE_BASE..STYLE_BASE + n].copy_from_slice(&t.style[..n]);
        let spoken = if t.spoken_style.len() >= n { &t.spoken_style[..n] } else { &t.style[..n] };
        uniforms[SPOKEN_STYLE_BASE..SPOKEN_STYLE_BASE + n].copy_from_slice(spoken);
        uniforms[SPOKEN_WORD] = t.spoken_word;
        uniforms[DERIVED_BASE..DERIVED_BASE + 7].copy_from_slice(&[
            layout.glyphs.len() as f32,
            layout.lines as f32,
            layout.words as f32,
            em as f32,
            sdf::spread_px(bucket) * k,
            (layout.size[0] * s) as f32,
            (layout.size[1] * s) as f32,
        ]);

        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("oa-text-uniforms"),
            contents: bytemuck::cast_slice(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let gbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("oa-text-glyphs"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-text"),
            layout: &state.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&state.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: gbuf.as_entire_binding() },
            ],
        });
        let target = self.target(t.size)?;
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oa-text"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // The boxes first, behind the letters.
        let box_bg = (!boxes.is_empty()).then(|| {
            let bbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("oa-text-boxes"),
                contents: bytemuck::cast_slice(&boxes),
                usage: wgpu::BufferUsages::STORAGE,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("oa-text-boxes"),
                layout: &state.pipelines.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&state.sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: bbuf.as_entire_binding() },
                ],
            })
        });
        if let Some(box_bg) = &box_bg {
            for p in &box_pipelines {
                pass.set_pipeline(p);
                pass.set_bind_group(0, box_bg, &[]);
                pass.draw(0..6, 0..(boxes.len() / BOX) as u32);
            }
        }
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..6, 0..count as u32);
        drop(pass);
        self.stats.passes += 1;
        Ok(GpuImage { tex: target, origin: t.origin, size: t.size })
    }
}

const MASK_LIB: &str = r#"
struct Cell { rect: vec4f, w: vec4f }
@group(0) @binding(3) var<storage, read> cells: array<Cell>;

struct MaskOut {
    @builtin(position) pos: vec4f,
    @location(0) w: f32,
}

@vertex
fn vs_mask(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> MaskOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(1.0, 0.0), vec2f(1.0, 1.0), vec2f(0.0, 1.0),
    );
    let c = cells[ii];
    let k = corners[vi];
    let p = mix(c.rect.xy, c.rect.zw, k);
    let o = (p - out_origin()) / out_size();
    var out: MaskOut;
    out.pos = vec4f(o.x * 2.0 - 1.0, 1.0 - o.y * 2.0, 0.0, 1.0);
    // Across a letter's cell, from how much its left edge gets to its right edge's.
    out.w = mix(c.w.x, c.w.y, k.x);
    return out;
}

@fragment
fn fs_mask(v: MaskOut) -> @location(0) vec4f {
    return vec4f(clamp(v.w, 0.0, 1.0));
}
"#;

/// How much a bounded effect gets at `x` letters from the text's start (`n` letters),
/// its range `b` as in `oa_graph::TextEffect::bounds`. Without a blend, whole letters:
/// the ones whose middles are inside.
fn bound_weight(x: f64, b: [f32; 4], n: f64, whole_letter: f64) -> f64 {
    let k = if b[0] > 0.5 { n / 100.0 } else { 1.0 };
    let (start, end, blend) = (b[1] as f64 * k, b[2] as f64 * k, (b[3] as f64 * k).max(0.0));
    if blend < 1e-4 {
        let m = whole_letter + 0.5;
        return if m >= start && m <= end { 1.0 } else { 0.0 };
    }
    ((x - start) / blend + 0.5).clamp(0.0, 1.0) * ((end - x) / blend + 0.5).clamp(0.0, 1.0)
}

/// A cell per letter for a bounded effect's mask (8 floats: x0, y0, x1, y1 in output px,
/// the weight at its left and right edges, 2 unused), within `area`. Cells reach halfway
/// to the neighbors on their line, and lines halfway to the next: every pixel belongs
/// to the letter it's nearest in reading order, so an effect that spreads (a blur, a
/// glow) spreads within its letters' share.
fn mask_cells(glyphs: &[oa_text::PlacedGlyph], size: f64, scale: f64, b: [f32; 4], area: [f64; 4]) -> Vec<f32> {
    const FAR: f64 = 1e7;
    let n = glyphs.len() as f64;
    let (up, down) = (0.82 * size, 0.24 * size);
    let mut lines: Vec<u32> = glyphs.iter().map(|g| g.line).collect();
    lines.dedup();
    let baseline = |line: u32| glyphs.iter().find(|g| g.line == line).map_or(0.0, |g| g.origin[1]);
    let mut out = Vec::with_capacity(glyphs.len() * 8);
    for (i, g) in glyphs.iter().enumerate() {
        let li = lines.iter().position(|l| *l == g.line).unwrap_or(0);
        let top = if li == 0 { -FAR } else { ((baseline(lines[li - 1]) + down) + (g.origin[1] - up)) / 2.0 };
        let bottom = if li + 1 == lines.len() { FAR } else { ((g.origin[1] + down) + (baseline(lines[li + 1]) - up)) / 2.0 };
        let center = |g: &oa_text::PlacedGlyph| (g.ink[0] + g.ink[2]) / 2.0;
        let prev = i.checked_sub(1).map(|j| &glyphs[j]).filter(|p| p.line == g.line);
        let next = glyphs.get(i + 1).filter(|p| p.line == g.line);
        let left = prev.map_or(-FAR, |p| (center(p) + center(g)) / 2.0);
        let right = next.map_or(FAR, |p| (center(p) + center(g)) / 2.0);
        let x0 = (left * scale).max(area[0]);
        let x1 = (right * scale).min(area[2]);
        let y0 = (top * scale).max(area[1]);
        let y1 = (bottom * scale).min(area[3]);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        // The weight along the letter axis at the clipped edges.
        let at = |x: f64| {
            let span = ((right - left) * scale).max(1e-6);
            i as f64 + ((x - left * scale) / span).clamp(0.0, 1.0)
        };
        let (wl, wr) = (bound_weight(at(x0), b, n, i as f64), bound_weight(at(x1), b, n, i as f64));
        out.extend([x0 as f32, y0 as f32, x1 as f32, y1 as f32, wl as f32, wr as f32, 0.0, 0.0]);
    }
    out
}

impl crate::renderer::GpuServices<'_> {
    /// A bounded effect's mask over `origin`/`size` (see `NodeOp::TextMask`).
    pub(crate) fn text_mask_pass(&mut self, state: &mut TextState, spec: &TextSpec, scale: f64, bounds: [f32; 4], origin: [f64; 2], size: [u32; 2]) -> Result<GpuImage, RenderError> {
        let device = &self.ctx.device;
        let atlas = state.atlas.get_or_insert_with(|| Atlas::new(device));
        let layout = oa_text::layout(spec);
        let area = [origin[0], origin[1], origin[0] + size[0] as f64, origin[1] + size[1] as f64];
        let mut cells = mask_cells(&layout.glyphs, spec.size, scale, bounds, area);
        let count = cells.len() / 8;
        if cells.is_empty() {
            cells.resize(8, 0.0);
        }
        let key = "textmask";
        let pipeline = state.pipelines.get(key, self.wait, || {
            let mut source = prelude();
            source.push_str(MASK_LIB);
            PipelineSpec { label: key.to_string(), source, vertex_entry: "vs_mask", fragment_entry: "fs_mask", format: WORKING_FORMAT, blend: None }
        })?;
        let mut uniforms = vec![0f32; SLOTS];
        uniforms[0..4].copy_from_slice(&[origin[0] as f32, origin[1] as f32, size[0] as f32, size[1] as f32]);
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("oa-textmask-uniforms"), contents: bytemuck::cast_slice(&uniforms), usage: wgpu::BufferUsages::UNIFORM });
        let cbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("oa-textmask-cells"), contents: bytemuck::cast_slice(&cells), usage: wgpu::BufferUsages::STORAGE });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-textmask"),
            layout: &state.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&state.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: cbuf.as_entire_binding() },
            ],
        });
        let target = self.target(size)?;
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oa-textmask"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if count > 0 {
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..count as u32);
        }
        drop(pass);
        self.stats.passes += 1;
        Ok(GpuImage { tex: target, origin, size })
    }
}

/// Compiles the text pipeline for `chain` ahead of use (warm-up), and the box pipelines
/// of its box effects.
pub fn warm(state: &TextState, chain: &[Link<'_>], wait: bool) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
    for ((shader, stage, len, _), base) in chain.iter().zip(bases(chain)) {
        if *stage == Stage::Box {
            let key = box_key(shader, base, *len);
            state.pipelines.get(&key, wait, || box_spec(&key, shader, base, *len))?;
        }
    }
    let key = text_key(chain);
    state.pipelines.get(&key, wait, || text_spec(&key, chain))
}

/// Box instances (`BOX` floats each, output px) for the whole text, each line and each
/// word of `glyphs`, laid out at `size` and drawn at `scale`.
fn text_boxes(glyphs: &[oa_text::PlacedGlyph], size: f64, scale: f64) -> Vec<f32> {
    // Across: the letters' ink. Down: a band from the cap height to below the baseline.
    let (up, down) = (0.82 * size, 0.24 * size);
    let mut all: Option<[f64; 4]> = None;
    let mut lines: Vec<(u32, [f64; 4])> = Vec::new();
    let mut words: Vec<(u32, u32, [f64; 4])> = Vec::new();
    let grow = |r: &mut Option<[f64; 4]>, b: [f64; 4]| {
        *r = Some(match *r {
            Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            None => b,
        })
    };
    for g in glyphs {
        if g.ink[2] - g.ink[0] <= 0.0 {
            continue; // a space
        }
        let b = [g.ink[0], g.origin[1] - up, g.ink[2], g.origin[1] + down];
        grow(&mut all, b);
        match lines.iter_mut().find(|(l, _)| *l == g.line) {
            Some((_, r)) => *r = [r[0].min(b[0]), r[1].min(b[1]), r[2].max(b[2]), r[3].max(b[3])],
            None => lines.push((g.line, b)),
        }
        match words.iter_mut().find(|(w, l, _)| *w == g.word && *l == g.line) {
            Some((_, _, r)) => *r = [r[0].min(b[0]), r[1].min(b[1]), r[2].max(b[2]), r[3].max(b[3])],
            None => words.push((g.word, g.line, b)),
        }
    }
    let mut out = Vec::new();
    let mut push = |r: [f64; 4], kind: f32, word: f32, line: f32| {
        out.extend([(r[0] * scale) as f32, (r[1] * scale) as f32, ((r[2] - r[0]) * scale) as f32, ((r[3] - r[1]) * scale) as f32, kind, word, line, 0.0]);
    };
    if let Some(r) = all {
        push(r, 0.0, -2.0, -2.0);
    }
    for (l, r) in lines {
        push(r, 1.0, -2.0, l as f32);
    }
    for (w, l, r) in words {
        push(r, 2.0, w as f32, l as f32);
    }
    out
}
