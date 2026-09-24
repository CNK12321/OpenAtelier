//! The wave editor: a property that swings back and forth on its own (an LFO) — its
//! shape (sine, triangle, square, saw), how fast (frequency), between which values
//! (min/max, or a gain around its keyframes), where in the cycle it starts (phase) and
//! how quickly it settles down (decay), with the result drawn over the clip. Opened from
//! a property's right-click menu ("Edit wave…"), below "Edit curve…".

use crate::band::Band;
use crate::App;
use eframe::egui;
use oa_doc::ItemId;
use oa_params::{LfoWave, Modulator, ParamSource, Value};
use oa_time::Time;

/// The open wave editor.
pub struct WaveEditor {
    pub item: ItemId,
    pub band: Band,
}

const WAVES: [(LfoWave, &str); 4] = [(LfoWave::Sine, "Sine"), (LfoWave::Triangle, "Triangle"), (LfoWave::Square, "Square"), (LfoWave::Saw, "Saw")];

/// A little drawing of one cycle of `wave`, as a selectable button.
fn wave_button(ui: &mut egui::Ui, wave: LfoWave, name: &str, on: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(58.0, 40.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&r, on);
    ui.painter().rect(rect, 4.0, if on { ui.visuals().selection.bg_fill } else { visuals.bg_fill }, visuals.bg_stroke, egui::StrokeKind::Inside);
    let g = egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 5.0), egui::pos2(rect.max.x - 8.0, rect.min.y + 22.0));
    let pts: Vec<egui::Pos2> = (0..=48)
        .map(|i| {
            let u = i as f64 / 48.0;
            egui::pos2(g.left() + u as f32 * g.width(), g.center().y - wave.at(u) as f32 * g.height() / 2.0)
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.5, if on { egui::Color32::WHITE } else { crate::style::ACCENT })));
    ui.painter().text(egui::pos2(rect.center().x, rect.bottom() - 4.0), egui::Align2::CENTER_BOTTOM, name, egui::FontId::proportional(10.0), visuals.text_color());
    r
}

impl App {
    pub(crate) fn edit_wave(&mut self, item: ItemId, band: Band) {
        self.wave_editor = Some(WaveEditor { item, band });
    }

    pub(crate) fn wave_window(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.wave_editor.as_ref() else { return };
        let (item, band) = (ed.item, ed.band.clone());
        let Some(it) = self.editor.item(item).cloned() else {
            self.wave_editor = None;
            return;
        };
        let mut open = true;
        let title = format!("Wave — {} · {}", it.name, band.param.rsplit('.').next().unwrap_or(&band.param));
        egui::Window::new(title)
            .id(egui::Id::new("wave-editor"))
            .open(&mut open)
            .default_width(460.0)
            .resizable(true)
            .show(ctx, |ui| self.wave_body(ui, item, &band, &it));
        if !open {
            self.wave_editor = None;
            self.editor.doc.seal();
        }
    }

    fn wave_body(&mut self, ui: &mut egui::Ui, item: ItemId, band: &Band, it: &oa_doc::Item) {
        let start = it.eval_context(it.range.start);
        let current = self.editor.param_value(item, &band.target, &band.param, it.range.start).unwrap_or(Value::Float(band.lo.max(0.0)));
        let source = self.editor.param_source(item, &band.target, &band.param).unwrap_or(ParamSource::Static(current.clone()));
        let Some((base, Modulator::Lfo { wave, amplitude, frequency, phase, decay, .. })) = source.find_lfo().map(|(b, m)| (b.clone(), m.clone())) else {
            ui.label("Makes this value swing back and forth on its own — a pulse, a bob, a flicker.");
            if ui.button("Add a wave").clicked() {
                let swing = ((band.hi - band.lo) * 0.1).abs().max(1e-3);
                let src = source.lfo(LfoWave::Sine, swing, 1.0);
                self.editor.set_param(item, band.target.clone(), &band.param, src, "wave-editor");
                self.editor.doc.seal();
            }
            return;
        };
        let read = |s: &ParamSource| s.eval(&start).as_float().unwrap_or(0.0);
        let (mut shape, mut amp, mut freq, mut deg, mut dec) = (wave, read(&amplitude), read(&frequency), phase * 360.0, decay);
        // With a plain value underneath, the swing is shown as min and max; over
        // keyframes it's a gain around them.
        let plain = match &base {
            ParamSource::Static(Value::Float(v)) => Some(*v),
            _ => None,
        };
        let (mut lo, mut hi) = plain.map_or((0.0, 0.0), |c| (c - amp.abs(), c + amp.abs()));
        let mut changed = false;
        let mut finished = false;
        let track = |r: &egui::Response, changed: &mut bool, finished: &mut bool| {
            *changed |= r.changed();
            *finished |= r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
        };

        ui.horizontal(|ui| {
            for (w, name) in WAVES {
                if wave_button(ui, w, name, shape == w).clicked() && shape != w {
                    shape = w;
                    changed = true;
                    finished = true;
                }
            }
        });
        ui.add_space(4.0);
        egui::Grid::new("wave-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Frequency");
            let r = ui.add(egui::Slider::new(&mut freq, 0.01..=30.0).logarithmic(true).suffix(" Hz").clamping(egui::SliderClamping::Edits));
            track(&r, &mut changed, &mut finished);
            ui.end_row();
            if plain.is_some() {
                ui.label("Min");
                let r = ui.add(egui::DragValue::new(&mut lo).speed((band.hi - band.lo).abs() * 0.005 + 1e-3).max_decimals(3));
                track(&r, &mut changed, &mut finished);
                ui.end_row();
                ui.label("Max");
                let r = ui.add(egui::DragValue::new(&mut hi).speed((band.hi - band.lo).abs() * 0.005 + 1e-3).max_decimals(3));
                track(&r, &mut changed, &mut finished);
                ui.end_row();
            } else {
                ui.label("Gain");
                let r = ui.add(egui::DragValue::new(&mut amp).speed((band.hi - band.lo).abs() * 0.005 + 1e-3).range(0.0..=f64::MAX).max_decimals(3))
                    .on_hover_text("How far it swings either side of its keyframed value");
                track(&r, &mut changed, &mut finished);
                ui.end_row();
            }
            ui.label("Phase");
            let r = ui.add(egui::Slider::new(&mut deg, 0.0..=360.0).suffix("°"));
            track(&r, &mut changed, &mut finished);
            ui.end_row();
            ui.label("Decay");
            ui.horizontal(|ui| {
                let r = ui.add(egui::Slider::new(&mut dec, 0.0..=5.0).suffix(" /s").clamping(egui::SliderClamping::Edits));
                track(&r, &mut changed, &mut finished);
                let note = if dec <= 0.0 { "steady".to_string() } else { format!("halves every {:.2} s", std::f64::consts::LN_2 / dec) };
                ui.label(egui::RichText::new(note).small().weak());
            });
            ui.end_row();
        });

        // The result over the clip.
        let (outer, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 110.0), egui::Sense::hover());
        let painter = ui.painter_at(outer);
        painter.rect_filled(outer, 4.0, ui.visuals().extreme_bg_color);
        let rect = outer.shrink2(egui::vec2(8.0, 10.0));
        let duration = it.range.duration;
        let samples: Vec<f64> = (0..=240)
            .map(|i| {
                let t = it.range.start + Time::from_seconds_f64(duration.as_seconds_f64() * i as f64 / 240.0).min(duration - Time(1));
                self.editor.param_value(item, &band.target, &band.param, t).and_then(|v| v.as_float()).unwrap_or(0.0)
            })
            .collect();
        let (vmin, vmax) = samples.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
        let pad = ((vmax - vmin) * 0.1).max(1e-3);
        let (vlo, vhi) = (vmin - pad, vmax + pad);
        let y = |v: f64| rect.bottom() - ((v - vlo) / (vhi - vlo)) as f32 * rect.height();
        for v in [vmin, vmax] {
            painter.line_segment([egui::pos2(rect.left(), y(v)), egui::pos2(rect.right(), y(v))], egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.4)));
            painter.text(egui::pos2(rect.left() + 2.0, y(v) - 1.0), egui::Align2::LEFT_BOTTOM, format!("{v:.2}"), egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
        }
        let pts: Vec<egui::Pos2> = samples.iter().enumerate().map(|(i, v)| egui::pos2(rect.left() + rect.width() * i as f32 / 240.0, y(*v))).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, crate::style::ACCENT)));
        let at = ((self.playhead - it.range.start).as_seconds_f64() / duration.as_seconds_f64().max(1e-9)).clamp(0.0, 1.0) as f32;
        let px = rect.left() + at * rect.width();
        painter.line_segment([egui::pos2(px, outer.top()), egui::pos2(px, outer.bottom())], egui::Stroke::new(1.0, egui::Color32::from_rgb(230, 70, 70)));

        ui.horizontal(|ui| {
            if ui.button("Remove wave").clicked() {
                let src = source.clone().remove_lfo();
                self.editor.set_param(item, band.target.clone(), &band.param, src, "wave-editor");
                self.editor.doc.seal();
            }
            ui.label(egui::RichText::new("Keyframes still work: the wave rides on top of them.").small().weak());
        });

        if changed {
            if plain.is_some() {
                if hi < lo {
                    std::mem::swap(&mut lo, &mut hi);
                }
                amp = (hi - lo) / 2.0;
            }
            let mut src = source.clone();
            if let Some((b, Modulator::Lfo { wave, amplitude, frequency, phase, decay, .. })) = src.find_lfo_mut() {
                if plain.is_some() {
                    *b = ParamSource::Static(Value::Float((lo + hi) / 2.0));
                }
                *wave = shape;
                **amplitude = ParamSource::Static(Value::Float(amp.max(0.0)));
                **frequency = ParamSource::Static(Value::Float(if freq.is_finite() { freq.clamp(0.0, 1000.0) } else { 1.0 }));
                *phase = (deg / 360.0).rem_euclid(1.0);
                *decay = dec.max(0.0);
            }
            self.editor.set_param(item, band.target.clone(), &band.param, src, "wave-editor");
        }
        if finished {
            self.editor.doc.seal();
        }
    }
}
