//! The keyframe editor: the selected clip's animation, in place of the timeline (the
//! Keyframes button under it switches). Each keyframed property is a row — numbers as a
//! curve of their values over the clip, with their keys (a position or a scale as two
//! curves, X and Y); colors, text and switches as their keys alone — and the rows
//! scroll. Drag a key (its time and value), drag the handles of a selected key to shape
//! its easing, right-click a key for easing presets, double-click to add a key, Delete
//! to remove one; click elsewhere to move the playhead there.
//!
//! The header holds the clip's "Stretch keys when the speed changes" switch: on (the
//! default), a new speed stretches its keys with it (`Item::keep_key_times`). Trims never
//! move keys.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{ItemId, Op, ParamTarget};
use oa_params::{Curve, Ease, Interp, KeyframeAnchor, Value};
use oa_time::Time;

/// Whether the editor is offered (the Keyframes button under the timeline). Off while it
/// gets more work; everything else here (and keys stretching with a new speed) stays.
pub const ENABLED: bool = false;

/// The width of the rows' names.
const LABELS: f32 = 170.0;
const CURVE_ROW: f32 = 92.0;
const KEY_ROW: f32 = 30.0;
const RULER: f32 = 22.0;

/// One row: a property (or one of a vector's components) and how it's named.
#[derive(Clone, Debug, PartialEq)]
struct Lane {
    target: ParamTarget,
    param: String,
    /// For a position or a scale: X (0) or Y (1).
    comp: Option<usize>,
    /// What owns it (the clip, an effect, this format) and its own name.
    group: String,
    name: String,
    /// A number (drawn as a curve) rather than a color, text or switch (keys only).
    numeric: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Grab {
    Key(usize),
    /// The handle leaving key `i`.
    Out(usize),
    /// The handle arriving at key `i + 1` (stored on key `i`).
    Into(usize),
}

/// The editor's state: whether it's showing, the selected key, a drag in progress.
#[derive(Default)]
pub struct KeyframeView {
    pub open: bool,
    selected: Option<(Lane, usize)>,
    drag: Option<(Lane, Grab, Curve)>,
    /// Where it was drawn last (Delete over it is the selected key's).
    pub rect: Option<egui::Rect>,
}

/// A number out of a value: the value itself, or one component of a vector.
fn number(v: &Value, comp: Option<usize>) -> Option<f64> {
    match comp {
        None => v.as_float(),
        Some(c) => v.as_vec2().map(|a| a[c.min(1)]),
    }
}

/// `v` with its number (or its component) set to `x`.
fn with_number(v: &Value, comp: Option<usize>, x: f64) -> Value {
    match (v, comp) {
        (Value::Vec2(a), Some(c)) => {
            let mut a = *a;
            a[c.min(1)] = x;
            Value::Vec2(a)
        }
        _ => Value::Float(x),
    }
}

/// A parameter id's own name: "transform.position" → "position".
fn pretty(id: &str) -> String {
    crate::i18n::t(&id.rsplit('.').next().unwrap_or(id).replace(['_', '-'], " ")).to_string()
}

impl App {
    /// Whether the pointer is over the keyframe editor (Delete there is a key's).
    pub(crate) fn pointer_on_keyframes(&self, ctx: &egui::Context) -> bool {
        self.keyframes.open && self.keyframes.rect.zip(ctx.pointer_hover_pos()).is_some_and(|(r, p)| r.contains(p))
    }

    /// Every keyframed property of a clip, as rows: its own, each effect's and its
    /// overrides in the format being edited.
    fn lanes(&self, it: &oa_doc::Item) -> Vec<Lane> {
        let mut out = Vec::new();
        let mut add = |target: ParamTarget, group: &str, params: &oa_params::ParamSet| {
            for (id, src) in &params.0 {
                let Some(curve) = src.curve().filter(|c| c.anchor == KeyframeAnchor::ClipStart && !c.keys.is_empty()) else { continue };
                let name = pretty(id.as_str());
                let base = Lane { target: target.clone(), param: id.as_str().to_string(), comp: None, group: group.to_string(), name: name.clone(), numeric: false };
                match &curve.keys[0].value {
                    Value::Float(_) => out.push(Lane { numeric: true, ..base }),
                    Value::Vec2(_) => {
                        for (c, axis) in ["X", "Y"].iter().enumerate() {
                            out.push(Lane { comp: Some(c), name: format!("{name} {axis}"), numeric: true, ..base.clone() });
                        }
                    }
                    _ => out.push(base),
                }
            }
        };
        add(ParamTarget::Item, tr("Clip"), &it.params);
        for fx in &it.effects {
            add(ParamTarget::Effect(fx.id), &self.effect_name(&fx.type_id), &fx.params);
        }
        let s = self.editor.sequence();
        if let Some(v) = s.variants.get(self.variant.min(s.variants.len() - 1))
            && let Some(o) = v.overrides.get(&it.id)
        {
            add(ParamTarget::VariantOverride(v.id), &trf("{format} only", &[("format", &v.name)]), o);
        }
        out
    }

    /// The editor, in `ui` (where the timeline's tracks go).
    pub(crate) fn keyframe_editor(&mut self, ui: &mut egui::Ui) {
        self.keyframes.rect = Some(ui.max_rect());
        let Some(item) = self.selection.filter(|id| self.editor.item(*id).is_some()) else {
            ui.add_space(crate::style::GAP_L);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new(tr("Select a clip to see its keyframes.")).weak()));
            return;
        };
        let it = self.editor.item(item).cloned().expect("checked");
        // The header: whose keys these are, and whether they stretch with it.
        ui.horizontal(|ui| {
            ui.add_space(crate::style::GAP_S);
            ui.label(egui::RichText::new(trf("Keyframes · {name}", &[("name", &it.name)])).strong());
            ui.add_space(crate::style::GAP_L);
            let mut stretch = !it.keep_key_times;
            if crate::widgets::toggle(ui, &mut stretch).on_hover_text(tr("On: when the clip's speed changes (and so its length), its keys stretch with it, so the animation stays with the footage. Off: they keep their times. Trimming the clip never moves them.")).changed() {
                let op = Op::SetKeepKeyTimes { seq: self.editor.seq, item, keep: !stretch };
                if let Err(e) = self.editor.apply(if stretch { tr("Stretch keys with the speed") } else { tr("Keep key times") }, vec![op]) {
                    self.error = Some(e.to_string());
                }
            }
            ui.label(tr("Stretch keys when the speed changes"));
        });
        let lanes = self.lanes(&it);
        if lanes.is_empty() {
            ui.add_space(crate::style::GAP_L);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new(tr("Nothing on this clip is keyframed yet: click a property's ◇ in the Properties panel to animate it.")).weak()));
            return;
        }
        // The time ruler, sharing the rows' scale: the clip, start to end.
        let duration = it.range.duration;
        let (ruler, ruler_r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), RULER), egui::Sense::click_and_drag());
        let graph_x = ruler.left() + LABELS..=ruler.right() - 12.0;
        let x_of = |t: Time| *graph_x.start() + (t.as_seconds_f64() / duration.as_seconds_f64().max(1e-9)) as f32 * (graph_x.end() - graph_x.start());
        let t_of = |x: f32| Time::from_seconds_f64((((x - graph_x.start()) / (graph_x.end() - graph_x.start())) as f64 * duration.as_seconds_f64()).clamp(0.0, duration.as_seconds_f64()));
        self.paint_ruler(ui, ruler, &x_of, duration);
        if (ruler_r.clicked() || ruler_r.dragged())
            && let Some(p) = ruler_r.interact_pointer_pos()
        {
            self.set_playhead(it.range.start + t_of(p.x));
        }
        let playhead = (self.playhead - it.range.start).clamp(Time::ZERO, duration);
        egui::ScrollArea::vertical().id_salt("keyframe-rows").auto_shrink([false, false]).show(ui, |ui| {
            let mut last_group = String::new();
            for lane in &lanes {
                if lane.group != last_group {
                    ui.label(egui::RichText::new(&lane.group).small().weak());
                    last_group = lane.group.clone();
                }
                self.lane_row(ui, item, &it, lane, &x_of, &t_of, playhead);
            }
        });
    }

    /// Seconds along the top, and the playhead.
    fn paint_ruler(&self, ui: &egui::Ui, rect: egui::Rect, x_of: &dyn Fn(Time) -> f32, duration: Time) {
        let painter = ui.painter_at(rect);
        let v = ui.visuals();
        let secs = duration.as_seconds_f64();
        // A tick every 1, 2, 5, 10… seconds: about one per 60 px.
        let px_per_s = (x_of(Time::from_seconds(1)) - x_of(Time::ZERO)).max(1e-3);
        let step = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter().find(|s| (*s as f32) * px_per_s >= 60.0).unwrap_or(120.0);
        let mut s = 0.0;
        while s <= secs + 1e-9 {
            let x = x_of(Time::from_seconds_f64(s));
            painter.line_segment([egui::pos2(x, rect.bottom() - 6.0), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, v.weak_text_color()));
            painter.text(egui::pos2(x + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, format!("{s:.2}").trim_end_matches('0').trim_end_matches('.').to_string() + "s", egui::FontId::proportional(10.0), v.weak_text_color());
            s += step;
        }
        let x = x_of((self.playhead - self.editor.item(self.selection.unwrap_or(ItemId(0))).map_or(Time::ZERO, |i| i.range.start)).clamp(Time::ZERO, duration));
        painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x - 5.0, rect.top()), egui::pos2(x + 5.0, rect.top()), egui::pos2(x, rect.top() + 7.0)], crate::style::GOLD, egui::Stroke::NONE));
    }

    /// One property's row: its name and value at the playhead, then its curve (or keys).
    #[allow(clippy::too_many_arguments)]
    fn lane_row(&mut self, ui: &mut egui::Ui, item: ItemId, it: &oa_doc::Item, lane: &Lane, x_of: &dyn Fn(Time) -> f32, t_of: &dyn Fn(f32) -> Time, playhead: Time) {
        let height = if lane.numeric { CURVE_ROW } else { KEY_ROW };
        let (row, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(row) {
            return;
        }
        let Some(source) = self.editor.param_source(item, &lane.target, &lane.param) else { return };
        let Some(mut curve) = source.curve().cloned() else { return };
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(row);
        let graph = egui::Rect::from_min_max(egui::pos2(row.left() + LABELS, row.top() + 6.0), egui::pos2(row.right() - 12.0, row.bottom() - 6.0));
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(graph.left(), row.top() + 2.0), egui::pos2(graph.right(), row.bottom() - 2.0)), 4.0, visuals.extreme_bg_color);
        // The name, and the value where the playhead is.
        let ctx = it.eval_context(it.range.start + playhead);
        let now = source.eval(&ctx);
        let shown = match number(&now, lane.comp) {
            Some(x) if lane.numeric => format!("{x:.3}").trim_end_matches('0').trim_end_matches('.').to_string(),
            _ => short(&now),
        };
        painter.text(egui::pos2(row.left() + 8.0, row.top() + 8.0), egui::Align2::LEFT_TOP, &lane.name, egui::FontId::proportional(crate::style::TEXT), visuals.text_color());
        painter.text(egui::pos2(row.left() + 8.0, row.top() + 26.0), egui::Align2::LEFT_TOP, shown, egui::FontId::proportional(crate::style::TEXT_S), visuals.weak_text_color());
        // The playhead, down every row.
        let px = x_of(playhead);
        painter.line_segment([egui::pos2(px, row.top()), egui::pos2(px, row.bottom())], egui::Stroke::new(1.0, crate::style::GOLD.gamma_multiply(0.7)));

        // The value range drawn: the keys' (and the value now), with room around it.
        let values: Vec<f64> = curve.keys.iter().filter_map(|k| number(&k.value, lane.comp)).chain(number(&now, lane.comp)).collect();
        let (lo, hi) = values.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let span = if hi - lo < 1e-9 { lo.abs().max(1.0) * 0.2 } else { (hi - lo) * 0.15 };
        let (lo, hi) = (lo - span, hi + span);
        let y_of = |v: f64| graph.bottom() - ((v - lo) / (hi - lo).max(1e-12)) as f32 * graph.height();
        let v_of = |y: f32| lo + ((graph.bottom() - y) / graph.height().max(1.0)) as f64 * (hi - lo);
        let key_pos = |c: &Curve, i: usize| egui::pos2(x_of(c.keys[i].t), if lane.numeric { y_of(number(&c.keys[i].value, lane.comp).unwrap_or(0.0)) } else { graph.center().y });
        let handles = |c: &Curve, i: usize| -> Option<(egui::Pos2, egui::Pos2)> {
            let (a, b) = (c.keys.get(i)?, c.keys.get(i + 1)?);
            let Interp::Bezier { out, into } = a.interp else { return None };
            let (va, vb) = (number(&a.value, lane.comp)?, number(&b.value, lane.comp)?);
            if (vb - va).abs() < 1e-12 {
                return None;
            }
            let at = |e: Ease| egui::pos2(x_of(a.t + Time::from_seconds_f64((b.t - a.t).as_seconds_f64() * e.x)), y_of(va + (vb - va) * e.y));
            Some((at(out), at(into)))
        };
        let selected = self.keyframes.selected.as_ref().filter(|(l, _)| l == lane).map(|(_, i)| *i).filter(|i| *i < curve.keys.len());

        // Interaction.
        let mut changed = false;
        let mut finished = false;
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let near_key = |c: &Curve, p: egui::Pos2| (0..c.keys.len()).find(|&i| key_pos(c, i).distance(p) < 8.0);
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            let mut grab = near_key(&curve, p).map(Grab::Key);
            if grab.is_none()
                && lane.numeric
                && let Some(sel) = selected
            {
                for seg in [sel, sel.wrapping_sub(1)] {
                    if let Some((o, n)) = handles(&curve, seg) {
                        if o.distance(p) < 8.0 {
                            grab = Some(Grab::Out(seg));
                        } else if n.distance(p) < 8.0 {
                            grab = Some(Grab::Into(seg));
                        }
                    }
                }
            }
            if let Some(Grab::Key(i)) = grab {
                self.keyframes.selected = Some((lane.clone(), i));
            }
            self.keyframes.drag = grab.map(|g| (lane.clone(), g, curve.clone()));
        }
        let dragging = self.keyframes.drag.as_ref().filter(|(l, _, _)| l == lane).map(|(_, g, c)| (*g, c.clone()));
        if response.dragged()
            && let (Some((grab, start)), Some(p)) = (dragging, pointer)
        {
            let frame = self.editor.sequence().rate.frame_start(1).max(Time(1));
            curve = start;
            match grab {
                Grab::Key(i) => {
                    let lo_t = if i > 0 { curve.keys[i - 1].t + frame } else { Time::ZERO.min(curve.keys[i].t) };
                    let hi_t = curve.keys.get(i + 1).map_or(Time::MAX, |k| k.t - frame);
                    curve.keys[i].t = t_of(p.x).max(lo_t).min(hi_t.max(lo_t));
                    if lane.numeric {
                        curve.keys[i].value = with_number(&curve.keys[i].value, lane.comp, v_of(p.y));
                    }
                }
                Grab::Out(i) | Grab::Into(i) => {
                    let (a, b) = (&curve.keys[i], &curve.keys[i + 1]);
                    let (va, vb) = (number(&a.value, lane.comp).unwrap_or(0.0), number(&b.value, lane.comp).unwrap_or(0.0));
                    let x = ((t_of(p.x) - a.t).as_seconds_f64() / (b.t - a.t).as_seconds_f64().max(1e-9)).clamp(0.0, 1.0);
                    let y = ((v_of(p.y) - va) / (vb - va)).clamp(-2.0, 3.0);
                    if let Interp::Bezier { out, into } = &mut curve.keys[i].interp {
                        *(if matches!(grab, Grab::Out(_)) { out } else { into }) = Ease { x, y };
                    }
                }
            }
            changed = true;
        }
        if response.drag_stopped() && self.keyframes.drag.as_ref().is_some_and(|(l, _, _)| l == lane) {
            self.keyframes.drag = None;
            finished = true;
        }
        if response.clicked()
            && let Some(p) = pointer
        {
            match near_key(&curve, p) {
                Some(i) => self.keyframes.selected = Some((lane.clone(), i)),
                None => {
                    self.keyframes.selected = None;
                    if p.x >= graph.left() {
                        self.set_playhead(it.range.start + t_of(p.x));
                    }
                }
            }
        }
        if response.double_clicked()
            && let Some(p) = pointer
            && p.x >= graph.left()
            && near_key(&curve, p).is_none()
        {
            let t = t_of(p.x);
            let v = curve.eval_at(t);
            curve.set_value_at(t, v);
            self.keyframes.selected = curve.keys.iter().position(|k| k.t == t).map(|i| (lane.clone(), i));
            changed = true;
            finished = true;
        }
        if self.pointer_on_keyframes(ui.ctx())
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && let Some(i) = selected
            && curve.keys.len() > 1
        {
            curve.keys.remove(i);
            self.keyframes.selected = None;
            changed = true;
            finished = true;
        }
        // Right-click a key: its easing, or remove it.
        let mut menu_change: Option<Curve> = None;
        if let Some(p) = response.hover_pos().filter(|_| response.secondary_clicked())
            && let Some(i) = near_key(&curve, p)
        {
            self.keyframes.selected = Some((lane.clone(), i));
        }
        let menu_key = selected.or_else(|| self.keyframes.selected.as_ref().filter(|(l, _)| l == lane).map(|(_, i)| *i));
        crate::widgets::context_menu(&response, |ui| {
            let Some(i) = menu_key.filter(|i| *i < curve.keys.len()) else {
                ui.label(egui::RichText::new(tr("Right-click a key for its easing")).weak());
                return;
            };
            ui.label(egui::RichText::new(tr("Easing after this key")).small().weak());
            for (name, preset) in crate::curves::PRESETS {
                let interp = crate::curves::preset_interp(preset);
                if ui.selectable_label(curve.keys[i].interp == interp, tr(name)).clicked() {
                    let mut c = curve.clone();
                    c.keys[i].interp = interp;
                    menu_change = Some(c);
                    ui.close();
                }
            }
            ui.separator();
            if ui.add_enabled(curve.keys.len() > 1, egui::Button::new(tr("Delete key"))).clicked() {
                let mut c = curve.clone();
                c.keys.remove(i);
                menu_change = Some(c);
                ui.close();
            }
        });
        if let Some(c) = menu_change {
            curve = c;
            self.keyframes.selected = None;
            changed = true;
            finished = true;
        }

        // Paint: the curve, the selected key's handles, the keys.
        if lane.numeric {
            let mut line = Vec::new();
            let mut x = graph.left();
            while x <= graph.right() {
                if let Some(v) = number(&curve.eval_at(t_of(x)), lane.comp) {
                    line.push(egui::pos2(x, y_of(v)));
                }
                x += 2.0;
            }
            painter.add(egui::Shape::line(line, egui::Stroke::new(2.0, crate::style::ACCENT)));
            let sel = self.keyframes.selected.as_ref().filter(|(l, _)| l == lane).map(|(_, i)| *i);
            if let Some(s) = sel {
                for seg in [s, s.wrapping_sub(1)] {
                    if let Some((o, n)) = handles(&curve, seg) {
                        let (a, b) = (key_pos(&curve, seg), key_pos(&curve, seg + 1));
                        for (from, h) in [(a, o), (b, n)] {
                            painter.line_segment([from, h], egui::Stroke::new(1.0, visuals.text_color().gamma_multiply(0.6)));
                            painter.circle(h, 4.0, visuals.extreme_bg_color, egui::Stroke::new(1.5, crate::style::GOLD));
                        }
                    }
                }
            }
        } else {
            painter.line_segment([egui::pos2(graph.left(), graph.center().y), egui::pos2(graph.right(), graph.center().y)], egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.4)));
        }
        let sel = self.keyframes.selected.as_ref().filter(|(l, _)| l == lane).map(|(_, i)| *i);
        for i in 0..curve.keys.len() {
            let c = key_pos(&curve, i);
            let r = 5.5;
            let diamond = vec![c + egui::vec2(0.0, -r), c + egui::vec2(r, 0.0), c + egui::vec2(0.0, r), c + egui::vec2(-r, 0.0)];
            let fill = if Some(i) == sel { crate::style::GOLD } else { egui::Color32::WHITE };
            painter.add(egui::Shape::convex_polygon(diamond, fill, egui::Stroke::new(1.0, egui::Color32::BLACK)));
        }
        let line_y = row.bottom() - 0.5;
        painter.line_segment([egui::pos2(row.left(), line_y), egui::pos2(row.right(), line_y)], egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.5)));

        if changed {
            let mut src = source;
            if let Some(c) = src.curve_mut() {
                *c = curve;
            }
            self.editor.set_param(item, lane.target.clone(), &lane.param, src, "keyframes");
        }
        if finished {
            self.editor.doc.seal();
        }
    }
}

/// A short description of a value that isn't a number (for keys-only rows).
fn short(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { tr("on").to_string() } else { tr("off").to_string() },
        Value::Text(t) => t.chars().take(24).collect(),
        Value::Enum(e) => e.clone(),
        Value::Vec2([a, b]) => format!("{a:.2}, {b:.2}"),
        Value::Float(f) => format!("{f:.3}"),
        _ => String::new(),
    }
}
