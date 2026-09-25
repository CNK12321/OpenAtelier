//! Sound effect cards that draw more than sliders: the equalizer's curve with its
//! draggable bands, and the live level meters on dynamics and stereo effects.

use crate::App;
use eframe::egui;
use oa_audio::fx::{eq_bands, eq_response_db, BandShape};
use oa_doc::{EffectInstance, ItemId, ParamTarget};
use oa_params::Value;
use oa_time::Time;

const LOW_HZ: f32 = 20.0;
const HIGH_HZ: f32 = 20_000.0;
const RANGE_DB: f32 = 18.0;
/// The rate the curve is drawn at (the response barely changes with it below 20 kHz).
const DRAW_RATE: f32 = 48_000.0;

/// Whether an effect's card shows a live meter (its manifest's `meter`), and what the
/// meter's third row shows.
fn meter_kind(type_id: &str) -> Option<MeterKind> {
    match oa_audio::fx::info(type_id)?.meter.as_deref()? {
        oa_graph::registry::METER_REDUCTION => Some(MeterKind::Reduction),
        oa_graph::registry::METER_CORRELATION => Some(MeterKind::Correlation),
        _ => None,
    }
}

enum MeterKind {
    Reduction,
    Correlation,
}

fn x_of(freq: f32, rect: egui::Rect) -> f32 {
    let u = (freq.max(LOW_HZ).ln() - LOW_HZ.ln()) / (HIGH_HZ.ln() - LOW_HZ.ln());
    rect.left() + u.clamp(0.0, 1.0) * rect.width()
}

fn freq_of(x: f32, rect: egui::Rect) -> f32 {
    let u = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    (LOW_HZ.ln() + u * (HIGH_HZ.ln() - LOW_HZ.ln())).exp()
}

fn y_of(db: f32, rect: egui::Rect) -> f32 {
    rect.center().y - (db / RANGE_DB).clamp(-1.0, 1.0) * rect.height() * 0.5
}

fn db_of(y: f32, rect: egui::Rect) -> f32 {
    ((rect.center().y - y) / (rect.height() * 0.5) * RANGE_DB).clamp(-RANGE_DB, RANGE_DB)
}

impl App {
    /// The equalizer's card: its response curve with one point per band. Drag a point
    /// to move its frequency and gain, scroll over a middle band to widen or narrow it,
    /// double-click a point to flatten it. The numbers stay underneath.
    pub(crate) fn eq_settings(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &EffectInstance, d: &oa_graph::registry::EffectDescriptor, t: Time) {
        let target = ParamTarget::Effect(fx.id);
        let salt = fx.id.0.to_string();
        let Some(it) = self.editor.item(item) else { return };
        let v = fx.params.eval(&d.params, None, &it.eval_context(t));
        let bands = eq_bands(&v);

        let width = ui.available_width().max(120.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 110.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        let grid = visuals.weak_text_color().gamma_multiply(0.35);
        for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
            let x = x_of(f, rect);
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, grid));
        }
        for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            let y = y_of(db, rect);
            let stroke = if db == 0.0 { egui::Stroke::new(1.0, visuals.weak_text_color()) } else { egui::Stroke::new(1.0, grid) };
            painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], stroke);
        }
        let small = egui::FontId::proportional(9.0);
        for (f, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            painter.text(egui::pos2(x_of(f, rect) + 2.0, rect.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, label, small.clone(), visuals.weak_text_color());
        }

        // The curve.
        let accent = visuals.selection.bg_fill;
        let steps = (rect.width() as usize / 2).max(32);
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let x = rect.left() + rect.width() * i as f32 / steps as f32;
                egui::pos2(x, y_of(eq_response_db(&v, freq_of(x, rect), DRAW_RATE), rect))
            })
            .collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, accent)));

        // The band points.
        let mut writes: Vec<(&'static str, f64)> = Vec::new();
        let mut released = false;
        for (i, band) in bands.iter().enumerate() {
            let center = egui::pos2(x_of(band.freq, rect), y_of(band.gain_db, rect));
            let hit = egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0));
            let r = ui.interact(hit, ui.id().with(("eq-band", fx.id.0, i)), egui::Sense::click_and_drag());
            let what = match band.shape {
                BandShape::LowShelf => "Low shelf",
                BandShape::HighShelf => "High shelf",
                BandShape::Peak => "Band",
            };
            let tip = format!("{what}: {:.0} Hz, {:+.1} dB{}\nDrag to move it{}; double-click to flatten it", band.freq, band.gain_db, if band.shape == BandShape::Peak { format!(", width {:.2}", band.q) } else { String::new() }, if band.shape == BandShape::Peak { ", scroll to widen or narrow it" } else { "" });
            let r = r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::Grab);
            if r.dragged()
                && let Some(p) = r.interact_pointer_pos()
            {
                writes.push((band.ids[0], freq_of(p.x, rect).round() as f64));
                writes.push((band.ids[1], (db_of(p.y, rect) * 10.0).round() as f64 / 10.0));
            }
            if r.double_clicked() {
                writes.push((band.ids[1], 0.0));
                released = true;
            }
            if r.hovered() && band.shape == BandShape::Peak {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let q = (band.q * (1.0 + scroll * 0.004)).clamp(0.2, 10.0);
                    writes.push((band.ids[2], (q * 100.0).round() as f64 / 100.0));
                }
            }
            released |= r.drag_stopped();
            let active = r.hovered() || r.dragged();
            let color = if band.gain_db.abs() > 0.01 { accent } else { visuals.weak_text_color() };
            painter.circle_filled(center, if active { 6.0 } else { 4.5 }, color);
            painter.circle_stroke(center, if active { 6.0 } else { 4.5 }, egui::Stroke::new(1.0, visuals.strong_text_color()));
        }
        for (param, value) in writes {
            if param.is_empty() {
                continue;
            }
            let value = match d.params.iter().find(|s| s.id.as_str() == param).and_then(|s| s.range) {
                Some((lo, hi)) => value.clamp(lo, hi),
                None => value,
            };
            self.editor.set_value_at(item, target.clone(), param, Value::Float(value), t, &format!("eq-{}-{param}", fx.id.0));
        }
        if released {
            self.editor.doc.seal();
        }

        egui::CollapsingHeader::new(egui::RichText::new("Numbers").small()).id_salt(("eq-numbers", fx.id.0)).show(ui, |ui| {
            for schema in &d.params {
                self.param_widget(ui, item, &target, schema, t, &salt);
            }
        });
    }

    /// Live levels for a dynamics or stereo effect while it plays: what goes in, what
    /// comes out, and how much it's turning down (or, for width, how mono-safe it is).
    pub(crate) fn sound_meter(&mut self, ui: &mut egui::Ui, fx: &EffectInstance) {
        let Some(kind) = meter_kind(&fx.type_id) else { return };
        let Some(m) = oa_audio::fx::meter(fx.id.0) else {
            ui.label(egui::RichText::new("Play to see its levels").small().weak());
            return;
        };
        // Keep the bars moving while sound is flowing; meters go stale on their own.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        let level = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let green = egui::Color32::from_rgb(90, 180, 110);
        let amber = egui::Color32::from_rgb(220, 160, 60);
        let red = egui::Color32::from_rgb(220, 80, 70);
        let hot = |db: f32| if db > -1.0 { red } else if db > -9.0 { amber } else { green };
        let row = |ui: &mut egui::Ui, label: &str, fraction: f32, from_right: bool, color: egui::Color32, value: String| {
            ui.horizontal(|ui| {
                ui.add_sized([34.0, 12.0], egui::Label::new(egui::RichText::new(label).small().weak()));
                let (rect, _) = ui.allocate_exact_size(egui::vec2((ui.available_width() - 52.0).max(40.0), 8.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                let w = rect.width() * fraction.clamp(0.0, 1.0);
                let bar = if from_right { egui::Rect::from_min_max(egui::pos2(rect.right() - w, rect.top()), rect.max) } else { egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + w, rect.bottom())) };
                ui.painter().rect_filled(bar, 2.0, color);
                ui.label(egui::RichText::new(value).small().monospace());
            });
        };
        row(ui, "in", level(m.input_db), false, hot(m.input_db), format!("{:>5.1}", m.input_db.max(-99.0)));
        row(ui, "out", level(m.output_db), false, hot(m.output_db), format!("{:>5.1}", m.output_db.max(-99.0)));
        match kind {
            MeterKind::Reduction => {
                let gr = m.reduction_db.abs();
                row(ui, "cut", gr / 24.0, true, amber, format!("{:>5.1}", -gr));
            }
            MeterKind::Correlation => {
                // +1 is mono-safe, 0 is wide, below 0 cancels when summed to mono.
                let c = m.correlation.clamp(-1.0, 1.0);
                let color = if c < 0.0 { red } else if c < 0.3 { amber } else { green };
                row(ui, "phase", (c + 1.0) / 2.0, false, color, format!("{c:>+5.2}"));
            }
        }
    }
}
