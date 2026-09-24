//! OpenAtelier preview UI.
//!
//!   oa-app [project.oaproj.json | files...] [--autoplay] [--script steps.txt]
//!
//! Drop media files on the window to import them, or a project file to open it. The UI
//! shares one wgpu device with the renderer and the hardware decoder, so decoded frames
//! go straight to the screen.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod audition;
mod autosave;
mod band;
mod bin;
mod clips;
mod color;
mod compound;
mod crop;
mod logo;
mod surface;
mod sound_cards;
mod connections;
mod tracks;
mod update;
mod deps;
mod winfocus;
mod curves;
mod command;
mod editor;
mod export_dialog;
mod export_worker;
mod fontpick;
mod formats;
mod guard;
mod home;
mod icons;
mod i18n;
mod inspector;
mod menu;
mod notify;
mod plugins;
mod prefs;
mod picker;
mod previews;
mod record;
mod captions;
mod layout;
mod script;
mod settings;
mod sources;
mod style;
mod thumbnail;
mod thumbs;
mod timeline;
mod viewer;
mod waves;
mod widgets;

use eframe::egui;
use editor::Editor;
use oa_audio::{AudioClip, AudioEngine, MixHandle, MixState, TimelineAudio};
use oa_doc::{ItemId, VariantId};
use oa_gpu::{readback, FrameSource, FusionMode, GpuContext, GpuImage, RenderOptions, RenderStats, Renderer};
use oa_graph::registry::Registry;
use oa_graph::{optimize, KeyContext, OptLevel};
use oa_media::{Imported, MediaKind, MediaProbe};
use oa_plan::{plan_frame, PlanOptions, PlanReport};
use oa_time::Time;
use sources::Sources;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// A project opened on a worker thread: the editor and warnings, or why it failed.
/// A save running on its own thread: the file, the snapshot being written, and the
/// thread.
type BackgroundSave = (PathBuf, Arc<oa_doc::Project>, std::thread::JoinHandle<Result<(), String>>);

type Opened = Result<(Editor, Vec<String>), String>;

/// A file being imported on a worker thread.
struct PendingImport {
    path: PathBuf,
    name: String,
    rx: std::sync::mpsc::Receiver<Result<Imported, String>>,
    /// Also put it at the end of the timeline (dropped or imported files), or only in
    /// the bin, in this folder (recordings).
    to_timeline: bool,
    folder: String,
}

const MEDIA_EXTENSIONS: &[&str] =
    &["mp4", "mov", "mkv", "m4v", "webm", "avi", "gif", "png", "jpg", "jpeg", "bmp", "webp", "mp3", "m4a", "wav", "flac", "aac", "ogg"];

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let autoplay = args.iter().any(|a| a == "--autoplay");
    let script_path = args.iter().position(|a| a == "--script").and_then(|i| args.get(i + 1)).cloned();
    let files: Vec<PathBuf> =
        args.iter().filter(|a| !a.starts_with("--") && Some(*a) != script_path.as_ref()).map(PathBuf::from).collect();
    let script = match script_path.map(|p| script::Script::load(Path::new(&p))) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
        None => None,
    };
    // The GPU: eframe makes the device (so the window's surface and, on Linux, the
    // display connection OpenGL needs are set up with it), choosing the adapter our way —
    // a real GPU over software, dedicated over integrated, the API that suits the
    // platform — among those that can draw into the window. Settings (or OA_GPU_BACKEND,
    // OA_GPU_ADAPTER) can pin one.
    let prefs = settings::Settings::load();
    let pref = oa_gpu::GpuPreference::new(&prefs.gpu_backend, &prefs.gpu_adapter);
    let mut instance = eframe::wgpu::InstanceDescriptor::new_without_display_handle();
    instance.backends = pref.backends();
    let chooser = pref.clone();
    let setup = eframe::egui_wgpu::WgpuSetupCreateNew {
        instance_descriptor: instance,
        display_handle: None,
        power_preference: eframe::wgpu::PowerPreference::HighPerformance,
        native_adapter_selector: Some(Arc::new(move |adapters, surface| {
            let usable: Vec<_> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).cloned().collect();
            oa_gpu::select::choose(&usable, &chooser).ok_or_else(|| {
                let found: Vec<String> = adapters.iter().map(|a| oa_gpu::select::describe(&a.get_info())).collect();
                format!("none of the GPUs can draw into the window (found: {})", if found.is_empty() { "none".into() } else { found.join(", ") })
            })
        })),
        device_descriptor: Arc::new(oa_gpu::select::device_descriptor),
    };
    // The window: last time's size (and maximized state); fixed for scripted runs, whose
    // coordinates assume it. Too-small sizes are grown once the screen size is known.
    let saved = prefs.window.clone().filter(|_| script.is_none());
    let size = saved.as_ref().map_or([1440.0, 900.0], |w| [w.size[0].max(1024.0), w.size[1].max(640.0)]);
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([800.0, 500.0])
        .with_title("OpenAtelier")
        .with_app_id("openatelier")
        .with_icon(logo::icon(256));
    let viewport = match &saved {
        Some(w) if w.maximized => viewport.with_maximized(true),
        _ => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        // The window's place is ours to remember (layout.rs), not eframe's.
        persist_window: false,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = eframe::run_native(
        "OpenAtelier",
        options,
        Box::new(move |cc| {
            let state = cc.wgpu_render_state.as_ref().ok_or("the window has no GPU renderer")?;
            let ctx = Arc::new(GpuContext::from_parts(state.instance.clone(), state.adapter.clone(), state.device.clone(), state.queue.clone()));
            let mut app = App::new(cc, ctx);
            app.gpu_names = state.available_adapters.iter().map(|a| (a.get_info().name, oa_gpu::select::describe(&a.get_info()))).collect();
            app.apply_settings(&cc.egui_ctx);
            app.script = script;
            app.start_updates();
            app.check_deps();
            if !files.is_empty() {
                app.open_paths(&files);
                app.screen = home::Screen::Editor;
            }
            if autoplay {
                app.set_playing(true);
            }
            Ok(Box::new(app))
        }),
    );
    // No window at all (no GPU driver that can draw one): say so, with what to try.
    if let Err(e) = &result {
        let advice = if cfg!(windows) {
            "Update the graphics driver, or try another graphics API: set the environment variable OA_GPU_BACKEND to vulkan or gl."
        } else {
            "Install your GPU's Vulkan driver (mesa-vulkan-drivers, or the vendor's), or try OpenGL: set OA_GPU_BACKEND=gl."
        };
        eprintln!("OpenAtelier couldn't start: {e}\n{advice}");
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("OpenAtelier couldn't start")
            .set_description(format!("The graphics card couldn't be set up:\n\n{e}\n\n{advice}"))
            .show();
    }
    result
}

/// "1.4 GB" for a byte count.
fn bytes(n: u64) -> String {
    match n {
        0..=999_999 => format!("{} KB", n / 1000),
        1_000_000..=999_999_999 => format!("{} MB", n / 1_000_000),
        _ => format!("{:.1} GB", n as f64 / 1e9),
    }
}

/// The system UI font first (easier to read than egui's built-in), then egui's own fonts,
/// then a symbol font as the last fallback — egui's fonts have no ▲ ▼ ✕ ◆ ↶ and would
/// draw empty boxes. Anything missing just leaves egui's defaults in place.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let load = |path: Option<PathBuf>| path.and_then(|p| std::fs::read(p).ok()).map(|b| Arc::new(egui::FontData::from_owned(b)));
    let (ui_font, symbol_font) = system_ui_fonts();
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    if let Some(ui) = load(ui_font) {
        fonts.font_data.insert("system-ui".into(), ui);
        proportional.insert(0, "system-ui".into());
    }
    icons::install(&mut fonts);
    if let Some(symbols) = load(symbol_font) {
        fonts.font_data.insert("system-symbols".into(), symbols);
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("system-symbols".into());
        }
    }
    ctx.set_fonts(fonts);
}

/// The system's interface font and a font with symbols in it, as files: Segoe UI on
/// Windows, San Francisco on macOS, and whatever fontconfig calls "sans-serif" on Linux
/// (DejaVu or Noto for the symbols).
fn system_ui_fonts() -> (Option<PathBuf>, Option<PathBuf>) {
    let exists = |p: PathBuf| p.is_file().then_some(p);
    if cfg!(windows) {
        let dir = std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts")).unwrap_or_default();
        return (exists(dir.join("segoeui.ttf")), exists(dir.join("seguisym.ttf")));
    }
    if cfg!(target_os = "macos") {
        let ui = ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"].into_iter().find_map(|p| exists(PathBuf::from(p)));
        return (ui, exists(PathBuf::from("/System/Library/Fonts/Apple Symbols.ttf")));
    }
    // fontconfig knows where the desktop's fonts are.
    let fc = |pattern: &str| {
        let out = oa_media::tool("fc-match").args(["-f", "%{file}", pattern]).output().ok().filter(|o| o.status.success())?;
        let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        // Only TrueType/OpenType files (egui can't read the others).
        path.extension().is_some_and(|e| ["ttf", "otf", "ttc"].iter().any(|x| e.eq_ignore_ascii_case(x))).then_some(path).and_then(exists)
    };
    let ui = fc("sans-serif:style=Regular");
    let symbols = fc("DejaVu Sans").or_else(|| fc("Noto Sans Symbols2")).or_else(|| fc("Noto Sans Symbols"));
    (ui, symbols)
}

struct Preview {
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
    /// Held so the pool doesn't reuse the working-format image mid-frame.
    _image: GpuImage,
}

#[derive(Clone, PartialEq, Eq)]
struct FrameKey {
    playhead: Time,
    variant: usize,
    scale: u32,
    reference: bool,
    document: usize,
    /// Envelopes ready for connected properties (a frame redraws as they arrive).
    follow: usize,
}

struct App {
    gpu: Arc<GpuContext>,
    renderer: Renderer,
    /// Shared with export threads.
    registry: Arc<Registry>,
    sources: Sources,
    editor: Editor,
    /// How video files get decoded (Media Foundation where it can, ffmpeg otherwise).
    decoders: oa_media::DecoderChoice,
    /// Every GPU the window could have used: its name, and a description (for Settings).
    gpu_names: Vec<(String, String)>,

    audio: Option<AudioEngine>,
    /// What the mixer plays; updated in place after every edit, even mid-playback.
    mix: MixHandle,
    /// The document snapshot the mix was last built from.
    mixed: Option<Arc<oa_doc::Project>>,
    volume: f32,

    playhead: Time,
    playing: bool,
    last_tick: Instant,
    variant: usize,
    /// Preview resolution relative to the canvas; 0 = Auto (match what the viewer shows).
    scale: f32,
    /// Physical screen pixels per canvas pixel in the viewer, for Auto resolution.
    display_scale: f32,
    reference: bool,
    selection: Option<ItemId>,
    /// Every selected clip (includes `selection`, the one the inspector shows).
    selected: std::collections::BTreeSet<ItemId>,
    clipboard: clips::Clipboard,
    /// Timeline moves and trims snap to edges and the playhead.
    snapping: bool,
    /// A track being renamed in its header menu, with the name typed so far.
    renaming: Option<(oa_doc::TrackId, String)>,
    /// The property each clip's keyframe line shows, where it isn't the default.
    bands: std::collections::HashMap<ItemId, band::Band>,
    /// Copied effects (right-click → Copy effect) and property value.
    effect_clipboard: Vec<oa_doc::EffectInstance>,
    /// With "Copy all effects": whether that clip's outro plays its intro backwards
    /// (pasted along with the effects). `None` for a single copied effect.
    effect_clipboard_reverse: Option<bool>,
    value_clipboard: Option<oa_params::Value>,
    transform_clipboard: Option<clips::TransformCopy>,
    /// The media bin's sort order and search.
    bin: bin::BinView,
    /// The asset library on this computer, shared by every project.
    assets: assets::Assets,
    /// A sound being listened to from the bin, on its own output.
    audition: Option<audition::Audition>,
    /// In a secondary format, transform edits normally touch only that format.
    link_formats: bool,
    viewer_drag: Option<viewer::ViewerDrag>,
    timeline_drag: Option<timeline::TimelineDrag>,
    /// The track picked by double-clicking it: pastes and duplicates go into its first
    /// free space after the playhead, and Alt+↑/↓ moves it.
    selected_track: Option<oa_doc::TrackId>,
    /// An effect being dragged out of the inspector, and where clips were on screen last
    /// frame (timeline boxes; the viewer's canvas) to drop it on.
    effect_drag: Option<inspector::EffectDrag>,
    drop_targets: Vec<(egui::Rect, oa_doc::ItemId)>,
    /// Effect tracks' lanes as last drawn: (rect, track, seconds at its left edge,
    /// seconds per point) — where a dropped effect starts a container.
    fx_lanes: Vec<(egui::Rect, oa_doc::TrackId, f64, f64)>,
    viewer_canvas: Option<(egui::Rect, egui::Rect)>,
    /// The open project's panel sizes, layout and format (see `layout.rs`), the epoch
    /// panels are keyed by (bumped to apply restored sizes), and the project's key.
    project_view: settings::ProjectView,
    view_epoch: u64,
    view_key: Option<(PathBuf, String)>,
    /// A new window size waiting to hold still before it's saved, and since when.
    window_pending: Option<(settings::WindowState, f64)>,
    /// Frames since start while the window is being fitted to the screen (`None`: done).
    window_fitting: Option<u32>,
    timeline_view: timeline::TimelineView,
    view: viewer::ViewerView,
    thumbs: thumbs::Thumbs,
    /// Filmstrips and waveforms drawn on timeline clips.
    clip_previews: previews::ClipPreviews,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    /// Dragging the playhead: audio seeks wait until the drag ends.
    scrubbing: bool,
    /// The playhead jumped during playback: frames don't wait for the decoder until it
    /// has caught up.
    seek_settling: bool,
    /// The start page, or the editor.
    screen: home::Screen,
    home_tab: home::HomeTab,
    /// Recent projects and which plugins are off.
    settings: settings::Settings,
    /// Installed plugins; the registry is built from the enabled ones.
    plugins: plugins::Plugins,
    /// Cards in the bottom-right corner: errors and notes.
    toasts: Vec<notify::Toast>,
    /// Font names drawn in their own font, for the font menu.
    font_samples: fontpick::FontSamples,
    /// How much the preview is being rendered down by to stay inside the GPU memory
    /// budget (1 = not at all). Eases back once there's room again.
    memory_backoff: f64,
    /// The graphics device has gone; the editor stops rendering and says so.
    gpu_lost: bool,
    /// What the OS window title currently says.
    window_title: String,
    /// Which part of the clip the inspector is showing.
    inspector_tab: inspector::Tab,
    /// The window was asked to close with unsaved work: what to do about it.
    closing: bool,
    /// When the project file was last brought up to date (save as you go).
    last_saved: Instant,
    /// Project pictures on the start page, loaded once each (`None`: there isn't one).
    project_thumbs: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// Put the keyboard in the inspector's text box next frame (a title was just added).
    focus_text: bool,
    /// A title being typed into right on the canvas (double-click it in the viewer), and
    /// whether its editor still has to take the keyboard.
    canvas_text: Option<(oa_doc::ItemId, bool)>,
    /// The timelines stepped out of to edit inside compound clips, outermost first.
    compound_trail: Vec<compound::Crumb>,
    /// The curve editor window, when open.
    curve_editor: Option<curves::CurveEditor>,
    /// The wave (LFO) editor window, when open.
    wave_editor: Option<waves::WaveEditor>,
    /// The connection (property follows the sound) editor window, when open.
    connection_editor: Option<connections::ConnectionEditor>,
    /// The track editor (a position following something in the picture), when open.
    track_editor: Option<tracks::TrackEditor>,
    /// The AI tracker's setup, while it runs (it outlives the editor).
    tracker_setup: tracks::TrackerSetup,
    /// Newer versions on GitHub, and installing one.
    updater: update::Updater,
    /// Whether ffmpeg and ffprobe can be run.
    deps: deps::Deps,
    /// Sound levels for properties connected to the sound, and what they were built
    /// from (document, envelopes ready).
    follower: Option<Arc<oa_audio::envelope::Follower>>,
    follower_key: (usize, usize),
    /// Every file the follower needs is analyzed (or failed) for `follower_key.0`.
    follower_complete: bool,
    /// Frames in a row that panicked and were recovered from (`guard.rs`).
    failed_frames: u32,
    /// The Settings window is open.
    settings_open: bool,
    /// A clip being cropped on the canvas (double-click it in the viewer), and the edge
    /// being dragged.
    crop_mode: Option<ItemId>,
    crop_drag: Option<crop::CropDrag>,
    /// A Surface point being dragged in the viewer.
    surface_drag: Option<surface::SurfaceDrag>,
    /// The Export window is open; files waiting to be exported after the running one;
    /// where the running one goes.
    export_dialog: bool,
    /// The Captions window, while it's open.
    captions: Option<Box<captions::CaptionsUi>>,
    /// The Captions window as it was when its captions went on the timeline: back if
    /// that's undone.
    captions_added: Option<Box<captions::CaptionsUi>>,
    export_queue: std::collections::VecDeque<export_dialog::ExportJob>,
    export_path: Option<PathBuf>,
    /// The part of the timeline the Export window is set to (None: all of it), and the
    /// live view of the running export.
    export_range: Option<(Time, Time)>,
    export_view: export_dialog::ExportView,
    /// The audio recorder window.
    recording: record::RecordWindow,

    preview: Option<Preview>,
    rendered: Option<FrameKey>,
    stats: RenderStats,
    report: PlanReport,
    frame_ms: std::collections::VecDeque<f32>,
    messages: Vec<String>,
    error: Option<String>,
    /// A running export, stepped a few frames per UI frame so the window stays alive.
    export: Option<export_worker::ExportRun>,
    /// Scripted input (`--script`), for testing the UI.
    script: Option<script::Script>,
    autosave: autosave::Autosave,
    /// Autosaves left by sessions that didn't end cleanly, offered at startup.
    recoveries: Vec<autosave::Recovery>,
    /// An autosave being opened to carry on with, and whether it stays its project's.
    recovering: Option<(autosave::Recovery, bool)>,
    /// Imports running on worker threads, oldest first.
    imports: std::collections::VecDeque<PendingImport>,
    /// A project being opened on a worker thread.
    opening: Option<(PathBuf, std::sync::mpsc::Receiver<Opened>)>,
    /// A save-as-you-go running on its own thread: the file, the snapshot being written.
    saving: Option<BackgroundSave>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, gpu: Arc<GpuContext>) -> Self {
        let settings = settings::Settings::load();
        i18n::init(settings.language.as_deref(), &settings::config_dir().join("locales"));
        let plugins = plugins::Plugins::load(settings::plugins_dir(), &settings.disabled_plugins);
        let (registry, issues) = plugins.registry();
        let renderer = Renderer::new(
            gpu.clone(),
            RenderOptions { fusion: FusionMode::Async, wait: false, vram_budget: settings.vram_budget(), ..Default::default() },
        );
        let _ = renderer.warm_up(&registry);
        // The font index is built once (it reads every installed font's names); do it now,
        // off the UI thread, so the first title doesn't wait for it.
        std::thread::spawn(|| {
            let _ = oa_text::default_family();
        });
        cc.egui_ctx.set_zoom_factor(1.1);
        install_fonts(&cc.egui_ctx);
        style::install(&cc.egui_ctx);
        // Media Foundation shares frames with a DX12 device only; anything else, and any
        // file it can't take, is decoded by ffmpeg.
        let decoders = oa_media::DecoderChoice {
            #[cfg(windows)]
            bridge: if gpu.is_dx12() { oa_media::windows::D3D11Bridge::new(&gpu).ok() } else { None },
            force_ffmpeg: settings.decode_with_ffmpeg || std::env::var("OA_DECODER").is_ok_and(|v| v == "ffmpeg"),
        };
        let autosave = autosave::Autosave::new(autosave::default_dir());
        let recoveries = autosave.recoverable();
        App {
            gpu,
            renderer,
            registry: Arc::new(registry),
            sources: Sources::new(),
            editor: Editor::new(),
            decoders,
            gpu_names: Vec::new(),
            audio: None,
            mix: MixHandle::default(),
            mixed: None,
            volume: 1.0,
            playhead: Time::ZERO,
            playing: false,
            last_tick: Instant::now(),
            variant: 0,
            scale: 0.0,
            display_scale: 1.0,
            reference: false,
            screen: home::Screen::Home,
            home_tab: Default::default(),
            settings,
            plugins,
            toasts: Vec::new(),
            font_samples: Default::default(),
            memory_backoff: 1.0,
            gpu_lost: false,
            window_title: String::new(),
            inspector_tab: Default::default(),
            closing: false,
            last_saved: Instant::now(),
            project_thumbs: Default::default(),
            selection: None,
            selected: Default::default(),
            clipboard: Vec::new(),
            snapping: true,
            renaming: None,
            bands: Default::default(),
            effect_clipboard: Vec::new(),
            effect_clipboard_reverse: None,
            value_clipboard: None,
            transform_clipboard: None,
            bin: Default::default(),
            assets: assets::Assets::new(settings::assets_dir()),
            audition: None,
            link_formats: false,
            viewer_drag: None,
            timeline_drag: None,
            selected_track: None,
            captions: None,
            captions_added: None,
            effect_drag: None,
            drop_targets: Vec::new(),
            fx_lanes: Vec::new(),
            viewer_canvas: None,
            project_view: Default::default(),
            // Different every launch: panel sizes egui kept from an earlier run never
            // stand in for the ones restored from the project.
            view_epoch: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64),
            view_key: None,
            window_pending: None,
            window_fitting: Some(0),
            timeline_view: timeline::TimelineView::default(),
            view: viewer::ViewerView::default(),
            thumbs: thumbs::Thumbs::default(),
            clip_previews: Default::default(),
            render_state: cc.wgpu_render_state.clone(),
            scrubbing: false,
            seek_settling: false,
            focus_text: false,
            canvas_text: None,
            compound_trail: Vec::new(),
            curve_editor: None,
            wave_editor: None,
            connection_editor: None,
            track_editor: None,
            tracker_setup: Default::default(),
            updater: Default::default(),
            deps: Default::default(),
            follower: None,
            follower_key: (0, 0),
            follower_complete: false,
            failed_frames: 0,
            settings_open: false,
            crop_mode: None,
            crop_drag: None,
            surface_drag: None,
            export_dialog: false,
            export_queue: Default::default(),
            export_path: None,
            export_range: None,
            export_view: Default::default(),
            recording: Default::default(),
            preview: None,
            rendered: None,
            stats: RenderStats::default(),
            report: PlanReport::default(),
            frame_ms: std::collections::VecDeque::new(),
            messages: issues,
            error: None,
            export: None,
            script: None,
            autosave,
            recoveries,
            recovering: None,
            imports: Default::default(),
            opening: None,
            saving: None,
        }
    }

    // ---- projects, plugins and the start page ------------------------------

    /// An empty project, as "New project" makes it.
    pub(crate) fn new_project(&mut self) {
        self.finish_background_save();
        self.editor = Editor::new();
        self.compound_trail.clear();
        self.sources.forget_all();
        self.reset_audio();
        self.selection = None;
        self.selected.clear();
        self.playhead = Time::ZERO;
        self.playing = false;
        self.renderer.clear_cache();
        self.autosave.clear();
        self.variant = 0;
        self.restore_view();
    }

    /// Reads the plugins folder again and rebuilds the registry.
    fn reload_plugins(&mut self) {
        self.plugins = plugins::Plugins::load(settings::plugins_dir(), &self.settings.disabled_plugins);
        self.rebuild_registry();
    }

    fn set_plugin_enabled(&mut self, id: &str, enabled: bool) {
        self.settings.set_plugin_enabled(id, enabled);
        self.plugins.disabled = self.settings.disabled_plugins.iter().cloned().collect();
        self.rebuild_registry();
    }

    /// The enabled plugins' effects become the registry everything renders with.
    fn rebuild_registry(&mut self) {
        let (registry, issues) = self.plugins.registry();
        self.registry = Arc::new(registry);
        self.messages.extend(issues);
        let _ = self.renderer.warm_up(&self.registry);
        self.renderer.clear_cache();
        self.forget_thumbs();
        self.rendered = None;
    }

    // ---- media & projects -------------------------------------------------

    /// Can this file be played as it is? Answering "no" makes the importer conform it to
    /// MP4 first. With Media Foundation's zero-copy decoder in use (Windows, DX12), files
    /// it can't take are conformed so they play on it too; otherwise ffmpeg decodes
    /// anything as it is.
    fn decodable(choice: &oa_media::DecoderChoice, path: &Path, probe: &MediaProbe) -> bool {
        #[cfg(windows)]
        if choice.hardware()
            && let (Some(bridge), Some(video)) = (choice.bridge.clone(), probe.video.as_ref())
        {
            return match oa_media::windows::MfDecoder::open(bridge, path, video) {
                Ok(mut decoder) => oa_media::VideoDecoder::next(&mut decoder).is_ok_and(|f| f.is_some()),
                Err(_) => false,
            };
        }
        let _ = (choice, path, probe);
        true
    }

    /// Probes (and if needed converts) one file. Slow — seconds for a conversion — so it
    /// runs on a worker thread (see [`App::open_paths`]).
    fn import_with(choice: oa_media::DecoderChoice, path: &Path) -> Result<Imported, String> {
        oa_media::import(path, |p, probe| Self::decodable(&choice, p, probe)).map_err(|e| e.to_string())
    }

    fn bridge(&self) -> oa_media::DecoderChoice {
        self.decoders.clone()
    }

    fn import_file(&mut self, path: &Path) -> Result<Imported, String> {
        Self::import_with(self.bridge(), path)
    }

    /// Opens a project file, or imports media files and appends them to the timeline.
    /// Both happen on worker threads: the editor stays responsive, and the media bin
    /// shows a loading row per file until it's in.
    pub(crate) fn open_paths(&mut self, paths: &[PathBuf]) {
        let is_project = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if let Some(project) = paths.iter().find(|p| is_project(p)) {
            self.open_project(project);
            return;
        }
        self.import_paths(paths, true, "");
    }

    /// Imports media files; with `to_timeline` they're also added to the end of the
    /// timeline, otherwise they only go into the bin's `folder`.
    pub(crate) fn import_paths(&mut self, paths: &[PathBuf], to_timeline: bool, folder: &str) {
        for path in paths {
            let (tx, rx) = std::sync::mpsc::channel();
            let (bridge, file) = (self.bridge(), path.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Self::import_with(bridge, &file));
            });
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string());
            self.imports.push_back(PendingImport { path: path.clone(), name, rx, to_timeline, folder: folder.to_string() });
        }
    }

    /// Takes in finished imports, in the order they were started (so clips land on the
    /// timeline in that order).
    fn poll_imports(&mut self) {
        let mut added = false;
        while let Some(front) = self.imports.front() {
            let result = match front.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the import stopped unexpectedly".into()),
            };
            let job = self.imports.pop_front().expect("front exists");
            match result {
                Ok(imported) => {
                    let duration = if imported.kind == MediaKind::Still { self.still_length() } else { imported.default_duration() };
                    let conformed = imported.conformed.clone();
                    let name = imported.name();
                    match self.editor.add_media(imported) {
                        Ok(media) => {
                            added = true;
                            if let Some(reason) = conformed {
                                self.notify(format!("{name}: converted for playback ({reason})"));
                            }
                            if !job.folder.is_empty() {
                                let op = oa_doc::Op::SetMediaFolder { media, folder: job.folder.clone() };
                                if let Err(e) = self.editor.apply("Move to folder", vec![op]) {
                                    self.error = Some(e.to_string());
                                }
                            }
                            if job.to_timeline {
                                match self.editor.append_clip(media, duration) {
                                    Ok(item) => self.selection = Some(item),
                                    Err(e) => self.error = Some(e.to_string()),
                                }
                            }
                        }
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                Err(e) => self.error = Some(format!("{}: {e}", job.path.display())),
            }
        }
        if added {
            self.add_new_sources();
        }
        if let Some((path, rx)) = &self.opening {
            let result = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(format!("{}: opening stopped unexpectedly", path.display()))),
            };
            if let Some(result) = result {
                let path = path.clone();
                self.opening = None;
                // An autosave being recovered: only now that it's loaded is it tied back
                // to its project (or not) and deleted.
                let recovered = self.recovering.take_if(|(r, _)| r.file == path);
                let ok = result.is_ok();
                self.finish_open(result, recovered.as_ref());
                if let Some((r, _)) = recovered.filter(|_| ok) {
                    if let Err(e) = r.discard() {
                        self.report_error(format!("{}: {e}", r.file.display()));
                    }
                    self.autosave.soon();
                    self.notify(format!("recovered unsaved work from {}", r.name()));
                }
            }
        }
    }

    pub(crate) fn open_project(&mut self, path: &Path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (bridge, file) = (self.bridge(), path.to_path_buf());
        std::thread::spawn(move || {
            let _ = tx.send(Editor::open(&file, |p| Self::import_with(bridge.clone(), p)));
        });
        self.opening = Some((path.to_path_buf(), rx));
    }

    /// Opens an autosave to carry on with it. `keep_original`: it stays that project's
    /// unsaved changes (saving writes the project's file); otherwise it becomes a new,
    /// untitled project and the saved one is left as it was.
    pub(crate) fn recover(&mut self, r: autosave::Recovery, keep_original: bool) {
        self.recoveries.retain(|x| x.file != r.file);
        self.open_project(&r.file);
        self.recovering = Some((r, keep_original));
        self.screen = home::Screen::Editor;
    }

    /// Throws an autosave away.
    pub(crate) fn discard_recovery(&mut self, r: &autosave::Recovery) {
        match r.discard() {
            Ok(()) => self.recoveries.retain(|x| x.file != r.file),
            Err(e) => self.report_error(format!("{}: {e}", r.file.display())),
        }
    }

    fn finish_open(&mut self, result: Opened, recovered: Option<&(autosave::Recovery, bool)>) {
        match result {
            Ok((mut editor, warnings)) => {
                if let Some((r, keep)) = recovered {
                    editor.mark_recovered(r.original.clone().filter(|_| *keep));
                }
                if let Some(path) = editor.path.clone() {
                    let name = editor.doc.project().name.clone();
                    let thumb = thumbnail::path_for(&path);
                    self.settings.remember_project(&path, &name, thumb.exists().then_some(thumb));
                }
                // A save of the old project still going belongs to the old project.
                self.finish_background_save();
                self.editor = editor;
                self.compound_trail.clear();
                self.messages.extend(warnings);
                self.playhead = Time::ZERO;
                self.variant = 0;
                self.restore_view();
                self.selection = None;
                self.playing = false;
                self.reset_audio();
                self.renderer.clear_cache();
                self.rebuild_sources();
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Rebuilds the frame sources from the pool (after import, open, or relink).
    fn rebuild_sources(&mut self) {
        self.sources.forget_all();
        self.renderer.clear_cache();
        self.sources.set_video(None);
        self.add_new_sources();
    }

    /// Registers pool media the frame sources don't know yet, leaving running decoders
    /// alone (an import doesn't restart playback of what's already there). Still images
    /// decode on worker threads.
    fn add_new_sources(&mut self) {
        {
            let mut video = Some(self.sources.take_video().unwrap_or_else(|| oa_media::frame_source(&self.gpu, self.decoders.clone())));
            let items: Vec<(u64, MediaKind, PathBuf, Option<oa_media::VideoTrack>, String)> = self
                .editor
                .pool
                .iter()
                .filter(|item| !item.missing && !self.sources.knows(item.id.0))
                .map(|item| (item.id.0, item.kind, item.decode_path.clone(), item.probe.video.clone(), item.name.clone()))
                .collect();
            for (id, kind, path, track, name) in items {
                self.sources.register(id, kind);
                let Some(track) = track else { continue };
                match kind {
                    MediaKind::Still => {
                        let _ = &name;
                        self.sources.stills_mut().add_in_background(id, &path, &track);
                    }
                    MediaKind::Video => {
                        if let Some(video) = video.as_mut() {
                            video.add(id, &path, track);
                        }
                    }
                    MediaKind::Audio => {}
                }
            }
            self.sources.set_video(video);
        }
        // Pool changes (relinks, imports) can change what's audible without a new clip.
        self.mixed = None;
    }

    /// The audible clips on the timeline, in order.
    pub(crate) fn audio_clips_of(&self, seq: oa_doc::SeqId) -> Vec<AudioClip> {
        self.audio_mix_of(seq).0
    }

    /// The timeline's sound for the mixer: its clips, and its audio effect tracks' buses.
    pub(crate) fn audio_mix_of(&self, seq: oa_doc::SeqId) -> (Vec<AudioClip>, Vec<oa_audio::AudioBus>) {
        let (mut clips, mut buses, _speed_changed) = oa_export::audio_mix(self.editor.doc.project(), seq, |media| {
            let pool = self.editor.pool_item(media)?;
            (!pool.missing && pool.probe.has_audio()).then(|| pool.decode_path.clone())
        });
        // Sound effects belong to plugins (Atelier Core's included): a turned-off
        // plugin's effects are skipped, as its picture effects are.
        for c in &mut clips {
            c.effects.retain(|e| self.registry.effect(&e.type_id).is_some());
        }
        for b in &mut buses {
            b.effects.retain(|e| self.registry.effect(&e.type_id).is_some());
        }
        (clips, buses)
    }

    fn reset_audio(&mut self) {
        self.audio = None;
        self.mix = MixHandle::default();
        self.mixed = None;
    }

    /// Keeps sound in step with the document: publishes the current clips to the mixer
    /// (cheap, and a no-op when nothing audible changed) and opens the output device the
    /// first time there's something to hear.
    fn sync_audio(&mut self) {
        let snapshot = self.editor.doc.snapshot();
        if self.mixed.as_ref().is_some_and(|m| Arc::ptr_eq(m, &snapshot)) {
            return;
        }
        self.mixed = Some(snapshot);
        let (clips, buses) = self.audio_mix_of(self.editor.seq);
        let state = MixState { clips, buses, end: self.editor.duration() };
        let has_sound = !state.clips.is_empty();
        self.mix.set(state);
        if self.audio.is_none() && has_sound {
            let (mix, start) = (self.mix.clone(), self.playhead);
            match AudioEngine::new(move |format| Box::new(TimelineAudio::with_handle(mix, format, start)), start) {
                Ok(engine) => {
                    engine.set_gain(self.volume);
                    engine.set_playing(self.playing);
                    self.audio = Some(engine);
                }
                Err(e) => {
                    self.messages.push(format!("sound unavailable: {e}"));
                    // Don't retry every frame; the next edit tries again.
                }
            }
        }
    }

    /// The properties: the inspector, then performance and messages, scrolling.
    fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        let compact = self.settings.compact_properties;
        egui::ScrollArea::vertical().show(ui, |ui| {
                inspector::set_compact(ui, compact);
                if !compact {
                    ui.heading("Clip");
                }
                // A property changed here changes on every selected clip.
                self.editor.linked = if self.selected.len() > 1 { self.selected_clips() } else { Vec::new() };
                self.inspector(ui);
                self.editor.linked.clear();
                if self.settings.show_performance {
                    ui.separator();
                    egui::CollapsingHeader::new("Performance").default_open(false).show(ui, |ui| self.stats_panel(ui));
                }
                if !self.messages.is_empty() {
                    egui::CollapsingHeader::new("Messages").default_open(true).show(ui, |ui| {
                        for m in self.messages.iter().rev().take(8) {
                            ui.label(egui::RichText::new(m).small().weak());
                        }
                    });
                }
            });
    }

    /// The viewer, rendering a fresh frame first when the one on screen is stale.
    fn viewer_area(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        self.refresh_follower(ui.ctx());
        let stale = self.preview.is_none() || self.rendered.as_ref() != Some(&self.frame_key());
        if let Some(render_state) = frame.wgpu_render_state().filter(|_| stale && !self.gpu_lost) {
            self.render_frame(render_state);
        }
        self.viewer(ui);
    }

    /// Opens the Export window (type, format, resolution, quality…).
    pub(crate) fn start_export(&mut self) {
        // The project's own timeline, even while a compound clip is open.
        let (seq, _) = self.main_timeline();
        if self.editor.doc.project().sequence(seq).is_none_or(|s| s.duration() <= Time::ZERO) {
            self.error = Some("nothing to export: the timeline is empty".into());
            return;
        }
        self.export_dialog = true;
    }

    /// Starts the next queued export, if nothing is running: on a thread of its own
    /// (`export_worker`), from a snapshot of the project as it is now.
    pub(crate) fn next_export(&mut self) {
        if self.export.is_some() {
            return;
        }
        let Some(job) = self.export_queue.pop_front() else { return };
        let media = self
            .editor
            .pool
            .iter()
            .filter(|m| !m.missing)
            .map(|m| export_worker::MediaEntry { id: m.id.0, kind: m.kind, path: m.decode_path.clone(), track: m.probe.video.clone() })
            .collect();
        self.messages.push(format!("exporting to {}", job.path.display()));
        self.export = Some(export_worker::start(export_worker::Job {
            gpu: self.gpu.clone(),
            decoders: self.decoders.clone(),
            registry: self.registry.clone(),
            project: self.editor.doc.snapshot(),
            media,
            seq: job.seq,
            path: job.path.clone(),
            options: job.options,
            vram_budget: self.settings.vram_budget(),
        }));
        self.export_path = Some(job.path);
        self.export_view.began = Some(Instant::now());
    }

    /// Checks on the running export: its picture, and whether it's finished.
    fn step_export(&mut self) {
        self.next_export();
        let Some(run) = self.export.as_ref() else { return };
        let export_worker::Poll::Finished(result) = run.poll() else { return };
        self.export = None;
        let name = self.export_path.take().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_default();
        match result {
            Ok(summary) => {
                self.messages.push(format!(
                    "exported {} frames at {}x{} in {:.1}s ({:.0} fps) with {} — {}",
                    summary.frames,
                    summary.size[0],
                    summary.size[1],
                    summary.seconds_elapsed,
                    summary.frames_per_second(),
                    summary.encoder,
                    summary.timings
                ));
                self.notify(format!("Exported {name} ({}×{}, {:.0} fps).", summary.size[0], summary.size[1], summary.frames_per_second()));
            }
            Err(e) => self.report_error(format!("{name}: {e}")),
        }
        self.next_export();
    }

    /// Stops the running export and everything queued after it.
    pub(crate) fn cancel_exports(&mut self) {
        self.export = None;
        self.export_path = None;
        self.export_queue.clear();
    }

    pub(crate) fn save(&mut self, save_as: bool) {
        // Never two writers on one file: let a background save finish first.
        self.finish_background_save();
        let path = match (&self.editor.path, save_as) {
            (Some(path), false) => Some(path.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("OpenAtelier project", &["json"])
                .set_file_name("project.oaproj.json")
                .save_file(),
        };
        let Some(path) = path else { return };
        match self.editor.save(&path) {
            Ok(()) => {
                self.notify(format!("saved {}", path.display()));
                // An untitled project takes its name from the file it was saved as.
                let name = self.editor.doc.project().name.clone();
                let name = if name.is_empty() || name == "Untitled" { thumbnail::name_from_path(&path) } else { name };
                self.editor.set_project_name(&name);
                let thumb = self.write_thumbnail(&path);
                self.settings.remember_project(&path, &name, thumb);
                self.autosave.clear();
            }
            Err(e) => self.error = Some(e),
        }
    }

    // ---- playback ---------------------------------------------------------

    pub(crate) fn set_playhead(&mut self, t: Time) {
        let t = t.max(Time::ZERO).min(self.editor.duration());
        if t != self.playhead {
            self.playhead = t;
            // A jump while playing: show stand-in frames until the decoder has caught up,
            // rather than stalling on the seek.
            if self.playing {
                self.seek_settling = true;
            }
            // While scrubbing, don't restart the audio decoders on every mouse move; the
            // sound catches up when the drag ends (`end_scrub`).
            if !self.scrubbing
                && let Some(audio) = &self.audio
            {
                audio.seek(t);
            }
        }
    }

    pub(crate) fn begin_scrub(&mut self) {
        self.scrubbing = true;
    }

    pub(crate) fn end_scrub(&mut self) {
        if std::mem::take(&mut self.scrubbing)
            && let Some(audio) = &self.audio
        {
            audio.seek(self.playhead);
        }
    }

    pub(crate) fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        if let Some(audio) = &self.audio {
            audio.set_playing(playing);
        }
    }

    fn step(&mut self, frames: i64) {
        let rate = self.editor.sequence().rate;
        let index = rate.frame_at(self.playhead) + frames;
        self.set_playhead(rate.frame_start(index.max(0)));
    }

    /// While playing, the **audio clock** decides where we are — video follows it. With
    /// no sound, the wall clock stands in.
    fn advance_playback(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        if !self.playing {
            return;
        }
        let end = self.editor.duration();
        match &self.audio {
            Some(audio) => {
                let position = audio.position();
                if position >= end || audio.ended() {
                    self.playhead = Time::ZERO;
                    audio.seek(Time::ZERO);
                } else {
                    self.playhead = position.max(Time::ZERO);
                }
            }
            None => {
                self.playhead += Time::from_seconds_f64(dt);
                if self.playhead >= end {
                    self.playhead = Time::ZERO;
                }
            }
        }
    }

    // ---- rendering --------------------------------------------------------

    /// The preview's render scale. Auto renders only as many pixels as the viewer shows
    /// (rounded up to full, ½ or ¼, so zooming doesn't re-render at every step): a 1080p
    /// frame shown at 40% is rendered at ½, a quarter of the pixels.
    fn preview_scale(&self) -> f64 {
        let asked = if self.scale > 0.0 {
            self.scale as f64
        } else {
            match self.display_scale {
                s if s > 0.5 => 1.0,
                s if s > 0.25 => 0.5,
                _ => 0.25,
            }
        };
        // Short of GPU memory, render fewer pixels rather than failing to render at all.
        (asked * self.memory_backoff).max(0.125)
    }

    /// Keeps the project file itself up to date while you work (a saved project, a
    /// pause in the editing, and only when something actually changed). Losing work to
    /// a crash should cost seconds, not an evening — the recovery autosave covers the
    /// rest, and a project that was never saved has nowhere to go but there.
    ///
    /// It saves on a thread of its own: writing an hour-long edit takes ~0.2 s, which
    /// was a hitch every 20 s. The snapshot it writes can't change, so editing carries
    /// on; once it's on disk, that snapshot is what counts as saved (anything edited
    /// since still shows as unsaved).
    fn save_as_you_go(&mut self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(20);
        if self.saving.as_ref().is_some_and(|(_, _, h)| h.is_finished()) {
            self.finish_background_save();
        }
        if self.saving.is_some() || !self.settings.save_as_you_go || !self.editor.dirty() || self.last_saved.elapsed() < EVERY {
            return;
        }
        // Not in the middle of something: a drag, an export, a frame being scrubbed.
        if self.timeline_drag.is_some() || self.viewer_drag.is_some() || self.export.is_some() || self.scrubbing {
            return;
        }
        let Some(path) = self.editor.path.clone() else { return };
        self.last_saved = Instant::now();
        let (snapshot, target) = (self.editor.doc.snapshot(), path.clone());
        let for_thread = snapshot.clone();
        let spawned = std::thread::Builder::new().name("oa-save".into()).spawn(move || {
            let json = oa_doc::ProjectFile::to_json(&for_thread).map_err(|e| e.to_string())?;
            autosave::write_atomic(&target, json.as_bytes()).map_err(|e| format!("{}: {e}", target.display()))
        });
        match spawned {
            Ok(handle) => self.saving = Some((path, snapshot, handle)),
            Err(e) => self.report_error(format!("couldn't start saving: {e}")),
        }
    }

    /// Waits for a background save (if one is running) and takes in how it went.
    pub(crate) fn finish_background_save(&mut self) {
        let Some((path, snapshot, handle)) = self.saving.take() else { return };
        match handle.join().unwrap_or_else(|_| Err("saving stopped unexpectedly".into())) {
            Ok(()) => {
                self.editor.mark_saved(&path, snapshot);
                // Written up to that snapshot: the recovery copy is only needed for edits
                // made since (the next autosave covers them).
                if !self.editor.dirty() {
                    self.autosave.clear();
                }
                let name = self.editor.doc.project().name.clone();
                let thumb = thumbnail::path_for(&path);
                self.settings.remember_project(&path, &name, thumb.exists().then_some(thumb));
            }
            // Don't nag every 20 seconds if the disk is unhappy; say it once.
            Err(e) => {
                self.settings.set_save_as_you_go(false);
                self.report_error(format!("{e} — saving as you go is off for now; the recovery autosave is still running."));
            }
        }
    }

    /// The window was asked to close with unsaved work: hold it open and ask.
    fn confirm_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.editor.dirty() && !self.closing {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if !self.closing {
            return;
        }
        let name = self.editor.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string());
        let mut choice: Option<&str> = None;
        egui::Modal::new(egui::Id::new("closing")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Save before closing?");
            ui.add_space(style::GAP_S);
            ui.label(match &name {
                Some(file) => format!("{file} has changes that aren't saved."),
                None => "This project has never been saved.".to_string(),
            });
            ui.label(egui::RichText::new("There's an autosave either way — this is the real file.").small().weak());
            ui.add_space(style::GAP_L);
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    choice = Some("save");
                }
                if ui.button("Close without saving").clicked() {
                    choice = Some("discard");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Keep editing").clicked() {
                        choice = Some("cancel");
                    }
                });
            });
        });
        match choice {
            Some("save") => {
                self.save(false);
                // A save that was cancelled (no file chosen) leaves the work unsaved.
                if !self.editor.dirty() {
                    self.closing = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some("discard") => {
                self.closing = false;
                self.autosave.clear();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some("cancel") => self.closing = false,
            _ => {}
        }
    }

    /// Reads the GPU's own report each frame: gives back memory by rendering smaller
    /// while the budget is full, and notices if the device goes away.
    fn check_gpu(&mut self) {
        if self.gpu_lost {
            return;
        }
        if self.gpu.health.is_lost() {
            self.gpu_lost = true;
            // The user's work first: get it on disk before saying anything.
            let _ = self.autosave.write_now(&self.editor.doc.snapshot(), self.editor.path.as_deref());
            let why = self.gpu.health.message().unwrap_or_else(|| "the graphics device was lost".into());
            self.report_error(format!("{why}. Your work has been autosaved — restart OpenAtelier to carry on."));
            return;
        }
        let memory = self.renderer.memory();
        let was = self.memory_backoff;
        self.memory_backoff = match memory.pressure {
            oa_gpu::Pressure::Over => (self.memory_backoff * 0.8).max(0.25),
            // Only climb back when there is real room, so it can't oscillate.
            oa_gpu::Pressure::Easy if memory.fraction() < 0.5 => (self.memory_backoff * 1.05).min(1.0),
            _ => self.memory_backoff,
        };
        if was >= 1.0 && self.memory_backoff < 1.0 {
            self.notify(format!(
                "Short of GPU memory ({} of {}): rendering the preview smaller.",
                bytes(memory.used()),
                bytes(memory.budget)
            ));
        }
    }

    fn variant_id(&self) -> VariantId {
        let seq = self.editor.sequence();
        seq.variants[self.variant.min(seq.variants.len() - 1)].id
    }

    fn frame_key(&self) -> FrameKey {
        FrameKey {
            playhead: self.playhead,
            variant: self.variant,
            scale: (self.preview_scale() as f32).to_bits(),
            reference: self.reference,
            document: Arc::as_ptr(&self.editor.doc.snapshot()) as usize,
            follow: self.follower_key.1,
        }
    }

    fn render_frame(&mut self, render_state: &eframe::egui_wgpu::RenderState) {
        let started = Instant::now();
        let opts = PlanOptions { variant: Some(self.variant_id()), render_scale: self.preview_scale(), ..Default::default() };
        let level = if self.reference { OptLevel::Reference } else { OptLevel::Full };
        let plan = || plan_frame(self.editor.doc.project(), self.editor.seq, self.playhead, &opts, &self.registry);
        let planned = match self.follower.as_ref().map_or_else(plan, |f| oa_params::signal::with(f.at(self.playhead), plan)) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.report = planned.report;
        let graph = optimize(&planned.graph, level, KeyContext::default());
        // Paused (scrubbing, stepping, editing): never wait on the decoder — show the
        // nearest frame on hand and fill in the exact one as soon as it's decoded.
        // Playing: exact frames, which the decode-ahead has ready anyway.
        self.sources.set_interactive(!self.playing || self.seek_settling);
        let image = match self.renderer.render(&graph, &self.registry, &mut self.sources) {
            Ok(img) => img,
            // A shader or glyphs are still being prepared in the background: keep the
            // last frame on screen and try again next frame.
            Err(oa_gpu::RenderError::NotReady) => {
                self.sources.set_interactive(false);
                self.rendered = None;
                return;
            }
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.sources.set_interactive(false);
        let settled = self.sources.settled();
        if settled {
            self.seek_settling = false;
        }
        self.stats = self.renderer.stats.clone();
        // Draw into the texture already on screen when the size hasn't changed: no
        // allocation and no re-registration with egui per frame.
        let old = self.preview.take();
        let reuse = old.as_ref().map(|p| p.texture.clone());
        let texture = match readback::display_texture_into(&self.gpu, self.renderer.pipelines(), &image, reuse) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let id = match old {
            Some(old) if old.texture == texture => old.id,
            Some(old) => {
                let view = readback::display_view(&texture);
                render_state.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, old.id);
                old.id
            }
            None => {
                let view = readback::display_view(&texture);
                render_state.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear)
            }
        };
        self.preview = Some(Preview { texture, id, size: image.size, _image: image });
        // Stand-ins were shown: render again next frame, when the exact frames will be in.
        self.rendered = if settled { Some(self.frame_key()) } else { None };
        self.error = None;
        self.frame_ms.push_back(started.elapsed().as_secs_f32() * 1000.0);
        if self.frame_ms.len() > 120 {
            self.frame_ms.pop_front();
        }
    }

    // ---- panels -----------------------------------------------------------

    pub(crate) fn relink(&mut self, media: oa_doc::MediaId, file: &Path) {
        match self.import_file(file) {
            Ok(imported) => {
                let path = imported.path.to_string_lossy().to_string();
                let Some(old) = self.editor.doc.project().media(media).cloned() else { return };
                let updated = oa_doc::MediaRef { path, fingerprint: Some(imported.fingerprint.clone()), ..old };
                let ops = vec![oa_doc::Op::RemoveMedia(media), oa_doc::Op::AddMedia(Arc::new(updated))];
                if let Err(e) = self.editor.doc.edit("Relink media", ops) {
                    self.error = Some(e.to_string());
                    return;
                }
                let name = imported.name();
                if let Some(item) = self.editor.pool.iter_mut().find(|m| m.id == media) {
                    item.missing = false;
                    item.decode_path = imported.decode_path;
                    item.probe = imported.probe;
                    item.name = name;
                }
                self.clip_previews.forget(media);
                self.rebuild_sources();
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl App {
    /// Keyboard shortcuts. Skipped while a text field has focus.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers as M};
        // Only a text field keeps the keyboard. The arrow keys in a one-line field (or
        // Tab) would otherwise walk focus over to a nearby button — the caret vanishes,
        // and Space then presses that button (the Captions one reopened its window)
        // while the shortcuts stay switched off.
        let arrows = [Key::ArrowUp, Key::ArrowDown, Key::ArrowLeft, Key::ArrowRight];
        if ctx.input(|i| arrows.iter().any(|k| i.key_pressed(*k))) {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let key_down = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })));
        if key_down
            && let Some(id) = ctx.memory(|m| m.focused())
            && !ctx.text_edit_focused()
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |m: M, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        // With the Captions window open, Ctrl+Z is its own (it takes the keys itself).
        if self.captions.is_none() {
            if pressed(M::COMMAND | M::SHIFT, Key::Z) || pressed(M::COMMAND, Key::Y) {
                self.redo();
            }
            if pressed(M::COMMAND, Key::Z) {
                self.undo();
            }
        }
        // Nudge the selected layer: Ctrl+arrows (Shift for 10 px).
        let arrows = [(Key::ArrowLeft, [-1.0, 0.0]), (Key::ArrowRight, [1.0, 0.0]), (Key::ArrowUp, [0.0, -1.0]), (Key::ArrowDown, [0.0, 1.0])];
        for (key, d) in arrows {
            for (mods, step) in [(M::COMMAND | M::SHIFT, 10.0), (M::COMMAND, 1.0)] {
                if pressed(mods, key)
                    && let Some(item) = self.selection
                {
                    let ops = oa_edit::transform::nudge(
                        self.editor.doc.project(),
                        self.editor.seq,
                        self.variant_id(),
                        item,
                        self.playhead,
                        [d[0] * step, d[1] * step],
                        self.transform_scope(),
                    );
                    if let Ok(ops) = ops
                        && let Err(e) = self.editor.apply_drag("Nudge layer", "nudge", ops)
                    {
                        self.error = Some(e.to_string());
                    }
                }
            }
        }
        if pressed(M::NONE, Key::Space) {
            let playing = !self.playing;
            self.set_playing(playing);
        }
        // With the Captions window open, Left and Right go from caption to caption there.
        if self.captions.is_none() {
            if pressed(M::NONE, Key::ArrowRight) {
                self.step(1);
            }
            if pressed(M::NONE, Key::ArrowLeft) {
                self.step(-1);
            }
            if pressed(M::SHIFT, Key::ArrowRight) {
                self.step(10);
            }
            if pressed(M::SHIFT, Key::ArrowLeft) {
                self.step(-10);
            }
        }
        if pressed(M::NONE, Key::Home) {
            self.set_playhead(Time::ZERO);
        }
        if pressed(M::NONE, Key::End) {
            let end = self.editor.duration();
            self.set_playhead(end);
        }
        if pressed(M::NONE, Key::S) || pressed(M::COMMAND, Key::K) {
            self.split_at_playhead();
        }
        if pressed(M::COMMAND, Key::T) {
            self.add_text();
        }
        if pressed(M::NONE, Key::T) {
            self.add_transition_at_playhead();
        }
        // Over the curve editor, Delete removes its key, not the clip.
        if !self.pointer_on_curve_editor(ctx) {
            let ripple = pressed(M::SHIFT, Key::Delete) || pressed(M::SHIFT, Key::Backspace);
            if ripple || pressed(M::NONE, Key::Delete) || pressed(M::NONE, Key::Backspace) {
                self.delete_selection(ripple);
            }
        }
        // Escape clears the selection; with nothing selected, it steps out of a compound.
        if self.crop_mode.is_some() && pressed(M::NONE, Key::Escape) {
            // Finish cropping; the clip stays selected.
            self.end_crop();
        } else if pressed(M::NONE, Key::Escape) {
            if self.selected_track.take().is_some() {
                // First Escape lets go of the track.
            } else if self.selected.is_empty() && self.selection.is_none() {
                self.close_compound(1);
            }
            self.selection = None;
            self.selected.clear();
        }
        if pressed(M::COMMAND, Key::Comma) {
            self.settings_open = !self.settings_open;
        }
        if pressed(M::COMMAND, Key::A) {
            self.select_all();
        }
        if pressed(M::COMMAND, Key::D) {
            match self.selected_track {
                Some(track) => self.duplicate_into_track(track),
                None => self.duplicate_selection(),
            }
        }
        if let Some(track) = self.selected_track {
            if pressed(M::ALT, Key::ArrowUp) {
                self.move_track(track, true);
            }
            if pressed(M::ALT, Key::ArrowDown) {
                self.move_track(track, false);
            }
        }
        if pressed(M::COMMAND | M::SHIFT, Key::G) {
            self.ungroup_selection();
        }
        if pressed(M::COMMAND, Key::G) {
            self.group_selection();
        }
        // egui turns Ctrl+C / X / V into these events rather than key presses.
        let (copy, cut, paste) = ctx.input(|i| {
            let has = |f: &dyn Fn(&egui::Event) -> bool| i.events.iter().any(f);
            (has(&|e| matches!(e, egui::Event::Copy)), has(&|e| matches!(e, egui::Event::Cut)), has(&|e| matches!(e, egui::Event::Paste(_))))
        });
        if copy {
            self.copy_selection();
        }
        if cut {
            self.cut_selection();
        }
        if paste {
            match self.selected_track {
                Some(track) => self.paste_into_track(track),
                None => {
                    let at = self.playhead;
                    self.paste_at(at);
                }
            }
        }
    }

    /// The razor: cuts the selected clip if it's under the playhead, otherwise every
    /// clip there.
    /// A new title at the playhead, above everything, selected so the inspector shows
    /// its text for editing.
    pub(crate) fn add_text(&mut self) {
        match self.editor.add_text(self.playhead, "Title", self.still_length()) {
            Ok(id) => {
                self.selection = Some(id);
                self.focus_text = true;
            }
            Err(e) => self.error = Some(format!("can't add text: {e}")),
        }
    }

    pub(crate) fn split_at_playhead(&mut self) {
        let t = self.playhead;
        let target = self.selection.filter(|id| self.editor.item(*id).is_some_and(|i| i.range.contains(t)));
        match self.editor.split(target, t) {
            Ok(Some(back)) => self.selection = Some(back),
            Ok(None) => {}
            Err(e) => self.error = Some(format!("can't split here: {e}")),
        }
    }

    /// T: a 1 s cross dissolve on the cut nearest the playhead (on the selected clip's
    /// track, or any video track); with no cut nearby, a fade on whichever end of the
    /// selected clip is nearer.
    fn add_transition_at_playhead(&mut self) {
        let t = self.playhead;
        let s = self.editor.sequence();
        let tolerance = Time::from_seconds_f64(0.5);
        let selected_track = self.selection.and_then(|id| s.find_item(id)).map(|(ti, _)| s.tracks[ti].id);
        let cut = s
            .tracks
            .iter()
            .filter(|tr| tr.kind == oa_doc::TrackKind::Video && selected_track.is_none_or(|id| id == tr.id))
            .find_map(|tr| oa_edit::timeline::nearest_cut(tr, t, tolerance));
        let target = match (cut, self.selection.and_then(|id| self.editor.item(id))) {
            (Some(item), _) => Some((item, oa_doc::ClipEnd::Head)),
            (None, Some(it)) => {
                let end = if (t - it.range.start).0.abs() <= (it.range.end() - t).0.abs() { oa_doc::ClipEnd::Head } else { oa_doc::ClipEnd::Tail };
                Some((it.id, end))
            }
            (None, None) => None,
        };
        let Some((item, end)) = target else {
            self.error = Some("no cut near the playhead; select a clip to fade it".into());
            return;
        };
        let ops = oa_edit::timeline::set_transition(
            self.editor.doc.project(),
            self.editor.seq,
            item,
            end,
            "oa.transition.crossfade",
            Time::from_seconds(1),
        );
        match ops.and_then(|ops| self.editor.apply("Add transition", ops)) {
            Ok(()) => self.selection = Some(item),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub(crate) fn undo(&mut self) {
        let captions = self.editor.doc.undo_label() == Some(captions::ADD_CAPTIONS);
        match self.editor.doc.undo() {
            Err(oa_doc::EditError::LastSequence) => self.notify("Nothing more to undo."),
            Err(e) => self.error = Some(e.to_string()),
            Ok(true) if captions => self.reopen_captions(),
            Ok(_) => {}
        }
    }

    pub(crate) fn redo(&mut self) {
        let captions = self.editor.doc.redo_label() == Some(captions::ADD_CAPTIONS);
        match self.editor.doc.redo() {
            Err(e) => self.error = Some(e.to_string()),
            Ok(true) if captions => self.put_captions_away(),
            Ok(_) => {}
        }
    }

    /// What a `print` step in a script logs.
    fn print_state(&self) {
        let t = self.playhead;
        eprintln!("[script] t={:.3}s playing={} selection={:?} undo={:?}", t.as_seconds_f64(), self.playing, self.selection, self.editor.doc.undo_label());
        if let Some(item) = self.selection
            && let Ok(p) = oa_edit::transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), item, t)
        {
            use oa_doc::schema;
            let v = &p.values;
            eprintln!(
                "[script]   position={:?} scale={:?} rotation={:.2} anchor={:?} corners={:?}",
                v.vec2(schema::POSITION),
                v.vec2(schema::SCALE),
                v.float(schema::ROTATION),
                v.vec2(schema::ANCHOR),
                p.corners().map(|c| [c[0].round(), c[1].round()])
            );
        }
        if let Some(item) = self.selection.and_then(|i| self.editor.item(i)) {
            eprintln!("[script]   range={:?}..{:?} source_in={:?}", item.range.start, item.range.end(), item.time_map.source_in);
            for (end, tr) in [("in", &item.transition_in), ("out", &item.transition_out)] {
                if let Some(tr) = tr {
                    eprintln!("[script]   transition {end}: {} {:?}", tr.type_id, tr.duration);
                }
            }
        }
        if let Some(e) = &self.error {
            eprintln!("[script]   error: {e}");
        }
        let v = &self.timeline_view;
        let tracks: Vec<String> = self.editor.sequence().tracks.iter().map(|t| format!("{}{}", t.name, if t.enabled { "" } else { "(off)" })).collect();
        eprintln!("[script]   timeline: start {:.2}s span {:.2}s fit {} tracks {tracks:?}", v.start, v.span, v.fit);
        if let Some(status) = self.sources.status() {
            eprintln!("[script]   sources: {status}");
        }
        if let Some(audio) = &self.audio {
            eprintln!("[script]   audio: {:.0} ms buffered, {} underruns", audio.buffered() * 1000.0, audio.underruns());
        }
    }

    fn stats_panel(&mut self, ui: &mut egui::Ui) {
        let avg: f32 = if self.frame_ms.is_empty() { 0.0 } else { self.frame_ms.iter().sum::<f32>() / self.frame_ms.len() as f32 };
        ui.label(format!("{avg:.1} ms per frame ({:.0} fps)", if avg > 0.0 { 1000.0 / avg } else { 0.0 }));
        if let Some(p) = &self.preview {
            ui.label(format!("{}x{} rendered", p.size[0], p.size[1]));
        }
        let s = &self.stats;
        ui.label(format!("{} passes, {} cache hits, {} fused", s.passes, s.cache_hits, s.fused_chains));
        ui.label(format!("pool {:.0} MB, cache {:.0} MB", s.pool_bytes as f64 / 1e6, s.cache_bytes as f64 / 1e6));
        let memory = self.renderer.memory();
        let note = match memory.pressure {
            oa_gpu::Pressure::Over => " — full, rendering smaller",
            oa_gpu::Pressure::Tight => " — trimming the cache",
            oa_gpu::Pressure::Easy => "",
        };
        ui.label(format!("GPU memory {} of {}{note}", bytes(memory.used()), bytes(memory.budget)));
        ui.add(egui::ProgressBar::new(memory.fraction().min(1.0)).desired_height(6.0));
        let mut budget_mb = memory.budget / (1 << 20);
        let r = ui.add(egui::Slider::new(&mut budget_mb, 256..=16384).text("budget (MB)").logarithmic(true));
        if r.changed() {
            self.settings.set_vram_budget(budget_mb);
            self.renderer.options.vram_budget = budget_mb << 20;
        }
        let mut as_you_go = self.settings.save_as_you_go;
        if ui
            .checkbox(&mut as_you_go, "save as you go")
            .on_hover_text("Keeps the project file up to date while you edit (every 20 s, when you pause)")
            .changed()
        {
            self.settings.set_save_as_you_go(as_you_go);
        }
        if self.memory_backoff < 1.0 {
            ui.label(egui::RichText::new(format!("preview at {:.0}% to fit", self.memory_backoff * 100.0)).small().weak());
        }
        if self.gpu.health.errors() > 0 {
            ui.colored_label(egui::Color32::YELLOW, format!("{} GPU errors reported", self.gpu.health.errors()));
        }
        if let Some(status) = self.sources.status() {
            ui.label(egui::RichText::new(status).small());
        }
        for u in &s.unsupported {
            ui.colored_label(egui::Color32::YELLOW, u);
        }
        // Effects clips ask for that no enabled plugin provides: say which plugin has
        // them, since the usual reason is that it's turned off.
        for type_id in &self.report.missing_effects.clone() {
            let note = match self.plugins.provider_of(type_id) {
                Some(p) if !self.plugins.is_enabled(&p.id) => format!("{type_id} — {} is turned off", p.name),
                Some(p) => format!("{type_id} — from {}, but it didn't load", p.name),
                None => format!("{type_id} — no plugin here provides it"),
            };
            ui.colored_label(egui::Color32::YELLOW, note);
        }
        let rest = PlanReport { missing_effects: Vec::new(), ..std::mem::take(&mut self.report) };
        if rest != PlanReport::default() {
            ui.colored_label(egui::Color32::YELLOW, format!("{rest:?}"));
        }
        self.report = rest;
        ui.separator();
        match &self.audio {
            Some(audio) => {
                ui.label(egui::RichText::new(audio.device_name()).small());
                ui.label(format!("{:.0} ms buffered, {} underruns", audio.buffered() * 1000.0, audio.underruns()));
                let drift = (self.playhead - audio.position()).as_seconds_f64() * 1000.0;
                ui.label(format!("clock: audio ({drift:+.0} ms)"));
            }
            None => {
                ui.label(egui::RichText::new("no sound (clock: wall time)").weak());
            }
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        // Restart now (after an update): the new version opens as this one closes.
        self.relaunch_after_update();
        // Leave an up-to-date autosave behind only if there's unsaved work.
        if self.editor.dirty() {
            let _ = self.autosave.write_now(&self.editor.doc.snapshot(), self.editor.path.as_deref());
        } else {
            self.autosave.clear();
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(script) = self.script.as_mut() else { return };
        let print = script.feed(ctx, raw_input);
        let finished = script.finished;
        if print {
            self.print_state();
        }
        if finished {
            eprintln!("[script] done");
            self.script = None;
        }
    }

    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.guarded_ui(root, frame);
    }
}

impl App {
    /// One frame of the whole UI.
    fn frame_ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        winfocus::keep(frame, ctx);
        self.toasts(ctx);
        self.confirm_close(ctx);
        self.check_gpu();
        self.advance_playback();
        if self.export.is_some() {
            self.step_export();
            let render_state = self.render_state.clone();
            self.update_export_view(render_state.as_ref());
            // The export runs on its own thread: the UI only needs to look in a few
            // times a second (drawing it every frame would take GPU time from the export).
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.open_paths(&dropped);
            self.screen = home::Screen::Editor;
        }
        // An effect drag the inspector didn't finish (its list went away) ends here.
        if self.effect_drag.is_some() && !ctx.input(|i| i.pointer.primary_down() || i.pointer.any_released()) {
            self.effect_drag = None;
        }
        self.shortcuts(ctx);
        self.audition_tick();
        self.compound_still_there();
        self.sync_selection();
        self.thumbs_frame_start();
        self.font_samples.frame_start();
        self.poll_imports();
        if !self.imports.is_empty() || self.opening.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Edits made anywhere last frame reach the speakers here.
        self.sync_audio();
        self.save_as_you_go();
        if let Some(Err(e)) = self.autosave.tick(&self.editor.doc.snapshot(), self.editor.dirty(), self.editor.path.as_deref()) {
            self.messages.push(format!("autosave failed: {e}"));
        }
        self.poll_updates(ctx);
        self.settings_window(ctx);
        if self.screen == home::Screen::Home {
            self.update_banner(root);
            self.deps_banner(root);
            egui::CentralPanel::default().show(root, |ui| self.home(ui));
            return;
        }
        self.curve_window(ctx);
        self.wave_window(ctx);
        self.connection_window(ctx);
        self.track_panel(ctx);
        self.export_window(ctx);
        self.captions_window(ctx);
        self.record_window(ctx);
        let rate = self.editor.sequence().rate;
        let duration = self.editor.duration().as_seconds_f64();

        // The window title says what's open and whether it's saved.
        let title = format!("{} — OpenAtelier", self.editor.title());
        if self.window_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
        egui::Panel::top("menu").show(root, |ui| self.menu_bar(ui));
        self.update_banner(root);
        self.deps_banner(root);

        if let Some(r) = self.recoveries.first().cloned() {
            egui::Panel::top("recovery").show(root, |ui| {
                ui.add_space(4.0);
                self.recovery_card(ui, &r);
                ui.add_space(4.0);
            });
        }

        // Layout: the inspector runs the full height on the right; the timeline spans the
        // rest of the width at the bottom; the media bin and the viewer share the top.
        // (egui gives earlier panels the full extent, so the order matters.)
        // The tall right-hand column holds the properties, or — in the vertical layout —
        // the viewer, and the middle holds the other.
        let vertical = self.vertical_layout();
        let epoch = self.view_epoch;
        let (right_width, right_range) = if vertical { (self.project_view.viewer, 220.0..=1600.0) } else { (self.project_view.inspector, 260.0..=560.0) };
        let right = egui::Panel::right(egui::Id::new(("right-column", epoch, vertical))).default_size(right_width).size_range(right_range).show(root, |ui| {
            if vertical {
                self.viewer_area(ui, frame);
            } else {
                self.inspector_panel(ui);
            }
        });
        let right_width = right.response.rect.width();

        let controls = egui::Panel::bottom(egui::Id::new(("controls", epoch))).resizable(true).default_size(self.project_view.timeline).size_range(150.0..=760.0).show(root, |ui| {
            ui.add_space(style::GAP_S);
            // Playback, centered under the viewer; then where we are in the sequence.
            ui.horizontal(|ui| {
                ui.add_space(style::GAP_S);
                if icons::button(ui, icons::SKIP_START, "Go to the start", "Home", true).clicked() {
                    self.set_playhead(Time::ZERO);
                }
                if icons::button(ui, icons::STEP_BACK, "Back a frame", "←", true).clicked() {
                    self.step(-1);
                }
                let (glyph, tip) = if self.playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
                if icons::button(ui, glyph, tip, "Space", true).clicked() {
                    let playing = !self.playing;
                    self.set_playing(playing);
                }
                if icons::button(ui, icons::STEP_ON, "On a frame", "→", true).clicked() {
                    self.step(1);
                }
                if icons::button(ui, icons::SKIP_END, "Go to the end", "End", true).clicked() {
                    let end = self.editor.duration();
                    self.set_playhead(end);
                }
                ui.label(
                    egui::RichText::new(format!("{:.2}s / {duration:.2}s", self.playhead.as_seconds_f64()))
                        .size(style::TEXT_L),
                );
                ui.label(egui::RichText::new(format!("frame {}", rate.frame_at(self.playhead))).small().weak());
                // How loud playback is here (not part of the project or the export).
                let mut volume = self.volume;
                let speaker = if volume <= 0.001 { "🔇" } else { "🔊" };
                if ui.small_button(speaker).on_hover_text("Mute playback (only here — the project and export are unchanged)").clicked() {
                    volume = if volume <= 0.001 { 1.0 } else { 0.0 };
                }
                let r = ui.add(egui::Slider::new(&mut volume, 0.0..=1.5).show_value(false)).on_hover_text("Playback volume, only here");
                if (volume - self.volume).abs() > f32::EPSILON || r.changed() {
                    self.volume = volume;
                    if let Some(audio) = &self.audio {
                        audio.set_gain(volume);
                    }
                }

                // Everything the selection can do, CapCut-style.
                ui.separator();
                self.action_bar(ui);
            });
            ui.add_space(style::GAP_S);
            // The timeline's own tools, next to its tracks.
            ui.horizontal(|ui| {
                ui.add_space(style::GAP_S);
                if icons::button(ui, icons::TITLE, "Add a title", "Ctrl+T", true).clicked() {
                    self.add_text();
                }
                if icons::button(ui, icons::CAPTIONS, "Captions: turn the speech on the timeline into captions", "", true).clicked() {
                    self.open_captions();
                }
                // Two clearly different buttons: a film strip for a video track, a note
                // for an audio one, each with a small + drawn into the corner.
                for (icon, what, kind) in [
                    (icons::VIDEO_TRACK, "video", oa_doc::TrackKind::Video),
                    (icons::AUDIO_TRACK, "audio", oa_doc::TrackKind::Audio),
                ] {
                    let r = icons::add_button(ui, icon, &format!("Add a {what} track"));
                    if r.clicked()
                        && let Err(e) = self.editor.add_track(kind)
                    {
                        self.report_error(e.to_string());
                    }
                }
                ui.separator();
                let snapping = self.snapping;
                let r = icons::button(ui, icons::SNAP, "Snap to clip edges and the playhead (Ctrl flips it while dragging)", "", true);
                if snapping {
                    ui.painter().rect_stroke(r.rect, style::ROUNDING, egui::Stroke::new(1.5, style::ACCENT), egui::StrokeKind::Inside);
                }
                if r.clicked() {
                    self.snapping = !snapping;
                }
                if icons::button(ui, icons::FIT, "Fit the whole sequence", "", true).clicked() {
                    self.timeline_view.fit = true;
                }
                // How tall the tracks are drawn: four steps, small to large.
                let tall = self.timeline_view.row_height;
                if icons::button(ui, icons::TRACK_HEIGHT, "Track height", "", true).clicked() {
                    self.timeline_view.row_height = (tall + 1) % timeline::ROW_HEIGHTS.len();
                }
                ui.label(egui::RichText::new("Ctrl+wheel zooms · wheel scrolls · drag the top edge for more room").small().weak());
            });
            ui.add_space(style::GAP_S);
            self.compound_trail_bar(ui);
            // The tracks scroll inside whatever height the panel has, rather than the
            // panel growing with every track until the viewer is gone.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.timeline(ui);
                ui.add_space(4.0);
            });
        });

        let media = egui::Panel::left(egui::Id::new(("media", epoch))).default_size(self.project_view.media).size_range(220.0..=520.0).show(root, |ui| {
            self.media_panel(ui);
        });

        egui::CentralPanel::default().show(root, |ui| {
            if vertical {
                self.inspector_panel(ui);
            } else {
                self.viewer_area(ui, frame);
            }
        });
        self.note_panel_sizes(right_width, media.response.rect.width(), controls.response.rect.height());
        self.remember_view(ctx);
        self.fit_window(ctx);
        self.remember_window(ctx);
        // A stand-in is on screen, or the frame is still being prepared: come back shortly.
        if self.rendered.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }

        if self.playing {
            ctx.request_repaint();
        }
    }
}
