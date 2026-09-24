//! The Captions window: speech to captions on their own track.
//!
//! 1. **Set up** (once): the caption engine — faster-whisper in a private Python, see
//!    `oa_captions::engine` — isn't part of the editor. The window says what it would
//!    download and how big it is, and only fetches it when asked. It can be removed
//!    again from the same place.
//! 2. **Listen**: the timeline's sound (all of it, or the selected clips' stretch) is
//!    mixed down exactly as it plays, to a 16 kHz WAV, and transcribed with word times.
//! 3. **Shape**: the words are grouped into captions live as the rules change — words
//!    per caption, longest caption, the pause that breaks one, punctuation — and each
//!    caption can be recolored (one color per speaker, say) or retyped.
//! 4. **Style** (the right-hand column, any time): a live preview over the footage and
//!    the caption look — font, size, colors, outline, position, a rounded background,
//!    the spoken word highlighted — or a title on the timeline copied, effects and all.
//!    It's remembered for next time.
//! 5. **Add**: the captions go on a new "Captions" track above everything, as title
//!    clips that know when each of their words is spoken, and the window closes.

use crate::App;
use eframe::egui;
use oa_captions::engine::{self, Engine, Job, Msg};
use oa_captions::{Event, Mode, Word, MODELS};
use oa_doc::{schema, EffectId, EffectInstance, Item, ItemId, ItemKind, Op, TrackId, TrackKind};
use oa_params::{Gradient, ParamSource, Value};
use oa_time::{Time, TimeRange};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

/// Colors offered for captions (one per speaker, say). The first is plain white.
const PALETTE: [[f64; 4]; 6] = [
    [1.0, 1.0, 1.0, 1.0],
    [1.0, 0.85, 0.2, 1.0],
    [0.35, 0.85, 1.0, 1.0],
    [1.0, 0.45, 0.7, 1.0],
    [0.5, 0.9, 0.45, 1.0],
    [1.0, 0.6, 0.25, 1.0],
];

const LANGUAGES: [(&str, &str); 18] = [
    ("", "Detect it"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("sv", "Swedish"),
];

const BACKGROUND: &str = "oa.text.background";
/// The undo step that puts captions on the timeline (undoing it reopens the window).
pub(crate) const ADD_CAPTIONS: &str = "Add captions";
/// Width of the style column.
const STYLE_WIDTH: f32 = 420.0;

/// What a running job is doing.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Work {
    Install,
    Model,
    Listen,
}

/// A transcript, with a color per word (so colors survive regrouping).
struct Transcript {
    words: Vec<Word>,
    colors: Vec<Option<[f64; 4]>>,
    language: String,
}

pub struct CaptionsUi {
    engine: Engine,
    job: Option<(Work, Job)>,
    /// The timeline's sound being mixed down: the WAV and its length, or why not.
    render: Option<Receiver<Result<(PathBuf, f64), String>>>,
    step: String,
    step_index: usize,
    progress: Option<f32>,
    log: Vec<String>,
    error: Option<String>,
    whole_timeline: bool,
    audio: Option<PathBuf>,
    /// Where the audio starts on the timeline, and its length in seconds.
    offset: Time,
    length: f64,
    events: Vec<Event>,
    transcript: Option<Transcript>,
    /// Captions retyped by hand, by their first word (cleared when the grouping changes).
    edits: BTreeMap<usize, String>,
    /// Captions split and joined by hand.
    manual: oa_captions::Manual,
    /// The caption whose text box has the cursor, and where it is (in characters).
    cursor: Option<(usize, usize)>,
    /// Times set by hand, by the caption's first word: (start, end) in seconds from
    /// `offset`. Set when a line's timing is nudged; cleared with the grouping.
    times: BTreeMap<usize, (f64, f64)>,
    /// Words of lines taken out of the captions: they stay out through regrouping.
    removed: BTreeSet<usize>,
    /// Captions ticked for coloring.
    picked: BTreeSet<usize>,
    /// The caption the preview shows.
    focus: usize,
    confirm_remove: bool,
    usage: Option<u64>,
    /// The style preview: its texture, and what it was made from.
    preview: Option<(egui::TextureId, wgpu::Texture)>,
    preview_key: u64,
    /// The caption in focus was changed from the keyboard: scroll the list to it.
    reveal: bool,
    /// The preview is being scrubbed (dragged across).
    scrubbing: bool,
    /// A caption's text box has the keyboard (this frame).
    text_focus: bool,
    /// Undo and redo for the window's own changes (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y while
    /// it's open), and the state they're measured from.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    committed: Option<Snapshot>,
    /// The caption last typed into, and when (a run of typing is one step).
    typing: Option<usize>,
    last_change: f64,
}

impl CaptionsUi {
    fn new(whole_timeline: bool) -> Self {
        CaptionsUi {
            engine: Engine::new(Engine::default_root()),
            job: None,
            render: None,
            step: String::new(),
            step_index: 0,
            progress: None,
            log: Vec::new(),
            error: None,
            whole_timeline,
            audio: None,
            offset: Time::ZERO,
            length: 0.0,
            events: Vec::new(),
            transcript: None,
            edits: BTreeMap::new(),
            manual: oa_captions::Manual::default(),
            cursor: None,
            times: BTreeMap::new(),
            removed: BTreeSet::new(),
            picked: BTreeSet::new(),
            focus: 0,
            confirm_remove: false,
            usage: None,
            preview: None,
            preview_key: 0,
            reveal: false,
            scrubbing: false,
            text_focus: false,
            undo: Vec::new(),
            redo: Vec::new(),
            committed: None,
            typing: None,
            last_change: 0.0,
        }
    }

    fn busy(&self) -> bool {
        self.job.is_some() || self.render.is_some()
    }

    fn start(&mut self, work: Work, steps: Vec<engine::Step>) {
        self.error = None;
        self.log.clear();
        self.progress = None;
        self.step.clear();
        self.step_index = 0;
        self.events.clear();
        self.job = Some((work, engine::run(&self.engine, steps)));
    }

    /// Takes in what the worker threads have reported.
    fn poll(&mut self, prefs: &crate::settings::CaptionPrefs) {
        if let Some(rx) = &self.render {
            match rx.try_recv() {
                Ok(Ok((path, seconds))) => {
                    self.render = None;
                    self.length = seconds;
                    self.audio = Some(path.clone());
                    let steps = self.engine.transcribe_steps(&prefs.model, &path, &prefs.language);
                    self.start(Work::Listen, steps);
                }
                Ok(Err(e)) => {
                    self.render = None;
                    self.error = Some(e);
                }
                Err(TryRecvError::Disconnected) => {
                    self.render = None;
                    self.error = Some("mixing the sound down stopped unexpectedly".into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut ended = None;
        if let Some((_, job)) = &self.job {
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    Msg::Step { index, label } => {
                        self.step_index = index;
                        self.step = label;
                        self.progress = None;
                    }
                    Msg::Log(line) => {
                        self.log.push(line);
                        if self.log.len() > 200 {
                            self.log.remove(0);
                        }
                    }
                    Msg::Progress(p) => self.progress = Some(p),
                    Msg::Data(_) => {}
                    Msg::Event(e) => {
                        if let Event::Segment { end, .. } = &e
                            && self.length > 0.0
                        {
                            self.progress = Some((end / self.length).clamp(0.0, 1.0) as f32);
                        }
                        self.events.push(e);
                    }
                    Msg::Failed(e) => ended = Some(Err(e)),
                    Msg::Finished => ended = Some(Ok(())),
                }
            }
        }
        let Some(result) = ended else { return };
        let Some((work, _job)) = self.job.take() else { return };
        self.usage = None;
        if work == Work::Listen
            && let Some(audio) = self.audio.take()
        {
            let _ = std::fs::remove_file(audio);
        }
        match result {
            Err(e) => self.error = Some(e),
            Ok(()) if work == Work::Listen => {
                let words = oa_captions::words_of(&self.events);
                let language = self.events.iter().find_map(|e| match e {
                    Event::Info { language, .. } => Some(language.clone()),
                    _ => None,
                });
                if words.is_empty() {
                    self.error = Some("No speech was heard in that stretch.".into());
                } else {
                    let colors = vec![None; words.len()];
                    self.transcript = Some(Transcript { words, colors, language: language.unwrap_or_default() });
                    self.edits.clear();
                    self.removed.clear();
                    self.times.clear();
                    self.manual = oa_captions::Manual::default();
                    self.picked.clear();
                    self.focus = 0;
                    self.forget_history();
                }
            }
            Ok(()) => {}
        }
    }
}

/// What one undo step in the Captions window puts back: the hand-made changes to the
/// captions (text, splits, joins, removals, colors) and the window's choices (grouping,
/// style).
#[derive(Clone, PartialEq)]
struct Snapshot {
    edits: BTreeMap<usize, String>,
    removed: BTreeSet<usize>,
    times: BTreeMap<usize, (f64, f64)>,
    manual: oa_captions::Manual,
    colors: Vec<Option<[f64; 4]>>,
    prefs: crate::settings::CaptionPrefs,
}

impl CaptionsUi {
    fn snapshot(&self, prefs: &crate::settings::CaptionPrefs) -> Snapshot {
        Snapshot {
            edits: self.edits.clone(),
            removed: self.removed.clone(),
            times: self.times.clone(),
            manual: self.manual.clone(),
            colors: self.transcript.as_ref().map(|t| t.colors.clone()).unwrap_or_default(),
            prefs: prefs.clone(),
        }
    }

    /// Starts the history over (a new transcript: the old steps don't apply to it).
    fn forget_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.committed = None;
        self.typing = None;
    }

    /// Called once a frame, after the window is drawn: anything changed since the last
    /// step becomes a new one. Nothing is taken mid-drag (a slider makes one step when
    /// let go), and typing into one caption is one step until something else changes or
    /// there's a pause.
    fn record(&mut self, prefs: &crate::settings::CaptionPrefs, now: f64, dragging: bool) {
        let current = self.snapshot(prefs);
        let Some(before) = &self.committed else {
            self.committed = Some(current);
            return;
        };
        if *before == current || dragging {
            return;
        }
        let only_text = before.manual == current.manual && before.removed == current.removed && before.times == current.times && before.colors == current.colors && before.prefs == current.prefs;
        let changed: Vec<usize> = before.edits.keys().chain(current.edits.keys()).filter(|k| before.edits.get(k) != current.edits.get(k)).copied().collect();
        let typed_into = match changed.as_slice() {
            [k, ..] if only_text && changed.iter().all(|x| x == k) => Some(*k),
            _ => None,
        };
        let same_run = typed_into.is_some() && typed_into == self.typing && now - self.last_change < 2.0;
        if !same_run {
            self.undo.push(before.clone());
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing = typed_into;
        self.last_change = now;
        self.committed = Some(current);
    }

    /// Steps back (or forward, `redo`); returns the state to restore, the window's
    /// choices included.
    fn step_history(&mut self, redo: bool) -> Option<Snapshot> {
        let current = self.committed.clone()?;
        let target = if redo { self.redo.pop() } else { self.undo.pop() }?;
        if redo {
            self.undo.push(current);
        } else {
            self.redo.push(current);
        }
        self.edits = target.edits.clone();
        self.removed = target.removed.clone();
        self.times = target.times.clone();
        self.manual = target.manual.clone();
        if let Some(t) = &mut self.transcript
            && t.colors.len() == target.colors.len()
        {
            t.colors = target.colors.clone();
        }
        self.committed = Some(target.clone());
        self.typing = None;
        self.cursor = None;
        Some(target)
    }
}

impl Drop for CaptionsUi {
    fn drop(&mut self) {
        if let Some(audio) = self.audio.take() {
            let _ = std::fs::remove_file(audio);
        }
    }
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0);
    format!("{}:{:04.1}", (s / 60.0).floor() as u64, s % 60.0)
}

fn megabytes(bytes: u64) -> String {
    if bytes >= 1 << 30 { format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64) } else { format!("{} MB", bytes >> 20) }
}


/// Setting a caption's timing by hand: its start and end in seconds, nudged a frame at a
/// time or dragged. Moving an end past the line below (or a start past the line above)
/// takes the neighbor's edge with it, so the two stay joined at the seam — which is what
/// splitting and joining leaves crooked. "By the words" puts the transcript's own times
/// back.
fn timing_menu(ui: &mut egui::Ui, c: &mut CaptionsUi, captions: &[(oa_captions::Caption, Option<[f64; 4]>)], i: usize, rate: oa_time::FrameRate) {
    let Some((cap, _)) = captions.get(i) else { return };
    let frame = 1.0 / rate.as_f64().max(1.0);
    let (mut start, mut end) = (cap.start, cap.end);
    let at = c.offset.as_seconds_f64();
    ui.label(egui::RichText::new("Timing").strong());
    ui.label(egui::RichText::new(cap.text.chars().take(40).collect::<String>()).small().weak());
    let row = |ui: &mut egui::Ui, label: &str, value: &mut f64| -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(label);
            if ui.small_button("−").on_hover_text("A frame earlier").clicked() {
                *value -= frame;
                changed = true;
            }
            let shown = &mut (*value + at);
            let drag = ui.add(egui::DragValue::new(shown).speed(0.01).max_decimals(2).suffix(" s"));
            if drag.changed() {
                *value = *shown - at;
                changed = true;
            }
            if ui.small_button("+").on_hover_text("A frame later").clicked() {
                *value += frame;
                changed = true;
            }
        });
        changed
    };
    let moved_start = row(ui, "Start", &mut start);
    let moved_end = row(ui, "End  ", &mut end);
    if moved_start || moved_end {
        // Never inside out, never before the recording.
        start = start.max(0.0);
        end = end.max(start + frame);
        if moved_start {
            start = start.min(end - frame);
        }
        c.times.insert(cap.words.start, (start, end));
        // Keep the seams: the line above ends where this one starts, the line below
        // starts where this one ends.
        if moved_start
            && let Some((above, _)) = i.checked_sub(1).and_then(|p| captions.get(p))
            && above.end != start
        {
            c.times.insert(above.words.start, (above.start.min(start - frame), start));
        }
        if moved_end
            && let Some((below, _)) = captions.get(i + 1)
            && below.start != end
        {
            c.times.insert(below.words.start, (end, below.end.max(end + frame)));
        }
    }
    ui.horizontal(|ui| {
        if ui.small_button("By the words").on_hover_text("Back to the times the transcript gives").clicked() {
            c.times.remove(&cap.words.start);
        }
        if ui.small_button("Close the gap").on_hover_text("Start where the line above ends").clicked()
            && let Some((above, _)) = i.checked_sub(1).and_then(|p| captions.get(p))
        {
            c.times.insert(cap.words.start, (above.end, end.max(above.end + frame)));
        }
    });
}
/// A section heading inside the window.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text).strong().size(crate::style::TEXT_L));
    ui.add_space(2.0);
}

/// A rounded box grouping one part of the window.
fn panel(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(10)).corner_radius(6.0).fill(ui.visuals().faint_bg_color).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(6.0);
}

/// The window's main button.
fn primary(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).strong().color(egui::Color32::WHITE)).fill(crate::style::ACCENT).min_size(egui::vec2(0.0, 30.0));
    ui.add_enabled(enabled, button)
}

/// A progress bar: filled to `fraction`, or — when how far along isn't known — a band
/// sweeping across, so it's plain that work is going on.
fn bar(ui: &mut egui::Ui, fraction: Option<f32>, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let r = height / 2.0;
    let painter = ui.painter();
    painter.rect_filled(rect, r, ui.visuals().extreme_bg_color);
    match fraction {
        Some(f) => {
            let w = rect.width() * f.clamp(0.0, 1.0);
            if w > 0.5 {
                painter.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(w.max(height), height)), r, crate::style::ACCENT);
            }
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let band = rect.width() * 0.3;
            let x = ((t * 0.7).fract() * (rect.width() + band)) - band;
            let seg = egui::Rect::from_min_max(egui::pos2(rect.left() + x.max(0.0), rect.top()), egui::pos2((rect.left() + x + band).min(rect.right()), rect.bottom()));
            if seg.width() > 0.0 {
                painter.rect_filled(seg, r, crate::style::ACCENT);
            }
            ui.ctx().request_repaint();
        }
    }
}

/// The caption look used until one is picked: bold white with a dark outline, in the
/// lower third, sized to the canvas.
fn default_style(canvas: oa_doc::CanvasSize) -> Item {
    let mut item = Item::new(ItemId(0), "Caption", ItemKind::Text, TimeRange::new(Time::ZERO, Time::from_seconds(1)));
    let size = canvas.height.min(canvas.width) as f64 * 0.075;
    item.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(size.round())));
    item.params.set(schema::TEXT_BOLD, ParamSource::Static(Value::Bool(true)));
    item.params.set(schema::TEXT_OUTLINE, ParamSource::Static(Value::Float((size * 0.09).round())));
    item.params.set(schema::POSITION, ParamSource::Static(Value::Vec2([0.0, 0.3])));
    item
}

fn value(style: &Item, id: &str) -> Option<Value> {
    style.params.get(id).map(|s| s.eval(&oa_params::EvalContext::at(Time::ZERO, Time::ZERO)))
}

fn text_default(id: &str) -> Value {
    schema::text().iter().find(|s| s.id.as_str() == id).map_or(Value::Float(0.0), |s| s.default.clone())
}

fn float(style: &Item, id: &str) -> f64 {
    value(style, id).and_then(|v| v.as_float()).unwrap_or_else(|| text_default(id).as_float().unwrap_or(0.0))
}

fn color(style: &Item, id: &str) -> [f64; 4] {
    match value(style, id).unwrap_or_else(|| text_default(id)) {
        Value::Gradient(g) => g.sorted_stops()[0].color,
        Value::Color(c) => c,
        _ => [1.0; 4],
    }
}

fn set(style: &mut Item, id: &str, v: Value) {
    style.params.set(id, ParamSource::Static(v));
}

impl App {
    /// The canvas of the format being viewed.
    fn viewed_canvas(&self) -> oa_doc::CanvasSize {
        let s = self.editor.sequence();
        s.variants[self.variant.min(s.variants.len() - 1)].size
    }

    /// Opens the Captions window (for the selected clips' stretch when some are selected).
    pub(crate) fn open_captions(&mut self) {
        if self.captions.is_none() {
            self.captions = Some(Box::new(CaptionsUi::new(self.selected.is_empty())));
        }
    }

    pub(crate) fn captions_window(&mut self, ctx: &egui::Context) {
        let Some(mut c) = self.captions.take() else { return };
        let prefs = self.settings.captions.clone();
        // While the window is open, undo and redo are its own (typing in a caption
        // included, so a split and the typing around it undo in order). Not while
        // typing in some other window's field.
        if !ctx.text_edit_focused() || c.text_focus {
            use egui::{Key, Modifiers as M};
            let redo = ctx.input_mut(|i| i.consume_key(M::COMMAND | M::SHIFT, Key::Z) || i.consume_key(M::COMMAND, Key::Y));
            let undo = !redo && ctx.input_mut(|i| i.consume_key(M::COMMAND, Key::Z));
            if (undo || redo)
                && let Some(s) = c.step_history(redo)
            {
                self.settings.captions = s.prefs;
            }
        }
        c.text_focus = false;
        c.poll(&prefs);
        if c.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut open = true;
        let mut close = false;
        let setting_up = !c.engine.is_installed() || c.job.as_ref().is_some_and(|(w, _)| *w == Work::Install);
        let window = egui::Window::new("Captions").open(&mut open).collapsible(false).resizable(true);
        let window = if setting_up { window.default_size([560.0, 480.0]) } else { window.default_size([1120.0, 720.0]).min_width(860.0) };
        window.show(ctx, |ui| {
            if setting_up {
                self.captions_setup(ui, &mut c);
                return;
            }
            // Two columns of fixed widths, each scrolling on its own. (Panels inside a
            // window fed their sizes back into the window's, and the left column ran
            // under the right.)
            let gap = 16.0;
            let height = ui.available_height();
            let left = (ui.available_width() - STYLE_WIDTH - gap).max(300.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(egui::vec2(left, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(left);
                    egui::ScrollArea::vertical().id_salt("caption-main-scroll").auto_shrink([false, false]).max_height(height).show(ui, |ui| {
                        ui.set_width(left - 12.0);
                        if c.transcript.is_none() {
                            self.captions_listen(ui, &mut c);
                        } else {
                            self.captions_review(ui, &mut c, &mut close);
                        }
                        Self::captions_progress(ui, &mut c);
                        self.caption_engine_info(ui, &mut c);
                    });
                });
                ui.add_space(gap);
                ui.allocate_ui_with_layout(egui::vec2(STYLE_WIDTH, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(STYLE_WIDTH);
                    egui::ScrollArea::vertical().id_salt("caption-style-scroll").auto_shrink([false, false]).max_height(height).show(ui, |ui| {
                        ui.set_width(STYLE_WIDTH - 12.0);
                        self.caption_style(ui, &mut c);
                    });
                });
            });
        });
        c.record(&self.settings.captions, ctx.input(|i| i.time), ctx.input(|i| i.pointer.any_down()));
        // The choices are remembered for next time (once a drag on them has finished).
        if self.settings.captions != prefs && !ctx.input(|i| i.pointer.any_down()) {
            self.settings.save();
        }
        if open && !close {
            self.captions = Some(c);
            return;
        }
        if let (Some((id, _)), Some(rs)) = (c.preview.take(), &self.render_state) {
            rs.renderer.write().free_texture(&id);
        }
        // Closed by adding the captions: kept, so undoing that brings the window back
        // just as it was (`reopen_captions`). Closed any other way, it's gone — and
        // whatever was running stops (the job stops when it's dropped).
        if close {
            self.captions_added = Some(c);
        }
    }

    /// After an undo of "Add captions": the window comes back as it was left.
    pub(crate) fn reopen_captions(&mut self) {
        if self.captions.is_none()
            && let Some(c) = self.captions_added.take()
        {
            self.captions = Some(c);
            self.notify("The captions came off the timeline — the Captions window is back as you left it.");
        }
    }

    /// After a redo of "Add captions": they're back on the timeline, so the window goes.
    pub(crate) fn put_captions_away(&mut self) {
        if let Some(mut c) = self.captions.take() {
            if let (Some((id, _)), Some(rs)) = (c.preview.take(), &self.render_state) {
                rs.renderer.write().free_texture(&id);
            }
            self.captions_added = Some(c);
        }
    }

    /// Before the engine is there: what it is, what it costs, and the button to get it.
    fn captions_setup(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), rect, crate::icons::CAPTIONS, crate::style::ACCENT);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Captions from speech").strong().size(crate::style::TITLE));
                ui.label(egui::RichText::new("Made on this computer by faster-whisper, an open-source speech recognizer.").weak());
            });
        });
        ui.add_space(8.0);
        if c.job.is_none() {
            panel(ui, |ui| {
                ui.label("It isn't included with OpenAtelier, to keep the editor small for people who don't need it. Pick a model and it's downloaded once, then works offline.");
                ui.add_space(6.0);
                let prefs = &mut self.settings.captions;
                for m in MODELS {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut prefs.model, m.name.to_string(), egui::RichText::new(m.name).strong());
                        ui.label(egui::RichText::new(format!("~{} MB", m.size_mb)).monospace().weak());
                        ui.label(egui::RichText::new(m.about).weak());
                    });
                }
            });
            let chosen = oa_captions::model(&self.settings.captions.model).copied().unwrap_or(MODELS[2]);
            ui.label(egui::RichText::new(format!(
                "About {} MB in all: uv (a small Python installer), Python, faster-whisper and its libraries, and the {} model. It all goes in one folder — nothing is installed anywhere else, and Remove deletes it:",
                200 + chosen.size_mb,
                chosen.name
            )).small());
            ui.label(egui::RichText::new(c.engine.root().display().to_string()).monospace().small().weak());
            ui.add_space(8.0);
            if primary(ui, true, &format!("Download and set up  (~{} MB)", 200 + chosen.size_mb)).clicked() {
                let steps = c.engine.install_steps(chosen.name);
                c.start(Work::Install, steps);
            }
        }
        Self::captions_progress(ui, c);
    }

    /// A running job's progress (overall and this step), its log, and Cancel; or the
    /// last error.
    fn captions_progress(ui: &mut egui::Ui, c: &mut CaptionsUi) {
        if let Some((work, job)) = &c.job {
            let total = job.steps.max(1);
            panel(ui, |ui| {
                let what = if c.step.is_empty() { "Starting…".to_string() } else { c.step.clone() };
                match work {
                    Work::Listen => {
                        ui.label(egui::RichText::new(format!("{what}…")).strong());
                        bar(ui, c.progress.filter(|p| *p > 0.0), 10.0);
                        ui.label(egui::RichText::new(if c.progress.is_some() { "Transcribing: captions arrive as it goes." } else { "Loading the model…" }).small().weak());
                    }
                    _ => {
                        // Overall, counting a step's own progress when it reports one.
                        let overall = (c.step_index as f32 + c.progress.unwrap_or(0.0)) / total as f32;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(what).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new(format!("step {} of {total}", c.step_index + 1)).weak());
                            });
                        });
                        bar(ui, Some(overall), 10.0);
                        ui.add_space(4.0);
                        bar(ui, c.progress, 4.0);
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        job.cancel();
                    }
                    egui::CollapsingHeader::new("Details").id_salt("caption-job-log").default_open(false).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                            for line in &c.log {
                                ui.label(egui::RichText::new(line).monospace().small().weak());
                            }
                        });
                    });
                });
            });
        } else if c.render.is_some() {
            panel(ui, |ui| {
                ui.label(egui::RichText::new("Mixing the sound down…").strong());
                bar(ui, None, 10.0);
            });
        }
        if let Some(e) = &c.error {
            panel(ui, |ui| {
                ui.colored_label(crate::style::ERROR, format!("⚠ {e}"));
                if !c.log.is_empty() {
                    egui::CollapsingHeader::new("What the tools said").id_salt("caption-error-log").show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                            for line in c.log.iter().rev().take(40).rev() {
                                ui.label(egui::RichText::new(line).monospace().small().weak());
                            }
                        });
                    });
                }
            });
        }
    }

    /// The engine's size on disk, and removing it.
    fn caption_engine_info(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi) {
        ui.add_space(8.0);
        let usage = *c.usage.get_or_insert_with(|| c.engine.disk_usage());
        egui::CollapsingHeader::new(egui::RichText::new(format!("Caption engine · {} on disk", megabytes(usage))).weak()).id_salt("caption-engine").show(ui, |ui| {
            ui.label(egui::RichText::new(c.engine.root().display().to_string()).monospace().small());
            if !c.confirm_remove {
                if ui.add_enabled(!c.busy(), egui::Button::new("Remove engine and models…")).clicked() {
                    c.confirm_remove = true;
                }
            } else {
                ui.label("Delete the caption engine and every downloaded model? Captions already on timelines stay.");
                ui.horizontal(|ui| {
                    if ui.button("Remove").clicked() {
                        match c.engine.remove() {
                            Ok(()) => self.notify("Removed the caption engine."),
                            Err(e) => c.error = Some(format!("couldn't remove it all: {e}")),
                        }
                        c.confirm_remove = false;
                        c.usage = None;
                        c.transcript = None;
                    }
                    if ui.button("Keep it").clicked() {
                        c.confirm_remove = false;
                    }
                });
            }
        });
    }

    /// Model, language, what to listen to, and Generate.
    fn captions_listen(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi) {
        let busy = c.busy();
        let has_model = c.engine.has_model(&self.settings.captions.model);
        heading(ui, "Listen");
        ui.add_enabled_ui(!busy, |ui| {
            panel(ui, |ui| {
                egui::Grid::new("caption-listen").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    let prefs = &mut self.settings.captions;
                    ui.label("Listen to");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut c.whole_timeline, true, "The whole timeline");
                        ui.add_enabled_ui(!self.selected.is_empty(), |ui| {
                            ui.radio_value(&mut c.whole_timeline, false, "The selected clips' stretch").on_disabled_hover_text("Select clips on the timeline first");
                        });
                    });
                    ui.end_row();
                    ui.label("Language");
                    let current = LANGUAGES.iter().find(|l| l.0 == prefs.language).map_or(prefs.language.clone(), |l| l.1.to_string());
                    egui::ComboBox::from_id_salt("caption-language").selected_text(current).width(200.0).show_ui(ui, |ui| {
                        for (code, name) in LANGUAGES {
                            ui.selectable_value(&mut prefs.language, code.to_string(), name);
                        }
                    });
                    ui.end_row();
                    ui.label("Model");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("caption-model").selected_text(prefs.model.clone()).width(200.0).show_ui(ui, |ui| {
                            for m in MODELS {
                                let have = c.engine.has_model(m.name);
                                let label = format!("{}{}  ~{} MB — {}", if have { "✓ " } else { "" }, m.name, m.size_mb, m.about);
                                ui.selectable_value(&mut prefs.model, m.name.to_string(), label);
                            }
                        });
                        if !has_model {
                            let size = oa_captions::model(&prefs.model).map_or(0, |m| m.size_mb);
                            if ui.button(format!("Download (~{size} MB)")).clicked() {
                                let steps = c.engine.model_steps(&prefs.model);
                                c.start(Work::Model, steps);
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Clean up");
                    ui.checkbox(&mut prefs.enhance_voice, "Enhance voices and reduce background noise first")
                        .on_hover_text("Before listening: cuts rumble and steady noise (hiss, hum, fans), lifts speech clarity and evens out loud and quiet speakers. Helps with noisy or quiet recordings; your timeline's sound isn't changed.");
                    ui.end_row();
                });
            });
            ui.add_space(4.0);
            if primary(ui, has_model && !busy, "Generate captions").on_disabled_hover_text("Download the model first").clicked() {
                self.start_listening(c);
            }
        });
    }

    /// Mixes the chosen stretch of the timeline's sound down to a WAV on a worker
    /// thread; transcription starts when it's written (see `CaptionsUi::poll`).
    fn start_listening(&mut self, c: &mut CaptionsUi) {
        let seq = self.editor.seq;
        let mut clips = self.audio_clips_of(seq);
        if self.settings.captions.enhance_voice {
            let cleanup = oa_audio::fx::voice_cleanup();
            for clip in &mut clips {
                clip.effects.extend(cleanup.iter().cloned());
            }
        }
        if clips.is_empty() {
            c.error = Some("There's no sound on this timeline to listen to.".into());
            return;
        }
        let range = if c.whole_timeline || self.selected.is_empty() {
            Some((Time::ZERO, self.editor.duration()))
        } else {
            let items: Vec<_> = self.selected.iter().filter_map(|id| self.editor.item(*id)).map(|i| i.range).collect();
            items.iter().map(|r| r.start).min().zip(items.iter().map(|r| r.end()).max())
        };
        let Some((start, end)) = range.filter(|(a, b)| b > a) else {
            c.error = Some("That stretch is empty.".into());
            return;
        };
        c.offset = start;
        c.error = None;
        c.transcript = None;
        let path = std::env::temp_dir().join(format!("oa-captions-{}-{}.wav", std::process::id(), start.0.unsigned_abs()));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // 16 kHz mono: what Whisper listens at.
            let format = oa_audio::AudioFormat { sample_rate: 16_000, channels: 1 };
            let mut mix = oa_audio::TimelineAudio::new(clips, format, start, end);
            let result = oa_export::write_wav(&path, &mut mix, end - start).map(|secs| (path, secs)).map_err(|e| format!("couldn't mix the sound down: {e}"));
            let _ = tx.send(result);
        });
        c.render = Some(rx);
    }

    /// The captions as grouped now, with their colors and any hand-typed text.
    fn current_captions(&self, c: &CaptionsUi) -> Vec<(oa_captions::Caption, Option<[f64; 4]>)> {
        let Some(t) = &c.transcript else { return Vec::new() };
        oa_captions::group_with(&t.words, &self.settings.captions.grouping, &c.manual)
            .into_iter()
            // Lines taken out (by their words, so splitting or joining keeps them out).
            .filter(|cap| !cap.words.clone().all(|w| c.removed.contains(&w)))
            .map(|mut cap| {
                // Times set by hand win over the ones the words give.
                if let Some((start, end)) = c.times.get(&cap.words.start) {
                    (cap.start, cap.end) = (*start, *end);
                }
                if let Some(text) = c.edits.get(&cap.words.start) {
                    cap.text = text.clone();
                }
                let color = t.colors.get(cap.words.start).copied().flatten();
                (cap, color)
            })
            .collect()
    }

    /// Grouping rules, then the captions (colors, text), then Add to timeline.
    fn captions_review(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi, close: &mut bool) {
        let before = self.settings.captions.grouping.clone();
        let language = c.transcript.as_ref().map_or(String::new(), |t| t.language.clone());
        let word_count = c.transcript.as_ref().map_or(0, |t| t.words.len());
        let rate = self.editor.sequence().rate;

        heading(ui, "Group");
        panel(ui, |ui| {
            let g = &mut self.settings.captions.grouping;
            ui.horizontal_wrapped(|ui| {
                for (mode, name, tip) in [
                    (Mode::Phrases, "Phrases", "Break at commas and sentence ends, and at the limits below"),
                    (Mode::Sentences, "Sentences", "Break at sentence ends, and at the limits below"),
                    (Mode::Words, "By count", "Break only at the limits below"),
                    (Mode::Single, "One word", "One word at a time"),
                ] {
                    ui.selectable_value(&mut g.mode, mode, name).on_hover_text(tip);
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("caption-rules").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Max words");
                ui.add_enabled(g.mode != Mode::Single, egui::DragValue::new(&mut g.max_words).range(1..=30));
                ui.label("Max length");
                ui.add(egui::DragValue::new(&mut g.max_seconds).range(0.3..=15.0).speed(0.05).max_decimals(2).suffix(" s"));
                ui.end_row();
                ui.label("Pause breaks after").on_hover_text("A pause this long ends a caption; shorter gaps between captions are closed");
                ui.add(egui::DragValue::new(&mut g.silence).range(0.05..=5.0).speed(0.02).max_decimals(2).suffix(" s"));
                ui.label("Shortest");
                ui.add(egui::DragValue::new(&mut g.min_seconds).range(0.0..=3.0).speed(0.02).max_decimals(2).suffix(" s"));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut g.uppercase, "UPPERCASE");
                ui.checkbox(&mut g.strip_punctuation, "No commas or periods");
            });
        });
        if self.settings.captions.grouping != before {
            // Hand-typed text and hand-set times belong to the old grouping.
            c.edits.clear();
            c.times.clear();
            c.picked.clear();
        }

        let captions = self.current_captions(c);
        c.focus = c.focus.min(captions.len().saturating_sub(1));
        // Left and right (when no caption is being typed in): the caption before or after,
        // shown in the preview, the sound moved to it, the list scrolled to it.
        if !ui.ctx().text_edit_focused() && !captions.is_empty() {
            let step = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) as isize - i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) as isize);
            let to = (c.focus as isize + step).clamp(0, captions.len() as isize - 1) as usize;
            if step != 0 && to != c.focus {
                c.focus = to;
                c.reveal = true;
                self.set_playhead(c.offset + Time::from_seconds_f64(captions[to].0.start));
            }
        }
        ui.horizontal(|ui| {
            heading(ui, &format!("{} captions", captions.len()));
            ui.label(egui::RichText::new(format!("from {word_count} words{}", if language.is_empty() { String::new() } else { format!(" · {language}") })).weak());
            // Lines taken out: how many, and a way back without reaching for undo.
            if !c.removed.is_empty() {
                let gone = c.transcript.as_ref().map_or(0, |t| {
                    oa_captions::group_with(&t.words, &self.settings.captions.grouping, &c.manual).iter().filter(|cap| cap.words.clone().all(|w| c.removed.contains(&w))).count()
                });
                ui.label(egui::RichText::new(format!("· {gone} removed")).weak());
                if ui.small_button("Restore").on_hover_text("Put every removed line back").clicked() {
                    c.removed.clear();
                }
            }
        });
        panel(ui, |ui| {
            // Coloring: tick captions, then pick a color (one per speaker works well).
            ui.horizontal(|ui| {
                let n = c.picked.len();
                ui.label(egui::RichText::new(if n == 0 { "Tick captions, then pick their color:".to_string() } else { format!("Color the {n} ticked:") }).weak());
                for color in PALETTE {
                    let (rect, r) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                    ui.painter().circle_filled(rect.center(), 8.0, crate::widgets::color32(color));
                    ui.painter().circle_stroke(rect.center(), 8.0, egui::Stroke::new(1.0, egui::Color32::from_gray(90)));
                    if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                        && let Some(t) = &mut c.transcript
                    {
                        for &i in &c.picked {
                            if let Some((cap, _)) = captions.get(i) {
                                for w in cap.words.clone() {
                                    t.colors[w] = if color == PALETTE[0] { None } else { Some(color) };
                                }
                            }
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("None").clicked() {
                        c.picked.clear();
                    }
                    if ui.small_button("All").clicked() {
                        c.picked = (0..captions.len()).collect();
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("caption-list").max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                // Splits and joins wait until the list is drawn (they regroup it).
                let mut split: Option<usize> = None;
                let mut join: Option<usize> = None;
                let mut remove: Option<usize> = None;
                // One caption per key press (the next row sees the same press).
                let mut moved = false;
                // Where the caret goes after a split or a join: (caption's first word, character).
                let mut refocus: Option<(usize, usize)> = None;
                for (i, (cap, color)) in captions.iter().enumerate() {
                    let focused = c.focus == i;
                    egui::Frame::NONE
                        .fill(if focused { crate::style::ACCENT.gamma_multiply(0.18) } else { egui::Color32::TRANSPARENT })
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(4, 2))
                        .show(ui, |ui| {
                            if focused && c.reveal {
                                ui.scroll_to_cursor(Some(egui::Align::Center));
                                c.reveal = false;
                            }
                            ui.horizontal(|ui| {
                                let mut ticked = c.picked.contains(&i);
                                if ui.checkbox(&mut ticked, "").changed() {
                                    if ticked {
                                        c.picked.insert(i);
                                    } else {
                                        c.picked.remove(&i);
                                    }
                                }
                                let at = c.offset.as_seconds_f64();
                                let by_hand = c.times.contains_key(&cap.words.start);
                                let shown = egui::RichText::new(format!("{}–{}", clock(at + cap.start), clock(at + cap.end))).monospace().small();
                                let time = ui.add(egui::Label::new(if by_hand { shown.color(crate::style::GOLD) } else { shown.weak() }).sense(egui::Sense::click()));
                                if time.clicked() {
                                    c.focus = i;
                                }
                                let time = time.on_hover_text("Click: show it in the preview · right-click: set its timing");
                                crate::widgets::context_menu(&time, |ui| {
                                    c.focus = i;
                                    timing_menu(ui, c, &captions, i, rate);
                                });
                                let mut swatch = color.unwrap_or(PALETTE[0]);
                                let (_, changed) = crate::widgets::color_swatch(ui, &mut swatch);
                                if changed && let Some(t) = &mut c.transcript {
                                    for w in cap.words.clone() {
                                        t.colors[w] = Some(swatch);
                                    }
                                }
                                // The buttons first (right to left), then the text fills the rest.
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let last = i + 1 == captions.len();
                                    if crate::icons::button(ui, crate::icons::DELETE, "Remove this line (Ctrl+Z puts it back)", "", true).clicked() {
                                        remove = Some(i);
                                    }
                                    if crate::icons::button(ui, crate::icons::GROUP, "Join with the caption below", "", !last).clicked() {
                                        join = Some(i);
                                    }
                                    let can_split = cap.words.len() > 1;
                                    if crate::icons::button(ui, crate::icons::SPLIT, "Split at the text cursor (or press Enter while typing)", "", can_split).clicked() {
                                        split = Some(i);
                                    }
                                    let mut text = cap.text.clone();
                                    let r = ui.add(
                                        egui::TextEdit::singleline(&mut text)
                                            .id(egui::Id::new(("caption-text", cap.words.start)))
                                            .desired_width(f32::INFINITY)
                                            .text_color(crate::widgets::color32(color.unwrap_or(PALETTE[0]))),
                                    );
                                    if r.gained_focus() {
                                        c.focus = i;
                                    }
                                    // Where the cursor was before this frame's keys.
                                    let was = c.cursor.filter(|(ci, _)| *ci == i).map(|(_, at)| at);
                                    if r.has_focus() {
                                        c.text_focus = true;
                                        if let Some(range) = egui::TextEdit::load_state(ui.ctx(), r.id).and_then(|s| s.cursor.char_range()) {
                                            c.cursor = Some((i, range.primary.index.into()));
                                        }
                                    }
                                    if r.changed() {
                                        c.edits.insert(cap.words.start, text.clone());
                                    }
                                    // Backspace at the very start joins this caption to the one
                                    // above; Delete at the very end joins the one below to it.
                                    if r.has_focus() && !moved {
                                        let (back, delete) = ui.input(|i| (i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::Delete)));
                                        let length = cap.text.chars().count();
                                        if back && was == Some(0) && i > 0 {
                                            let above = &captions[i - 1].0;
                                            join = Some(i - 1);
                                            refocus = Some((above.words.start, above.text.trim_end().chars().count() + 1));
                                            moved = true;
                                        } else if delete && was == Some(length) && !last {
                                            join = Some(i);
                                            refocus = Some((cap.words.start, cap.text.trim_end().chars().count()));
                                            moved = true;
                                        }
                                    }
                                    // Up and down go to the caption above or below.
                                    if r.has_focus() && !moved {
                                        let step = ui.input(|i| i.key_pressed(egui::Key::ArrowDown) as isize - i.key_pressed(egui::Key::ArrowUp) as isize);
                                        if let Some((next, _)) = (step != 0).then(|| captions.get(i.wrapping_add_signed(step))).flatten() {
                                            ui.memory_mut(|m| m.request_focus(egui::Id::new(("caption-text", next.words.start))));
                                            c.focus = i.wrapping_add_signed(step);
                                            moved = true;
                                        }
                                    }
                                    // Left at the very start and right at the very end carry on
                                    // into the caption before or after.
                                    if r.has_focus() && !moved {
                                        let (left, right) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
                                        let length = cap.text.chars().count();
                                        if left && was == Some(0) && i > 0 {
                                            let above = &captions[i - 1].0;
                                            refocus = Some((above.words.start, above.text.chars().count()));
                                            c.focus = i - 1;
                                            moved = true;
                                        } else if right && was == Some(length) && i + 1 < captions.len() {
                                            refocus = Some((captions[i + 1].0.words.start, 0));
                                            c.focus = i + 1;
                                            moved = true;
                                        }
                                    }
                                    // Enter splits where the cursor is.
                                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_split {
                                        split = Some(i);
                                    }
                                });
                            });
                        });
                }
                if let Some(i) = split
                    && let Some((cap, _)) = captions.get(i)
                {
                    // The word the cursor is in (or before) starts the new caption; without
                    // a cursor in this caption, split it in the middle.
                    let words: Vec<usize> = c.transcript.as_ref().map_or(Vec::new(), |t| cap.words.clone().filter(|w| !t.words[*w].text.trim().is_empty()).collect());
                    let cursor = c.cursor.filter(|(ci, _)| *ci == i).map(|(_, chars)| cap.text.char_indices().nth(chars).map_or(cap.text.len(), |(b, _)| b));
                    let at = match cursor {
                        Some(byte) => {
                            let before = &cap.text[..byte];
                            let mut n = before.split_whitespace().count();
                            // In the middle of a word: that word moves down.
                            if !before.is_empty() && !before.ends_with(char::is_whitespace) && cap.text[byte..].starts_with(|ch: char| !ch.is_whitespace()) {
                                n = n.saturating_sub(1);
                            }
                            n
                        }
                        None => words.len() / 2,
                    };
                    let at = at.clamp(1, words.len().saturating_sub(1).max(1));
                    if let Some(&word) = words.get(at) {
                        c.manual.split(word);
                        // Hand-typed text is cut where the cursor was, not thrown away.
                        match (c.edits.remove(&cap.words.start), cursor) {
                            (Some(typed), Some(byte)) if byte <= typed.len() && typed.is_char_boundary(byte) => {
                                c.edits.insert(cap.words.start, typed[..byte].trim_end().to_string());
                                c.edits.insert(word, typed[byte..].trim_start().to_string());
                            }
                            _ => {}
                        }
                        c.cursor = None;
                        // Carry on typing at the start of the new caption.
                        refocus = Some((word, 0));
                    }
                }
                // Taking a line out: by its words, so it stays out if the rest is
                // regrouped, and the caret goes to the line that takes its place.
                if let Some(i) = remove
                    && let Some((cap, _)) = captions.get(i)
                {
                    c.removed.extend(cap.words.clone());
                    c.edits.remove(&cap.words.start);
                    c.picked.remove(&i);
                    c.cursor = None;
                    c.focus = i.min(captions.len().saturating_sub(2));
                    if let Some((next, _)) = captions.get(i + 1).or(i.checked_sub(1).and_then(|p| captions.get(p))) {
                        refocus = Some((next.words.start, 0));
                    }
                }
                if let Some(i) = join
                    && let (Some((a, _)), Some((b, _))) = (captions.get(i), captions.get(i + 1))
                {
                    let typed = c.edits.contains_key(&a.words.start) || c.edits.contains_key(&b.words.start);
                    c.manual.join(b.words.start);
                    c.edits.remove(&b.words.start);
                    if typed {
                        c.edits.insert(a.words.start, format!("{} {}", a.text.trim_end(), b.text.trim_start()));
                    } else {
                        c.edits.remove(&a.words.start);
                    }
                    c.cursor = None;
                }
                // The caret goes where the text it was in went.
                if let Some((first_word, chars)) = refocus {
                    let id = egui::Id::new(("caption-text", first_word));
                    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(chars))));
                    state.store(ui.ctx(), id);
                    ui.memory_mut(|m| m.request_focus(id));
                }
            });
        });
        ui.horizontal(|ui| {
            if primary(ui, !captions.is_empty(), &format!("Add {} captions to the timeline", captions.len())).clicked() {
                match self.add_captions(c.offset, &captions, c.transcript.as_ref().map(|t| t.words.as_slice()).unwrap_or(&[])) {
                    Ok(n) => {
                        self.notify(format!("Added {n} captions on a new Captions track."));
                        *close = true;
                    }
                    Err(e) => c.error = Some(e),
                }
            }
            if ui.button("Listen again").on_hover_text("Discard these captions and transcribe again (another model or language)").clicked() {
                c.transcript = None;
                c.manual = oa_captions::Manual::default();
                c.edits.clear();
                c.times.clear();
                c.removed.clear();
                c.picked.clear();
            }
        });
    }

    /// The caption style: a live preview, a title to copy, and the look.
    fn caption_style(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi) {
        let canvas = self.viewed_canvas();
        let mut style = self.settings.captions.style.clone().unwrap_or_else(|| default_style(canvas));
        let before = style.clone();
        heading(ui, "Style");
        self.caption_preview(ui, c, &style);

        // Copy a title that's already in the project: its whole look, effects included.
        let titles: Vec<(ItemId, String)> = self
            .editor
            .sequence()
            .tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|i| i.kind == ItemKind::Text)
            .map(|i| {
                let words = i.params.get(schema::TEXT_CONTENT).map(|p| p.eval(&i.eval_context(i.range.start)));
                let words = words.as_ref().and_then(|v| v.as_text()).unwrap_or("Title").lines().next().unwrap_or_default().to_string();
                (i.id, format!("{words}  ({:.1}s)", i.range.start.as_seconds_f64()))
            })
            .collect();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("caption-copy-title").selected_text("Copy a title's look…").width(210.0).show_ui(ui, |ui| {
                if titles.is_empty() {
                    ui.label(egui::RichText::new("No titles on this timeline yet").weak());
                }
                for (id, name) in &titles {
                    if ui.selectable_label(false, name).on_hover_text("Its font, size, colors, outline, position, keyframes and effects").clicked()
                        && let Some(src) = self.editor.item(*id)
                    {
                        style = src.clone();
                        style.transition_in = None;
                        style.transition_out = None;
                        style.group = None;
                        style.word_times.clear();
                    }
                }
            });
            if ui.button("Reset").on_hover_text("The default caption look").clicked() {
                style = default_style(canvas);
            }
        });

        panel(ui, |ui| {
            egui::Grid::new("caption-look").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Font");
                ui.horizontal(|ui| {
                    let family = value(&style, schema::TEXT_FONT).and_then(|v| v.as_text().map(str::to_string)).unwrap_or_default();
                    if let Some(f) = self.font_menu(ui, egui::Id::new("caption-font"), &family) {
                        set(&mut style, schema::TEXT_FONT, Value::Text(f));
                    }
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    for (id, label) in [(schema::TEXT_BOLD, egui::RichText::new("B").strong()), (schema::TEXT_ITALIC, egui::RichText::new("I").italics())] {
                        let on = matches!(value(&style, id), Some(Value::Bool(true)));
                        if ui.selectable_label(on, label).clicked() {
                            set(&mut style, id, Value::Bool(!on));
                        }
                    }
                    let mut size = float(&style, schema::TEXT_SIZE);
                    if ui.add(egui::DragValue::new(&mut size).range(8.0..=600.0).speed(0.5).suffix(" px")).changed() {
                        set(&mut style, schema::TEXT_SIZE, Value::Float(size));
                    }
                });
                ui.end_row();
                ui.label("Color");
                let mut fill = color(&style, schema::TEXT_COLOR);
                if crate::widgets::color_swatch(ui, &mut fill).1 {
                    set(&mut style, schema::TEXT_COLOR, Value::Gradient(Gradient::solid(fill)));
                }
                ui.end_row();
                ui.label("Outline");
                ui.horizontal(|ui| {
                    let mut width = float(&style, schema::TEXT_OUTLINE);
                    if ui.add(egui::Slider::new(&mut width, 0.0..=30.0).show_value(true).max_decimals(0)).changed() {
                        set(&mut style, schema::TEXT_OUTLINE, Value::Float(width));
                    }
                    let mut edge = color(&style, schema::TEXT_OUTLINE_COLOR);
                    if crate::widgets::color_swatch(ui, &mut edge).1 {
                        set(&mut style, schema::TEXT_OUTLINE_COLOR, Value::Gradient(Gradient::solid(edge)));
                    }
                });
                ui.end_row();
                ui.label("Height");
                let mut pos = value(&style, schema::POSITION).and_then(|v| v.as_vec2()).unwrap_or([0.0, 0.3]);
                let mut pct = pos[1] * 100.0;
                if ui.add(egui::Slider::new(&mut pct, -45.0..=45.0).suffix(" %").max_decimals(0)).on_hover_text("Up or down from the middle of the frame").changed() {
                    pos[1] = pct / 100.0;
                    set(&mut style, schema::POSITION, Value::Vec2(pos));
                }
                ui.end_row();
            });
        });

        // A rounded box behind the text.
        let bg = style.effects.iter().position(|e| e.type_id == BACKGROUND && e.role == oa_doc::EffectRole::Passive);
        panel(ui, |ui| {
            let mut on = bg.is_some();
            if ui.checkbox(&mut on, egui::RichText::new("Background").strong()).on_hover_text("A rounded box behind the text").changed() {
                if on {
                    let mut fx = EffectInstance::new(EffectId(1), BACKGROUND);
                    fx.params.set("shape", ParamSource::Static(Value::Enum("lines".into())));
                    style.effects.push(fx);
                } else if let Some(i) = bg {
                    style.effects.remove(i);
                }
            }
            if let Some(i) = style.effects.iter().position(|e| e.type_id == BACKGROUND && e.role == oa_doc::EffectRole::Passive) {
                let fx = &mut style.effects[i];
                let get = |fx: &EffectInstance, id: &str| fx.params.get(id).map(|s| s.eval(&oa_params::EvalContext::at(Time::ZERO, Time::ZERO)));
                egui::Grid::new("caption-background").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Shape");
                    ui.horizontal(|ui| {
                        let shape = get(fx, "shape").and_then(|v| v.as_enum().map(str::to_string)).unwrap_or_else(|| "lines".into());
                        for (s, name, tip) in [("text", "Whole", "One box behind all the text"), ("lines", "Each line", "A box behind each line"), ("words", "Each word", "A box behind each word")] {
                            if ui.selectable_label(shape == s, name).on_hover_text(tip).clicked() {
                                fx.params.set("shape", ParamSource::Static(Value::Enum(s.into())));
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("Color");
                    ui.horizontal(|ui| {
                        let mut c = match get(fx, "color") {
                            Some(Value::Color(c)) => c,
                            _ => [0.0, 0.0, 0.0, 0.75],
                        };
                        // The swatch shows the color itself; how solid it is has its own field.
                        let mut solid = [c[0], c[1], c[2], 1.0];
                        if crate::widgets::color_swatch(ui, &mut solid).1 {
                            c = [solid[0], solid[1], solid[2], c[3]];
                            fx.params.set("color", ParamSource::Static(Value::Color(c)));
                        }
                        ui.label("Opacity");
                        let mut alpha = c[3] * 100.0;
                        if ui.add(egui::DragValue::new(&mut alpha).range(0.0..=100.0).speed(0.5).max_decimals(0).suffix(" %")).on_hover_text("How solid the box is").changed() {
                            c[3] = alpha / 100.0;
                            fx.params.set("color", ParamSource::Static(Value::Color(c)));
                        }
                    });
                    ui.end_row();
                    ui.label("Corners");
                    let mut round = get(fx, "roundness").and_then(|v| v.as_float()).unwrap_or(0.25) * 100.0;
                    if ui.add(egui::Slider::new(&mut round, 0.0..=100.0).suffix(" %").max_decimals(0)).on_hover_text("0 % square, 100 % fully round").changed() {
                        fx.params.set("roundness", ParamSource::Static(Value::Float(round / 100.0)));
                    }
                    ui.end_row();
                });
            }
        });

        // The word being spoken: its own color, and a box behind it.
        panel(ui, |ui| {
            ui.label(egui::RichText::new("Highlight the spoken word").strong());
            let key = schema::spoken(schema::TEXT_COLOR);
            ui.horizontal(|ui| {
                let mut on = style.params.get(&key).is_some();
                if ui.checkbox(&mut on, "Its color").changed() {
                    if on {
                        style.params.set(&key, ParamSource::Static(Value::Gradient(Gradient::solid([1.0, 0.85, 0.2, 1.0]))));
                    } else {
                        style.params.0.remove(&oa_params::ParamId::new(&key));
                    }
                }
                if on {
                    let mut c = color(&style, &key);
                    if crate::widgets::color_swatch(ui, &mut c).1 {
                        style.params.set(&key, ParamSource::Static(Value::Gradient(Gradient::solid(c))));
                    }
                }
            });
            let marker = style.effects.iter().position(|e| e.type_id == BACKGROUND && e.params.get(&schema::spoken("color")).is_some());
            ui.horizontal(|ui| {
                let mut on = marker.is_some();
                if ui.checkbox(&mut on, "A box behind it").changed() {
                    if on {
                        let mut fx = EffectInstance::new(EffectId(2), BACKGROUND);
                        fx.params.set("shape", ParamSource::Static(Value::Enum("words".into())));
                        fx.params.set("color", ParamSource::Static(Value::Color([0.0, 0.0, 0.0, 0.0])));
                        fx.params.set(&schema::spoken("color"), ParamSource::Static(Value::Color([0.2, 0.55, 1.0, 1.0])));
                        style.effects.push(fx);
                    } else if let Some(i) = marker {
                        style.effects.remove(i);
                    }
                }
                if let Some(i) = style.effects.iter().position(|e| e.type_id == BACKGROUND && e.params.get(&schema::spoken("color")).is_some()) {
                    let key = schema::spoken("color");
                    let fx = &mut style.effects[i];
                    let mut c = match fx.params.get(&key).map(|s| s.eval(&oa_params::EvalContext::at(Time::ZERO, Time::ZERO))) {
                        Some(Value::Color(c)) => c,
                        _ => [0.2, 0.55, 1.0, 1.0],
                    };
                    if crate::widgets::color_swatch(ui, &mut c).1 {
                        fx.params.set(&key, ParamSource::Static(Value::Color(c)));
                    }
                }
            });
            ui.label(egui::RichText::new("Each caption knows when its words are said; the preview plays it through.").small().weak());
        });

        // Anything else copied from a title (animations, glows…).
        let others: Vec<(usize, String)> = style
            .effects
            .iter()
            .enumerate()
            .filter(|(_, e)| e.type_id != BACKGROUND)
            .map(|(i, e)| (i, self.effect_name(&e.type_id)))
            .collect();
        if !others.is_empty() {
            panel(ui, |ui| {
                ui.label(egui::RichText::new("Also copied").strong());
                let mut remove = None;
                for (i, name) in &others {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        if ui.small_button("✕").on_hover_text("Leave it off the captions").clicked() {
                            remove = Some(*i);
                        }
                    });
                }
                if let Some(i) = remove {
                    style.effects.remove(i);
                }
            });
        }
        if style != before {
            self.settings.captions.style = Some(style);
        }
    }

    /// The caption in focus (or a sample line) in the style, over the footage where it
    /// will play — played through when the spoken word is highlighted.
    fn caption_preview(&mut self, ui: &mut egui::Ui, c: &mut CaptionsUi, style: &Item) {
        // The format being viewed (the one the preview renders), not the project's first.
        let canvas = self.viewed_canvas();
        let rate = self.editor.sequence().rate;
        let width = ui.available_width();
        let height = (width * canvas.height as f32 / canvas.width.max(1) as f32).min(width * 1.4);
        // Dragging across the picture scrubs the whole stretch that was listened to.
        let (rect, canvas_drag) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());

        // What to show: the focused caption with its word times, or a sample. While
        // playing — or while being scrubbed — the caption under the playhead, in time
        // with the sound.
        let captions = self.current_captions(c);
        let words = c.transcript.as_ref().map(|t| t.words.clone()).unwrap_or_default();
        let span = captions.last().map_or(0.0, |(cap, _)| cap.end).max(c.length);
        if !captions.is_empty() && span > 0.0 {
            let scrub = |x: f32, r: egui::Rect| ((x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0) as f64 * span;
            if canvas_drag.dragged() || canvas_drag.drag_started() {
                if let Some(p) = canvas_drag.interact_pointer_pos() {
                    self.set_playing(false);
                    self.set_playhead(c.offset + Time::from_seconds_f64(scrub(p.x, rect)));
                    c.scrubbing = true;
                }
            } else if canvas_drag.drag_stopped() {
                c.scrubbing = false;
            }
            if canvas_drag.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        let playing = (self.playing || c.scrubbing) && !captions.is_empty();
        if playing {
            let local = (self.playhead - c.offset).as_seconds_f64();
            if let Some(i) = captions.iter().rposition(|(cap, _)| cap.start <= local) {
                c.focus = i;
            }
            // Past the last caption: stop.
            if captions.last().is_some_and(|(cap, _)| local > cap.end + 0.5) {
                self.set_playing(false);
            }
        }
        let (text, start, length, times) = match captions.get(c.focus) {
            Some((cap, _)) => {
                let times = words[cap.words.clone()].iter().filter(|w| !w.text.trim().is_empty()).map(|w| (w.start - cap.start, w.end - w.start)).collect::<Vec<_>>();
                (cap.text.clone(), c.offset + Time::from_seconds_f64(cap.start), (cap.end - cap.start).max(0.4), times)
            }
            None => ("Your captions will look like this".to_string(), self.playhead, 2.0, Vec::new()),
        };
        let highlight = style.params.0.keys().any(|k| k.as_str().ends_with(schema::SPOKEN_SUFFIX))
            || style.effects.iter().any(|e| e.params.0.keys().any(|k| k.as_str().ends_with(schema::SPOKEN_SUFFIX)));
        // Played through (on frames) when something follows the spoken word.
        let into = if highlight { (ui.input(|i| i.time) % (length + 0.6)).min(length - 0.01) } else { length * 0.5 };
        let at = if playing { self.playhead } else { start + Time::from_seconds_f64(into) };
        let at = rate.frame_start(rate.frame_at(at));
        if highlight || playing {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }

        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(style).unwrap_or_default().hash(&mut h);
            text.hash(&mut h);
            at.0.hash(&mut h);
            (width as u32).hash(&mut h);
            (std::sync::Arc::as_ptr(&self.editor.doc.snapshot()) as usize).hash(&mut h);
            self.variant.hash(&mut h);
            h.finish()
        };
        if key != c.preview_key || c.preview.is_none() {
            // A scratch copy of the project with the caption on top; nothing is edited.
            let mut project = (*self.editor.doc.snapshot()).clone();
            if let Some(seq) = project.sequences.get_mut(&self.editor.seq) {
                let seq = std::sync::Arc::make_mut(seq);
                let mut item = style.clone();
                item.id = ItemId(u64::MAX - 3);
                item.range = TimeRange::new(start, Time::from_seconds_f64(length));
                item.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text(text)));
                item.word_times = times.iter().map(|(a, d)| TimeRange::new(Time::from_seconds_f64(a.max(0.0)), Time::from_seconds_f64(d.max(0.01)))).collect();
                let mut track = oa_doc::Track::new(TrackId(u64::MAX - 4), "preview", TrackKind::Video);
                track.items.push(item);
                seq.tracks.push(std::sync::Arc::new(track));
            }
            let scale = (width * ui.ctx().pixels_per_point()) as f64 / canvas.width.max(1) as f64;
            let reuse = c.preview.as_ref().map(|(_, t)| t.clone());
            match self.render_scaled(&project, at, scale, reuse) {
                Some((texture, exact)) => {
                    if let Some(rs) = &self.render_state {
                        let view = oa_gpu::readback::display_view(&texture);
                        let id = match c.preview.take() {
                            Some((id, old)) if old == texture => id,
                            Some((id, _)) => {
                                rs.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, id);
                                id
                            }
                            None => rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
                        };
                        c.preview = Some((id, texture));
                    }
                    if exact {
                        c.preview_key = key;
                    } else {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
                    }
                }
                None => ui.ctx().request_repaint_after(std::time::Duration::from_millis(30)),
            }
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, egui::Color32::from_gray(12));
        match &c.preview {
            Some((id, _)) => {
                // Fit the frame in the box.
                let k = (rect.width() / canvas.width as f32).min(rect.height() / canvas.height as f32);
                let shown = egui::Rect::from_center_size(rect.center(), egui::vec2(canvas.width as f32 * k, canvas.height as f32 * k));
                painter.image(*id, shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Preparing the preview…", egui::FontId::proportional(12.0), egui::Color32::from_gray(140));
            }
        }
        // Under it: every line as a block along the stretch that was listened to, the
        // one shown lit, and where the sound is. Drag it (or the picture) to scrub.
        if !captions.is_empty() && span > 0.0 {
            let (bar, drag) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::click_and_drag());
            let painter = ui.painter_at(bar);
            painter.rect_filled(bar, 3.0, ui.visuals().extreme_bg_color);
            let x_of = |t: f64| bar.left() + (t / span).clamp(0.0, 1.0) as f32 * bar.width();
            for (i, (cap, color)) in captions.iter().enumerate() {
                let block = egui::Rect::from_min_max(egui::pos2(x_of(cap.start), bar.top() + 4.0), egui::pos2(x_of(cap.end).max(x_of(cap.start) + 1.5), bar.bottom() - 4.0));
                let color = crate::widgets::color32(color.unwrap_or(PALETTE[0]));
                painter.rect_filled(block, 1.0, if i == c.focus { color } else { color.gamma_multiply(0.45) });
            }
            if drag.dragged() || drag.drag_started() || drag.clicked() {
                if let Some(p) = drag.interact_pointer_pos() {
                    self.set_playing(false);
                    let t = ((p.x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0) as f64 * span;
                    self.set_playhead(c.offset + Time::from_seconds_f64(t));
                    c.scrubbing = true;
                }
            } else if drag.drag_stopped() {
                c.scrubbing = false;
            }
            let head = x_of((self.playhead - c.offset).as_seconds_f64());
            painter.line_segment([egui::pos2(head, bar.top()), egui::pos2(head, bar.bottom())], egui::Stroke::new(1.5, crate::style::GOLD));
            drag.on_hover_text("Drag to move through the captions (the picture scrubs too)");
        }
        // Play: the timeline plays with its sound from this caption, and the preview
        // follows it caption by caption.
        let any = !captions.is_empty();
        ui.horizontal(|ui| {
            if self.playing {
                if crate::icons::text_button(ui, crate::icons::PAUSE, "Pause", true).clicked() {
                    self.set_playing(false);
                }
            } else {
                let mut from = None;
                if crate::icons::text_button(ui, crate::icons::SKIP_START, "From the start", false).on_hover_text("Play every caption from the first, with the sound").clicked() {
                    from = captions.first().map(|(cap, _)| cap.start);
                }
                if crate::icons::text_button(ui, crate::icons::PLAY, "From the selected", true).on_hover_text("Play from the caption selected in the list, with the sound").clicked() {
                    from = captions.get(c.focus).map(|(cap, _)| cap.start);
                }
                if let Some(start) = from.filter(|_| any) {
                    self.set_playhead(c.offset + Time::from_seconds_f64(start));
                    self.set_playing(true);
                }
            }
        });
        if !any {
            ui.label(egui::RichText::new("Generate captions to play them with the sound").small().weak());
        }
        ui.add_space(4.0);
    }

    /// Puts `captions` (seconds from `offset`) on a new "Captions" track above the other
    /// video tracks, as title clips in the caption style, on frame boundaries, each
    /// knowing when its words are spoken. One undo step; returns how many were added.
    fn add_captions(&mut self, offset: Time, captions: &[(oa_captions::Caption, Option<[f64; 4]>)], words: &[Word]) -> Result<usize, String> {
        let s = self.editor.sequence().clone();
        let rate = s.rate;
        let style = self.settings.captions.style.clone().unwrap_or_else(|| default_style(self.viewed_canvas()));
        let snap = |seconds: f64| rate.frame_start(rate.frame_at(offset + Time::from_seconds_f64(seconds)));
        let track_id = TrackId(self.editor.doc.alloc_id());
        let mut track = oa_doc::Track::new(track_id, "Captions", TrackKind::Video);
        let mut last_end = Time::ZERO;
        for (i, (cap, color)) in captions.iter().enumerate() {
            let start = snap(cap.start).max(last_end);
            let next = captions.get(i + 1).map(|(n, _)| snap(n.start));
            let mut end = snap(cap.end).max(start + rate.frame_start(1));
            if let Some(next) = next.filter(|n| *n > start) {
                end = end.min(next);
            }
            if end <= start || cap.text.trim().is_empty() {
                continue;
            }
            last_end = end;
            let mut item = style.clone();
            item.id = ItemId(self.editor.doc.alloc_id());
            item.name = "Caption".into();
            item.range = TimeRange::new(start, end - start);
            item.time_map = oa_doc::TimeMap::default();
            for fx in &mut item.effects {
                fx.id = EffectId(self.editor.doc.alloc_id());
            }
            item.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text(cap.text.clone())));
            if let Some(color) = color {
                item.params.set(schema::TEXT_COLOR, ParamSource::Static(Value::Gradient(Gradient::solid(*color))));
            }
            // When each of its words is said, on the clip's own clock.
            let clip_start = start.as_seconds_f64() - offset.as_seconds_f64();
            item.word_times = words
                .get(cap.words.clone())
                .unwrap_or(&[])
                .iter()
                .filter(|w| !w.text.trim().is_empty())
                .map(|w| TimeRange::new(Time::from_seconds_f64((w.start - clip_start).max(0.0)), Time::from_seconds_f64((w.end - w.start).max(0.01))))
                .collect();
            track.items.push(item);
        }
        let n = track.items.len();
        if n == 0 {
            return Err("there's nothing to add".into());
        }
        let index = s.tracks.iter().rposition(|t| t.kind == TrackKind::Video).map_or(s.tracks.len(), |i| i + 1);
        let op = Op::InsertTrack { seq: self.editor.seq, index, track: std::sync::Arc::new(track) };
        self.editor.apply(ADD_CAPTIONS, vec![op]).map_err(|e| e.to_string())?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typing into one caption is one undo step; a split is another; undoing them and
    /// redoing comes back to the same state, and a grouping change undoes too.
    #[test]
    fn caption_changes_undo_in_steps() {
        let mut c = CaptionsUi::new(true);
        let mut prefs = crate::settings::CaptionPrefs::default();
        c.record(&prefs, 0.0, false);
        for (t, text) in [(0.1, "h"), (0.2, "he"), (0.3, "hey")] {
            c.edits.insert(0, text.into());
            c.record(&prefs, t, false);
        }
        c.manual.split(3);
        c.record(&prefs, 0.4, false);
        // A slider being dragged: nothing until it's let go.
        prefs.grouping.max_words += 2;
        c.record(&prefs, 0.5, true);
        assert_eq!(c.undo.len(), 2);
        c.record(&prefs, 0.6, false);
        assert_eq!(c.undo.len(), 3);

        let back = c.step_history(false).unwrap();
        assert_eq!(back.prefs.grouping.max_words, prefs.grouping.max_words - 2, "the grouping comes back");
        c.step_history(false).unwrap();
        assert_eq!(c.manual, oa_captions::Manual::default(), "the split is undone");
        assert_eq!(c.edits.get(&0).map(String::as_str), Some("hey"), "but not the typing");
        c.step_history(false).unwrap();
        assert!(c.edits.is_empty(), "the whole run of typing is one step");
        assert!(c.step_history(false).is_none());

        c.step_history(true).unwrap();
        c.step_history(true).unwrap();
        assert_eq!(c.edits.get(&0).map(String::as_str), Some("hey"));
        assert_ne!(c.manual, oa_captions::Manual::default(), "redo splits again");
    }

    /// Removing a line is a step of its own: undo puts its words back, and a removal
    /// doesn't coalesce into the typing before it.
    #[test]
    fn removing_a_line_undoes_on_its_own() {
        let mut c = CaptionsUi::new(true);
        let prefs = crate::settings::CaptionPrefs::default();
        c.record(&prefs, 0.0, false);
        c.edits.insert(0, "hey".into());
        c.record(&prefs, 0.1, false);
        c.removed.extend([3, 4, 5]);
        c.record(&prefs, 0.2, false);
        assert_eq!(c.undo.len(), 2);
        c.step_history(false).unwrap();
        assert!(c.removed.is_empty(), "the line comes back");
        assert_eq!(c.edits.get(&0).map(String::as_str), Some("hey"), "and the typing stays");
        c.step_history(true).unwrap();
        assert_eq!(c.removed.len(), 3, "redo takes it out again");
    }
}
