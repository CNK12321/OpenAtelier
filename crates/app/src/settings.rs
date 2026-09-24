//! What the app remembers between runs: recent projects and which plugins are off.
//!
//! One small JSON file in the app folder (`%LOCALAPPDATA%/OpenAtelier/settings.json` on
//! Windows, `~/.local/share/OpenAtelier` on Linux) (`OA_CONFIG_DIR`
//! overrides the folder, for tests and portable installs). It's written atomically, and
//! anything unreadable is ignored rather than fatal — settings are a convenience, never
//! the user's data.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const RECENT_MAX: usize = 12;

/// A project as the start page lists it: where it is, what it's called, and a picture
/// of a frame from it. The name and picture are kept here so the page doesn't have to
/// open every project to draw itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    #[serde(default)]
    pub name: String,
    /// A PNG beside the project (`<project>.thumb.png`), written when it's saved.
    #[serde(default)]
    pub thumb: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Projects, most recent first.
    #[serde(default, rename = "recent_projects")]
    pub recent: Vec<RecentProject>,
    /// Plugin ids the user turned off.
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
    /// The language to show the app in ("en", "fr"/"fr-CA"…). None: the system's.
    #[serde(default)]
    pub language: Option<String>,
    /// How much GPU memory the renderer may hold, in MB. None: the default (2 GB).
    #[serde(default)]
    pub vram_budget_mb: Option<u64>,
    /// Keep the project file itself up to date while editing (on by default). The
    /// crash-recovery autosave happens either way.
    #[serde(default = "yes")]
    pub save_as_you_go: bool,
    /// Color management controls (source color, output tone mapping). Off: files are
    /// read by their tags and the controls stay out of the way.
    pub advanced_color: bool,
    /// Squash and the crop sliders in Transform (crop by double-clicking a clip in the
    /// viewer works either way).
    pub advanced_transform: bool,
    /// How new keyframes ease.
    pub default_curve: DefaultCurve,
    /// The Performance panel under the inspector.
    pub show_performance: bool,
    /// A denser properties panel: tighter rows, smaller controls, section notes hidden
    /// (shown on hover instead).
    #[serde(default)]
    pub compact_properties: bool,
    /// The whole interface's size (1 = 100%).
    #[serde(default = "one")]
    pub ui_scale: f32,
    /// How long a picture or a title is when added to the timeline, in seconds.
    #[serde(default = "five")]
    pub still_seconds: f64,
    /// The Export window's last choices.
    pub export: ExportPrefs,
    /// Where the viewer and the properties go, for projects that haven't picked.
    pub layout: Layout,
    /// Each saved project's panel sizes, format and layout, by file path: reopening a
    /// project puts them back.
    pub project_views: std::collections::BTreeMap<String, ProjectView>,
    /// The main window's size (points) and whether it was maximized, last time.
    pub window: Option<WindowState>,
    /// Graphics API: "auto", "dx12", "vulkan", "metal" or "gl" (applies on restart).
    #[serde(default = "auto")]
    pub gpu_backend: String,
    /// Part of the GPU's name to prefer, or "" for the best one (applies on restart).
    #[serde(default)]
    pub gpu_adapter: String,
    /// Decode video with ffmpeg even where the system's hardware decoder could (for
    /// drivers whose decoder misbehaves).
    #[serde(default)]
    pub decode_with_ffmpeg: bool,
    /// The caption generator's last choices.
    pub captions: CaptionPrefs,
    #[serde(skip)]
    path: Option<PathBuf>,
}

/// How the editor's panels are arranged.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Vertical when the format being edited is taller than it is wide.
    Auto,
    /// The viewer in the middle, the properties in the tall column on the right.
    #[default]
    Standard,
    /// For vertical video: the viewer in the tall column on the right, the properties
    /// in the middle.
    Vertical,
}

/// How one project was last seen: panel sizes (points), the format being viewed and
/// its layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectView {
    /// The format variant's id.
    pub variant: Option<u64>,
    /// `None`: the default from Settings.
    pub layout: Option<Layout>,
    pub inspector: f32,
    pub media: f32,
    pub timeline: f32,
    /// The right-hand column's width in the vertical layout (the viewer's).
    pub viewer: f32,
}

impl Default for ProjectView {
    fn default() -> Self {
        ProjectView { variant: None, layout: None, inspector: 320.0, media: 300.0, timeline: 280.0, viewer: 420.0 }
    }
}

/// The caption generator's choices (Captions window).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionPrefs {
    /// The Whisper model to use.
    pub model: String,
    /// A language code ("en", "fr"…), or empty to detect it.
    pub language: String,
    pub grouping: oa_captions::Grouping,
    /// How captions look: a title clip used as the template (its text is replaced).
    /// `None` until the style is first changed: the default caption look.
    pub style: Option<oa_doc::Item>,
    /// Clean the sound up before listening (`oa_audio::fx::voice_cleanup`).
    pub enhance_voice: bool,
}

impl Default for CaptionPrefs {
    fn default() -> Self {
        CaptionPrefs { model: "small".into(), language: String::new(), grouping: oa_captions::Grouping::default(), style: None, enhance_voice: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub size: [f32; 2],
    #[serde(default)]
    pub maximized: bool,
}

/// Views kept for this many projects at most.
pub const MAX_PROJECT_VIEWS: usize = 200;

fn auto() -> String {
    "auto".into()
}

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

fn five() -> f64 {
    5.0
}

/// The shape new keyframes get.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultCurve {
    pub shape: CurveShape,
    /// The power of the ease (1 is linear, 2 quadratic…).
    pub power: f64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveShape {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Hold,
}

impl Default for DefaultCurve {
    fn default() -> Self {
        DefaultCurve { shape: CurveShape::EaseInOut, power: 1.5 }
    }
}

impl DefaultCurve {
    pub fn interp(self) -> oa_params::Interp {
        use oa_params::{EaseDir, Interp};
        let power = self.power.clamp(0.1, 10.0);
        match self.shape {
            CurveShape::Linear => Interp::Linear,
            CurveShape::Hold => Interp::Hold,
            CurveShape::EaseIn => Interp::Power { power, ease: EaseDir::In },
            CurveShape::EaseOut => Interp::Power { power, ease: EaseDir::Out },
            CurveShape::EaseInOut => Interp::Power { power, ease: EaseDir::InOut },
        }
    }
}

/// What the Export window remembers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportPrefs {
    /// "h264", "hevc", "prores" or "gif".
    pub codec: String,
    /// Short edge in px; 0 = the format's own size.
    pub short_edge: u32,
    /// Quality on x264's scale (lower is better).
    pub crf: u32,
    /// "auto", "hardware" or "software".
    pub encoder: String,
    pub audio: bool,
    /// Frames per second for GIFs.
    pub gif_fps: u32,
}

impl Default for ExportPrefs {
    fn default() -> Self {
        ExportPrefs { codec: "h264".into(), short_edge: 0, crf: 18, encoder: "auto".into(), audio: true, gif_fps: 15 }
    }
}

/// `OA_CONFIG_DIR`, else the app folder (`oa_media::app_dir`: `%LOCALAPPDATA%OpenAtelier`,
/// `~/.local/share/OpenAtelier`, …).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OA_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    oa_media::app_dir()
}

/// The asset library: files kept for every project (`OA_ASSETS_DIR` overrides it).
pub fn assets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OA_ASSETS_DIR") {
        return PathBuf::from(dir);
    }
    config_dir().join("assets")
}

/// Where plugins live: `<config>/plugins`, one folder each.
pub fn plugins_dir() -> PathBuf {
    config_dir().join("plugins")
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: Vec::new(),
            disabled_plugins: Vec::new(),
            language: None,
            vram_budget_mb: None,
            save_as_you_go: true,
            advanced_color: false,
            advanced_transform: false,
            default_curve: DefaultCurve::default(),
            show_performance: false,
            compact_properties: false,
            ui_scale: 1.0,
            still_seconds: 5.0,
            export: ExportPrefs::default(),
            layout: Layout::Standard,
            project_views: Default::default(),
            window: None,
            gpu_backend: auto(),
            gpu_adapter: String::new(),
            decode_with_ffmpeg: false,
            captions: CaptionPrefs::default(),
            path: None,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = config_dir().join("settings.json");
        let mut settings: Settings =
            std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        settings.path = Some(path);
        settings
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = crate::autosave::write_atomic(path, json.as_bytes());
        }
    }

    /// Puts `path` at the top of the list (no duplicates), with what it's called and a
    /// picture of it if there is one, and saves.
    pub fn remember_project(&mut self, path: &Path, name: &str, thumb: Option<PathBuf>) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, RecentProject { path: path.to_path_buf(), name: name.to_string(), thumb });
        self.recent.truncate(RECENT_MAX);
        self.save();
    }

    pub fn forget_project(&mut self, path: &Path) {
        self.recent.retain(|r| r.path != path);
        self.save();
    }

    /// The GPU memory budget in bytes, kept to something sane.
    pub fn vram_budget(&self) -> u64 {
        self.vram_budget_mb.unwrap_or(2048).clamp(256, 16384) << 20
    }

    pub fn set_save_as_you_go(&mut self, on: bool) {
        self.save_as_you_go = on;
        self.save();
    }

    pub fn set_vram_budget(&mut self, mb: u64) {
        self.vram_budget_mb = Some(mb.clamp(256, 16384));
        self.save();
    }

    pub fn set_language(&mut self, lang: Option<String>) {
        self.language = lang;
        self.save();
    }

    pub fn set_plugin_enabled(&mut self, id: &str, enabled: bool) {
        self.disabled_plugins.retain(|p| p != id);
        if !enabled {
            self.disabled_plugins.push(id.to_string());
        }
        self.save();
    }
}
