//! Shaping and line layout: text + style → glyphs placed in a box.
//!
//! Each line is split into runs by font (characters the chosen font lacks come from a
//! fallback face), and each run is shaped with HarfBuzz rules (ligatures, kerning,
//! marks, complex scripts). Layouts are pure functions of the [`TextSpec`] and cached,
//! so the planner (which needs the box to place the layer) and the renderer (which
//! draws the glyphs) share one.

use crate::fonts::{self, Face, FontId};
use ab_glyph::Font as _;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

impl Align {
    pub fn parse(s: &str) -> Align {
        match s {
            "left" => Align::Left,
            "right" => Align::Right,
            _ => Align::Center,
        }
    }
}

/// Everything that decides where glyphs go.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec {
    pub content: String,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// Em size in px.
    pub size: f64,
    pub align: Align,
    /// Extra space after each letter, in ems.
    pub tracking: f64,
    /// Baseline-to-baseline distance, in ems.
    pub line_height: f64,
}

impl Hash for TextSpec {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.content.hash(h);
        self.family.hash(h);
        self.bold.hash(h);
        self.italic.hash(h);
        self.align.hash(h);
        for x in [self.size, self.tracking, self.line_height] {
            x.to_bits().hash(h);
        }
    }
}

impl Eq for TextSpec {}

/// One drawn glyph (whitespace isn't one).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub font: FontId,
    pub glyph: u16,
    /// Pen position on the baseline, px from the layout's top-left.
    pub origin: [f64; 2],
    /// Outline bounds, px from the layout's top-left: `[x0, y0, x1, y1]`.
    pub ink: [f64; 4],
    /// Order among drawn glyphs, 0-based (per-letter animation staggers by this).
    pub index: u32,
    pub line: u32,
    pub word: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The box holding every line and all ink, px.
    pub size: [f64; 2],
    pub glyphs: Vec<PlacedGlyph>,
    pub lines: u32,
    pub words: u32,
    pub em: f64,
}

struct Shaped {
    font: FontId,
    glyph: u16,
    x: f64,
    y: f64,
    advance: f64,
    word: u32,
    blank: bool,
}

/// The font for each character: the chosen one when it has the glyph, else a fallback.
fn face_for(primary: &'static Face, c: char) -> &'static Face {
    if c.is_whitespace() || c.is_control() || primary.has_char(c) {
        primary
    } else {
        fonts::fallback_for(c).unwrap_or(primary)
    }
}

fn shape_line(primary: &'static Face, line: &str, spec: &TextSpec, word: &mut u32, in_word: &mut bool) -> Vec<Shaped> {
    // Runs of consecutive characters from one face.
    let mut runs: Vec<(&'static Face, usize, usize)> = Vec::new();
    for (i, c) in line.char_indices() {
        let face = face_for(primary, c);
        match runs.last_mut() {
            Some((f, _, end)) if f.id == face.id => *end = i + c.len_utf8(),
            _ => runs.push((face, i, i + c.len_utf8())),
        }
    }
    let tracking = spec.tracking * spec.size;
    let mut out = Vec::new();
    let mut pen = 0.0;
    for (face, start, end) in runs {
        let text = &line[start..end];
        // Word index of each character, by byte offset in the run.
        let mut words = Vec::with_capacity(text.len());
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                if *in_word {
                    *word += 1;
                }
                *in_word = false;
            } else {
                *in_word = true;
            }
            words.push((i, *word, c.is_whitespace()));
        }
        let mut buffer = harfrust::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        let shaper = face.shaper.shaper(&face.shape_ref).build();
        let glyphs = shaper.shape(buffer, harfrust::ShapeOptions::new());
        let k = spec.size / face.units_per_em() as f64;
        let mut last_cluster = None;
        for (info, pos) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
            let at = words.partition_point(|w| w.0 <= info.cluster as usize).saturating_sub(1);
            let (_, w, blank) = words.get(at).copied().unwrap_or((0, *word, false));
            // Letter spacing goes between clusters, never inside one (a base and its marks).
            if last_cluster.is_some_and(|c| c != info.cluster) {
                pen += tracking;
            }
            last_cluster = Some(info.cluster);
            out.push(Shaped {
                font: face.id,
                glyph: info.glyph_id as u16,
                x: pen + pos.x_offset as f64 * k,
                y: -(pos.y_offset as f64) * k,
                advance: pos.x_advance as f64 * k,
                word: w,
                blank,
            });
            pen += pos.x_advance as f64 * k;
        }
        if !out.is_empty() {
            pen += tracking;
        }
    }
    out
}

fn build(spec: &TextSpec) -> Layout {
    let primary = fonts::resolve(&spec.family, spec.bold, spec.italic);
    let k = spec.size / primary.units_per_em() as f64;
    let ascent = primary.font.ascent_unscaled() as f64 * k;
    let descent = primary.font.descent_unscaled() as f64 * k;
    let line_h = spec.line_height * spec.size;

    let (mut word, mut in_word) = (0u32, false);
    let lines: Vec<Vec<Shaped>> = spec
        .content
        .split('\n')
        .map(|l| {
            // A line break ends a word.
            if std::mem::take(&mut in_word) {
                word += 1;
            }
            shape_line(primary, l.trim_end_matches('\r'), spec, &mut word, &mut in_word)
        })
        .collect();
    // Width of a line: where its last glyph's advance ends (trailing letter spacing excluded).
    let widths: Vec<f64> = lines.iter().map(|l| l.last().map_or(0.0, |g| g.x + g.advance)).collect();
    let box_w = widths.iter().copied().fold(0.0, f64::max);
    let box_h = (lines.len().saturating_sub(1)) as f64 * line_h + (ascent - descent);

    let mut glyphs = Vec::new();
    let mut ink_box = [0.0, 0.0, box_w, box_h];
    let mut index = 0;
    for (n, (line, width)) in lines.iter().zip(&widths).enumerate() {
        let x0 = match spec.align {
            Align::Left => 0.0,
            Align::Center => (box_w - width) / 2.0,
            Align::Right => box_w - width,
        };
        let baseline = ascent + n as f64 * line_h;
        for g in line.iter().filter(|g| !g.blank) {
            let face = fonts::face(g.font);
            let Some(outline) = face.font.outline(ab_glyph::GlyphId(g.glyph)) else { continue };
            let fk = spec.size / face.units_per_em() as f64;
            let b = outline.bounds;
            let origin = [x0 + g.x, baseline + g.y];
            let ink = [
                origin[0] + b.min.x as f64 * fk,
                origin[1] - b.max.y as f64 * fk,
                origin[0] + b.max.x as f64 * fk,
                origin[1] - b.min.y as f64 * fk,
            ];
            ink_box = [ink_box[0].min(ink[0]), ink_box[1].min(ink[1]), ink_box[2].max(ink[2]), ink_box[3].max(ink[3])];
            glyphs.push(PlacedGlyph { font: g.font, glyph: g.glyph, origin, ink, index, line: n as u32, word: g.word });
            index += 1;
        }
    }
    // Shift so the box (typographic lines ∪ ink) starts at 0,0.
    let [dx, dy] = [-ink_box[0], -ink_box[1]];
    for g in &mut glyphs {
        g.origin = [g.origin[0] + dx, g.origin[1] + dy];
        g.ink = [g.ink[0] + dx, g.ink[1] + dy, g.ink[2] + dx, g.ink[3] + dy];
    }
    Layout {
        size: [(ink_box[2] - ink_box[0]).max(1.0), (ink_box[3] - ink_box[1]).max(1.0)],
        glyphs,
        lines: lines.len() as u32,
        words: word + in_word as u32,
        em: spec.size,
    }
}

/// The layout of `spec`, cached.
pub fn layout(spec: &TextSpec) -> Arc<Layout> {
    static CACHE: OnceLock<Mutex<HashMap<TextSpec, Arc<Layout>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(l) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(spec) {
        return l.clone();
    }
    let l = Arc::new(build(spec));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 512 {
        c.clear();
    }
    c.insert(spec.clone(), l.clone());
    l
}
