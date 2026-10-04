//! The eyedropper: a color property's value picked from the picture in the viewer.
//!
//! The pipette beside a color arms it. The frame at the playhead is read once — without
//! what the color belongs to (that effect, or that clip for its own colors), so a key
//! color is picked from the footage rather than from what the key already did to it —
//! and the color under the pointer shows beside it; a click takes it.

use crate::i18n::tr;
use crate::preview_worker::FramePixels;
use crate::App;
use eframe::egui;
use oa_doc::{ItemId, ParamTarget};
use oa_params::Value;
use oa_time::Time;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// A color property waiting for its pick.
pub(crate) struct ColorPick {
    item: ItemId,
    target: ParamTarget,
    param: String,
    /// A gradient property (set to one color) rather than a color.
    gradient: bool,
    /// The frame read for it, and the moment it shows (read again when that changes).
    frame: Option<(Time, FramePixels)>,
    /// A frame being read: for when.
    reading: Option<(Time, Receiver<Option<FramePixels>>)>,
    /// A click made before the frame arrived: where (a fraction of the canvas).
    clicked: Option<[f64; 2]>,
}

impl ColorPick {
    /// The display color (sRGB bytes) at `at`, a fraction of the canvas.
    fn color_at(&self, at: [f64; 2]) -> Option<[u8; 3]> {
        let (_, frame) = self.frame.as_ref()?;
        let [w, h] = frame.size;
        if w == 0 || h == 0 || !(0.0..=1.0).contains(&at[0]) || !(0.0..=1.0).contains(&at[1]) {
            return None;
        }
        let (x, y) = (((at[0] * w as f64) as usize).min(w - 1), ((at[1] * h as f64) as usize).min(h - 1));
        let p = frame.pixels.get((y * w + x) * 4..(y * w + x) * 4 + 3)?;
        Some([p[0], p[1], p[2]])
    }
}

impl App {
    /// The pipette beside a color property: click, then click in the viewer.
    pub(crate) fn eyedropper_button(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, param: &str, gradient: bool) {
        let armed = self.color_pick.as_ref().is_some_and(|p| p.item == item && p.target == *target && p.param == param);
        let r = crate::widgets::eyedropper(ui, armed).on_hover_text(if armed {
            tr("Click in the viewer to take its color · Esc cancels")
        } else {
            tr("Pick this color from the picture in the viewer")
        });
        if r.clicked() {
            self.color_pick = (!armed).then(|| ColorPick { item, target: target.clone(), param: param.to_string(), gradient, frame: None, reading: None, clicked: None });
        }
    }

    /// Whether a pick is armed (the viewer hands its clicks here).
    pub(crate) fn picking_color(&self) -> bool {
        self.color_pick.is_some()
    }

    /// The viewer while a pick is armed: a crosshair with the color under it, and a
    /// click takes that color. `at` is the pointer as a fraction of the canvas.
    pub(crate) fn eyedropper_viewer(&mut self, ui: &mut egui::Ui, response: &egui::Response, at: Option<[f64; 2]>, rect: egui::Rect) {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.color_pick = None;
            return;
        }
        self.read_pick_frame();
        let Some(pick) = self.color_pick.as_mut() else { return };
        let ready = pick.frame.is_some();
        if response.hovered() {
            ui.ctx().set_cursor_icon(if ready { egui::CursorIcon::Crosshair } else { egui::CursorIcon::Progress });
        }
        ui.painter().text(
            rect.center_top() + egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            tr("Click to take a color · Esc cancels"),
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );
        // The color under the pointer, in a swatch beside it.
        if let (Some(pos), Some(at)) = (response.hover_pos(), at)
            && let Some([r, g, b]) = pick.color_at(at)
        {
            let color = egui::Color32::from_rgb(r, g, b);
            let swatch = egui::Rect::from_min_size(pos + egui::vec2(16.0, 16.0), egui::vec2(46.0, 46.0));
            let swatch = swatch.translate(egui::vec2(if swatch.right() > rect.right() { -(46.0 + 32.0) } else { 0.0 }, if swatch.bottom() > rect.bottom() { -(46.0 + 32.0) } else { 0.0 }));
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("eyedropper-swatch")));
            painter.rect_filled(swatch.expand(2.0), 6.0, egui::Color32::from_gray(20));
            painter.rect_filled(swatch, 4.0, color);
            painter.text(
                swatch.center_bottom() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_CENTER,
                format!("#{r:02X}{g:02X}{b:02X}"),
                egui::FontId::monospace(11.0),
                egui::Color32::WHITE,
            );
        }
        if response.clicked()
            && let Some(at) = at.filter(|a| (0.0..=1.0).contains(&a[0]) && (0.0..=1.0).contains(&a[1]))
        {
            pick.clicked = Some(at);
        }
        self.take_picked_color();
        if self.color_pick.as_ref().is_some_and(|p| p.frame.is_none()) {
            ui.ctx().request_repaint();
        }
    }

    /// Asks for the frame at the playhead without what the color belongs to, unless it
    /// has it (or is reading it) already.
    fn read_pick_frame(&mut self) {
        let t = self.playhead;
        let Some(pick) = self.color_pick.as_ref() else { return };
        if pick.frame.as_ref().is_some_and(|f| f.0 == t) || pick.reading.as_ref().is_some_and(|r| r.0 == t) {
            return;
        }
        let mut project = (*self.editor.doc.snapshot()).clone();
        if let Some(s) = project.sequences.get_mut(&self.editor.seq) {
            for track in &mut Arc::make_mut(s).tracks {
                if !track.items.iter().any(|i| i.id == pick.item) {
                    continue;
                }
                if let Some(it) = Arc::make_mut(track).items.iter_mut().find(|i| i.id == pick.item) {
                    match &pick.target {
                        ParamTarget::Effect(e) => it.effects.iter_mut().filter(|fx| fx.id == *e).for_each(|fx| fx.enabled = false),
                        _ => it.enabled = false,
                    }
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.preview_worker.pixels(
            crate::preview_worker::Request {
                slot: 0,
                tag: 0,
                project: Arc::new(project),
                registry: self.registry.clone(),
                seq: self.editor.seq,
                variant: self.variant_id(),
                at: t,
                scale: 1.0,
                wanted: None,
                png: None,
                see_through: false,
            },
            tx,
        );
        if let Some(p) = self.color_pick.as_mut() {
            p.reading = Some((t, rx));
        }
    }

    /// A frame that's arrived for the pick, kept for hovering; and a click waiting for
    /// it, taken.
    pub(crate) fn poll_eyedropper(&mut self) {
        let Some(pick) = self.color_pick.as_mut() else { return };
        if let Some((t, rx)) = pick.reading.as_ref() {
            match rx.try_recv() {
                Ok(Some(frame)) => {
                    pick.frame = Some((*t, frame));
                    pick.reading = None;
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.color_pick = None;
                    self.error = Some(tr("Couldn't read the picture for the color").into());
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        self.take_picked_color();
    }

    /// With a click and its frame: the color there becomes the property's (its alpha
    /// kept), and the pipette puts itself away.
    fn take_picked_color(&mut self) {
        let Some(pick) = self.color_pick.as_ref() else { return };
        let Some(at) = pick.clicked else { return };
        let Some([r, g, b]) = pick.color_at(at) else {
            if pick.frame.is_some() {
                // Off the picture.
                self.color_pick.as_mut().expect("checked").clicked = None;
            }
            return;
        };
        let pick = self.color_pick.take().expect("checked");
        // Colors are kept in linear light (as egui's color pickers edit them).
        let linear = |c: u8| egui::ecolor::linear_f32_from_gamma_u8(c) as f64;
        let rgb = [linear(r), linear(g), linear(b)];
        let t = self.playhead;
        let current = self.editor.param_value(pick.item, &pick.target, &pick.param, t);
        let value = if pick.gradient {
            let g = current.as_ref().and_then(|v| v.as_gradient()).cloned().unwrap_or_else(|| oa_params::Gradient::solid([1.0; 4]));
            let alpha = g.sorted_stops().first().map_or(1.0, |s| s.color[3]);
            Value::Gradient(oa_params::Gradient { angle: g.angle, stops: vec![oa_params::GradientStop { pos: 0.0, color: [rgb[0], rgb[1], rgb[2], alpha] }] })
        } else {
            let alpha = current.and_then(|v| if let Value::Color(c) = v { Some(c[3]) } else { None }).unwrap_or(1.0);
            Value::Color([rgb[0], rgb[1], rgb[2], alpha])
        };
        self.editor.set_value_at(pick.item, pick.target, &pick.param, value, t, "Pick color");
        self.editor.doc.seal();
    }
}
