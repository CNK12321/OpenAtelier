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
mod color_tab;
mod compound;
mod crop;
mod cut_transitions;
mod logo;
mod mask_draw;
mod masks;
mod media_pick;
mod points;
mod surface;
mod sound_cards;
mod connections;
mod tracks;
mod update;
mod deps;
mod drop_in;
mod eyedropper;
mod winfocus;
mod curves;
mod command;
mod editor;
mod export_dialog;
mod export_worker;
mod fontpick;
mod fullscreen;
mod formats;
mod gpu_reset;
mod gpu_watch;
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
mod plan_ahead;
mod plugin_previews;
mod plugin_scripts;
mod preview_worker;
mod proxies;
mod roto;
mod previews;
mod record;
mod captions;
mod layout;
mod script;
mod settings;
mod scopes;
mod sources;
mod style;
mod thumbnail;
mod text_edit;
mod thumbs;
mod timeline;
mod viewer;
mod viewer_render;
mod waves;
mod widgets;

use crate::i18n::{tr, trf};
use eframe::egui;
use editor::Editor;
use oa_audio::{AudioClip, AudioEngine, MixHandle, MixState, TimelineAudio};
use oa_doc::{ItemId, VariantId};
use oa_gpu::{readback, FusionMode, GpuContext, RenderOptions, RenderStats};
use oa_graph::registry::Registry;
use oa_media::{Imported, MediaKind, MediaProbe};
use oa_plan::{PlanOptions, PlanReport};
use oa_time::Time;
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
    /// Dropped onto the timeline: put there (`Placing`).
    place: Option<Placing>,
}

/// Where files dropped onto the timeline go: one after another from the time they were
/// dropped at (the next one's start, shared by the batch), on the track under the
/// pointer if it's free and the right kind.
#[derive(Clone)]
struct Placing {
    at: std::rc::Rc<std::cell::Cell<Time>>,
    track: Option<oa_doc::TrackId>,
}

/// During playback, how long the preview waits on a video frame before showing a
/// stand-in instead: time for the decode-ahead to hand over a frame, never a stall.
const PLAYBACK_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

const MEDIA_EXTENSIONS: &[&str] =
    &["mp4", "mov", "mkv", "m4v", "webm", "avi", "gif", "png", "jpg", "jpeg", "bmp", "webp", "svg", "mp3", "m4a", "wav", "flac", "aac", "ogg"];

fn main() -> eframe::Result {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // A restart after the GPU was lost: what to bring back (`gpu_reset.rs`).
    let resume = gpu_reset::Resume::take_from(&mut args);
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
    // On Windows the window takes drops itself (drop_in.rs: pictures from browsers too).
    let viewport = viewport.with_drag_and_drop(!cfg!(windows));
    // Maximized last time, or the first run: maximized from the start.
    let viewport = match &saved {
        Some(w) if w.maximized => viewport.with_maximized(true),
        None if script.is_none() => viewport.with_maximized(true),
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
            app.resume_after_gpu_reset(resume);
            if autoplay {
                app.set_playing(true);
            }
            Ok(Box::new(app))
        }),
    );
    // No window at all (no GPU driver that can draw one): say so, with what to try.
    if let Err(e) = &result {
        let advice = if cfg!(windows) {
            tr("Update the graphics driver, or try another graphics API: set the environment variable OA_GPU_BACKEND to vulkan or gl.")
        } else {
            tr("Install your GPU's Vulkan driver (mesa-vulkan-drivers, or the vendor's), or try OpenGL: set OA_GPU_BACKEND=gl.")
        };
        eprintln!("OpenAtelier couldn't start: {e}\n{advice}");
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(tr("OpenAtelier couldn't start"))
            .set_description(trf("The graphics card couldn't be set up:\n\n{e}\n\n{advice}", &[("e", &(e).to_string()), ("advice", (advice))]))
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
    let symbols = fc("DejaVu Sans").or_else(|| fc(tr("Noto Sans Symbols2"))).or_else(|| fc(tr("Noto Sans Symbols")));
    (ui, symbols)
}

struct Preview {
    /// Held while egui shows it (the render thread cycles its textures).
    _texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
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
    /// The idle, refined render (see [`App::refine_when_idle`]).
    refined: bool,
    see_through: bool,
}

struct App {
    gpu: Arc<GpuContext>,
    /// The viewer's picture: planned and rendered on a thread of its own (`viewer_render`).
    viewer_render: viewer_render::ViewerRender,
    /// The viewer's frames on the GPU, watched for plugin shaders that stall it.
    gpu_watch: gpu_watch::GpuWatch,
    /// Plugins turned off because the GPU was lost while they drew (told to the restarted
    /// editor).
    gpu_turned_off: Vec<String>,
    /// Shared with export threads.
    registry: Arc<Registry>,
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
    /// Playback muted (the volume kept for when it isn't).
    muted: bool,

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
    /// Text for the system clipboard, put there next frame (see `copy_selection`).
    clipboard_note: Option<String>,
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
    /// The tracks picked by clicking their headers (Ctrl+click for more, Shift+click for a
    /// run): pastes and duplicates go into the first one's first free space after the
    /// playhead, and Alt+↑/↓ moves them all.
    selected_tracks: Vec<oa_doc::TrackId>,
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
    /// Proxies of heavy footage, which the viewer plays instead (`proxies.rs`), and
    /// whether the viewer's files were last set with them on.
    proxies: proxies::Proxies,
    proxies_shown: bool,
    /// Where to put the playhead once the project being opened is in (a restart after
    /// the GPU was lost).
    resume_playhead: Option<Time>,
    /// The room the panels were last fitted to: when the window changes size, they are
    /// fitted again from `project_view` (the sizes wanted, not the ones squeezed in).
    fitted_room: egui::Vec2,
    /// A new window size waiting to hold still before it's saved, and since when.
    window_pending: Option<(settings::WindowState, f64)>,
    /// Frames since start while the window is being fitted to the screen (`None`: done).
    window_fitting: Option<u32>,
    timeline_view: timeline::TimelineView,
    view: viewer::ViewerView,
    thumbs: thumbs::Thumbs,
    /// Effect previews, the caption preview and the project picture render here, on a
    /// thread of their own.
    preview_worker: preview_worker::PreviewWorker,
    /// Filmstrips and waveforms drawn on timeline clips.
    clip_previews: previews::ClipPreviews,
    render_state: Option<eframe::egui_wgpu::RenderState>,
    /// Dragging the playhead: audio seeks wait until the drag ends.
    scrubbing: bool,
    /// The scrub paused playback; letting go plays on from where it was dropped.
    resume_after_scrub: bool,
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
    project_thumbs: std::collections::HashMap<String, Arc<std::sync::OnceLock<Option<egui::TextureHandle>>>>,
    /// Put the keyboard in the inspector's text box next frame (a title was just added).
    focus_text: bool,
    /// A title being typed into right on the canvas (double-click it in the viewer).
    canvas_text: Option<viewer::CanvasText>,
    /// Fullscreen playback (the frame alone), and its controls' state.
    fullscreen: fullscreen::Fullscreen,
    /// The Plugins page's opened effect list and its previews.
    plugin_previews: plugin_previews::PluginPreviews,
    /// The Color tab's scopes, curve and mixer choices, and a copied grade.
    color_tab: color_tab::ColorTab,
    /// The Masks tab: the mask being drawn, the tool, and a copied mask.
    masks: masks::Masks,
    /// A color property waiting to be picked from the viewer (the eyedropper).
    color_pick: Option<eyedropper::ColorPick>,
    /// What's dropped on the window from outside (files, pictures from a browser),
    /// where something being dragged in is now, and a drop the timeline may take this
    /// frame (where it landed) before it goes to the bin.
    drop_in: drop_in::DropIn,
    file_hover: Option<egui::Pos2>,
    file_drop: Option<(Vec<drop_in::Dropped>, egui::Pos2)>,
    /// Each compound clip's sound (its clips, in its own time) and whether it shows a
    /// picture, for drawing it on the timeline — worked out again after each edit (the
    /// project snapshot it was made from).
    compound_looks: std::collections::HashMap<oa_doc::SeqId, (usize, bool, Arc<Vec<AudioClip>>)>,
    /// The security confirmation for a plugin's scripts, while it's asked.
    script_consent: Option<plugin_scripts::ScriptConsent>,
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
    /// An effect's point being dragged in the viewer.
    point_drag: Option<points::PointDrag>,
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
    /// What the running export is ("MP4 · H.264 · 1920×1080 — Project").
    export_label: String,
    /// The part of the timeline the Export window is set to (None: all of it), and the
    /// live view of the running export.
    export_range: Option<(Time, Time)>,
    export_view: export_dialog::ExportView,
    /// The audio recorder window.
    recording: record::RecordWindow,

    preview: Option<Preview>,
    rendered: Option<FrameKey>,
    /// The picture on screen is (to be) the refined one — see `refine_when_idle`.
    refine: bool,
    /// The frame the picture last settled on, and since when (for `refine_when_idle`).
    idle_since: Option<(FrameKey, Instant)>,
    stats: RenderStats,
    /// The viewer's renderer's memory and its decoders' status, as of its last frame.
    memory: oa_gpu::Memory,
    sources_status: Option<String>,
    /// The frame asked of the render thread and not answered yet (its number and key).
    asked: Option<(u64, FrameKey)>,
    requests: u64,
    /// Results of requests before this one are of another project: not shown.
    fresh_from: u64,
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
        let (registry, issues) = plugins.registry(&settings.trusted_scripts);
        let registry = Arc::new(registry);
        let render_options = RenderOptions { fusion: FusionMode::Async, wait: false, vram_budget: settings.vram_budget(), ..Default::default() };
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
        let preview_worker = preview_worker::PreviewWorker::start(gpu.clone(), decoders.clone(), settings.vram_budget(), cc.egui_ctx.clone());
        let gpu_watch = gpu_watch::GpuWatch::default();
        let viewer_render = viewer_render::ViewerRender::start(gpu.clone(), decoders.clone(), render_options, registry.clone(), cc.egui_ctx.clone(), gpu_watch.clone());
        App {
            gpu,
            viewer_render,
            gpu_watch,
            gpu_turned_off: Vec::new(),
            registry,
            editor: Editor::new(),
            decoders,
            gpu_names: Vec::new(),
            audio: None,
            mix: MixHandle::default(),
            mixed: None,
            volume: 1.0,
            muted: false,
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
            clipboard_note: None,
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
            selected_tracks: Vec::new(),
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
            proxies: proxies::Proxies::default(),
            proxies_shown: false,
            resume_playhead: None,
            fitted_room: egui::Vec2::ZERO,
            window_pending: None,
            window_fitting: Some(0),
            timeline_view: timeline::TimelineView::default(),
            view: viewer::ViewerView::default(),
            thumbs: thumbs::Thumbs::default(),
            preview_worker,
            clip_previews: Default::default(),
            render_state: cc.wgpu_render_state.clone(),
            scrubbing: false,
            resume_after_scrub: false,
            seek_settling: false,
            focus_text: false,
            canvas_text: None,
            fullscreen: Default::default(),
            plugin_previews: Default::default(),
            color_tab: Default::default(),
            masks: Default::default(),
            color_pick: None,
            drop_in: Default::default(),
            file_hover: None,
            file_drop: None,
            compound_looks: Default::default(),
            script_consent: None,
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
            point_drag: None,
            export_dialog: false,
            export_queue: Default::default(),
            export_path: None,
            export_label: String::new(),
            export_range: None,
            export_view: Default::default(),
            recording: Default::default(),
            preview: None,
            rendered: None,
            refine: false,
            idle_since: None,
            stats: RenderStats::default(),
            memory: oa_gpu::Memory::default(),
            sources_status: None,
            asked: None,
            requests: 0,
            fresh_from: 0,
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
        self.set_aside_project();
        self.editor = Editor::new();
        self.reset_workspace();
        self.viewer_render.set_media(Vec::new(), true);
    }

    /// Back to the start page: the project is put away (saved, or kept as unsaved work
    /// the start page offers back) and the editor starts empty next time.
    pub(crate) fn go_home(&mut self) {
        self.new_project();
        self.screen = home::Screen::Home;
    }

    /// Before another project takes the editor's place: its unsaved work isn't lost. A
    /// saved project that saves as you go is saved now; otherwise (never saved, or
    /// saving as you go is off) its autosave is written and left for the start page's
    /// "Unsaved work", and a fresh autosave begins for what comes next.
    fn set_aside_project(&mut self) {
        self.finish_background_save();
        if !self.editor.dirty() {
            self.autosave.clear();
            return;
        }
        if let Some(path) = self.editor.path.clone().filter(|_| self.settings.save_as_you_go)
            && self.editor.save(&path).is_ok()
        {
            let name = self.editor.doc.project().name.clone();
            let thumb = thumbnail::path_for(&path);
            self.settings.remember_project(&path, &name, thumb.exists().then_some(thumb));
            self.autosave.clear();
            return;
        }
        let project = self.editor.doc.snapshot();
        match self.autosave.write_now(&project, self.editor.path.as_deref()) {
            Ok(()) => {
                self.autosave.move_on();
                self.recoveries = self.autosave.recoverable();
            }
            Err(e) => self.report_error(trf("couldn't keep the unsaved work: {e}", &[("e", &e.to_string())])),
        }
    }

    /// Everything that belonged to the project that was open, gone: what was selected,
    /// being dragged, cropped or typed into, the editor windows, clipboards (they point
    /// at its media), cached pictures and filmstrips (keyed by its ids, which the next
    /// project reuses), the sound, the viewer's view. Called once the new project (or
    /// an empty one) is in `self.editor`; exports carry on — each has its own copy.
    fn reset_workspace(&mut self) {
        self.compound_trail.clear();
        self.playhead = Time::ZERO;
        self.set_playing(false);
        self.reset_audio();
        self.audition = None;
        self.variant = 0;
        self.selection = None;
        self.selected.clear();
        self.selected_tracks.clear();
        self.clipboard.clear();
        self.clipboard_note = None;
        self.effect_clipboard.clear();
        self.effect_clipboard_reverse = None;
        self.value_clipboard = None;
        self.transform_clipboard = None;
        self.renaming = None;
        self.bands.clear();
        self.viewer_drag = None;
        self.timeline_drag = None;
        self.effect_drag = None;
        self.drop_targets.clear();
        self.fx_lanes.clear();
        self.viewer_canvas = None;
        self.scrubbing = false;
        self.resume_after_scrub = false;
        self.seek_settling = false;
        self.focus_text = false;
        self.canvas_text = None;
        self.crop_mode = None;
        self.crop_drag = None;
        self.surface_drag = None;
        self.point_drag = None;
        self.curve_editor = None;
        self.wave_editor = None;
        self.connection_editor = None;
        self.track_editor = None;
        self.captions = None;
        self.captions_added = None;
        self.follower = None;
        self.follower_key = (0, 0);
        self.follower_complete = false;
        self.export_range = None;
        self.bin.folder.clear();
        self.bin.search.clear();
        self.bin.naming = None;
        self.timeline_view = timeline::TimelineView { row_height: self.timeline_view.row_height, ..Default::default() };
        self.view = viewer::ViewerView { thirds: self.view.thirds, safe_areas: self.view.safe_areas, center: self.view.center, ..Default::default() };
        self.forget_thumbs();
        self.clip_previews = Default::default();
        self.error = None;
        self.report = PlanReport::default();
        self.frame_ms.clear();
        self.idle_since = None;
        self.refine = false;
        self.failed_frames = 0;
        self.viewer_render.clear_cache();
        self.blank_preview();
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
        let (registry, issues) = self.plugins.registry(&self.settings.trusted_scripts);
        self.registry = Arc::new(registry);
        self.messages.extend(issues);
        self.viewer_render.warm_up(self.registry.clone());
        self.forget_thumbs();
        self.rendered = None;
    }

    // ---- media & projects -------------------------------------------------

    /// Can this file be played as it is? Answering "no" makes the importer conform it to
    /// MP4 first. With Media Foundation's zero-copy decoder in use (Windows, DX12), files
    /// it can't take are conformed so they play on it too; otherwise ffmpeg decodes
    /// anything as it is.
    fn decodable(choice: &oa_media::DecoderChoice, path: &Path, probe: &MediaProbe) -> bool {
        // Video with transparency (an animated GIF…) plays as it is, through ffmpeg:
        // conformed to H.264 it would lose it.
        if probe.video.as_ref().is_some_and(|v| v.has_alpha) {
            return true;
        }
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

    /// Clips of `seq` first seen between `from` and `to` (its own time), as (when on the
    /// outer timeline, media, where in the file): a clip counts from where it first shows
    /// — the start of a transition into it, which shows it before its own start — and
    /// clips inside compound clips count too, at their place and speed. `offset` is where
    /// `seq`'s time 0 is on the outer timeline (for ordering only).
    fn upcoming_in(project: &oa_doc::Project, seq: oa_doc::SeqId, from: Time, to: Time, offset: Time, depth: usize, found: &mut Vec<(Time, u64, Time)>) {
        let Some(s) = project.sequence(seq).filter(|_| depth < 8) else { return };
        for track in s.tracks.iter().filter(|t| t.enabled && t.kind == oa_doc::TrackKind::Video) {
            for (i, item) in track.items.iter().enumerate().filter(|(_, i)| i.enabled) {
                let shows_from = oa_plan::transitions::window(track, i, oa_doc::ClipEnd::Head).map_or(item.range.start, |w| w.start.min(item.range.start));
                match item.kind {
                    oa_doc::ItemKind::Media { media } if shows_from > from && shows_from <= to => {
                        found.push((offset + shows_from, media.0, item.time_map.source_time(shows_from - item.range.start)));
                    }
                    // A compound clip playing now or soon: what starts inside it over the
                    // same stretch, in its own time.
                    oa_doc::ItemKind::Nested { sequence } if item.range.end() > from && shows_from <= to => {
                        let inner = |t: Time| item.time_map.source_time(t.max(item.range.start) - item.range.start);
                        let (a, b) = (inner(from), inner(to.min(item.range.end())));
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        // Its first frame counts as coming when the compound itself is.
                        let lo = if shows_from > from { lo - Time(1) } else { lo };
                        Self::upcoming_in(project, sequence, lo, hi, offset + item.range.start, depth + 1, found);
                    }
                    _ => {}
                }
            }
        }
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

    /// Opens a project file, or imports media files into the media bin (only there: they
    /// go on the timeline when you put them there — drag a card, double-click it or ＋).
    /// Both happen on worker threads: the editor stays responsive, and the media bin
    /// shows a loading card per file until it's in.
    pub(crate) fn open_paths(&mut self, paths: &[PathBuf]) {
        let is_project = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if let Some(project) = paths.iter().find(|p| is_project(p)) {
            self.open_project(project);
            return;
        }
        // Into the folder the bin is showing, so they appear where you're looking.
        let folder = if self.bin.tab == bin::BinTab::Project { self.bin.folder.clone() } else { String::new() };
        // A folder brings its media in as a bin folder of the same name, and the folders
        // inside it as folders inside that one.
        let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = paths.iter().cloned().partition(|p| p.is_dir());
        self.import_paths(&files, false, &folder);
        for (bin_folder, files) in dirs.iter().flat_map(|d| bin::media_in_folder(d, &folder)) {
            self.import_paths(&files, false, &bin_folder);
        }
    }

    /// What was dropped on the window: files open or import as they would from the
    /// bin; pictures from a browser are saved first (`<app>/dropped`), and addresses
    /// downloaded. With `place` (dropped onto the timeline: a time and the track under
    /// it), the media also goes there, one after another.
    pub(crate) fn take_dropped(&mut self, items: Vec<drop_in::Dropped>, place: Option<(Time, Option<oa_doc::TrackId>)>) {
        let mut files = Vec::new();
        let mut urls = Vec::new();
        for item in items {
            match item {
                drop_in::Dropped::File(path) => files.push(path),
                drop_in::Dropped::Data { name, bytes } => match drop_in::save(&name, &bytes) {
                    Ok(path) => files.push(path),
                    Err(e) => self.report_error(e),
                },
                drop_in::Dropped::Url(url) => match drop_in::data_url(&url) {
                    Some(bytes) => match drop_in::save("dropped", &bytes) {
                        Ok(path) => files.push(path),
                        Err(e) => self.report_error(e),
                    },
                    None => urls.push(url),
                },
            }
        }
        let is_project = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let Some((at, track)) = place.filter(|_| !files.iter().any(|f| is_project(f))) else {
            self.open_paths(&files);
            for url in urls {
                self.import_url(url, None);
            }
            return;
        };
        // Onto the timeline: a folder's media too, all in a row.
        let place = Placing { at: std::rc::Rc::new(std::cell::Cell::new(at)), track };
        let folder = if self.bin.tab == bin::BinTab::Project { self.bin.folder.clone() } else { String::new() };
        let (dirs, mut files): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(|p| p.is_dir());
        files.extend(dirs.iter().flat_map(|d| bin::media_in_folder(d, &folder)).flat_map(|(_, f)| f));
        self.import_files(&files, false, &folder, Some(place.clone()));
        for url in urls {
            self.import_url(url, Some(place.clone()));
        }
    }

    /// Downloads `url` (a picture or a video dragged out of a browser as a link) into
    /// `<app>/dropped`, then imports it like a file.
    fn import_url(&mut self, url: String, place: Option<Placing>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = self.bridge();
        let name = url.split(['?', '#']).next().unwrap_or(&url).rsplit('/').find(|s| !s.is_empty()).unwrap_or("download").to_string();
        let address = url.clone();
        std::thread::spawn(move || {
            let result = drop_in::download(&address).and_then(|file| Self::import_with(bridge, &file));
            let _ = tx.send(result);
        });
        let folder = if self.bin.tab == bin::BinTab::Project { self.bin.folder.clone() } else { String::new() };
        self.imports.push_back(PendingImport { path: PathBuf::from(&url), name: drop_in::safe_name(&name), rx, to_timeline: false, folder, place });
    }

    /// Imports media files; with `to_timeline` they're also added to the end of the
    /// timeline, otherwise they only go into the bin's `folder`.
    pub(crate) fn import_paths(&mut self, paths: &[PathBuf], to_timeline: bool, folder: &str) {
        self.import_files(paths, to_timeline, folder, None);
    }

    fn import_files(&mut self, paths: &[PathBuf], to_timeline: bool, folder: &str, place: Option<Placing>) {
        for path in paths {
            let (tx, rx) = std::sync::mpsc::channel();
            let (bridge, file) = (self.bridge(), path.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Self::import_with(bridge, &file));
            });
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string());
            self.imports.push_back(PendingImport { path: path.clone(), name, rx, to_timeline, folder: folder.to_string(), place: place.clone() });
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
                                self.notify(trf("{name}: converted for playback ({reason})", &[("name", &name.to_string()), ("reason", &reason.to_string())]));
                            }
                            if !job.folder.is_empty() {
                                let op = oa_doc::Op::SetMediaFolder { media, folder: job.folder.clone() };
                                if let Err(e) = self.editor.apply(tr("Move to folder"), vec![op]) {
                                    self.error = Some(e.to_string());
                                }
                            }
                            if let Some(place) = &job.place {
                                // Where it was dropped; the next one in the batch after it.
                                match self.editor.place_clip(&name, oa_doc::ItemKind::Media { media }, duration, place.at.get(), place.track) {
                                    Ok(item) => {
                                        place.at.set(place.at.get() + duration);
                                        self.selection = Some(item);
                                    }
                                    Err(e) => self.error = Some(e.to_string()),
                                }
                            } else if job.to_timeline {
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
                if let Some(t) = self.resume_playhead.take().filter(|_| ok) {
                    self.set_playhead(t);
                }
                if let Some((r, _)) = recovered.filter(|_| ok) {
                    self.recoveries.retain(|x| x.file != r.file);
                    if let Err(e) = r.discard() {
                        self.report_error(trf("{0}: {e}", &[("0", &(r.file.display()).to_string()), ("e", &(e).to_string())]));
                    }
                    self.autosave.soon();
                    self.notify(trf("recovered unsaved work from {0}", &[("0", &(r.name()).to_string())]));
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
            Err(e) => self.report_error(trf("{0}: {e}", &[("0", &(r.file.display()).to_string()), ("e", &(e).to_string())])),
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
                // The old project's unsaved work is kept (and a save of it still going
                // belongs to it).
                self.set_aside_project();
                self.editor = editor;
                self.reset_workspace();
                self.messages.extend(warnings);
                self.rebuild_sources();
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Takes the viewer's picture down (another project was opened): until the new one's
    /// first frame is ready, the viewer shows nothing rather than a frame of the last
    /// project — the viewer keeps its last picture while a frame isn't ready.
    fn blank_preview(&mut self) {
        if let Some(old) = self.preview.take()
            && let Some(rs) = &self.render_state
        {
            rs.renderer.write().free_texture(&old.id);
        }
        self.rendered = None;
        // A frame of the last project still on its way isn't shown when it comes.
        self.asked = None;
        self.fresh_from = self.requests + 1;
    }

    /// Rebuilds the frame sources from the pool (after import, open, or relink).
    fn rebuild_sources(&mut self) {
        self.viewer_render.set_media(self.viewer_entries(), true);
        self.add_new_sources();
    }

    /// The files the viewer plays: each file's proxy where there's one ready and proxies
    /// are on (the same times, a smaller picture), the file itself otherwise.
    fn viewer_entries(&self) -> Vec<export_worker::MediaEntry> {
        let mut entries = self.media_entries();
        if self.settings.use_proxies {
            for e in &mut entries {
                if let Some((path, track)) = self.editor.pool_item(oa_doc::MediaId(e.id)).and_then(|m| self.proxies.ready(&m.decode_path)) {
                    e.path = path.to_path_buf();
                    e.track = Some(track.clone());
                }
            }
        }
        entries
    }

    /// Looks for each video file's proxy (making one for heavy footage, if that's on),
    /// and when what the viewer plays changes — a proxy ready, proxies switched — starts
    /// its decoders over on the new files.
    fn proxy_tick(&mut self) {
        for m in &self.editor.pool {
            if m.missing || m.kind != MediaKind::Video {
                continue;
            }
            let Some(track) = &m.probe.video else { continue };
            let make = self.settings.auto_proxies && proxies::heavy(track);
            self.proxies.find(&m.decode_path, make);
        }
        let using = self.settings.use_proxies;
        if self.proxies.poll() | (self.proxies_shown != using) {
            self.proxies_shown = using;
            self.viewer_render.set_media(self.viewer_entries(), true);
            // The frame on screen came from the other files: rendered again (it stays
            // up meanwhile).
            self.rendered = None;
            self.asked = None;
        }
    }

    /// Registers pool media the frame sources don't know yet, leaving running decoders
    /// alone (an import doesn't restart playback of what's already there). Still images
    /// decode on worker threads.
    fn add_new_sources(&mut self) {
        // The viewer's render thread registers the files it doesn't know yet (its
        // decoders; stills decode in the background); the preview thread decodes for
        // itself too.
        self.viewer_render.set_media(self.viewer_entries(), false);
        // Previews (and the Color tab's scopes) read the files themselves.
        self.preview_worker.set_media(self.media_entries());
        // Pool changes (relinks, imports) can change what's audible without a new clip.
        self.mixed = None;
    }

    /// The pool's files, for a thread that decodes on its own (exports, previews).
    fn media_entries(&self) -> Vec<export_worker::MediaEntry> {
        self.editor
            .pool
            .iter()
            .filter(|m| !m.missing)
            .map(|m| export_worker::MediaEntry { id: m.id.0, kind: m.kind, path: m.decode_path.clone(), track: m.probe.video.clone() })
            .collect()
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
                    engine.set_muted(self.muted);
                    engine.set_output_delay(std::time::Duration::from_millis(self.settings.output_delay_ms as u64));
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
                    ui.heading(tr("Clip"));
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
        self.refine_when_idle(ui.ctx());
        // Playing: the frame on screen first; background previews wait.
        self.preview_worker.set_busy(self.playing);
        // The render thread's finished pictures first; then, if what's on screen isn't
        // the frame wanted now, it's asked for (and drawn meanwhile as it is).
        if let Some(render_state) = frame.wgpu_render_state() {
            self.receive_frames(render_state);
        }
        let stale = self.preview.is_none() || self.rendered.as_ref() != Some(&self.frame_key());
        if stale && !self.gpu_lost {
            self.request_frame();
        }
        self.viewer(ui);
    }

    /// Writes the open timeline as OpenTimelineIO (`oa_export::otio`), for finishing in
    /// another editor: the edit, pointing at the original files.
    pub(crate) fn export_otio(&mut self) {
        // The project's own timeline, even while a compound clip is open.
        let (seq, _) = self.main_timeline();
        let name = self.editor.doc.project().sequence(seq).map_or_else(|| "Timeline".to_string(), |s| s.name.clone());
        let stem = self.editor.path.as_ref().and_then(|p| p.file_stem()).map_or(name.clone(), |s| s.to_string_lossy().trim_end_matches(".oaproj").to_string());
        let Some(path) = rfd::FileDialog::new().add_filter(tr("OpenTimelineIO"), &["otio"]).set_file_name(format!("{stem}.otio")).save_file() else { return };
        let project = self.editor.doc.project();
        let media_path = |id: oa_doc::MediaId| {
            let m = project.media(id)?;
            let audio = m.info.as_ref().is_none_or(|i| i.has_audio);
            Some((PathBuf::from(&m.path), audio))
        };
        match oa_export::otio::write(project, seq, &media_path, &path) {
            Ok(()) => self.notify(trf("Wrote {0} — open it in DaVinci Resolve (File → Import → Timeline) or another editor that reads OpenTimelineIO. Effects and titles stay in OpenAtelier.", &[("0", &(path.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string())).to_string())])),
            Err(e) => self.report_error(trf("Couldn't write the timeline: {e}", &[("e", &e.to_string())])),
        }
    }

    /// Opens the Export window (type, format, resolution, quality…).
    pub(crate) fn start_export(&mut self) {
        // The project's own timeline, even while a compound clip is open.
        let (seq, _) = self.main_timeline();
        if self.editor.doc.project().sequence(seq).is_none_or(|s| s.duration() <= Time::ZERO) {
            self.error = Some(tr("nothing to export: the timeline is empty").into());
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
        self.messages.push(format!("exporting to {}", job.path.display()));
        self.export_label = job.label.clone();
        self.export = Some(export_worker::start(export_worker::Job {
            gpu: self.gpu.clone(),
            decoders: self.decoders.clone(),
            registry: self.registry.clone(),
            // As it was queued.
            project: job.project,
            media: job.media,
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
                self.notify(trf("Exported {name} ({0}×{1}, {2} fps).", &[("name", &(name).to_string()), ("0", &(summary.size[0]).to_string()), ("1", &(summary.size[1]).to_string()), ("2", &format!("{:.0}", summary.frames_per_second()))]));
            }
            Err(e) => self.report_error(trf("{name}: {e}", &[("name", &name.to_string()), ("e", &e.to_string())])),
        }
        self.next_export();
    }

    /// Stops the running export and everything queued after it.
    pub(crate) fn cancel_exports(&mut self) {
        self.export_queue.clear();
        self.skip_export();
    }

    /// Stops the running export (its file isn't finished) and goes on with the next
    /// queued one.
    pub(crate) fn skip_export(&mut self) {
        if self.export.take().is_some()
            && let Some(path) = self.export_path.take()
        {
            self.notify(trf("Canceled {0}.", &[("0", &(path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string())).to_string())]));
        }
        self.export_label.clear();
        self.next_export();
    }

    pub(crate) fn save(&mut self, save_as: bool) {
        // Never two writers on one file: let a background save finish first.
        self.finish_background_save();
        let path = match (&self.editor.path, save_as) {
            (Some(path), false) => Some(path.clone()),
            _ => rfd::FileDialog::new()
                .add_filter(tr("OpenAtelier project"), &["json"])
                .set_file_name("project.oaproj.json")
                .save_file(),
        };
        let Some(path) = path else { return };
        match self.editor.save(&path) {
            Ok(()) => {
                self.notify(trf("saved {0}", &[("0", &(path.display()).to_string())]));
                // An untitled project takes its name from the file it was saved as.
                let name = self.editor.doc.project().name.clone();
                let name = if name.is_empty() || name == "Untitled" { thumbnail::name_from_path(&path) } else { name };
                self.editor.set_project_name(&name);
                let thumb = self.write_thumbnail(&path);
                // The start page loads the new picture next time it's shown.
                if let Some(t) = &thumb {
                    self.project_thumbs.remove(&t.to_string_lossy().to_string());
                }
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

    /// Scrubbing pauses playback (the picture follows the pointer, not the clock);
    /// `end_scrub` plays on from where it was let go.
    pub(crate) fn begin_scrub(&mut self) {
        self.scrubbing = true;
        self.resume_after_scrub = self.playing;
        if self.playing {
            self.set_playing(false);
        }
    }

    pub(crate) fn end_scrub(&mut self) {
        if !std::mem::take(&mut self.scrubbing) {
            return;
        }
        if let Some(audio) = &self.audio {
            audio.seek(self.playhead);
        }
        if std::mem::take(&mut self.resume_after_scrub) {
            self.last_tick = Instant::now();
            self.set_playing(true);
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

    /// The playhead to the first frame (`start`) or the last frame of the selected clip —
    /// or, with none selected, the topmost clip under the playhead. Already there: on to
    /// the clip before or after it on its track (which becomes the selection, if a clip
    /// was selected).
    pub(crate) fn go_to_clip_edge(&mut self, start: bool) {
        let s = self.editor.sequence();
        let rate = s.rate;
        let t = self.playhead;
        let under = || {
            // Picture tracks top down, then sound.
            let order = s.tracks.iter().enumerate().rev().filter(|(_, tr)| tr.kind == oa_doc::TrackKind::Video).chain(s.tracks.iter().enumerate().filter(|(_, tr)| tr.kind == oa_doc::TrackKind::Audio));
            order.filter(|(_, tr)| tr.enabled).find_map(|(ti, tr)| tr.items.iter().position(|i| i.range.contains(t)).map(|ii| (ti, ii)))
        };
        let Some((ti, ii)) = self.selection.and_then(|id| s.find_item(id)).or_else(under) else { return };
        let items = &s.tracks[ti].items;
        let edge = |i: &oa_doc::Item| if start { i.range.start } else { rate.frame_start((rate.frame_at(i.range.end()) - 1).max(rate.frame_at(i.range.start))) };
        let mut at = (ii, edge(&items[ii]));
        if at.1 == t {
            let next = if start { ii.checked_sub(1) } else { Some(ii + 1).filter(|n| *n < items.len()) };
            if let Some(n) = next {
                at = (n, edge(&items[n]));
            }
        }
        let landed = items[at.0].id;
        if self.selection.is_some_and(|s| s != landed) {
            self.select_clip(landed, false);
        }
        self.set_playhead(at.1);
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
        self.preview_scale_at(self.refine)
    }

    fn preview_scale_at(&self, refine: bool) -> f64 {
        let mut asked = if self.scale > 0.0 {
            self.scale as f64
        } else {
            match self.display_scale {
                s if s > 0.5 => 1.0,
                s if s > 0.25 => 0.5,
                _ => 0.25,
            }
        };
        // Refining an idle frame: twice the pixels each way the viewer shows (up to 2×
        // the canvas), averaged down on screen — anti-aliasing for everything.
        if refine {
            asked = asked.max((self.display_scale as f64 * 2.0).min(2.0));
        }
        // Never more than the preview limit on the short side: a 4K format previews at
        // 1080p by default (and its 4K files are fetched at that size, too).
        let limit = self.settings.preview_limit;
        let size = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
        let short = size.width.min(size.height).max(1);
        let asked = if limit > 0 && short > limit { asked.min(limit as f64 / short as f64) } else { asked };
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
            Err(e) => self.report_error(trf("couldn't start saving: {e}", &[("e", &e.to_string())])),
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
                self.report_error(trf("{e} — saving as you go is off for now; the recovery autosave is still running.", &[("e", &e.to_string())]));
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
            ui.heading(tr("Save before closing?"));
            ui.add_space(style::GAP_S);
            ui.label(match &name {
                Some(file) => format!("{file} has changes that aren't saved."),
                None => tr("This project has never been saved.").to_string(),
            });
            ui.label(egui::RichText::new(tr("There's an autosave either way — this is the real file.")).small().weak());
            ui.add_space(style::GAP_L);
            ui.horizontal(|ui| {
                if ui.button(tr("Save and close")).clicked() {
                    choice = Some("save");
                }
                if ui.button(tr("Close without saving")).clicked() {
                    choice = Some("discard");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Keep editing")).clicked() {
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
            // A plugin's effect on the GPU when it went: turned off first, so the
            // restarted editor doesn't run it straight into the same reset.
            // (Only the setting: nothing can be rebuilt on the lost device.)
            let effects = self.gpu_watch.in_flight();
            for (id, name) in self.plugins_providing(&effects) {
                self.settings.set_plugin_enabled(&id, false);
                self.gpu_turned_off.push(name);
            }
            // Scripted runs (tests) stop there; otherwise the work is saved and a new
            // OpenAtelier opens it (`gpu_reset.rs`) — this one can't draw any more.
            let not_restarted = if self.script.is_some() { Err("scripted run".to_string()) } else { self.restart_after_gpu_reset() };
            // Still here: it didn't restart. The user's work first, then say so.
            let _ = self.autosave.write_now(&self.editor.doc.snapshot(), self.editor.path.as_deref());
            let why = self.gpu.health.message().unwrap_or_else(|| "the graphics device was lost".into());
            let also = not_restarted.err().filter(|e| e != "scripted run").map(|e| format!(" ({e})")).unwrap_or_default();
            self.report_error(trf("{why}{also}. Your work has been autosaved — restart OpenAtelier to carry on. If it keeps happening, try another graphics API in Settings → Graphics, or update the graphics driver.", &[("why", &why.to_string()), ("also", &also.to_string())]));
            return;
        }
        let memory = self.memory;
        let was = self.memory_backoff;
        self.memory_backoff = match memory.pressure {
            oa_gpu::Pressure::Over => (self.memory_backoff * 0.8).max(0.25),
            // Only climb back when there is real room, so it can't oscillate.
            oa_gpu::Pressure::Easy if memory.fraction() < 0.5 => (self.memory_backoff * 1.05).min(1.0),
            _ => self.memory_backoff,
        };
        if was >= 1.0 && self.memory_backoff < 1.0 {
            self.notify(trf("Short of GPU memory ({0} of {1}): rendering the preview smaller.", &[("0", &(bytes(memory.used())).to_string()), ("1", &(bytes(memory.budget)).to_string())]));
        }
    }

    fn variant_id(&self) -> VariantId {
        let seq = self.editor.sequence();
        seq.variants[self.variant.min(seq.variants.len() - 1)].id
    }

    fn frame_key(&self) -> FrameKey {
        self.frame_key_with(self.refine)
    }

    /// The moment the preview shows: the playhead — or, playing, the start of the
    /// sequence frame it's in. Playback shows the timeline's own frames (as an export
    /// does): a screen faster than the frame rate doesn't render one frame several
    /// times, and the next frame is known, so it can be planned ahead (`plan_ahead`).
    fn render_time(&self) -> Time {
        if self.playing {
            let rate = self.editor.sequence().rate;
            rate.frame_start(rate.frame_at(self.playhead))
        } else {
            self.playhead
        }
    }

    fn frame_key_with(&self, refine: bool) -> FrameKey {
        FrameKey {
            playhead: self.render_time(),
            variant: self.variant,
            scale: (self.preview_scale_at(refine) as f32).to_bits(),
            reference: self.reference,
            document: Arc::as_ptr(&self.editor.doc.snapshot()) as usize,
            follow: self.follower_key.1,
            refined: refine,
            see_through: !self.compound_trail.is_empty(),
        }
    }

    /// Progressive refinement: once the picture has held still for a moment (paused,
    /// nothing being dragged or scrubbed, the exact frame in), it's rendered once more at
    /// a higher quality — supersampled 2× and with smooth text, as an export would draw
    /// it. Anything that changes the frame drops back to the fast preview at once.
    fn refine_when_idle(&mut self, ctx: &egui::Context) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(350);
        let base = self.frame_key_with(false);
        let busy = self.playing || self.scrubbing || self.timeline_drag.is_some() || self.viewer_drag.is_some() || ctx.input(|i| i.pointer.any_down());
        if self.idle_since.as_ref().is_none_or(|(k, _)| *k != base) || busy {
            self.idle_since = Some((base, Instant::now()));
            self.refine = false;
            return;
        }
        if self.refine || self.rendered.is_none() {
            return;
        }
        let waited = self.idle_since.as_ref().map_or(std::time::Duration::ZERO, |(_, since)| since.elapsed());
        if waited >= SETTLE {
            self.refine = true;
        } else {
            ctx.request_repaint_after(SETTLE - waited);
        }
    }


    /// Asks the viewer's render thread (`viewer_render`) for the frame on screen now:
    /// the project as it is, the moment, the options — it plans and renders there, and
    /// the picture comes back through `receive_frames`.
    fn request_frame(&mut self) {
        let key = self.frame_key();
        if self.asked.as_ref().is_some_and(|(_, k)| *k == key) {
            return; // on its way
        }
        let opts = PlanOptions {
            variant: Some(self.variant_id()),
            render_scale: self.preview_scale(),
            // Inside a compound clip: see-through, as it is where it's used.
            see_through: !self.compound_trail.is_empty(),
            // The refined idle frame draws titles as an export does.
            text_supersample: if self.refine { 2 } else { 1 },
            ..Default::default()
        };
        let t = self.render_time();
        let rate = self.editor.sequence().rate;
        let next = rate.frame_start(rate.frame_at(t) + 1);
        self.requests += 1;
        self.asked = Some((self.requests, key));
        self.viewer_render.request(viewer_render::Request {
            id: self.requests,
            project: self.editor.doc.snapshot(),
            seq: self.editor.seq,
            t,
            opts,
            reference: self.reference,
            registry: self.registry.clone(),
            follower: self.follower.clone(),
            follow: self.follower_key.1,
            // Paused (scrubbing, stepping, editing): never wait on the decoder — the
            // nearest frame on hand now, the exact one as soon as it's decoded. Playing:
            // exact frames, which the decode-ahead has ready anyway, waiting at most a
            // moment on one (a clip starting whose decoder is still seeking).
            interactive: !self.playing || self.seek_settling,
            wait_budget: self.playing.then_some(PLAYBACK_WAIT),
            warm: if self.playing { self.upcoming_clips() } else { Vec::new() },
            // Playing: the next frame is planned while this one renders.
            next: (self.playing && next < self.editor.duration()).then_some(next),
        });
    }

    /// Puts the pictures the render thread finished on screen (the newest), with what it
    /// reports alongside.
    fn receive_frames(&mut self, render_state: &eframe::egui_wgpu::RenderState) {
        for done in self.viewer_render.results() {
            self.stats = done.stats;
            self.memory = done.memory;
            self.sources_status = done.status;
            if done.id < self.fresh_from {
                continue; // a frame of the project before
            }
            let answered = self.asked.as_ref().filter(|(id, _)| *id == done.id).map(|(_, k)| k.clone());
            if answered.is_some() {
                self.asked = None;
            }
            if let Some(e) = done.error {
                self.error = Some(e);
                continue;
            }
            self.report = done.report;
            let Some(texture) = done.texture else {
                // Not ready yet: the last picture stays up; asked again next frame.
                self.rendered = None;
                continue;
            };
            let view = readback::display_view(&texture);
            let id = match self.preview.take() {
                Some(old) => {
                    render_state.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, old.id);
                    old.id
                }
                None => render_state.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
            };
            self.preview = Some(Preview { _texture: texture, id, size: done.size });
            if done.settled {
                self.seek_settling = false;
            }
            // Stand-ins were shown: asked again, for the exact frames.
            self.rendered = if done.settled { answered } else { None };
            self.error = None;
            self.frame_ms.push_back(done.ms);
            if self.frame_ms.len() > 120 {
                self.frame_ms.pop_front();
            }
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
        // Typing into a title on the canvas: the keys (and copy/cut/paste) are its.
        if ctx.egui_wants_keyboard_input() || self.canvas_text.is_some() {
            return;
        }
        let pressed = |m: M, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        // F: fullscreen playback, in and out. While it's on, only playing and stepping —
        // no edits made by a key pressed while watching.
        if self.screen == home::Screen::Editor && pressed(M::NONE, Key::F) {
            self.set_fullscreen(ctx, !self.fullscreen.on);
        }
        if self.fullscreen.on {
            if pressed(M::NONE, Key::Space) {
                self.set_playing(!self.playing);
                self.last_tick = Instant::now();
            }
            if pressed(M::NONE, Key::ArrowRight) {
                self.step(1);
            }
            if pressed(M::NONE, Key::ArrowLeft) {
                self.step(-1);
            }
            return;
        }
        // With the Captions window open, Ctrl+Z is its own (it takes the keys itself).
        if self.captions.is_none() {
            if pressed(M::COMMAND | M::SHIFT, Key::Z) || pressed(M::COMMAND, Key::Y) {
                self.redo();
            }
            if pressed(M::COMMAND, Key::Z) {
                self.undo();
            }
        }
        // Ctrl+← / Ctrl+→: to the start / end of the clip.
        if pressed(M::COMMAND, Key::ArrowLeft) {
            self.go_to_clip_edge(true);
        }
        if pressed(M::COMMAND, Key::ArrowRight) {
            self.go_to_clip_edge(false);
        }
        // Nudge the selected layer: Alt+arrows (Shift for 10 px). With tracks picked,
        // Alt+↑/↓ moves them instead (below).
        let arrows = [(Key::ArrowLeft, [-1.0, 0.0]), (Key::ArrowRight, [1.0, 0.0]), (Key::ArrowUp, [0.0, -1.0]), (Key::ArrowDown, [0.0, 1.0])];
        for (key, d) in arrows {
            if d[0] == 0.0 && !self.selected_tracks.is_empty() {
                continue;
            }
            for (mods, step) in [(M::ALT | M::SHIFT, 10.0), (M::ALT, 1.0)] {
                if pressed(mods, key) && self.selection.is_some() {
                    // Every selected clip that has a picture (and is on screen now).
                    let mut ops = Vec::new();
                    for item in self.selected_clips() {
                        if let Ok(more) = oa_edit::transform::nudge(
                            self.editor.doc.project(),
                            self.editor.seq,
                            self.variant_id(),
                            item,
                            self.playhead,
                            [d[0] * step, d[1] * step],
                            self.transform_scope(),
                        ) {
                            ops.extend(more);
                        }
                    }
                    if !ops.is_empty()
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
            if !std::mem::take(&mut self.selected_tracks).is_empty() {
                // First Escape lets go of the tracks.
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
            match self.selected_tracks.first().copied() {
                Some(track) => self.duplicate_into_track(track),
                None => self.duplicate_selection(),
            }
        }
        if !self.selected_tracks.is_empty() {
            let tracks = self.selected_tracks.clone();
            if pressed(M::ALT, Key::ArrowUp) {
                self.move_tracks(&tracks, true);
            }
            if pressed(M::ALT, Key::ArrowDown) {
                self.move_tracks(&tracks, false);
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
            match self.selected_tracks.first().copied() {
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

    /// S: cuts every selected clip under the playhead (the back halves become the
    /// selection); with none of them there, every clip under it.
    /// While playing: the clips starting in the next couple of seconds and where in
    /// their files — their decoders are opened there ahead of time, so a new file doesn't
    /// stall the frame it first shows in.
    fn upcoming_clips(&self) -> Vec<(u64, Time)> {
        const AHEAD: Time = Time::from_seconds(2);
        let project = self.editor.doc.project();
        let mut found = Vec::new();
        Self::upcoming_in(project, self.editor.seq, self.playhead, self.playhead + AHEAD, Time::ZERO, 0, &mut found);
        // Soonest first: when one file is cut into more clips than it has decoders, the
        // next cuts get them.
        found.sort_by_key(|(at, _, _)| *at);
        found.into_iter().map(|(_, media, source)| (media, source)).collect()
    }

    pub(crate) fn split_at_playhead(&mut self) {
        let t = self.playhead;
        let under: Vec<ItemId> = self.selected_clips().into_iter().filter(|id| self.editor.item(*id).is_some_and(|i| i.range.contains(t))).collect();
        if under.is_empty() {
            if let Err(e) = self.editor.split(None, t) {
                self.error = Some(format!("can't split here: {e}"));
            }
            return;
        }
        match self.editor.split_many(&under, t) {
            Ok(backs) => {
                // The front halves that weren't cut stay selected alongside the back halves.
                self.selected.extend(backs.iter().copied());
                self.selection = backs.last().copied().or(self.selection);
            }
            Err(e) => self.error = Some(format!("can't split here: {e}")),
        }
    }

    /// T: a 1 s cross dissolve on the cut nearest the playhead (on the selected clip's
    /// track, or any video track); with no cut nearby, a fade on whichever end of the
    /// selected clip is nearer.
    fn add_transition_at_playhead(&mut self) {
        let t = self.playhead;
        // Several clips: a dissolve (or fade in) at the start of each.
        if self.selected.len() > 1 {
            let mut ops = Vec::new();
            for item in self.selected_clips() {
                if let Ok(more) = oa_edit::timeline::set_transition(self.editor.doc.project(), self.editor.seq, item, oa_doc::ClipEnd::Head, "oa.transition.crossfade", Time::from_seconds(1)) {
                    ops.extend(more);
                }
            }
            if let Err(e) = self.editor.apply("Add transitions", ops) {
                self.error = Some(e.to_string());
            }
            return;
        }
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
            self.error = Some(tr("no cut near the playhead; select a clip to fade it").into());
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
            Err(oa_doc::EditError::LastSequence) => self.notify(tr("Nothing more to undo.")),
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
        if let Some(status) = &self.sources_status {
            eprintln!("[script]   sources: {status}");
        }
        if let Some(audio) = &self.audio {
            eprintln!("[script]   audio: {:.0} ms buffered, {} underruns", audio.buffered() * 1000.0, audio.underruns());
        }
    }

    fn stats_panel(&mut self, ui: &mut egui::Ui) {
        let avg: f32 = if self.frame_ms.is_empty() { 0.0 } else { self.frame_ms.iter().sum::<f32>() / self.frame_ms.len() as f32 };
        ui.label(trf("{avg} ms per frame ({0} fps)", &[("avg", &format!("{:.1}", avg)), ("0", &format!("{:.0}", if avg > 0.0 { 1000.0 / avg } else { 0.0 }))]));
        if let Some(p) = &self.preview {
            ui.label(trf("{0}x{1} rendered", &[("0", &(p.size[0]).to_string()), ("1", &(p.size[1]).to_string())]));
        }
        let s = &self.stats;
        ui.label(trf("{0} passes, {1} cache hits, {2} fused", &[("0", &(s.passes).to_string()), ("1", &(s.cache_hits).to_string()), ("2", &(s.fused_chains).to_string())]));
        ui.label(trf("pool {0} MB, cache {1} MB", &[("0", &format!("{:.0}", s.pool_bytes as f64 / 1e6)), ("1", &format!("{:.0}", s.cache_bytes as f64 / 1e6))]));
        let memory = self.memory;
        let note = match memory.pressure {
            oa_gpu::Pressure::Over => " — full, rendering smaller",
            oa_gpu::Pressure::Tight => " — trimming the cache",
            oa_gpu::Pressure::Easy => "",
        };
        ui.label(trf("GPU memory {0} of {1}{note}", &[("0", &(bytes(memory.used())).to_string()), ("1", &(bytes(memory.budget)).to_string()), ("note", (note))]));
        ui.add(egui::ProgressBar::new(memory.fraction().min(1.0)).desired_height(6.0));
        let mut budget_mb = memory.budget / (1 << 20);
        let r = ui.add(egui::Slider::new(&mut budget_mb, 256..=16384).text(tr("budget (MB)")).logarithmic(true));
        if r.changed() {
            self.settings.set_vram_budget(budget_mb);
            self.viewer_render.set_budget(budget_mb << 20);
        }
        let mut as_you_go = self.settings.save_as_you_go;
        if ui
            .checkbox(&mut as_you_go, tr("save as you go"))
            .on_hover_text(tr("Keeps the project file up to date while you edit (every 20 s, when you pause)"))
            .changed()
        {
            self.settings.set_save_as_you_go(as_you_go);
        }
        if self.memory_backoff < 1.0 {
            ui.label(egui::RichText::new(trf("preview at {0}% to fit", &[("0", &format!("{:.0}", self.memory_backoff * 100.0))])).small().weak());
        }
        if self.gpu.health.errors() > 0 {
            ui.colored_label(egui::Color32::YELLOW, format!("{} GPU errors reported", self.gpu.health.errors()));
        }
        if let Some(status) = self.sources_status.clone() {
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
                ui.label(trf("{0} ms buffered, {1} underruns", &[("0", &format!("{:.0}", audio.buffered() * 1000.0)), ("1", &(audio.underruns()).to_string())]));
                let drift = (self.playhead - audio.position()).as_seconds_f64() * 1000.0;
                ui.label(trf("clock: audio ({drift} ms)", &[("drift", &format!("{:+.0}", drift))]));
            }
            None => {
                ui.label(egui::RichText::new(tr("no sound (clock: wall time)")).weak());
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
        self.drop_in.install(frame, ctx);
        self.toasts(ctx);
        self.confirm_close(ctx);
        self.check_gpu();
        self.watch_gpu();
        self.poll_roto(ctx);
        self.advance_playback();
        if self.export.is_some() {
            self.step_export();
            let render_state = self.render_state.clone();
            self.update_export_view(render_state.as_ref());
            // The export runs on its own thread: the UI only needs to look in a few
            // times a second (drawing it every frame would take GPU time from the export).
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // A drop the timeline didn't take last frame (it landed elsewhere, or the timeline
        // wasn't showing) goes into the bin; then this frame's drops, for the timeline.
        if let Some((items, _)) = self.file_drop.take() {
            self.take_dropped(items, None);
        }
        self.file_hover = self.drop_in.hover(ctx);
        for (items, at) in self.drop_in.take(ctx) {
            self.screen = home::Screen::Editor;
            if let Some((earlier, _)) = self.file_drop.replace((items, at)) {
                self.take_dropped(earlier, None);
            }
        }
        // An effect drag the inspector didn't finish (its list went away) ends here.
        if self.effect_drag.is_some() && !ctx.input(|i| i.pointer.primary_down() || i.pointer.any_released()) {
            self.effect_drag = None;
        }
        self.shortcuts(ctx);
        // Something was copied inside the app: say so on the system clipboard too, so
        // Ctrl+V reaches the app (egui only reports it when there's text to paste).
        if let Some(note) = self.clipboard_note.take() {
            ctx.copy_text(note);
        }
        self.audition_tick();
        self.compound_still_there();
        self.sync_selection();
        self.thumbs_frame_start();
        self.font_samples.frame_start();
        self.poll_imports();
        self.poll_eyedropper();
        if !self.imports.is_empty() || self.opening.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.proxy_tick();
        if self.proxies.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // Edits made anywhere last frame reach the speakers here.
        self.sync_audio();
        self.save_as_you_go();
        if let Some(Err(e)) = self.autosave.tick(&self.editor.doc.snapshot(), self.editor.dirty(), self.editor.path.as_deref()) {
            self.messages.push(format!("autosave failed: {e}"));
        }
        self.poll_updates(ctx);
        self.settings_window(ctx);
        self.script_consent_window(ctx);
        if self.screen == home::Screen::Home {
            self.update_banner(root);
            self.deps_banner(root);
            egui::CentralPanel::default().show(root, |ui| self.home(ui));
            return;
        }
        if self.fullscreen.on {
            self.fullscreen_view(root, frame);
            return;
        }
        self.curve_window(ctx);
        self.mask_paste_modal(ctx);
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
        // The top bar: a shade darker than the panels, with room around its controls.
        let bar = egui::Frame::new().fill(root.visuals().panel_fill.lerp_to_gamma(root.visuals().extreme_bg_color, 0.45)).inner_margin(egui::Margin::symmetric(6, 5));
        egui::Panel::top("menu").frame(bar).show(root, |ui| self.menu_bar(ui));
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
        // The project's panel sizes, fitted to this window (they only apply when a
        // project opens, the layout changes or the window is resized: `epoch`). The
        // window opens small and is maximized a few frames later, so fitting only once
        // left the panels squeezed to that first size.
        let room = root.available_rect_before_wrap().size();
        if (room - self.fitted_room).length() > 1.0 {
            self.fitted_room = room;
            self.view_epoch += 1;
        }
        let epoch = self.view_epoch;
        let (fit_right, fit_media, fit_timeline) = layout::fit_panels(&self.project_view, vertical, [room.x, room.y]);
        let right_range = if vertical { 220.0..=1600.0 } else { 260.0..=560.0 };
        let right_width = fit_right;
        let right = egui::Panel::right(egui::Id::new(("right-column", epoch, vertical))).default_size(right_width).size_range(right_range.clone()).show(root, |ui| {
            if vertical {
                self.viewer_area(ui, frame);
            } else {
                self.inspector_panel(ui);
            }
        });
        let right_width = right.response.rect.width();

        let controls = egui::Panel::bottom(egui::Id::new(("controls", epoch))).resizable(true).default_size(fit_timeline).size_range(150.0..=760.0).show(root, |ui| {
            ui.add_space(style::GAP_S);
            // The tracks are as tall as the settings say.
            self.timeline_view.row_height = self.settings.track_height.min(timeline::ROW_HEIGHTS.len() - 1);
            // Where we are on the left, playback in the middle, what the selection can do
            // and the playback volume on the right.
            let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), style::ICON + 4.0), egui::Sense::hover());
            let gap = ui.spacing().item_spacing.x;
            let transport = egui::Rect::from_center_size(row.center(), egui::vec2(5.0 * style::ICON + 4.0 * gap, row.height()));
            let left = egui::Rect::from_min_max(row.min + egui::vec2(style::GAP_S, 0.0), egui::pos2(transport.left() - style::GAP_L, row.bottom()));
            let right = egui::Rect::from_min_max(egui::pos2(transport.right() + style::GAP_L, row.top()), row.max - egui::vec2(style::GAP_S, 0.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.set_clip_rect(left.intersect(ui.clip_rect()));
                ui.label(
                    egui::RichText::new(trf("{0}s / {duration}s", &[("0", &format!("{:.2}", self.playhead.as_seconds_f64())), ("duration", &format!("{:.2}", duration))]))
                        .size(style::TEXT_L),
                );
                ui.label(egui::RichText::new(trf("frame {0}", &[("0", &(rate.frame_at(self.playhead)).to_string())])).small().weak());
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(transport).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                if icons::button(ui, icons::SKIP_START, tr("Go to the start"), "Home", true).clicked() {
                    self.set_playhead(Time::ZERO);
                }
                if icons::button(ui, icons::STEP_BACK, tr("Back a frame"), "←", true).clicked() {
                    self.step(-1);
                }
                let (glyph, tip) = if self.playing { (icons::PAUSE, "Pause") } else { (icons::PLAY, "Play") };
                if icons::button(ui, glyph, tip, "Space", true).clicked() {
                    let playing = !self.playing;
                    self.set_playing(playing);
                }
                if icons::button(ui, icons::STEP_ON, tr("On a frame"), "→", true).clicked() {
                    self.step(1);
                }
                if icons::button(ui, icons::SKIP_END, tr("Go to the end"), "End", true).clicked() {
                    let end = self.editor.duration();
                    self.set_playhead(end);
                }
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.set_clip_rect(right.intersect(ui.clip_rect()));
                // Everything the selection can do, CapCut-style.
                self.action_bar(ui);
                // How loud playback is here (not part of the project or the export).
                // Both act at the output (after what's already buffered): at once, and
                // unmuting comes back at the volume it was.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut volume = self.volume;
                    let r = ui.add(egui::Slider::new(&mut volume, 0.0..=1.5).show_value(false)).on_hover_text(tr("Playback volume, only here"));
                    if (volume - self.volume).abs() > f32::EPSILON || r.changed() {
                        self.volume = volume;
                        if let Some(audio) = &self.audio {
                            audio.set_gain(volume);
                        }
                    }
                    let speaker = if self.muted || volume <= 0.001 { "🔇" } else { "🔊" };
                    if ui.small_button(speaker).on_hover_text(tr("Mute playback (only here — the project and export are unchanged)")).clicked() {
                        self.muted = !self.muted;
                        if let Some(audio) = &self.audio {
                            audio.set_muted(self.muted);
                        }
                    }
                });
            });
            ui.add_space(style::GAP_S);
            self.compound_trail_bar(ui);
            // The rest of the panel, cut in two exact pieces: the tracks scroll in the top
            // one and the bar under them fills the bottom one. Nothing is laid out past
            // them — a panel holding more than its size grows by that much every frame.
            let area = ui.available_rect_before_wrap();
            // (Its buttons are an icon tall; a little room around them.)
            let bar_height = style::ICON + 2.0 * style::GAP_S + 4.0;
            let tracks = egui::Rect::from_min_max(area.min, egui::pos2(area.right(), (area.bottom() - bar_height).max(area.top())));
            let bar = egui::Rect::from_min_max(egui::pos2(area.left(), tracks.bottom()), area.max);
            ui.scope_builder(egui::UiBuilder::new().max_rect(tracks).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                ui.set_clip_rect(tracks.intersect(ui.clip_rect()));
                egui::ScrollArea::vertical().max_height(tracks.height()).min_scrolled_height(0.0).auto_shrink([false, false]).show(ui, |ui| {
                    self.timeline(ui);
                    ui.add_space(4.0);
                });
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                ui.set_clip_rect(bar.intersect(ui.clip_rect()));
                // Two clearly different buttons: a film strip for a video track, a note
                // for an audio one, each with a small + drawn into the corner.
                for (icon, kind) in [
                    (icons::VIDEO_TRACK, oa_doc::TrackKind::Video),
                    (icons::AUDIO_TRACK, oa_doc::TrackKind::Audio),
                ] {
                    let r = icons::add_button(ui, icon, if kind == oa_doc::TrackKind::Video { tr("Add a video track") } else { tr("Add an audio track") });
                    if r.clicked()
                        && let Err(e) = self.editor.add_track(kind)
                    {
                        self.report_error(e.to_string());
                    }
                }
                // Captions, once there's sound among the selected clips.
                if self.selected_clips().into_iter().any(|id| self.is_audible(id))
                    && icons::button(ui, icons::CAPTIONS, tr("Captions: turn the speech on the timeline into captions"), "", true).clicked()
                {
                    self.open_captions();
                }
                ui.label(egui::RichText::new(tr("Wheel scrolls through time · Shift+wheel up and down · Ctrl+wheel zooms")).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(style::GAP_S);
                    if icons::flat_button(ui, icons::FIT, tr("Fit")).on_hover_text(tr("Fit the whole sequence")).clicked() {
                        self.timeline_view.fit = true;
                    }
                });
            });
        });

        let media = egui::Panel::left(egui::Id::new(("media", epoch))).default_size(fit_media).size_range(220.0..=520.0).show(root, |ui| {
            self.media_panel(ui);
        });

        egui::CentralPanel::default().show(root, |ui| {
            if vertical {
                self.inspector_panel(ui);
            } else {
                self.viewer_area(ui, frame);
            }
        });
        // (What each panel was fit to, as egui holds it: within its size range.)
        let fit = [
            fit_right.clamp(*right_range.start(), *right_range.end()),
            fit_media.clamp(220.0, 520.0),
            fit_timeline.clamp(150.0, 760.0),
        ];
        self.note_panel_sizes(ctx, [right_width, media.response.rect.width(), controls.response.rect.height()], fit);
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
