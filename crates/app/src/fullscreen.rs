//! Fullscreen playback: the frame alone, fitted to the screen on black, the window made
//! fullscreen. Controls (play/pause, the time, a scrub bar, leave) show while the pointer
//! moves and fade after a moment. Space plays and pauses, ←/→ step, F or Esc leaves.
//! Everything else keeps running as usual — the frame is rendered at the screen's size
//! (Auto resolution follows what's shown), sound plays, edits made elsewhere show.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_time::Time;

/// Seconds the controls stay after the pointer last moved.
const CONTROLS_FOR: f64 = 2.0;

#[derive(Default)]
pub struct Fullscreen {
    pub on: bool,
    /// UI time the pointer last moved (controls show until `CONTROLS_FOR` after).
    moved_at: f64,
    last_pointer: Option<egui::Pos2>,
}

impl App {
    /// In or out of fullscreen playback.
    pub(crate) fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen.on == on {
            return;
        }
        self.fullscreen.on = on;
        self.fullscreen.moved_at = ctx.input(|i| i.time);
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        self.rendered = None;
    }

    /// The whole window, while fullscreen: just the picture and its controls.
    pub(crate) fn fullscreen_view(&mut self, root: &mut egui::Ui, frame: &eframe::Frame) {
        let ctx = root.ctx().clone();
        let leave = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) || i.consume_key(egui::Modifiers::NONE, egui::Key::F));
        if leave {
            self.set_fullscreen(&ctx, false);
            return;
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::BLACK)).show(root, |ui| {
            self.refresh_follower(ui.ctx());
            self.refine_when_idle(ui.ctx());
            self.preview_worker.set_busy(self.playing);
            if let Some(render_state) = frame.wgpu_render_state() {
                self.receive_frames(render_state);
            }
            let stale = self.preview.is_none() || self.rendered.as_ref() != Some(&self.frame_key());
            if stale && !self.gpu_lost {
                self.request_frame();
            }
            let area = ui.max_rect();
            let canvas = {
                let s = self.editor.sequence();
                s.variants[self.variant.min(s.variants.len() - 1)].size
            };
            let fit = (area.width() / canvas.width.max(1) as f32).min(area.height() / canvas.height.max(1) as f32);
            let rect = egui::Rect::from_center_size(area.center(), egui::vec2(canvas.width as f32 * fit, canvas.height as f32 * fit));
            // Auto resolution renders what the screen shows.
            self.display_scale = fit * ui.ctx().pixels_per_point();
            let response = ui.allocate_rect(area, egui::Sense::click());
            if let Some(p) = &self.preview {
                ui.painter().image(p.id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            if response.double_clicked() {
                self.set_fullscreen(&ctx, false);
                return;
            }
            if response.clicked() {
                self.set_playing(!self.playing);
                self.last_tick = std::time::Instant::now();
            }
            self.fullscreen_controls(ui, area);
        });
        // As the editor does (this view returns before its end): playing asks for the next
        // frame at once, and a frame still on its way is looked for again shortly. Without
        // this, fullscreen only redrew when the mouse moved — the lag.
        if self.rendered.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
        if self.playing {
            ctx.request_repaint();
        }
    }

    /// Play/pause, the time and a scrub bar along the bottom, and a way out — while the
    /// pointer moves.
    fn fullscreen_controls(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let now = ui.input(|i| i.time);
        let pointer = ui.input(|i| i.pointer.latest_pos());
        if pointer != self.fullscreen.last_pointer {
            self.fullscreen.last_pointer = pointer;
            self.fullscreen.moved_at = now;
        }
        let shown = now - self.fullscreen.moved_at;
        if shown > CONTROLS_FOR {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            return;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        let bar = egui::Rect::from_min_max(egui::pos2(area.left() + 24.0, area.bottom() - 64.0), egui::pos2(area.right() - 24.0, area.bottom() - 16.0));
        ui.painter().rect_filled(bar, 10.0, egui::Color32::from_black_alpha(170));
        let duration = self.editor.duration();
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(12.0, 8.0))), |ui| {
            ui.horizontal_centered(|ui| {
                let (icon, tip) = if self.playing { (crate::icons::PAUSE, "Pause (Space)") } else { (crate::icons::PLAY, "Play (Space)") };
                if crate::icons::button(ui, icon, tip, "Space", true).clicked() {
                    self.set_playing(!self.playing);
                    self.last_tick = std::time::Instant::now();
                }
                let clock = |t: Time| {
                    let s = t.as_seconds_f64().max(0.0);
                    format!("{}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
                };
                ui.label(egui::RichText::new(trf("{0} / {1}", &[("0", &(clock(self.playhead)).to_string()), ("1", &(clock(duration)).to_string())])).color(egui::Color32::WHITE).monospace());
                // The scrub bar fills what's left, before the way out.
                let width = (ui.available_width() - 110.0).max(40.0);
                let (track, r) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::click_and_drag());
                let line = egui::Rect::from_center_size(track.center(), egui::vec2(track.width(), 4.0));
                ui.painter().rect_filled(line, 2.0, egui::Color32::from_white_alpha(60));
                let f = (self.playhead.as_seconds_f64() / duration.as_seconds_f64().max(1e-6)).clamp(0.0, 1.0) as f32;
                let done = egui::Rect::from_min_max(line.min, egui::pos2(line.left() + line.width() * f, line.bottom()));
                ui.painter().rect_filled(done, 2.0, crate::style::ACCENT);
                ui.painter().circle_filled(egui::pos2(done.right(), line.center().y), 6.0, egui::Color32::WHITE);
                if r.drag_started() {
                    self.begin_scrub();
                }
                if (r.dragged() || r.clicked())
                    && let Some(p) = r.interact_pointer_pos()
                {
                    let at = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) as f64;
                    self.set_playhead(Time::from_seconds_f64(at * duration.as_seconds_f64()));
                }
                if r.drag_stopped() {
                    self.end_scrub();
                }
                if ui.button(tr("Leave fullscreen")).on_hover_text(tr("F, Esc or double-click")).clicked() {
                    let ctx = ui.ctx().clone();
                    self.set_fullscreen(&ctx, false);
                }
            });
        });
    }
}
