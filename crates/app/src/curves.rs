//! The curve editor: one number property's animation drawn as a graph over its clip —
//! value up, time across. Drag keys (time and value), drag the bezier handles either
//! side of the selected key to shape its easing, pick an easing preset, double-click to
//! add a key, Delete to remove one. Opened from a property's right-click menu
//! ("Edit curve…"); the keyframe line on the timeline shows the same curve.

use crate::band::Band;
use crate::App;
use eframe::egui;
use oa_doc::ItemId;
use oa_params::{Curve, Ease, Interp, KeyframeAnchor, ParamSource, Value};
use oa_time::Time;

/// The open curve editor.
pub struct CurveEditor {
    pub item: ItemId,
    pub band: Band,
    /// Index of the selected key.
    selected: Option<usize>,
    /// What's being dragged, with the curve as it was when the drag began.
    drag: Option<(Grab, Curve)>,
    /// Where the window was last drawn (keys pressed over it are its own).
    rect: Option<egui::Rect>,
}

#[derive(Copy, Clone, PartialEq)]
enum Grab {
    Key(usize),
    /// The handle leaving key `i` (`Interp::Bezier::out`).
    Out(usize),
    /// The handle arriving at key `i + 1` (`Interp::Bezier::into` of key `i`).
    Into(usize),
}

impl CurveEditor {
    pub fn new(item: ItemId, band: Band) -> Self {
        CurveEditor { item, band, selected: None, drag: None, rect: None }
    }
}

/// Bezier handles: leaving x, y, arriving x, y (segment-normalized).
type Handles4 = (f64, f64, f64, f64);

/// Easing presets for the segment after a key (`None`: hold).
const PRESETS: [(&str, Option<Handles4>); 6] = [
    ("Hold", None),
    ("Linear", Some((0.0, 0.0, 1.0, 1.0))),
    ("Ease in", Some((0.42, 0.0, 1.0, 1.0))),
    ("Ease out", Some((0.0, 0.0, 0.58, 1.0))),
    ("Ease in-out", Some((0.42, 0.0, 0.58, 1.0))),
    ("Overshoot", Some((0.34, 0.0, 0.3, 1.35))),
];

fn preset_interp(p: Option<Handles4>) -> Interp {
    match p {
        None => Interp::Hold,
        Some((0.0, 0.0, 1.0, 1.0)) => Interp::Linear,
        Some((a, b, c, d)) => Interp::Bezier { out: Ease { x: a, y: b }, into: Ease { x: c, y: d } },
    }
}

/// Maps between the curve's (time, value) and the graph's screen rect.
struct View {
    rect: egui::Rect,
    duration: f64,
    lo: f64,
    hi: f64,
}

impl View {
    fn x(&self, t: Time) -> f32 {
        self.rect.left() + (t.as_seconds_f64() / self.duration.max(1e-9)) as f32 * self.rect.width()
    }
    fn y(&self, v: f64) -> f32 {
        self.rect.bottom() - ((v - self.lo) / (self.hi - self.lo).max(1e-9)) as f32 * self.rect.height()
    }
    fn t(&self, x: f32) -> Time {
        Time::from_seconds_f64(((x - self.rect.left()) / self.rect.width()) as f64 * self.duration)
    }
    fn v(&self, y: f32) -> f64 {
        self.lo + ((self.rect.bottom() - y) / self.rect.height()) as f64 * (self.hi - self.lo)
    }
}

impl App {
    /// Opens the curve editor on a clip's number property.
    pub(crate) fn edit_curve(&mut self, item: ItemId, band: Band) {
        self.curve_editor = Some(CurveEditor::new(item, band));
    }

    /// Whether the pointer is over the curve editor (its keys, not the timeline's).
    pub(crate) fn pointer_on_curve_editor(&self, ctx: &egui::Context) -> bool {
        let rect = self.curve_editor.as_ref().and_then(|e| e.rect);
        rect.zip(ctx.pointer_hover_pos()).is_some_and(|(r, p)| r.contains(p))
    }

    /// The curve editor window, when open.
    pub(crate) fn curve_window(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.curve_editor.as_ref() else { return };
        let (item, band) = (ed.item, ed.band.clone());
        let Some(it) = self.editor.item(item).cloned() else {
            self.curve_editor = None;
            return;
        };
        let mut open = true;
        let title = format!("Curve — {} · {}", it.name, band.param.rsplit('.').next().unwrap_or(&band.param));
        let shown = egui::Window::new(title)
            .id(egui::Id::new("curve-editor"))
            .open(&mut open)
            .default_size([560.0, 300.0])
            .min_size([320.0, 180.0])
            .resizable(true)
            .show(ctx, |ui| self.curve_body(ui, item, &band, &it))
            .map(|r| r.response.rect);
        if let Some(ed) = self.curve_editor.as_mut() {
            ed.rect = shown;
        }
        if !open {
            self.curve_editor = None;
            self.editor.doc.seal();
        }
    }

    fn curve_body(&mut self, ui: &mut egui::Ui, item: ItemId, band: &Band, it: &oa_doc::Item) {
        let source = self.editor.param_source(item, &band.target, &band.param);
        let at = (self.playhead - it.range.start).max(Time::ZERO).min(it.range.duration - Time(1));
        let playhead_ctx = it.eval_context(it.range.start + at);
        let current = self.editor.param_value(item, &band.target, &band.param, it.range.start + at).unwrap_or(Value::Float(band.lo.max(0.0)));
        let curve = source.as_ref().and_then(|s| s.curve()).filter(|c| c.anchor == KeyframeAnchor::ClipStart).cloned();
        let Some(mut curve) = curve else {
            if source.as_ref().and_then(|s| s.curve()).is_some() {
                ui.label("This property is keyed to the source media's clock; edit it on the timeline.");
                return;
            }
            ui.label("Not animated yet.");
            if ui.button("Animate: add a key at the playhead").clicked() {
                let mut src = source.unwrap_or(ParamSource::Static(current.clone())).keyframed(&playhead_ctx, KeyframeAnchor::ClipStart);
                src.set_at(&playhead_ctx, current);
                self.editor.set_param(item, band.target.clone(), &band.param, src, "curve-editor");
                self.editor.doc.seal();
            }
            return;
        };
        let Some(ed) = self.curve_editor.as_mut() else { return };
        ed.selected = ed.selected.filter(|i| *i < curve.keys.len());
        let mut changed = false;
        let mut finished = false;

        // Toolbar: easing for the selected key's next segment, add/remove.
        ui.horizontal_wrapped(|ui| {
            let sel = ed.selected.filter(|i| *i + 1 < curve.keys.len());
            ui.label(egui::RichText::new("Easing after the key:").small().weak());
            // The default curve (Settings), which removes the "custom" mark.
            let default = oa_params::default_interp();
            let on = sel.is_some_and(|i| curve.keys[i].interp == default);
            if ui.add_enabled(sel.is_some(), egui::Button::selectable(on, "Default")).on_hover_text("The default curve from Settings").clicked()
                && let Some(i) = sel
            {
                curve.keys[i].interp = default;
                changed = true;
                finished = true;
            }
            for (name, p) in PRESETS {
                let interp = preset_interp(p);
                let on = sel.is_some_and(|i| curve.keys[i].interp == interp);
                if ui.add_enabled(sel.is_some(), egui::Button::selectable(on, name)).clicked()
                    && let Some(i) = sel
                {
                    curve.keys[i].interp = interp;
                    changed = true;
                    finished = true;
                }
            }
            ui.separator();
            if ui.button("Key at playhead").on_hover_text("Adds a key (or updates the one there) with the value at the playhead").clicked() {
                let v = curve.eval_at(at);
                curve.set_value_at(at, v);
                ed.selected = curve.keys.iter().position(|k| k.t == at);
                changed = true;
                finished = true;
            }
            let removable = ed.selected.is_some() && curve.keys.len() > 1;
            if ui.add_enabled(removable, egui::Button::new("Delete key")).on_hover_text("Or press Delete").clicked()
                && let Some(i) = ed.selected
            {
                curve.keys.remove(i);
                ed.selected = None;
                changed = true;
                finished = true;
            }
        });

        // The graph.
        let size = ui.available_size().max(egui::vec2(200.0, 120.0));
        let (outer, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let rect = outer.shrink2(egui::vec2(10.0, 12.0));
        let values: Vec<f64> = curve.keys.iter().filter_map(|k| k.value.as_float()).collect();
        let (vmin, vmax) = values.iter().fold((band.lo, band.hi), |(a, b), v| (a.min(*v), b.max(*v)));
        let pad = ((vmax - vmin) * 0.08).max(1e-3);
        let view = View { rect, duration: it.range.duration.as_seconds_f64(), lo: vmin - pad, hi: vmax + pad };
        let painter = ui.painter_at(outer);
        let visuals = ui.visuals().clone();
        painter.rect_filled(outer, 4.0, visuals.extreme_bg_color);
        // Grid: the property's own range, and a line per second.
        for v in [band.lo, band.hi] {
            painter.line_segment([egui::pos2(rect.left(), view.y(v)), egui::pos2(rect.right(), view.y(v))], egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.4)));
            painter.text(egui::pos2(rect.left() + 2.0, view.y(v) - 2.0), egui::Align2::LEFT_BOTTOM, format!("{v:.2}"), egui::FontId::proportional(10.0), visuals.weak_text_color());
        }
        let mut s = 1.0;
        while s < view.duration {
            let x = view.x(Time::from_seconds_f64(s));
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.2)));
            s += 1.0;
        }
        // Playhead.
        let px = view.x(at);
        painter.line_segment([egui::pos2(px, outer.top()), egui::pos2(px, outer.bottom())], egui::Stroke::new(1.0, egui::Color32::from_rgb(230, 70, 70)));

        let key_pos = |c: &Curve, i: usize| egui::pos2(view.x(c.keys[i].t), view.y(c.keys[i].value.as_float().unwrap_or(0.0)));
        // Handle positions of segment i (key i → i + 1), when it's a bezier with rise.
        let handles = |c: &Curve, i: usize| -> Option<(egui::Pos2, egui::Pos2)> {
            let (a, b) = (c.keys.get(i)?, c.keys.get(i + 1)?);
            let Interp::Bezier { out, into } = a.interp else { return None };
            let (va, vb) = (a.value.as_float()?, b.value.as_float()?);
            if (vb - va).abs() < 1e-9 {
                return None;
            }
            let at = |e: Ease| {
                let t = a.t + Time::from_seconds_f64((b.t - a.t).as_seconds_f64() * e.x);
                egui::pos2(view.x(t), view.y(va + (vb - va) * e.y))
            };
            Some((at(out), at(into)))
        };

        // Interaction.
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            let near = |q: egui::Pos2| q.distance(p) < 8.0;
            let mut grab = (0..curve.keys.len()).find(|&i| near(key_pos(&curve, i))).map(Grab::Key);
            if grab.is_none()
                && let Some(sel) = ed.selected
            {
                for seg in [sel, sel.wrapping_sub(1)] {
                    if let Some((o, n)) = handles(&curve, seg) {
                        if near(o) {
                            grab = Some(Grab::Out(seg));
                        } else if near(n) {
                            grab = Some(Grab::Into(seg));
                        }
                    }
                }
            }
            if let Some(Grab::Key(i)) = grab {
                ed.selected = Some(i);
            }
            ed.drag = grab.map(|g| (g, curve.clone()));
        }
        if response.dragged()
            && let (Some((grab, start)), Some(p)) = (ed.drag.as_ref(), pointer)
        {
            let frame = self.editor.sequence().rate.frame_start(1).max(Time(1));
            curve = start.clone();
            match *grab {
                Grab::Key(i) => {
                    let lo = if i > 0 { curve.keys[i - 1].t + frame } else { Time::ZERO };
                    let hi = curve.keys.get(i + 1).map_or(it.range.duration - Time(1), |k| k.t - frame);
                    curve.keys[i].t = view.t(p.x).max(lo).min(hi.max(lo));
                    curve.keys[i].value = Value::Float(view.v(p.y));
                }
                Grab::Out(i) | Grab::Into(i) => {
                    let (a, b) = (&curve.keys[i], &curve.keys[i + 1]);
                    let (va, vb) = (a.value.as_float().unwrap_or(0.0), b.value.as_float().unwrap_or(0.0));
                    let x = ((view.t(p.x) - a.t).as_seconds_f64() / (b.t - a.t).as_seconds_f64().max(1e-9)).clamp(0.0, 1.0);
                    let y = (view.v(p.y) - va) / (vb - va);
                    if let Interp::Bezier { out, into } = &mut curve.keys[i].interp {
                        let e = if matches!(grab, Grab::Out(_)) { out } else { into };
                        *e = Ease { x, y: y.clamp(-2.0, 3.0) };
                    }
                }
            }
            changed = true;
        }
        if response.drag_stopped() && ed.drag.take().is_some() {
            finished = true;
        }
        if response.clicked()
            && let Some(p) = pointer
        {
            ed.selected = (0..curve.keys.len()).find(|&i| key_pos(&curve, i).distance(p) < 8.0);
        }
        if response.double_clicked()
            && let Some(p) = pointer
            && (0..curve.keys.len()).all(|i| key_pos(&curve, i).distance(p) >= 8.0)
        {
            // A key where you double-click, on the curve's own value there.
            let t = view.t(p.x).max(Time::ZERO).min(it.range.duration - Time(1));
            let v = curve.eval_at(t);
            curve.set_value_at(t, v);
            ed.selected = curve.keys.iter().position(|k| k.t == t);
            changed = true;
            finished = true;
        }
        if response.hovered()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && let Some(i) = ed.selected
            && curve.keys.len() > 1
        {
            curve.keys.remove(i);
            ed.selected = None;
            changed = true;
            finished = true;
        }

        // Paint the curve, then handles and keys on top.
        let line: Vec<egui::Pos2> = {
            let mut pts = Vec::new();
            let mut x = rect.left();
            while x <= rect.right() {
                pts.push(egui::pos2(x, view.y(curve.eval_at(view.t(x)).as_float().unwrap_or(0.0))));
                x += 2.0;
            }
            pts
        };
        painter.add(egui::Shape::line(line, egui::Stroke::new(2.0, crate::style::ACCENT)));
        let selected = ed.selected;
        if let Some(sel) = selected {
            for seg in [sel, sel.wrapping_sub(1)] {
                if let Some((o, n)) = handles(&curve, seg) {
                    let (a, b) = (key_pos(&curve, seg), key_pos(&curve, seg + 1));
                    for (from, h) in [(a, o), (b, n)] {
                        painter.line_segment([from, h], egui::Stroke::new(1.0, visuals.text_color().gamma_multiply(0.6)));
                        painter.circle(h, 4.0, visuals.extreme_bg_color, egui::Stroke::new(1.5, crate::style::GOLD));
                    }
                }
            }
        }
        for i in 0..curve.keys.len() {
            let c = key_pos(&curve, i);
            let r = 5.5;
            let diamond = vec![c + egui::vec2(0.0, -r), c + egui::vec2(r, 0.0), c + egui::vec2(0.0, r), c + egui::vec2(-r, 0.0)];
            let fill = if Some(i) == selected { crate::style::GOLD } else { egui::Color32::WHITE };
            painter.add(egui::Shape::convex_polygon(diamond, fill, egui::Stroke::new(1.0, egui::Color32::BLACK)));
        }
        if let Some(i) = selected {
            let k = &curve.keys[i];
            let label = format!("{:.2}s  ·  {:.3}", k.t.as_seconds_f64(), k.value.as_float().unwrap_or(0.0));
            painter.text(outer.right_top() + egui::vec2(-6.0, 4.0), egui::Align2::RIGHT_TOP, label, egui::FontId::proportional(11.0), visuals.text_color());
        }
        painter.text(
            outer.left_bottom() + egui::vec2(6.0, -4.0),
            egui::Align2::LEFT_BOTTOM,
            "drag keys and handles · double-click adds · Delete removes",
            egui::FontId::proportional(10.0),
            visuals.weak_text_color(),
        );

        if changed {
            let mut src = source.unwrap_or(ParamSource::Animated(curve.clone()));
            if let Some(c) = src.curve_mut() {
                *c = curve;
            }
            self.editor.set_param(item, band.target.clone(), &band.param, src, "curve-editor");
        }
        if finished {
            self.editor.doc.seal();
        }
    }
}
