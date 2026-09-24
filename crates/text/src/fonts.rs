//! Fonts: an index of the system's font families (built once, by reading only each
//! file's name table through a memory map) and the faces loaded from it.
//!
//! Loaded faces live for the rest of the process — a project uses a handful, and
//! glyph atlases and layouts refer to them by [`FontId`] without lifetimes.

use ab_glyph::Font as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A loaded face, stable for the life of the process.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u32);

/// Where a face comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum FaceSource {
    File(PathBuf, u32),
    /// Compiled in: always available, so text renders even without system fonts.
    Bundled,
}

#[derive(Clone, Debug)]
struct FaceInfo {
    source: FaceSource,
    family: String,
    italic: bool,
    weight: u16,
}

pub struct Face {
    pub id: FontId,
    pub family: String,
    pub font: ab_glyph::FontRef<'static>,
    pub(crate) shaper: harfrust::ShaperData,
    pub(crate) shape_ref: harfrust::FontRef<'static>,
}

impl Face {
    pub fn has_char(&self, c: char) -> bool {
        self.font.glyph_id(c).0 != 0
    }

    pub fn units_per_em(&self) -> f32 {
        self.font.units_per_em().unwrap_or(1000.0)
    }
}

/// The family everything falls back to when a name isn't installed.
pub const BUNDLED_FAMILY: &str = "Ubuntu Light";

/// Families tried, in order, for characters the chosen font lacks.
const FALLBACKS: &[&str] = &[
    "Segoe UI",
    "Segoe UI Symbol",
    "Segoe UI Emoji",
    "Nirmala UI",
    "Microsoft YaHei",
    "Yu Gothic",
    "Malgun Gothic",
    "Leelawadee UI",
    "Ebrima",
    "Gadugi",
    "Segoe UI Historic",
    "Arial",
    "Helvetica",
    "DejaVu Sans",
    "Noto Sans",
    BUNDLED_FAMILY,
];

/// The default family for new text: the platform's UI font when installed.
pub fn default_family() -> &'static str {
    for f in ["Segoe UI", "Helvetica Neue", "Helvetica", "Arial", "DejaVu Sans", "Noto Sans"] {
        if index().families.iter().any(|x| x.eq_ignore_ascii_case(f)) {
            return f;
        }
    }
    BUNDLED_FAMILY
}

struct Index {
    faces: Vec<FaceInfo>,
    /// Sorted, unique.
    families: Vec<String>,
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(&home).join(".fonts"));
            dirs.push(PathBuf::from(home).join(".local/share/fonts"));
        }
    }
    dirs
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && depth < 4 {
            walk(&path, out, depth + 1);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && ["ttf", "otf", "ttc", "otc"].iter().any(|x| ext.eq_ignore_ascii_case(x))
        {
            out.push(path);
        }
    }
}

/// The family name a face reports, preferring the typographic family ("Segoe UI" rather
/// than "Segoe UI Semibold") and English.
fn family_name(face: &ttf_parser::Face<'_>) -> Option<String> {
    let mut best: Option<(u8, String)> = None;
    for name in face.names() {
        let rank = match name.name_id {
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY => 2,
            ttf_parser::name_id::FAMILY => 0,
            _ => continue,
        } + (name.language_id == 0x0409) as u8;
        if best.as_ref().is_some_and(|(r, _)| *r >= rank) || !name.is_unicode() {
            continue;
        }
        if let Some(s) = name.to_string().filter(|s| !s.trim().is_empty()) {
            best = Some((rank, s));
        }
    }
    best.map(|(_, s)| s)
}

fn describe(source: FaceSource, face: &ttf_parser::Face<'_>) -> Option<FaceInfo> {
    let weight = face.weight().to_number();
    Some(FaceInfo { source, family: family_name(face)?, italic: face.is_italic(), weight: if face.is_bold() { weight.max(700) } else { weight } })
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut files = Vec::new();
        for dir in font_dirs() {
            walk(&dir, &mut files, 0);
        }
        let mut faces = Vec::new();
        for path in files {
            let Ok(file) = std::fs::File::open(&path) else { continue };
            // SAFETY: font files are only read; a font changing underneath us would at
            // worst give a garbled name, which the parser tolerates.
            let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
            let count = ttf_parser::fonts_in_collection(&map).unwrap_or(1);
            for i in 0..count.min(64) {
                if let Ok(face) = ttf_parser::Face::parse(&map, i) {
                    faces.extend(describe(FaceSource::File(path.clone(), i), &face));
                }
            }
        }
        if let Ok(face) = ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0) {
            faces.extend(describe(FaceSource::Bundled, &face).map(|f| FaceInfo { family: BUNDLED_FAMILY.into(), ..f }));
        }
        let mut families: Vec<String> = faces.iter().map(|f| f.family.clone()).collect();
        families.sort_by_key(|f| f.to_lowercase());
        families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        Index { faces, families }
    })
}

/// Every installed family, sorted (for font pickers).
pub fn families() -> &'static [String] {
    &index().families
}

struct Loaded {
    faces: Vec<&'static Face>,
    by_source: HashMap<FaceSource, FontId>,
    /// (family lowercased, bold, italic) → face.
    resolved: HashMap<(String, bool, bool), FontId>,
    fallback: HashMap<char, Option<FontId>>,
}

fn loaded() -> &'static Mutex<Loaded> {
    static L: OnceLock<Mutex<Loaded>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Loaded { faces: Vec::new(), by_source: HashMap::new(), resolved: HashMap::new(), fallback: HashMap::new() }))
}

fn load(l: &mut Loaded, info: &FaceInfo) -> Option<FontId> {
    if let Some(id) = l.by_source.get(&info.source) {
        return Some(*id);
    }
    let (data, index): (&'static [u8], u32) = match &info.source {
        FaceSource::Bundled => (epaint_default_fonts::UBUNTU_LIGHT, 0),
        FaceSource::File(path, i) => (Box::leak(std::fs::read(path).ok()?.into_boxed_slice()), *i),
    };
    let font = ab_glyph::FontRef::try_from_slice_and_index(data, index).ok()?;
    let shape_ref = harfrust::FontRef::from_index(data, index).ok()?;
    let shaper = harfrust::ShaperData::new(&shape_ref);
    let id = FontId(l.faces.len() as u32);
    l.faces.push(Box::leak(Box::new(Face { id, family: info.family.clone(), font, shaper, shape_ref })));
    l.by_source.insert(info.source.clone(), id);
    Some(id)
}

/// The face of `family` closest to the requested style; an unknown family falls back
/// to the default one (and ultimately the bundled font, which always loads).
pub fn resolve(family: &str, bold: bool, italic: bool) -> &'static Face {
    let key = (family.to_lowercase(), bold, italic);
    let mut l = loaded().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = l.resolved.get(&key) {
        return l.faces[id.0 as usize];
    }
    let pick = |family: &str| -> Option<FaceInfo> {
        let want_weight = if bold { 700 } else { 400 };
        index()
            .faces
            .iter()
            .filter(|f| f.family.eq_ignore_ascii_case(family))
            .min_by_key(|f| ((f.italic != italic) as u32 * 10_000) + (f.weight as i32 - want_weight).unsigned_abs())
            .cloned()
    };
    let candidates = [family, default_family(), BUNDLED_FAMILY];
    let id = candidates.iter().filter_map(|f| pick(f)).find_map(|info| load(&mut l, &info));
    let id = id.or_else(|| {
        let bundled = FaceInfo { source: FaceSource::Bundled, family: BUNDLED_FAMILY.into(), italic: false, weight: 300 };
        load(&mut l, &bundled)
    });
    let id = id.expect("the bundled font always loads");
    l.resolved.insert(key, id);
    l.faces[id.0 as usize]
}

/// A face that has `c`, for characters the chosen font lacks.
pub fn fallback_for(c: char) -> Option<&'static Face> {
    {
        let l = loaded().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = l.fallback.get(&c) {
            return id.map(|id| l.faces[id.0 as usize]);
        }
    }
    let found = FALLBACKS.iter().map(|f| resolve(f, false, false)).find(|face| face.has_char(c));
    let mut l = loaded().lock().unwrap_or_else(|e| e.into_inner());
    l.fallback.insert(c, found.map(|f| f.id));
    found
}

pub fn face(id: FontId) -> &'static Face {
    let l = loaded().lock().unwrap_or_else(|e| e.into_inner());
    l.faces[id.0 as usize]
}

/// A small grayscale picture of `text` set in `family` at `px` tall: (width, height,
/// one alpha byte per pixel). For menus that show a font by drawing with it. `None` if
/// the family has nothing to draw with.
///
/// Deliberately simple — advances and kerning, no shaping — because it draws short
/// samples of a font's name, not real text (that's [`crate::layout`]).
pub fn sample_image(family: &str, text: &str, px: f32) -> Option<(u32, u32, Vec<u8>)> {
    use ab_glyph::{Font as _, ScaleFont as _};
    let face = resolve(family, false, false);
    let font = face.font.as_scaled(px);
    let (ascent, descent) = (font.ascent(), font.descent());
    let height = (ascent - descent).ceil().max(1.0);
    let mut pen = 0.0f32;
    let mut glyphs = Vec::new();
    let mut previous = None;
    for c in text.chars().take(64) {
        let id = font.glyph_id(c);
        if let Some(prev) = previous {
            pen += font.kern(prev, id);
        }
        previous = Some(id);
        glyphs.push((id, pen));
        pen += font.h_advance(id);
    }
    let width = pen.ceil().max(1.0);
    if width > 4096.0 || height > 256.0 {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; w * h];
    let mut drew = false;
    for (id, x) in glyphs {
        let Some(outline) = font.outline_glyph(id.with_scale_and_position(px, ab_glyph::point(x, ascent))) else { continue };
        let b = outline.px_bounds();
        outline.draw(|gx, gy, c| {
            let (px_, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
            if px_ >= 0 && py >= 0 && (px_ as usize) < w && (py as usize) < h {
                let i = py as usize * w + px_ as usize;
                pixels[i] = pixels[i].max((c.clamp(0.0, 1.0) * 255.0) as u8);
                drew = true;
            }
        });
    }
    drew.then_some((w as u32, h as u32, pixels))
}
