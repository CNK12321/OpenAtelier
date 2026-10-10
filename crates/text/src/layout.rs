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
    /// The box's width, px: lines wrap between words to stay inside it (a word too long
    /// for a line of its own is split), and are aligned within it. `None`: each line is
    /// as long as it is, and only line breaks start new ones.
    pub wrap: Option<f64>,
}

impl Hash for TextSpec {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.content.hash(h);
        self.family.hash(h);
        self.bold.hash(h);
        self.italic.hash(h);
        self.align.hash(h);
        for x in [self.size, self.tracking, self.line_height, self.wrap.unwrap_or(-1.0)] {
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
    /// Where a text cursor goes, line by line — for editing a title where it's drawn.
    pub carets: Vec<LineCarets>,
}

/// One line's cursor positions, px from the layout's top-left.
#[derive(Clone, Debug, PartialEq)]
pub struct LineCarets {
    /// The content's character index (line breaks count) of the line's first character.
    pub first_char: usize,
    /// x before each of the line's characters, then after the last (`chars + 1` of them).
    pub xs: Vec<f64>,
    /// The line's top and bottom (its ascent and descent around the baseline).
    pub top: f64,
    pub bottom: f64,
}

impl Layout {
    /// The line and x of the cursor before character `index` of the content (the end of
    /// a line for its line break; past the end: the end of the text).
    pub fn caret(&self, index: usize) -> Option<(&LineCarets, f64)> {
        let line = self.carets.iter().rev().find(|l| l.first_char <= index).or(self.carets.first())?;
        let k = (index - line.first_char.min(index)).min(line.xs.len() - 1);
        Some((line, line.xs[k]))
    }

    /// The character index a click at `p` (layout px) puts the cursor before.
    pub fn index_at(&self, p: [f64; 2]) -> usize {
        // The line whose middle is nearest (lines can overlap when set tight).
        let Some(line) = self.carets.iter().min_by(|a, b| ((a.top + a.bottom) / 2.0 - p[1]).abs().total_cmp(&((b.top + b.bottom) / 2.0 - p[1]).abs())) else {
            return 0;
        };
        // The nearest caret on that line.
        let k = line.xs.iter().enumerate().min_by(|a, b| (a.1 - p[0]).abs().total_cmp(&(b.1 - p[0]).abs())).map_or(0, |(k, _)| k);
        line.first_char + k
    }
}

struct Shaped {
    font: FontId,
    glyph: u16,
    x: f64,
    y: f64,
    advance: f64,
    word: u32,
    blank: bool,
    /// Byte offset in the line of the character (cluster) it draws.
    cluster: usize,
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
                cluster: start + info.cluster as usize,
            });
            pen += pos.x_advance as f64 * k;
        }
        if !out.is_empty() {
            pen += tracking;
        }
    }
    out
}

/// A line as drawn: a whole paragraph, or (wrapped) part of one.
struct Row {
    /// Its glyphs, x measured from the row's start.
    glyphs: Vec<Shaped>,
    /// What it's aligned by: where its last glyph ends — a wrapped row's last word, not
    /// the spaces after it.
    width: f64,
    /// Where its last glyph ends, spaces too (the cursor after its last character).
    extent: f64,
    /// Its text: a byte range of its paragraph's (the glyphs' clusters are offsets into
    /// the paragraph).
    bytes: std::ops::Range<usize>,
    /// The content's character index of its first character.
    first_char: usize,
}

/// Where a shaped paragraph breaks into rows no wider than `width`: before the word that
/// would cross it, or — a word too long for a row of its own — before the letter that
/// would (never inside a cluster: a base and its marks stay together). Returns the
/// indices of the glyphs that start rows, the first always 0.
fn breaks(glyphs: &[Shaped], width: Option<f64>) -> Vec<usize> {
    let mut cuts = vec![0];
    let Some(width) = width.filter(|w| *w > 0.0) else { return cuts };
    let (mut start, mut word_start) = (0, None);
    let mut i = 0;
    while i < glyphs.len() {
        let g = &glyphs[i];
        if i > start && !g.blank && glyphs[i - 1].blank {
            word_start = Some(i);
        }
        if i > start && !g.blank && g.x + g.advance - glyphs[start].x > width {
            let mut at = word_start.filter(|&b| b > start).unwrap_or(i);
            while at > start + 1 && glyphs[at].cluster == glyphs[at - 1].cluster {
                at -= 1;
            }
            cuts.push(at);
            (start, word_start) = (at, None);
            // Measured again from the new row's start (it may need breaking again).
            if at < i {
                continue;
            }
        }
        i += 1;
    }
    cuts
}

/// A paragraph's rows: all of it, or wrapped at `breaks`.
fn rows(glyphs: Vec<Shaped>, text: &str, first_char: usize, width: Option<f64>) -> Vec<Row> {
    let cuts = breaks(&glyphs, width);
    let mut out = Vec::with_capacity(cuts.len());
    let mut glyphs = glyphs.into_iter();
    for (n, &at) in cuts.iter().enumerate() {
        let last = n + 1 == cuts.len();
        let count = cuts.get(n + 1).map_or(usize::MAX, |next| next - at);
        let mut row: Vec<Shaped> = glyphs.by_ref().take(count).collect();
        let x0 = row.first().map_or(0.0, |g| g.x);
        for g in &mut row {
            g.x -= x0;
        }
        let extent = row.last().map_or(0.0, |g| g.x + g.advance);
        // A row that wraps is aligned by its words, not the space it breaks at.
        let width = if last { extent } else { row.iter().rev().find(|g| !g.blank).map_or(0.0, |g| g.x + g.advance) };
        let from = if n == 0 { 0 } else { row.first().map_or(text.len(), |g| g.cluster) };
        out.push(Row { glyphs: row, width, extent, bytes: from..text.len(), first_char: first_char + text[..from].chars().count() });
        if let Some(prev) = out.len().checked_sub(2) {
            out[prev].bytes.end = from;
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
    let mut first_char = 0;
    let mut paragraphs: Vec<(&str, Vec<Row>)> = Vec::new();
    for raw in spec.content.split('\n') {
        // A line break ends a word.
        if std::mem::take(&mut in_word) {
            word += 1;
        }
        let text = raw.trim_end_matches('\r');
        let shaped = shape_line(primary, text, spec, &mut word, &mut in_word);
        paragraphs.push((text, rows(shaped, text, first_char, spec.wrap)));
        first_char += raw.chars().count() + 1;
    }
    let lines: Vec<(&str, &Row)> = paragraphs.iter().flat_map(|(text, rows)| rows.iter().map(move |r| (*text, r))).collect();
    // Wrapping: the box is as wide as asked (wider only for a word that can't be split
    // narrower, e.g. one letter wider than the box).
    let widest = lines.iter().map(|(_, r)| r.width).fold(0.0, f64::max);
    let box_w = spec.wrap.filter(|w| *w > 0.0).map_or(widest, |w| w.max(widest));
    let box_h = (lines.len().saturating_sub(1)) as f64 * line_h + (ascent - descent);

    let mut glyphs = Vec::new();
    let mut carets = Vec::new();
    let mut ink_box = [0.0, 0.0, box_w, box_h];
    let mut index = 0;
    for (n, (text, row)) in lines.iter().enumerate() {
        let x0 = match spec.align {
            Align::Left => 0.0,
            Align::Center => (box_w - row.width) / 2.0,
            Align::Right => box_w - row.width,
        };
        let baseline = ascent + n as f64 * line_h;
        // Cursor positions: before each character where its glyphs start (inside a
        // ligature, where the one before is), and after the row.
        let line = &row.glyphs;
        let part = &text[row.bytes.clone()];
        let mut xs = Vec::with_capacity(part.chars().count() + 1);
        let mut prev = x0;
        for (b, _) in part.char_indices() {
            let at = line.iter().filter(|g| g.cluster == row.bytes.start + b).map(|g| g.x).fold(f64::INFINITY, f64::min);
            prev = if at.is_finite() { x0 + at } else { prev };
            xs.push(prev);
        }
        xs.push(x0 + row.extent);
        carets.push(LineCarets { first_char: row.first_char, xs, top: baseline - ascent, bottom: baseline - descent });
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
    for l in &mut carets {
        l.xs.iter_mut().for_each(|x| *x += dx);
        (l.top, l.bottom) = (l.top + dy, l.bottom + dy);
    }
    Layout {
        size: [(ink_box[2] - ink_box[0]).max(1.0), (ink_box[3] - ink_box[1]).max(1.0)],
        glyphs,
        lines: lines.len() as u32,
        words: word + in_word as u32,
        em: spec.size,
        carets,
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
