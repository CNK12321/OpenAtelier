//! The audio recorder (the microphone button in the media bin): record a take, then chop
//! it into clips — click the waveform to cut, drag a cut to move it, right-click to
//! remove it, or split at the pauses — name them, listen to each, and add the ones you
//! keep to the media bin (as WAV files in a "Recordings" folder, next to the project).

use crate::App;
use eframe::egui;
use oa_audio::{Recorder, Take};
use oa_time::Time;
use std::path::PathBuf;

/// One piece of the take between two cuts.
#[derive(Clone)]
struct Part {
    name: String,
    keep: bool,
}

#[derive(Default)]
pub struct RecordWindow {
    pub open: bool,
    devices: Vec<String>,
    device: Option<String>,
    recorder: Option<Recorder>,
    take: Take,
    /// Smoothed meter level, 0..1.
    level: f32,
    /// Cut points in seconds, sorted.
    cuts: Vec<f64>,
    parts: Vec<Part>,
    /// A cut being dragged.
    dragging: Option<usize>,
    /// Pause detection: level below this (dB) for at least `gap` seconds.
    threshold_db: f32,
    gap: f64,
    /// How many takes have been added (for names).
    takes: usize,
    error: Option<String>,
}

const RED: egui::Color32 = egui::Color32::from_rgb(225, 60, 60);

fn clock(s: f64) -> String {
    let s = s.max(0.0);
    format!("{}:{:04.1}", (s / 60.0).floor() as u64, s % 60.0)
}

impl RecordWindow {
    /// Start and end (seconds) of each part.
    fn bounds(&self) -> Vec<(f64, f64)> {
        let end = self.take.seconds();
        let mut edges = vec![0.0];
        edges.extend(self.cuts.iter().copied().filter(|c| *c > 0.0 && *c < end));
        edges.push(end);
        edges.windows(2).map(|w| (w[0], w[1])).collect()
    }

    /// Keeps one part per piece, holding on to names already typed.
    fn sync_parts(&mut self) {
        self.cuts.sort_by(f64::total_cmp);
        self.cuts.dedup_by(|a, b| (*a - *b).abs() < 0.02);
        let n = self.bounds().len();
        let base = format!("Recording {}", self.takes + 1);
        while self.parts.len() < n {
            let i = self.parts.len() + 1;
            self.parts.push(Part { name: format!("{base}-{i}"), keep: true });
        }
        self.parts.truncate(n);
        if n == 1 {
            self.parts[0].name = self.parts[0].name.trim_end_matches("-1").to_string();
        }
    }
}

impl App {
    pub(crate) fn open_recorder(&mut self) {
        let r = &mut self.recording;
        r.open = true;
        r.devices = Recorder::devices();
        if r.threshold_db == 0.0 {
            r.threshold_db = -38.0;
            r.gap = 0.4;
        }
    }

    /// Where recordings are written: beside the project, else in the app's folder.
    fn recordings_dir(&self) -> PathBuf {
        match self.editor.path.as_ref().and_then(|p| p.parent()) {
            Some(dir) => dir.join("Recordings"),
            None => crate::settings::config_dir().join("recordings"),
        }
    }

    pub(crate) fn record_window(&mut self, ctx: &egui::Context) {
        if !self.recording.open {
            return;
        }
        // Collect what the microphone sent since last frame.
        if let Some(rec) = &self.recording.recorder {
            rec.drain(&mut self.recording.take);
            let peak = rec.take_peak();
            let r = &mut self.recording;
            r.level = if peak > r.level { peak } else { r.level * 0.9 };
            ctx.request_repaint();
        }
        let mut open = true;
        let mut action: Option<&str> = None;
        let mut play: Option<(PathBuf, Time)> = None;
        egui::Window::new("Record audio").open(&mut open).collapsible(false).resizable(true).default_width(560.0).show(ctx, |ui| {
            let r = &mut self.recording;
            let recording = r.recorder.is_some();

            // Input and the big button.
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!recording, |ui| {
                    let current = r.device.clone().unwrap_or_else(|| r.devices.first().cloned().unwrap_or_else(|| "Default input".into()));
                    egui::ComboBox::from_id_salt("record-device").selected_text(current).width(240.0).show_ui(ui, |ui| {
                        for d in &r.devices {
                            ui.selectable_value(&mut r.device, Some(d.clone()), d);
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if recording {
                        if ui.add(egui::Button::new(egui::RichText::new("■  Stop").strong())).clicked() {
                            action = Some("stop");
                        }
                        let paused = r.recorder.as_ref().is_some_and(|x| x.is_paused());
                        if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                            action = Some("pause");
                        }
                    } else {
                        let label = if r.take.samples.is_empty() { "●  Record" } else { "●  Record again" };
                        let button = egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE)).fill(RED).corner_radius(14.0);
                        let hint = if r.take.samples.is_empty() { "Start recording" } else { "Start over (this take is replaced)" };
                        if ui.add(button).on_hover_text(hint).clicked() {
                            action = Some("record");
                        }
                    }
                });
            });

            // Time and level.
            ui.horizontal(|ui| {
                let dot = if recording && (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0 { RED } else { ui.visuals().weak_text_color() };
                ui.label(egui::RichText::new("●").color(if recording { dot } else { ui.visuals().weak_text_color() }));
                ui.label(egui::RichText::new(clock(r.take.seconds())).size(crate::style::TEXT_L).monospace());
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width() - 8.0, 8.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
                let db = 20.0 * r.level.max(1e-5).log10();
                let fill = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                let color = if db > -3.0 { RED } else if db > -12.0 { crate::style::WARNING } else { egui::Color32::from_rgb(80, 200, 120) };
                ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fill, rect.height())), 3.0, color);
            });

            // The waveform, with its cuts.
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 130.0), egui::Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
            let seconds = r.take.seconds();
            // While recording, the view grows in 10 s steps so it doesn't crawl.
            let span = if recording { (seconds / 10.0).ceil().max(1.0) * 10.0 } else { seconds.max(0.01) };
            let x_of = |s: f64| rect.left() + (s / span) as f32 * rect.width();
            let s_at = |x: f32| (((x - rect.left()) / rect.width()) as f64 * span).clamp(0.0, seconds);
            if !recording {
                for (i, (a, b)) in r.bounds().into_iter().enumerate() {
                    let seg = egui::Rect::from_min_max(egui::pos2(x_of(a), rect.top()), egui::pos2(x_of(b), rect.bottom()));
                    let keep = r.parts.get(i).is_none_or(|p| p.keep);
                    let tint = if !keep { egui::Color32::from_black_alpha(90) } else if i % 2 == 0 { crate::style::ACCENT.gamma_multiply(0.10) } else { crate::style::ACCENT.gamma_multiply(0.18) };
                    painter.rect_filled(seg, 0.0, tint);
                    painter.text(seg.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, format!("{}", i + 1), egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
                }
            }
            let frames = r.take.frames();
            let width = rect.width().max(1.0) as usize;
            let upto = ((seconds / span) * width as f64).ceil() as usize;
            let peaks = r.take.peaks(0, frames, upto.clamp(1, width));
            let mid = rect.center().y;
            let wave = if recording { RED.gamma_multiply(0.8) } else { crate::style::ACCENT };
            for (i, p) in peaks.iter().enumerate() {
                let x = rect.left() + i as f32 + 0.5;
                let h = (p.min(1.0) * rect.height() * 0.46).max(0.5);
                painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], egui::Stroke::new(1.0, wave));
            }
            if recording {
                let x = x_of(seconds);
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.5, RED));
            } else if frames > 0 {
                for c in &r.cuts {
                    let x = x_of(*c);
                    painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, egui::Color32::WHITE));
                    let tri = vec![egui::pos2(x - 5.0, rect.top()), egui::pos2(x + 5.0, rect.top()), egui::pos2(x, rect.top() + 7.0)];
                    painter.add(egui::Shape::convex_polygon(tri, egui::Color32::WHITE, egui::Stroke::NONE));
                }
                // Cut interactions.
                let near = |x: f32, cuts: &[f64]| cuts.iter().position(|c| (x_of(*c) - x).abs() < 6.0);
                if let Some(h) = resp.hover_pos() {
                    ui.ctx().set_cursor_icon(if near(h.x, &r.cuts).is_some() { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Text });
                }
                if resp.drag_started()
                    && let Some(p) = ui.input(|i| i.pointer.press_origin())
                {
                    r.dragging = near(p.x, &r.cuts);
                }
                if let (Some(i), Some(p)) = (r.dragging, resp.interact_pointer_pos())
                    && resp.dragged()
                    && i < r.cuts.len()
                {
                    r.cuts[i] = s_at(p.x);
                }
                if resp.drag_stopped() && r.dragging.take().is_some() {
                    r.sync_parts();
                }
                if resp.clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                    && near(p.x, &r.cuts).is_none()
                {
                    r.cuts.push(s_at(p.x));
                    r.sync_parts();
                }
                if resp.secondary_clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                    && let Some(i) = near(p.x, &r.cuts)
                {
                    r.cuts.remove(i);
                    r.sync_parts();
                }
            } else {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Press Record and talk", egui::FontId::proportional(14.0), ui.visuals().weak_text_color());
            }

            if !recording && frames > 0 {
                ui.label(egui::RichText::new("Click the waveform to cut · drag a cut to move it · right-click a cut to remove it").small().weak());
                ui.horizontal(|ui| {
                    if ui.button("✂ Split at pauses").on_hover_text("Cut in the middle of every quiet stretch").clicked() {
                        let threshold = 10f32.powf(r.threshold_db / 20.0);
                        r.cuts = r.take.pauses(threshold, r.gap);
                        r.sync_parts();
                    }
                    ui.add(egui::Slider::new(&mut r.threshold_db, -60.0..=-15.0).suffix(" dB").text("quiet below"));
                    ui.add(egui::DragValue::new(&mut r.gap).range(0.1..=3.0).speed(0.02).suffix(" s")).on_hover_text("Shortest pause that counts");
                    if !r.cuts.is_empty() && ui.button("Clear cuts").clicked() {
                        r.cuts.clear();
                        r.sync_parts();
                    }
                });

                // The parts.
                ui.separator();
                let bounds = r.bounds();
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for (i, (a, b)) in bounds.iter().enumerate() {
                        let Some(part) = r.parts.get_mut(i) else { continue };
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut part.keep, "").on_hover_text("Add this part to the bin");
                            ui.add(egui::TextEdit::singleline(&mut part.name).desired_width(200.0));
                            ui.label(egui::RichText::new(format!("{} – {} · {:.1} s", clock(*a), clock(*b), b - a)).small().weak());
                            if ui.small_button("▶").on_hover_text("Listen (click again to stop)").clicked() {
                                let path = std::env::temp_dir().join(format!("oa-recording-preview-{i}.wav"));
                                if r.take.write_wav(&path, *a, *b).is_ok() {
                                    play = Some((path, Time::from_seconds_f64(b - a)));
                                }
                            }
                        });
                    }
                });
                ui.separator();
                let kept = r.parts.iter().filter(|p| p.keep).count();
                ui.horizontal(|ui| {
                    let label = if kept == 1 { "Add 1 clip to the media bin".to_string() } else { format!("Add {kept} clips to the media bin") };
                    let button = egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE)).fill(crate::style::ACCENT).corner_radius(8.0);
                    if ui.add_enabled(kept > 0, button).clicked() {
                        action = Some("add");
                    }
                    if ui.button("Discard").clicked() {
                        action = Some("discard");
                    }
                });
            }
            if let Some(e) = &r.error {
                ui.colored_label(crate::style::ERROR, e);
            }
        });

        if let Some((path, length)) = play {
            self.audition(&path, length);
        }
        match action {
            Some("record") => {
                self.set_playing(false);
                let r = &mut self.recording;
                match Recorder::start(r.device.as_deref()) {
                    Ok(rec) => {
                        r.take = rec.new_take();
                        r.cuts.clear();
                        r.parts.clear();
                        r.error = None;
                        r.level = 0.0;
                        r.recorder = Some(rec);
                    }
                    Err(e) => r.error = Some(format!("Couldn't start recording: {e}")),
                }
            }
            Some("pause") => {
                if let Some(rec) = &self.recording.recorder {
                    rec.set_paused(!rec.is_paused());
                }
            }
            Some("stop") => {
                let r = &mut self.recording;
                if let Some(rec) = r.recorder.take() {
                    rec.stop(&mut r.take);
                }
                r.sync_parts();
            }
            Some("add") => self.add_recording_parts(),
            Some("discard") => {
                let r = &mut self.recording;
                r.take = Take::default();
                r.cuts.clear();
                r.parts.clear();
            }
            _ => {}
        }
        if !open {
            if let Some(rec) = self.recording.recorder.take() {
                rec.stop(&mut self.recording.take);
                self.recording.sync_parts();
            }
            self.recording.open = false;
        }
    }

    /// Writes the kept parts as WAV files and brings them into the bin.
    fn add_recording_parts(&mut self) {
        let dir = self.recordings_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.recording.error = Some(format!("{}: {e}", dir.display()));
            return;
        }
        let r = &self.recording;
        let mut files = Vec::new();
        for ((a, b), part) in r.bounds().into_iter().zip(&r.parts) {
            if !part.keep || b - a < 0.05 {
                continue;
            }
            let stem: String = part.name.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '-' }).collect();
            let stem = if stem.trim().is_empty() { "Recording".to_string() } else { stem.trim().to_string() };
            let mut path = dir.join(format!("{stem}.wav"));
            let mut n = 2;
            while path.exists() {
                path = dir.join(format!("{stem} {n}.wav"));
                n += 1;
            }
            match r.take.write_wav(&path, a, b) {
                Ok(()) => files.push(path),
                Err(e) => {
                    self.recording.error = Some(format!("{}: {e}", path.display()));
                    return;
                }
            }
        }
        let count = files.len();
        self.import_paths(&files, false, "Recordings");
        self.notify(format!("{count} recording{} added to the media bin (Recordings).", if count == 1 { "" } else { "s" }));
        let r = &mut self.recording;
        r.takes += 1;
        r.take = Take::default();
        r.cuts.clear();
        r.parts.clear();
    }
}
