//! The icon set.
//!
//! Icons are named, not typed as emoji: `icons::PLAY` rather than "▶". Each name is
//! drawn one of two ways.
//!
//! * **Google Material Symbols** (`assets/fonts/MaterialSymbolsRounded.ttf`, Apache-2.0,
//!   subset to the icons below). egui lays text out itself and doesn't do ligature
//!   shaping, so each icon is drawn by its **codepoint** — the fallback Google documents
//!   for exactly this case — with the ligature name kept beside it as the source of
//!   truth. A copy in `<config>/fonts/` wins, so a full font can be dropped in.
//! * **Drawn here** if the font is missing, as simple shapes in the Material geometry:
//!   a 24-unit grid, flat fills, round joins. No empty boxes, ever.
//!
//! Adding an icon: find its name and codepoint on fonts.google.com/icons, add a line
//! below, and re-run `assets/fonts/subset.py` so the font carries it.
//!
//! Either way the call site is the same, so adding the font later changes the look
//! without touching a single panel.

use eframe::egui;

/// An icon: its Material name, the codepoint that draws it, and how to draw it
/// without the font.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Icon {
    pub name: &'static str,
    pub code: char,
    shape: Shape,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Shape {
    Play,
    Pause,
    SkipStart,
    SkipEnd,
    StepBack,
    StepOn,
    Cut,
    Copy,
    Delete,
    Group,
    HalfCircle,
    Volume,
    Title,
    Magnet,
    FitScreen,
    Height,
    Undo,
    Redo,
    Home,
    Add,
    Folder,
    NewFolder,
    Movie,
    Music,
    Upload,
    Library,
    Star,
    Stop,
    Mic,
    Record,
    Subtitles,
    FolderOpen,
    Puzzle,
    Refresh,
    History,
    Warning,
    Globe,
    Move,
    Gear,
    Save,
    Download,
    Sparkle,
    Info,
}

macro_rules! icons {
    ($($konst:ident = $name:literal, $code:literal, $shape:ident;)*) => {
        $(pub const $konst: Icon = Icon { name: $name, code: $code, shape: Shape::$shape };)*

        /// Every icon, for the subsetting script and the tests.
        #[allow(dead_code)]
        pub const ALL: &[Icon] = &[$($konst),*];
    };
}

icons! {
    PLAY = "play_arrow", '\u{e037}', Play;
    PAUSE = "pause", '\u{e034}', Pause;
    SKIP_START = "skip_previous", '\u{e045}', SkipStart;
    SKIP_END = "skip_next", '\u{e044}', SkipEnd;
    STEP_BACK = "chevron_left", '\u{e408}', StepBack;
    STEP_ON = "chevron_right", '\u{e409}', StepOn;
    SPLIT = "content_cut", '\u{e14e}', Cut;
    DUPLICATE = "content_copy", '\u{e14d}', Copy;
    DELETE = "delete", '\u{e872}', Delete;
    GROUP = "link", '\u{e157}', Group;
    ENABLE = "contrast", '\u{eb37}', HalfCircle;
    EXTRACT_AUDIO = "volume_up", '\u{e050}', Volume;
    TITLE = "title", '\u{e264}', Title;
    SNAP = "align_horizontal_center", '\u{e00f}', Magnet;
    FIT = "fit_screen", '\u{ea10}', FitScreen;
    TRACK_HEIGHT = "height", '\u{ea16}', Height;
    UNDO = "undo", '\u{e166}', Undo;
    REDO = "redo", '\u{e15a}', Redo;
    HOME = "home", '\u{e88a}', Home;
    ADD = "add", '\u{e145}', Add;
    FOLDER = "folder", '\u{e2c7}', Folder;
    VIDEO_TRACK = "movie", '\u{e02c}', Movie;
    AUDIO_TRACK = "music_note", '\u{e405}', Music;
    NEW_FOLDER = "create_new_folder", '\u{e2cc}', NewFolder;
    IMPORT = "upload", '\u{e2c6}', Upload;
    SAMPLES = "photo_library", '\u{e413}', Library;
    ASSETS = "inventory_2", '\u{e1a1}', Star;
    STOP = "stop", '\u{e047}', Stop;
    MIC = "mic", '\u{e029}', Mic;
    RECORD = "fiber_manual_record", '\u{e061}', Record;
    CAPTIONS = "subtitles", '\u{e048}', Subtitles;
    FOLDER_OPEN = "folder_open", '\u{e2c8}', FolderOpen;
    PLUGINS = "extension", '\u{e87b}', Puzzle;
    REFRESH = "refresh", '\u{e5d5}', Refresh;
    HISTORY = "history", '\u{e889}', History;
    WARNING = "warning", '\u{e002}', Warning;
    LANGUAGE = "language", '\u{e894}', Globe;
    MOVE = "open_with", '\u{e89f}', Move;
    SETTINGS = "settings", '\u{e8b8}', Gear;
    SAVE = "save", '\u{e161}', Save;
    EXPORT = "file_download", '\u{e2c4}', Download;
    EFFECTS = "auto_awesome", '\u{e65f}', Sparkle;
    INFO = "info", '\u{e88e}', Info;
}

/// Whether the Material font was found and installed.
pub fn material_font() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| font_path().is_some())
}

/// Where a Material Symbols font may be dropped, in the order they're looked for.
pub fn font_dirs() -> Vec<std::path::PathBuf> {
    vec![std::path::PathBuf::from("assets/fonts"), crate::settings::config_dir().join("fonts")]
}

fn font_path() -> Option<std::path::PathBuf> {
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name.starts_with("materialsymbols") && (name.ends_with(".ttf") || name.ends_with(".otf")) {
                return Some(path);
            }
        }
    }
    None
}

/// Adds the Material font to egui's families, if it's there. Called once at startup,
/// before the style is installed.
pub fn install(fonts: &mut egui::FontDefinitions) {
    let Some(path) = font_path() else { return };
    let Ok(bytes) = std::fs::read(&path) else { return };
    fonts.font_data.insert("material".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    fonts.families.insert(egui::FontFamily::Name("material".into()), vec!["material".into()]);
}

/// Whether the installed Material font has `code` (remembered per icon).
fn in_font(painter: &egui::Painter, code: char) -> bool {
    static KNOWN: std::sync::Mutex<Vec<(char, bool)>> = std::sync::Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, has)) = known.iter().find(|(c, _)| *c == code) {
        return *has;
    }
    let font = egui::FontId::new(16.0, egui::FontFamily::Name("material".into()));
    let has = painter.ctx().fonts_mut(|f| f.has_glyph(&font, code));
    known.push((code, has));
    has
}

/// Draws `icon` centered in `rect`, in `color`.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: egui::Color32) {
    let size = rect.width().min(rect.height());
    // The font installed may be a subset made before this icon was added: draw the
    // shape then, rather than an empty box.
    if material_font() && in_font(painter, icon.code) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon.code,
            egui::FontId::new(size, egui::FontFamily::Name("material".into())),
            color,
        );
        return;
    }
    let c = rect.center();
    // A 24-unit Material grid mapped onto the rect, so the shapes below read as
    // coordinates from the icon sheet.
    let u = size / 24.0;
    let at = |x: f32, y: f32| egui::pos2(c.x + (x - 12.0) * u, c.y + (y - 12.0) * u);
    let stroke = egui::Stroke::new((2.0 * u).max(1.0), color);
    let round = egui::Stroke { width: stroke.width, color };
    let line = |a: (f32, f32), b: (f32, f32)| painter.line_segment([at(a.0, a.1), at(b.0, b.1)], round);
    let poly = |pts: &[(f32, f32)]| {
        painter.add(egui::Shape::convex_polygon(pts.iter().map(|p| at(p.0, p.1)).collect(), color, egui::Stroke::NONE));
    };
    let bar = |x0: f32, y0: f32, x1: f32, y1: f32| {
        painter.rect_filled(egui::Rect::from_min_max(at(x0, y0), at(x1, y1)), 1.0 * u, color);
    };
    match icon.shape {
        Shape::Play => poly(&[(8.0, 5.0), (19.0, 12.0), (8.0, 19.0)]),
        Shape::Pause => {
            bar(6.0, 5.0, 10.0, 19.0);
            bar(14.0, 5.0, 18.0, 19.0);
        }
        Shape::SkipStart => {
            bar(6.0, 6.0, 8.5, 18.0);
            poly(&[(19.0, 6.0), (19.0, 18.0), (9.5, 12.0)]);
        }
        Shape::SkipEnd => {
            poly(&[(5.0, 6.0), (14.5, 12.0), (5.0, 18.0)]);
            bar(15.5, 6.0, 18.0, 18.0);
        }
        Shape::StepBack => {
            line((15.0, 5.0), (9.0, 12.0));
            line((9.0, 12.0), (15.0, 19.0));
        }
        Shape::StepOn => {
            line((9.0, 5.0), (15.0, 12.0));
            line((15.0, 12.0), (9.0, 19.0));
        }
        Shape::Cut => {
            // Scissors: two blades crossing over two rings.
            line((7.0, 4.0), (16.0, 16.0));
            line((17.0, 4.0), (8.0, 16.0));
            painter.circle_stroke(at(7.0, 18.5), 2.6 * u, round);
            painter.circle_stroke(at(17.0, 18.5), 2.6 * u, round);
        }
        Shape::Copy => {
            painter.rect_stroke(egui::Rect::from_min_max(at(8.0, 4.0), at(20.0, 16.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            painter.rect_stroke(egui::Rect::from_min_max(at(4.0, 8.0), at(16.0, 20.0)), 2.0 * u, round, egui::StrokeKind::Inside);
        }
        Shape::Delete => {
            // Bin: lid, body, two slots.
            bar(5.0, 5.5, 19.0, 7.5);
            bar(9.0, 3.0, 15.0, 5.0);
            painter.rect_stroke(egui::Rect::from_min_max(at(6.5, 8.0), at(17.5, 21.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            line((10.0, 11.0), (10.0, 18.0));
            line((14.0, 11.0), (14.0, 18.0));
        }
        Shape::Group => {
            // Two links of a chain.
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 9.0), at(13.0, 15.0)), 3.0 * u, round, egui::StrokeKind::Inside);
            painter.rect_stroke(egui::Rect::from_min_max(at(11.0, 9.0), at(21.0, 15.0)), 3.0 * u, round, egui::StrokeKind::Inside);
        }
        Shape::HalfCircle => {
            painter.circle_stroke(c, 8.0 * u, round);
            painter.add(egui::Shape::convex_polygon(
                (0..=16)
                    .map(|i| {
                        let a = std::f32::consts::PI * (i as f32 / 16.0) - std::f32::consts::FRAC_PI_2;
                        egui::pos2(c.x + a.cos() * 8.0 * u, c.y + a.sin() * 8.0 * u)
                    })
                    .collect(),
                color,
                egui::Stroke::NONE,
            ));
        }
        Shape::Volume => {
            poly(&[(4.0, 9.5), (8.0, 9.5), (12.0, 5.0), (12.0, 19.0), (8.0, 14.5), (4.0, 14.5)]);
            painter.circle_stroke(at(13.0, 12.0), 4.0 * u, round);
            painter.circle_stroke(at(13.0, 12.0), 7.0 * u, round);
        }
        Shape::Title => {
            bar(4.0, 5.0, 20.0, 7.0);
            bar(10.5, 7.0, 13.5, 19.0);
        }
        Shape::Magnet => {
            // A horseshoe: two prongs and the arc over them.
            painter.circle_stroke(at(12.0, 12.0), 7.0 * u, egui::Stroke::new(3.0 * u, color));
            bar(4.0, 12.0, 8.0, 20.0);
            bar(16.0, 12.0, 20.0, 20.0);
        }
        Shape::FitScreen => {
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 6.0), at(21.0, 18.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            line((8.0, 12.0), (16.0, 12.0));
            poly(&[(6.0, 12.0), (9.5, 9.5), (9.5, 14.5)]);
            poly(&[(18.0, 12.0), (14.5, 9.5), (14.5, 14.5)]);
        }
        Shape::Height => {
            line((12.0, 5.0), (12.0, 19.0));
            poly(&[(12.0, 3.0), (8.5, 7.5), (15.5, 7.5)]);
            poly(&[(12.0, 21.0), (8.5, 16.5), (15.5, 16.5)]);
        }
        Shape::Undo | Shape::Redo => {
            let flip = icon.shape == Shape::Redo;
            let x = |v: f32| if flip { 24.0 - v } else { v };
            painter.add(egui::Shape::line(
                (0..=12)
                    .map(|i| {
                        let a = std::f32::consts::PI * (0.15 + 0.85 * i as f32 / 12.0);
                        at(x(12.0 - a.cos() * 7.0), 14.0 - a.sin() * 6.0)
                    })
                    .collect(),
                round,
            ));
            poly(&[(x(5.0), 6.0), (x(5.0), 13.0), (x(11.0), 9.5)]);
        }
        Shape::Home => {
            poly(&[(12.0, 3.0), (22.0, 12.0), (2.0, 12.0)]);
            painter.rect_stroke(egui::Rect::from_min_max(at(5.0, 11.0), at(19.0, 21.0)), 1.0 * u, round, egui::StrokeKind::Inside);
        }
        Shape::Add => {
            line((12.0, 5.0), (12.0, 19.0));
            line((5.0, 12.0), (19.0, 12.0));
        }
        Shape::NewFolder => {
            poly(&[(3.0, 5.5), (10.0, 5.5), (12.0, 8.0), (3.0, 8.0)]);
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 7.0), at(21.0, 19.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            line((12.0, 10.5), (12.0, 16.5));
            line((9.0, 13.5), (15.0, 13.5));
        }
        Shape::Upload => {
            line((12.0, 4.0), (12.0, 15.0));
            poly(&[(12.0, 2.5), (7.5, 8.0), (16.5, 8.0)]);
            line((4.0, 18.5), (20.0, 18.5));
        }
        Shape::Library => {
            painter.rect_stroke(egui::Rect::from_min_max(at(7.0, 4.0), at(21.0, 16.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            poly(&[(9.0, 14.0), (13.0, 9.0), (17.0, 14.0)]);
            line((3.0, 8.0), (3.0, 20.0));
            line((3.0, 20.0), (16.0, 20.0));
        }
        Shape::Stop => bar(6.0, 6.0, 18.0, 18.0),
        Shape::Mic => {
            // A capsule on a stand.
            painter.rect_stroke(egui::Rect::from_min_max(at(9.0, 3.0), at(15.0, 14.0)), 3.0 * u, round, egui::StrokeKind::Inside);
            line((6.0, 11.0), (6.5, 13.5));
            line((18.0, 11.0), (17.5, 13.5));
            line((6.5, 13.5), (12.0, 17.5));
            line((17.5, 13.5), (12.0, 17.5));
            line((12.0, 17.5), (12.0, 21.0));
        }
        Shape::Subtitles => {
            // A screen with two lines of captions along its bottom.
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 5.0), at(21.0, 19.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            bar(6.0, 12.5, 9.0, 14.5);
            bar(10.5, 12.5, 18.0, 14.5);
            bar(6.0, 15.5, 14.0, 17.5);
            bar(15.5, 15.5, 18.0, 17.5);
        }
        Shape::Record => {
            painter.circle_filled(c, 7.0 * u, round.color);
        }
        Shape::Star => {
            let mut pts = Vec::new();
            for i in 0..10 {
                let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
                let r = if i % 2 == 0 { 9.0 } else { 4.0 };
                pts.push((12.0 + a.cos() * r, 12.0 + a.sin() * r));
            }
            painter.add(egui::Shape::closed_line(pts.iter().map(|p| at(p.0, p.1)).collect(), round));
        }
        Shape::Folder => {
            poly(&[(3.0, 5.5), (10.0, 5.5), (12.0, 8.0), (3.0, 8.0)]);
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 7.0), at(21.0, 19.0)), 2.0 * u, round, egui::StrokeKind::Inside);
        }
        Shape::Movie => {
            // A strip of film: a frame with perforations down both sides.
            painter.rect_stroke(egui::Rect::from_min_max(at(3.0, 6.0), at(21.0, 18.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            for i in 0..3 {
                let y = 7.5 + i as f32 * 3.6;
                bar(4.5, y, 6.5, y + 1.8);
                bar(17.5, y, 19.5, y + 1.8);
            }
        }
        Shape::Music => {
            painter.circle_stroke(at(8.5, 17.0), 3.0 * u, round);
            line((11.5, 17.0), (11.5, 5.5));
            line((11.5, 5.5), (19.0, 8.0));
            line((11.5, 9.5), (19.0, 12.0));
        }
        Shape::FolderOpen => {
            // The folder's back, and its front tipped open.
            poly(&[(3.0, 5.5), (10.0, 5.5), (12.0, 8.0), (3.0, 8.0)]);
            line((3.0, 7.0), (3.0, 19.0));
            line((3.0, 7.0), (19.0, 7.0));
            painter.add(egui::Shape::closed_line(vec![at(3.0, 19.0), at(6.5, 11.0), at(22.0, 11.0), at(18.5, 19.0)], round));
        }
        Shape::Puzzle => {
            // A jigsaw piece: a square with a knob on top and one on the right.
            painter.rect_stroke(egui::Rect::from_min_max(at(4.0, 7.0), at(17.0, 20.0)), 1.5 * u, round, egui::StrokeKind::Inside);
            painter.circle_filled(at(10.5, 5.5), 2.6 * u, color);
            painter.circle_filled(at(18.5, 13.5), 2.6 * u, color);
        }
        Shape::Refresh | Shape::History => {
            // Most of a circle, with an arrowhead where it stops.
            let r = 7.5;
            painter.add(egui::Shape::line(
                (0..=20)
                    .map(|i| {
                        let a = std::f32::consts::TAU * (0.12 + 0.8 * i as f32 / 20.0);
                        at(12.0 + a.cos() * r, 12.0 + a.sin() * r)
                    })
                    .collect(),
                round,
            ));
            if icon.shape == Shape::History {
                poly(&[(2.5, 8.0), (8.5, 8.5), (4.5, 13.0)]);
                line((12.0, 8.0), (12.0, 12.5));
                line((12.0, 12.5), (15.0, 14.5));
            } else {
                poly(&[(21.5, 4.0), (21.0, 11.0), (14.5, 9.0)]);
            }
        }
        Shape::Warning => {
            painter.add(egui::Shape::closed_line(vec![at(12.0, 3.0), at(22.0, 20.0), at(2.0, 20.0)], round));
            line((12.0, 9.0), (12.0, 14.0));
            painter.circle_filled(at(12.0, 17.0), 1.2 * u, color);
        }
        Shape::Globe => {
            painter.circle_stroke(c, 9.0 * u, round);
            line((3.0, 12.0), (21.0, 12.0));
            let meridian = |k: f32| {
                painter.add(egui::Shape::line(
                    (0..=12)
                        .map(|i| {
                            let a = std::f32::consts::PI * (i as f32 / 12.0 - 0.5);
                            at(12.0 + k * a.cos() * 4.0, 12.0 + a.sin() * 9.0)
                        })
                        .collect(),
                    round,
                ));
            };
            meridian(1.0);
            meridian(-1.0);
        }
        Shape::Move => {
            line((12.0, 4.0), (12.0, 20.0));
            line((4.0, 12.0), (20.0, 12.0));
            poly(&[(12.0, 2.0), (9.0, 6.0), (15.0, 6.0)]);
            poly(&[(12.0, 22.0), (9.0, 18.0), (15.0, 18.0)]);
            poly(&[(2.0, 12.0), (6.0, 9.0), (6.0, 15.0)]);
            poly(&[(22.0, 12.0), (18.0, 9.0), (18.0, 15.0)]);
        }
        Shape::Gear => {
            // Eight teeth around a ring.
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let (s, k) = (a.sin(), a.cos());
                painter.line_segment([at(12.0 + k * 6.0, 12.0 + s * 6.0), at(12.0 + k * 9.5, 12.0 + s * 9.5)], egui::Stroke::new(3.2 * u, color));
            }
            painter.circle_stroke(c, 6.0 * u, egui::Stroke::new(2.4 * u, color));
            painter.circle_stroke(c, 2.2 * u, round);
        }
        Shape::Save => {
            painter.rect_stroke(egui::Rect::from_min_max(at(4.0, 4.0), at(20.0, 20.0)), 2.0 * u, round, egui::StrokeKind::Inside);
            bar(7.0, 5.0, 15.0, 9.0);
            painter.circle_stroke(at(12.0, 14.5), 2.5 * u, round);
        }
        Shape::Download => {
            line((12.0, 3.0), (12.0, 14.0));
            poly(&[(12.0, 16.5), (7.5, 11.0), (16.5, 11.0)]);
            line((4.0, 19.5), (20.0, 19.5));
        }
        Shape::Sparkle => {
            // A big four-pointed star and a small one.
            poly(&[(10.0, 4.0), (12.0, 10.0), (18.0, 12.0), (12.0, 14.0), (10.0, 20.0), (8.0, 14.0), (2.0, 12.0), (8.0, 10.0)]);
            poly(&[(18.0, 2.0), (19.0, 5.0), (22.0, 6.0), (19.0, 7.0), (18.0, 10.0), (17.0, 7.0), (14.0, 6.0), (17.0, 5.0)]);
        }
        Shape::Info => {
            painter.circle_stroke(c, 9.0 * u, round);
            line((12.0, 11.0), (12.0, 17.0));
            painter.circle_filled(at(12.0, 7.5), 1.3 * u, color);
        }
    }
}

/// A button with an icon before its text. `primary` fills it with the accent (the one
/// thing a screen most wants done).
pub fn text_button(ui: &mut egui::Ui, icon: Icon, text: &str, primary: bool) -> egui::Response {
    let font = egui::FontId::proportional(crate::style::TEXT);
    let text_color = if primary { egui::Color32::WHITE } else { ui.visuals().text_color() };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, text_color);
    let icon_size = 18.0;
    let pad = egui::vec2(10.0, 6.0);
    let size = egui::vec2(pad.x * 2.0 + icon_size + 6.0 + galley.size().x, (galley.size().y.max(icon_size) + pad.y * 2.0).max(30.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let fill = if primary {
            if response.hovered() { crate::style::ACCENT.gamma_multiply(1.15) } else { crate::style::ACCENT }
        } else {
            visuals.weak_bg_fill
        };
        ui.painter().rect(rect, crate::style::ROUNDING, fill, if primary { egui::Stroke::NONE } else { visuals.bg_stroke }, egui::StrokeKind::Inside);
        let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + pad.x + icon_size / 2.0, rect.center().y), egui::vec2(icon_size, icon_size));
        paint(ui.painter(), icon_rect, icon, if primary { egui::Color32::WHITE } else { visuals.fg_stroke.color });
        let at = egui::pos2(icon_rect.right() + 6.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(at, galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A menu row: icon (or the room for one), text, and the shortcut on the right.
pub fn menu_item(ui: &mut egui::Ui, icon: Option<Icon>, text: &str, shortcut: &str, enabled: bool) -> egui::Response {
    let font = egui::FontId::proportional(crate::style::TEXT);
    let weak = ui.visuals().weak_text_color();
    let color = if enabled { ui.visuals().text_color() } else { weak };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), color);
    let keys = ui.painter().layout_no_wrap(shortcut.to_string(), egui::FontId::proportional(crate::style::TEXT_S), weak);
    let width = ui.available_width().max(22.0 + galley.size().x + 24.0 + keys.size().x + 8.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 24.0), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        if let Some(icon) = icon {
            let r = egui::Rect::from_center_size(egui::pos2(rect.left() + 12.0, rect.center().y), egui::vec2(16.0, 16.0));
            paint(ui.painter(), r, icon, if enabled { ui.visuals().widgets.inactive.fg_stroke.color } else { weak });
        }
        ui.painter().galley(egui::pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0), galley, color);
        ui.painter().galley(egui::pos2(rect.right() - 6.0 - keys.size().x, rect.center().y - keys.size().y / 2.0), keys, weak);
    }
    response
}

/// An icon button that *adds* one of something: the thing's own icon with a small plus
/// over its corner. Two of these side by side read as "add a video track" and "add an
/// audio track"; two identical plus signs don't.
pub fn add_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    let size = egui::vec2(crate::style::ICON, crate::style::ICON);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if response.hovered() || response.is_pointer_button_down_on() {
            ui.painter().rect_filled(rect, crate::style::ROUNDING, visuals.bg_fill);
        }
        let color = visuals.fg_stroke.color;
        // The thing, slightly up and left; the plus in the bottom-right corner.
        paint(ui.painter(), rect.shrink(5.0).translate(egui::vec2(-2.0, -2.0)), icon, color);
        let plus = egui::Rect::from_min_size(rect.right_bottom() - egui::vec2(11.0, 11.0), egui::vec2(10.0, 10.0));
        ui.painter().circle_filled(plus.center(), 5.5, ui.visuals().panel_fill);
        paint(ui.painter(), plus, ADD, crate::style::ACCENT);
    }
    response.on_hover_text(tip)
}

/// A square icon button: the icon, a tooltip that always names its shortcut, and the
/// same hit area as every other one.
pub fn button(ui: &mut egui::Ui, icon: Icon, tip: &str, shortcut: &str, enabled: bool) -> egui::Response {
    let size = egui::vec2(crate::style::ICON, crate::style::ICON);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = if enabled { response } else { response.on_disabled_hover_text(tip) };
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
            ui.painter().rect_filled(rect, crate::style::ROUNDING, visuals.bg_fill);
        }
        let color = if enabled { visuals.fg_stroke.color } else { ui.visuals().weak_text_color() };
        paint(ui.painter(), rect.shrink(4.0), icon, color);
    }
    if !enabled || tip.is_empty() {
        return response;
    }
    response.on_hover_text(if shortcut.is_empty() { tip.to_string() } else { format!("{tip}  ·  {shortcut}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon has a name, a codepoint in Material's private-use range, and its own
    /// drawn shape — the three have to agree, because the font can be missing.
    #[test]
    fn icons_are_complete() {
        for icon in ALL {
            assert!(!icon.name.is_empty());
            let c = icon.code as u32;
            assert!((0xe000..=0xf8ff).contains(&c), "{} is {c:#x}, not a Material codepoint", icon.name);
        }
        let mut codes: Vec<u32> = ALL.iter().map(|i| i.code as u32).collect();
        codes.sort();
        let count = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), count, "two icons share a codepoint");
    }
}
