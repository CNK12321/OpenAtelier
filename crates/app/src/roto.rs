//! The Rotoscope tool of the Masks tab: click what to cut out on one frame (Alt-click
//! what to leave out), and SAM 2 (`oa_track::roto`) finds its outline on every frame of
//! the clip — or as long before and after as asked — into a frame-by-frame matte
//! ([`MaskShape::Matte`]) in the selected mask. Softness is the matte's feather, and
//! stays editable afterwards like any shape's.
//!
//! SAM 2 is set up on request into the AI engine's folder the point tracker uses (the
//! same Python and PyTorch), with the model of the user's choosing; each model is its
//! own download.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::mask::{Bitmap, Mask, MaskMode, MaskShape, MatteFrame};
use oa_doc::{ItemId, ItemKind, Op};
use oa_time::{Time, TimeRange};
use oa_track::engine::{Compute, Footage, Job, Msg, Tracker};
use oa_track::roto::{Click, Mattes, Roto, SamModel, MAX_FRAMES, MAX_SIDE};

fn roto() -> Roto {
    Roto::new(Tracker::new(Tracker::default_root()))
}

/// A click on the clip: on which clip, at what timeline time, where (a fraction of the
/// clip's picture) and whether it's the thing to cut out.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoClick {
    pub item: ItemId,
    pub t: Time,
    pub at: [f64; 2],
    pub include: bool,
    /// The document's undo depth when it was placed: Ctrl+Z takes clicks back only while
    /// nothing was edited since (after a run, it undoes the run first).
    pub depth: usize,
}

/// The tool's state.
pub struct RotoState {
    pub clicks: Vec<RotoClick>,
    /// Clicks taken back with Ctrl+Z, for Ctrl+Shift+Z.
    pub undone: Vec<RotoClick>,
    /// The whole clip, or `before`/`after` seconds around the clicked frame.
    pub whole: bool,
    pub before: f64,
    pub after: f64,
    /// Every frame (1), every 2nd or every 4th.
    pub detail: usize,
    setup: Option<Job>,
    setup_step: String,
    setup_progress: Option<f32>,
    error: Option<String>,
    run: Option<Run>,
    note: Option<String>,
}

impl Default for RotoState {
    fn default() -> Self {
        RotoState {
            clicks: Vec::new(),
            undone: Vec::new(),
            whole: true,
            before: 2.0,
            after: 2.0,
            detail: 1,
            setup: None,
            setup_step: String::new(),
            setup_progress: None,
            error: None,
            run: None,
            note: None,
        }
    }
}

/// A run: the footage handed over, the clip and mask it's for, and what came back.
struct Run {
    job: Job,
    step: String,
    progress: Option<f32>,
    device: Option<String>,
    footage: Footage,
    clip: ItemId,
    mask: u64,
    /// Where on the timeline the footage is.
    range: TimeRange,
    result: Option<Mattes>,
}

impl App {
    fn sam_model(&self) -> SamModel {
        SamModel::from_key(&self.settings.sam_model).unwrap_or(SamModel::Small)
    }

    /// A click in the viewer with the Rotoscope tool: on `item` at the playhead, at
    /// `at` (a fraction of its picture). Clicks on another frame start over.
    pub(crate) fn roto_click(&mut self, item: ItemId, at: [f64; 2], include: bool) {
        let t = self.playhead;
        let depth = self.editor.doc.undo_depth();
        let r = &mut self.masks.roto;
        r.clicks.retain(|c| c.item == item && c.t == t);
        r.clicks.push(RotoClick { item, t, at, include, depth });
        r.undone.clear();
        r.note = None;
    }

    /// Whether the Rotoscope tool is the one in use (the Masks tab open).
    fn rotoscoping(&self) -> bool {
        self.settings.masking && self.inspector_tab == crate::inspector::Tab::Masks && self.masks.tool == crate::masks::Tool::Rotoscope
    }

    /// Ctrl+Z with the Rotoscope tool: its last click goes, if nothing was edited since
    /// it was placed (otherwise the edit is undone, as usual). Returns whether it took one.
    pub(crate) fn undo_roto_click(&mut self) -> bool {
        let depth = self.editor.doc.undo_depth();
        let r = &self.masks.roto;
        if !self.rotoscoping() || r.run.is_some() || r.clicks.last().is_none_or(|c| c.depth != depth) {
            return false;
        }
        if let Some(c) = self.masks.roto.clicks.pop() {
            self.set_playhead(c.t);
            self.masks.roto.undone.push(c);
        }
        true
    }

    /// Ctrl+Shift+Z after [`App::undo_roto_click`]: the click comes back, if nothing was
    /// edited since. Returns whether it brought one.
    pub(crate) fn redo_roto_click(&mut self) -> bool {
        let depth = self.editor.doc.undo_depth();
        if !self.rotoscoping() || self.masks.roto.undone.last().is_none_or(|c| c.depth != depth) {
            return false;
        }
        if let Some(c) = self.masks.roto.undone.pop() {
            self.set_playhead(c.t);
            self.masks.roto.clicks.push(c);
        }
        true
    }

    /// The clicks drawn over the picture, while the playhead is on their frame: a green
    /// dot with a + on the thing to cut out, a red one with a − on what to leave out.
    pub(crate) fn paint_roto_clicks(&self, painter: &egui::Painter, item: ItemId, screen_of: &dyn Fn([f64; 2]) -> egui::Pos2) {
        for c in self.masks.roto.clicks.iter().filter(|c| c.item == item && c.t == self.playhead) {
            let at = screen_of(c.at);
            let color = if c.include { egui::Color32::from_rgb(52, 199, 89) } else { egui::Color32::from_rgb(235, 72, 72) };
            painter.circle_filled(at + egui::vec2(0.0, 1.5), 9.5, egui::Color32::from_black_alpha(90));
            painter.circle(at, 8.0, color, egui::Stroke::new(2.0, egui::Color32::WHITE));
            let s = egui::Stroke::new(2.0, egui::Color32::WHITE);
            painter.line_segment([at - egui::vec2(3.5, 0.0), at + egui::vec2(3.5, 0.0)], s);
            if c.include {
                painter.line_segment([at - egui::vec2(0.0, 3.5), at + egui::vec2(0.0, 3.5)], s);
            }
        }
    }

    /// Over the viewer while rotoscoping: what to do next, and the button that does it —
    /// or, while SAM 2 runs, how far along it is.
    pub(crate) fn roto_banner(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        if !self.rotoscoping() {
            return;
        }
        let Some(item) = self.selection.filter(|i| self.maskable(*i)) else { return };
        let width = (area.width() - 120.0).clamp(220.0, 460.0);
        let rect = egui::Rect::from_min_size(egui::pos2(area.center().x - width / 2.0, area.top() + 8.0), egui::vec2(width, 36.0));
        let v = ui.visuals().clone();
        ui.painter().add(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: egui::Color32::from_black_alpha(90) }.as_shape(rect, 8.0));
        ui.painter().rect(rect, 8.0, v.window_fill.gamma_multiply(0.96), egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
        // The banner takes the pointer: clicking it doesn't place a point.
        ui.interact(rect, ui.id().with("roto-banner"), egui::Sense::click_and_drag());
        let inner = rect.shrink2(egui::vec2(10.0, 4.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, crate::icons::TOOL_ROTO, crate::style::ACCENT);
            if let Some(run) = &self.masks.roto.run {
                ui.add(egui::ProgressBar::new(run.progress.unwrap_or(0.0)).desired_width(inner.width() - 120.0).text(run.step.clone()).animate(run.progress.is_none()));
                if ui.small_button(tr("Cancel")).clicked() {
                    run.job.cancel();
                }
                return;
            }
            let here: Vec<&RotoClick> = self.masks.roto.clicks.iter().filter(|c| c.item == item && c.t == self.playhead).collect();
            let ready = here.iter().any(|c| c.include);
            let text = if here.is_empty() {
                tr("Click what to cut out · Alt-click what to leave out").to_string()
            } else {
                let (yes, no) = (here.iter().filter(|c| c.include).count(), here.iter().filter(|c| !c.include).count());
                trf("{yes} on it · {no} left out · Ctrl+Z takes one back", &[("yes", &yes.to_string()), ("no", &no.to_string())])
            };
            ui.label(egui::RichText::new(text).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let go = ui.add_enabled(ready, egui::Button::new(egui::RichText::new(tr("Rotoscope")).strong().color(egui::Color32::WHITE)).fill(crate::style::ACCENT));
                if go.on_hover_text(tr("Find its outline on every frame, into the selected mask")).on_disabled_hover_text(tr("Click on the thing to cut out first")).clicked() {
                    let native = self.mask_placement(item, self.playhead).map_or([1920.0, 1080.0], |p| p.native);
                    let mask = self.masks.selected.unwrap_or(0);
                    self.start_roto(item, mask, native);
                }
            });
        });
    }

    /// The tool's panel in the Masks tab: setup, then the model, how long, how much
    /// detail and how soft. The clicks and the run are in the viewer's banner.
    pub(crate) fn roto_panel(&mut self, ui: &mut egui::Ui, item: ItemId) {
        let roto = roto();
        let model = self.sam_model();
        if !roto.is_installed() || !roto.has_model(model) || self.masks.roto.setup.is_some() {
            self.roto_setup_ui(ui);
            if !roto.is_installed() || !roto.has_model(model) {
                return;
            }
        }
        if let Some(run) = &self.masks.roto.run {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(&run.step).small());
            });
            let p = run.progress.unwrap_or(0.0);
            ui.add(egui::ProgressBar::new(p).desired_width(280.0).show_percentage().animate(run.progress.is_none()));
            if let Some(d) = &run.device {
                ui.label(egui::RichText::new(trf("Running on {d}", &[("d", &d.to_string())])).small().weak());
            }
            if ui.button(tr("Cancel")).clicked() {
                run.job.cancel();
            }
            return;
        }
        let rate = self.editor.sequence().rate.as_f64();
        let clip_len = self.editor.item(item).map_or(0.0, |i| i.range.duration.as_seconds_f64());
        let installed = roto.models();
        let compact = self.settings.compact_properties;
        egui::Grid::new("roto-settings").num_columns(2).spacing([10.0, if compact { 3.0 } else { 6.0 }]).show(ui, |ui| {
            ui.label(tr("Model"));
            let mut chosen = model;
            egui::ComboBox::from_id_salt("sam-model").selected_text(chosen.name()).show_ui(ui, |ui| {
                for m in SamModel::ALL {
                    let have = if installed.contains(&m) { "" } else { " — download" };
                    ui.selectable_value(&mut chosen, m, format!("{} ({} MB{have})", m.name(), m.download_mb())).on_hover_text(m.describe());
                }
            });
            if chosen != model {
                self.settings.sam_model = chosen.key().into();
                self.settings.save();
            }
            ui.end_row();

            let r = &mut self.masks.roto;
            ui.label(tr("Range"));
            ui.horizontal(|ui| {
                if crate::widgets::chip(ui, r.whole, tr("Whole clip")).clicked() {
                    r.whole = true;
                }
                if crate::widgets::chip(ui, !r.whole, tr("Around here")).on_hover_text(tr("Only a stretch around the clicked frame")).clicked() {
                    r.whole = false;
                }
            });
            ui.end_row();
            if !r.whole {
                ui.label("");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut r.before).range(0.0..=600.0).speed(0.05).prefix(tr("before ")).suffix(" s"));
                    ui.add(egui::DragValue::new(&mut r.after).range(0.0..=600.0).speed(0.05).prefix(tr("after ")).suffix(" s"));
                });
                ui.end_row();
            }

            ui.label(tr("Detail"));
            ui.horizontal(|ui| {
                for (k, name, tip) in [(1, "Every frame", tr("Most exact, slowest")), (2, "Every 2nd", tr("About twice as fast; each outline holds for two frames")), (4, "Every 4th", tr("Fastest; for slow movement"))] {
                    if crate::widgets::chip(ui, r.detail == k, tr(name)).on_hover_text(tr(tip)).clicked() {
                        r.detail = k;
                    }
                }
            });
            ui.end_row();

            ui.label(tr("Softness"));
            let mut px = self.settings.roto_softness_px;
            ui.add(egui::Slider::new(&mut px, 0.0..=40.0).suffix(" px"))
                .on_hover_text(tr("How far the edge fades out. It's the shape's feather, so it can be changed after, under Shapes."));
            if (px - self.settings.roto_softness_px).abs() > 1e-9 {
                self.settings.roto_softness_px = px;
                self.settings.save();
            }
            ui.end_row();
        });
        let r = &self.masks.roto;
        let seconds = if r.whole { clip_len } else { r.before + r.after };
        let frames = (seconds * rate / r.detail.max(1) as f64).ceil() as usize + 1;
        if frames > MAX_FRAMES {
            ui.label(egui::RichText::new(trf("That's {frames} frames; up to {MAX_FRAMES} are done, spread over it. Less time or less detail keeps every one.", &[("frames", &frames.to_string()), ("MAX_FRAMES", &MAX_FRAMES.to_string())])).small().color(crate::style::WARNING));
        }
        if let Some(note) = &r.note {
            ui.label(egui::RichText::new(note).small().color(crate::style::WARNING));
        }
        let any = r.clicks.iter().any(|c| c.item == item && c.t == self.playhead);
        ui.horizontal(|ui| {
            if !compact {
                ui.label(egui::RichText::new(tr("Click on the picture, then Rotoscope in the bar over the viewer.")).small().weak());
            }
            if any && ui.small_button(tr("Clear points")).clicked() {
                self.masks.roto.clicks.clear();
                self.masks.roto.undone.clear();
            }
        });
    }

    /// Setting SAM 2 up, or downloading the chosen model, with progress.
    fn roto_setup_ui(&mut self, ui: &mut egui::Ui) {
        let roto = roto();
        let r = &self.masks.roto;
        if let Some(job) = &r.setup {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(&r.setup_step).small());
            });
            if let Some(p) = r.setup_progress {
                ui.add(egui::ProgressBar::new(p).desired_width(280.0));
            }
            if ui.button(tr("Cancel")).clicked() {
                job.cancel();
            }
            return;
        }
        if let Some(e) = &r.error {
            ui.label(egui::RichText::new(e).small().color(crate::style::ERROR));
        }
        let model = self.sam_model();
        let mut chosen = model;
        ui.horizontal(|ui| {
            ui.label(tr("Model"));
            egui::ComboBox::from_id_salt("sam-model-setup").selected_text(format!("{} ({} MB)", chosen.name(), chosen.download_mb())).show_ui(ui, |ui| {
                for m in SamModel::ALL {
                    ui.selectable_value(&mut chosen, m, format!("{} ({} MB) — {}", m.name(), m.download_mb(), m.describe()));
                }
            });
        });
        if chosen != model {
            self.settings.sam_model = chosen.key().into();
            self.settings.save();
        }
        let mut start = None;
        if roto.is_installed() {
            ui.label(egui::RichText::new(trf("The {0} model isn't downloaded yet.", &[("0", (chosen.name()))])).small());
            if ui.button(trf("Download it ({0} MB)", &[("0", &(chosen.download_mb()).to_string())])).clicked() {
                start = Some(roto.model_steps(chosen));
            }
        } else {
            let tracker = roto.tracker();
            let shared = tracker.has_base();
            let base = if shared { String::new() } else { trf("Python and PyTorch (~{0} MB, shared with the AI tracker), ", &[("0", &(Compute::Cpu.download_mb()).to_string())]) };
            ui.label(
                egui::RichText::new(trf("Rotoscoping uses SAM 2, Meta's Segment Anything model (Apache 2.0), which runs on your computer. Setting it up downloads {base}SAM 2 and the {0} model ({1} MB) into {2}.", &[("base", &(base).to_string()), ("0", (chosen.name())), ("1", &(chosen.download_mb()).to_string()), ("2", &(tracker.root().display()).to_string())]))
                .small(),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Set up")).on_hover_text(if shared { tr("Adds SAM 2 to the AI engine already set up") } else { tr("Works on any computer, using every core") }).clicked() {
                    start = Some(roto.install_steps(Compute::Cpu, chosen));
                }
                if !shared
                    && let Some(name) = crate::tracks::nvidia()
                    && ui.button(trf("Set up for {name} (~{0} MB)", &[("name", &(name).to_string()), ("0", &(Compute::Nvidia.download_mb()).to_string())])).on_hover_text(tr("Runs on the graphics card: many times faster")).clicked()
                {
                    start = Some(roto.install_steps(Compute::Nvidia, chosen));
                }
            });
        }
        if let Some(steps) = start {
            let r = &mut self.masks.roto;
            r.error = None;
            r.setup = Some(oa_track::engine::run(roto.tracker().engine(), steps));
        }
    }

    /// Starts SAM 2 on `item`'s own footage from the clicks on the playhead's frame.
    fn start_roto(&mut self, item: ItemId, mask: u64, native: [f64; 2]) {
        let t = self.playhead;
        let note = |app: &mut App, s: &str| app.masks.roto.note = Some(s.to_string());
        let Some(it) = self.editor.item(item).cloned() else { return };
        let ItemKind::Media { media } = it.kind else {
            return note(self, tr("Only a video clip can be rotoscoped."));
        };
        let Some(pool) = self.editor.pool_item(media).filter(|p| !p.missing && p.probe.has_video()) else {
            return note(self, tr("Only a video clip can be rotoscoped."));
        };
        if pool.kind == oa_media::MediaKind::Still {
            return note(self, tr("A still picture doesn't move: draw its mask with the other tools (Magic select is quick)."));
        }
        let path = pool.decode_path.clone();
        let r = &self.masks.roto;
        let (a, b) = if r.whole {
            (it.range.start, it.range.end())
        } else {
            (it.range.start.max(t - Time::from_seconds_f64(r.before)), it.range.end().min(t + Time::from_seconds_f64(r.after)))
        };
        if b <= a {
            return note(self, tr("Nothing to rotoscope there: set Before or After."));
        }
        let (s0, s1) = (it.eval_context(a).source_time.as_seconds_f64(), it.eval_context(b).source_time.as_seconds_f64());
        if s1 <= s0 {
            return note(self, tr("Reversed or frozen footage can't be rotoscoped."));
        }
        let frames = ((b - a).as_seconds_f64() * self.editor.sequence().rate.as_f64() / r.detail.max(1) as f64).ceil() as usize + 1;
        let footage = Footage::fit(path, s0, s1 - s0, frames, native, MAX_SIDE, MAX_FRAMES);
        let at_file = it.eval_context(t).source_time.as_seconds_f64();
        let clicks: Vec<Click> = r
            .clicks
            .iter()
            .filter(|c| c.item == item && c.t == t)
            .map(|c| Click { at: [c.at[0] * footage.size[0] as f64, c.at[1] * footage.size[1] as f64], include: c.include })
            .collect();
        let roto = roto();
        let job = oa_track::engine::run(roto.tracker().engine(), roto.segment_steps("ffmpeg", &footage, at_file, &clicks, self.sam_model()));
        self.set_playing(false);
        let r = &mut self.masks.roto;
        r.note = None;
        r.run = Some(Run { job, step: "Starting".into(), progress: None, device: None, footage, clip: item, mask, range: TimeRange::new(a, b - a), result: None });
    }

    /// Every frame: setup's and a run's news; a finished run becomes the matte.
    pub(crate) fn poll_roto(&mut self, ctx: &egui::Context) {
        let r = &mut self.masks.roto;
        if let Some(job) = &r.setup {
            let mut ended = None;
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    Msg::Step { label, .. } => {
                        r.setup_step = label;
                        r.setup_progress = None;
                    }
                    Msg::Progress(p) => r.setup_progress = Some(p),
                    Msg::Failed(e) => ended = Some(Some(e)),
                    Msg::Finished => ended = Some(None),
                    _ => {}
                }
            }
            if let Some(error) = ended {
                r.setup = None;
                r.error = error;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let Some(run) = r.run.as_mut() else { return };
        let mut ended = None;
        let mut newest = None;
        while let Ok(m) = run.job.rx.try_recv() {
            match m {
                Msg::Step { label, .. } => {
                    run.step = label;
                    run.progress = None;
                }
                Msg::Progress(p) => run.progress = Some(p),
                Msg::Log(line) => {
                    if let Some(n) = oa_track::engine::ffmpeg_frames(&line) {
                        run.progress = Some(n as f32 / run.footage.frames.max(1) as f32);
                    }
                }
                Msg::Data(line) => {
                    if let Some(d) = oa_track::engine::parse_device(&line) {
                        run.device = Some(d);
                    }
                    if let Some(f) = oa_track::roto::parse_at(&line) {
                        newest = Some(f);
                    }
                    if let Some(m) = Mattes::parse(&line) {
                        run.result = Some(m);
                    }
                }
                Msg::Failed(e) => ended = Some(Err(e)),
                Msg::Finished => ended = Some(Ok(())),
                _ => {}
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        // The playhead follows along.
        if let Some(f) = newest {
            let t = run_time(run, f);
            self.set_playhead(t);
        }
        let Some(ended) = ended else { return };
        let run = self.masks.roto.run.take().expect("checked");
        let outcome = match (ended, &run.result) {
            (Ok(()), Some(m)) => m.read().and_then(|frames| self.apply_roto(&run, frames)),
            (Ok(()), None) => Err(tr("SAM 2 finished without an outline.").into()),
            (Err(e), _) => Err(trf("Couldn't rotoscope it: {e}", &[("e", &(e).to_string())])),
        };
        match outcome {
            Ok(n) => self.notify(trf("Rotoscoped {n} frames.", &[("n", &n.to_string())])),
            Err(e) => self.masks.roto.note = Some(e),
        }
    }

    /// A finished run into the mask: each frame's coverage at its clip time, softened by
    /// the softness chosen. Into the selected mask when it's drawn over the whole clip
    /// (as mattes are), else a mask of its own. Returns how many frames.
    fn apply_roto(&mut self, run: &Run, frames: Vec<Vec<u8>>) -> Result<usize, String> {
        let Some(it) = self.editor.item(run.clip).cloned() else { return Err("the clip is gone".into()) };
        let height = self.mask_placement(run.clip, run.range.start).map_or(1080.0, |p| p.native[1]).max(1.0);
        let feather = self.settings.roto_softness_px / height;
        let matte: Vec<MatteFrame> = frames
            .iter()
            .enumerate()
            .map(|(i, coverage)| MatteFrame { t: run_time(run, i) - it.range.start, bitmap: Bitmap::encode(run.footage.size, coverage) })
            .collect();
        let n = matte.len();
        let shape = MaskShape::Matte { frames: matte, expand: 0.0, feather, erase: false };
        let mut masks = it.masks.clone();
        match masks.iter_mut().find(|m| m.id == run.mask && m.frame == [1.0, 1.0]) {
            Some(m) => m.shapes.push(shape),
            None => {
                let id = self.editor.doc.alloc_id();
                masks.push(Mask { id, name: format!("Rotoscope {}", masks.len() + 1), enabled: true, invert: false, mode: MaskMode::Add, shapes: vec![shape], frame: [1.0, 1.0] });
                self.masks.selected = Some(id);
            }
        }
        self.editor.apply("Rotoscope", vec![Op::SetMasks { seq: self.editor.seq, item: run.clip, masks }]).map_err(|e| e.to_string())?;
        // The clicks stay: undo the result, and they're there to change and run again.
        Ok(n)
    }

    /// SAM 2 in Settings (under the AI tracker): what's set up, models to add or remove.
    pub(crate) fn roto_settings(&mut self, ui: &mut egui::Ui) {
        let roto = roto();
        ui.label(egui::RichText::new(tr("Rotoscoping (SAM 2)")).strong());
        if !roto.is_installed() {
            ui.label(egui::RichText::new(tr("Not set up. It's set up from the Masks tab (the Rotoscope tool), or here.")).small().weak());
            self.roto_setup_ui(ui);
            return;
        }
        if self.masks.roto.setup.is_some() {
            self.roto_setup_ui(ui);
            return;
        }
        let mut remove = None;
        let mut download = None;
        for m in SamModel::ALL {
            ui.horizontal(|ui| {
                ui.label(trf("{0} — {1}", &[("0", (m.name())), ("1", (m.describe()))]));
                if roto.has_model(m) {
                    if ui.small_button(tr("Remove")).on_hover_text(trf("Delete the model ({0} MB)", &[("0", &(m.download_mb()).to_string())])).clicked() {
                        remove = Some(m);
                    }
                } else if ui.small_button(trf("Download ({0} MB)", &[("0", &(m.download_mb()).to_string())])).clicked() {
                    download = Some(m);
                }
            });
        }
        if let Some(m) = remove
            && let Err(e) = roto.remove_model(m)
        {
            self.masks.roto.error = Some(e.to_string());
        }
        if let Some(m) = download {
            self.masks.roto.setup = Some(oa_track::engine::run(roto.tracker().engine(), roto.model_steps(m)));
        }
        let confirm = egui::Id::new("remove-sam-confirm");
        let asked = ui.data(|d| d.get_temp::<bool>(confirm)).unwrap_or(false);
        ui.horizontal(|ui| {
            if !asked {
                if ui.button(tr("Remove SAM 2")).on_hover_text(tr("Its code and models (rotoscoped masks in projects stay; the tracker's Python stays)")).clicked() {
                    ui.data_mut(|d| d.insert_temp(confirm, true));
                }
            } else {
                ui.label(egui::RichText::new(tr("Delete it?")).color(crate::style::ERROR));
                if ui.button(tr("Remove")).clicked() {
                    if let Err(e) = roto.remove() {
                        self.masks.roto.error = Some(e.to_string());
                    }
                    ui.data_mut(|d| d.remove::<bool>(confirm));
                }
                if ui.button(tr("Keep it")).clicked() {
                    ui.data_mut(|d| d.remove::<bool>(confirm));
                }
            }
        });
        if let Some(e) = &self.masks.roto.error {
            ui.label(egui::RichText::new(e).small().color(crate::style::ERROR));
        }
    }
}

/// The timeline time of a footage frame of `run`.
fn run_time(run: &Run, frame: usize) -> Time {
    let f = (run.footage.time_of(frame) - run.footage.start) / run.footage.duration.max(1e-9);
    run.range.start + Time::from_seconds_f64(run.range.duration.as_seconds_f64() * f.clamp(0.0, 1.0))
}
