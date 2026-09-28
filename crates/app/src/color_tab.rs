//! The Color tab (with Advanced color on): grading the selected clip the way a colorist
//! does — scopes of the frame on screen, lift/gamma/gain/offset wheels, exposure, white
//! balance, contrast and tone sliders, curves (master, red, green, blue, saturation by
//! hue) and an HSL mixer.
//!
//! Underneath it's three ordinary effects on the clip — Color Grade, Curves and HSL
//! Mixer (Atelier Core, `editor: "color"`), put first in its chain the first time they're
//! touched — so a grade renders, exports, copies and keyframes like any effect (the
//! Effects tab has their numbers, with keyframe buttons). With several clips selected,
//! a change grades each of them.

use crate::scopes::Scope;
use crate::App;
use eframe::egui;
use oa_doc::{EffectInstance, ItemId, Op, ParamTarget};
use oa_params::Value;
use oa_time::Time;
use std::sync::mpsc::Receiver;

pub const GRADE: &str = "oa.color.grade";
pub const CURVES: &str = "oa.color.curves";
pub const HSL: &str = "oa.color.hsl";
const GRADING: [&str; 3] = [GRADE, CURVES, HSL];

/// How far from the center a wheel's rim is, in offset.
const WHEEL_RIM: f64 = 0.25;

#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum CurveChannel {
    #[default]
    Master,
    Red,
    Green,
    Blue,
    HueSat,
}

impl CurveChannel {
    const ALL: [CurveChannel; 5] = [CurveChannel::Master, CurveChannel::Red, CurveChannel::Green, CurveChannel::Blue, CurveChannel::HueSat];

    fn name(self) -> &'static str {
        match self {
            CurveChannel::Master => "Master",
            CurveChannel::Red => "Red",
            CurveChannel::Green => "Green",
            CurveChannel::Blue => "Blue",
            CurveChannel::HueSat => "Hue vs Sat",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            CurveChannel::Master => "master",
            CurveChannel::Red => "red",
            CurveChannel::Green => "green",
            CurveChannel::Blue => "blue",
            CurveChannel::HueSat => "huesat",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            CurveChannel::Master => egui::Color32::from_gray(225),
            CurveChannel::Red => egui::Color32::from_rgb(255, 90, 90),
            CurveChannel::Green => egui::Color32::from_rgb(90, 230, 110),
            CurveChannel::Blue => egui::Color32::from_rgb(110, 150, 255),
            CurveChannel::HueSat => egui::Color32::from_rgb(230, 200, 90),
        }
    }

    /// A point's value when nothing's changed.
    fn neutral(self, i: usize) -> f64 {
        if self == CurveChannel::HueSat { 1.0 } else { i as f64 / 7.0 }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum HslPart {
    #[default]
    Hue,
    Saturation,
    Luminance,
}

const BANDS: [(&str, &str, [u8; 3]); 8] = [
    ("red", "Red", [235, 70, 70]),
    ("orange", "Orange", [240, 150, 60]),
    ("yellow", "Yellow", [235, 220, 70]),
    ("green", "Green", [80, 200, 90]),
    ("aqua", "Aqua", [70, 210, 210]),
    ("blue", "Blue", [80, 120, 240]),
    ("purple", "Purple", [160, 90, 230]),
    ("magenta", "Magenta", [230, 80, 200]),
];

#[derive(Default)]
pub struct ColorTab {
    scope: Scope,
    /// The frame the scopes are of (what it was made from) and its pixels.
    frame: Option<(u64, Vec<u8>, [usize; 2])>,
    /// The scope picture drawn from it: (what frame, which scope, texture).
    picture: Option<(u64, Scope, egui::TextureHandle)>,
    job: Option<(u64, Receiver<Option<crate::preview_worker::FramePixels>>)>,
    curve: CurveChannel,
    hsl: HslPart,
    /// A copied grade: the grading effects as they were on the clip.
    clipboard: Option<Vec<EffectInstance>>,
}

/// An offset (zero-sum RGB) as a point on a wheel: red to the right, green up-left,
/// blue down-left; the rim is `WHEEL_RIM` away.
fn wheel_point(rgb: [f64; 3]) -> [f64; 2] {
    let x = (2.0 * rgb[0] - rgb[1] - rgb[2]) / 3.0;
    let y = (rgb[1] - rgb[2]) / 3f64.sqrt();
    [x / WHEEL_RIM, y / WHEEL_RIM]
}

/// A point on a wheel as the offset it means (zero-sum: brightness is the master's).
fn wheel_rgb(p: [f64; 2]) -> [f64; 3] {
    let (x, y) = (p[0] * WHEEL_RIM, p[1] * WHEEL_RIM);
    let s = 3f64.sqrt() / 2.0;
    [x, -0.5 * x + s * y, -0.5 * x - s * y]
}

/// The curve through eight points at 0, 1/7 … 1, as the Curves shader draws it: a
/// monotone cubic, never overshooting its points.
fn curve_at(points: &[f64; 8], x: f64) -> f64 {
    let slope = |k: usize| -> f64 {
        match k {
            0 => points[1] - points[0],
            7 => points[7] - points[6],
            _ => {
                let (a, b) = (points[k] - points[k - 1], points[k + 1] - points[k]);
                if a * b <= 0.0 { 0.0 } else { 2.0 / (1.0 / a + 1.0 / b) }
            }
        }
    };
    let t = x.clamp(0.0, 1.0) * 7.0;
    let k = (t.floor() as usize).min(6);
    let f = t - k as f64;
    let (f2, f3) = (f * f, f * f * f);
    (2.0 * f3 - 3.0 * f2 + 1.0) * points[k] + (f3 - 2.0 * f2 + f) * slope(k) + (-2.0 * f3 + 3.0 * f2) * points[k + 1] + (f3 - f2) * slope(k + 1)
}

impl App {
    /// The first grading effect of `type_id` on the clip, if it has one.
    fn grading_effect(&self, item: ItemId, type_id: &str) -> Option<oa_doc::EffectId> {
        self.editor.item(item)?.effects.iter().find(|e| e.type_id == type_id).map(|e| e.id)
    }

    /// A grading parameter at `t`: the clip's, or its neutral value.
    fn grade_value(&self, item: ItemId, type_id: &str, param: &str, t: Time) -> f64 {
        let set = self.grading_effect(item, type_id).and_then(|fx| self.editor.param_value(item, &ParamTarget::Effect(fx), param, t)).and_then(|v| v.as_float());
        set.or_else(|| self.registry.effect(type_id)?.params.iter().find(|s| s.id.as_str() == param)?.default.as_float()).unwrap_or(0.0)
    }

    /// Sets a grading parameter (adding the effect the first time), on every selected clip.
    fn set_grade(&mut self, item: ItemId, type_id: &str, param: &str, value: f64, t: Time) {
        let Some(fx) = self.editor.ensure_effect(item, type_id) else { return };
        self.editor.set_value_at(item, ParamTarget::Effect(fx), param, Value::Float(value), t, &format!("color-{type_id}-{param}"));
    }

    pub(crate) fn color_tab(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        self.scopes_panel(ui);
        ui.add_space(crate::style::GAP_S);
        self.grade_actions(ui, item);
        crate::inspector::section(ui, "Wheels", "Lift moves the shadows, gamma the midtones, gain the highlights, offset everything. Drag the dot (Shift: finely); the slider under each is its brightness. Double-click a wheel to reset it.");
        let width = ui.available_width();
        let size = ((width - 16.0) / 2.0).clamp(90.0, 150.0);
        let mut released = false;
        for row in [["lift", "gamma"], ["gain", "offset"]] {
            ui.horizontal(|ui| {
                for name in row {
                    ui.vertical(|ui| {
                        ui.set_width(size);
                        released |= self.wheel(ui, item, name, size, t);
                    });
                }
            });
        }
        crate::inspector::section(ui, "Light and color", "In order: exposure and white balance in light, then the wheels, contrast around its pivot, tone, saturation and hue.");
        let sliders: [(&str, &str, f64, f64); 12] = [
            ("exposure", "Exposure (stops)", -5.0, 5.0),
            ("temperature", "Temperature", -1.0, 1.0),
            ("tint", "Tint", -1.0, 1.0),
            ("contrast", "Contrast", 0.0, 3.0),
            ("pivot", "Pivot", 0.0, 1.0),
            ("highlights", "Highlights", -1.0, 1.0),
            ("shadows", "Shadows", -1.0, 1.0),
            ("whites", "Whites", -1.0, 1.0),
            ("blacks", "Blacks", -1.0, 1.0),
            ("saturation", "Saturation", 0.0, 3.0),
            ("vibrance", "Vibrance", -1.0, 1.0),
            ("hue", "Hue (°)", -180.0, 180.0),
        ];
        for (param, label, lo, hi) in sliders {
            let mut v = self.grade_value(item, GRADE, param, t);
            let r = ui.add(egui::Slider::new(&mut v, lo..=hi).text(label).fixed_decimals(if hi > 10.0 { 1 } else { 3 }));
            if r.changed() {
                self.set_grade(item, GRADE, param, v, t);
            }
            if r.double_clicked() {
                let neutral = self.registry.effect(GRADE).and_then(|d| d.params.iter().find(|s| s.id.as_str() == param)?.default.as_float()).unwrap_or(0.0);
                self.set_grade(item, GRADE, param, neutral, t);
                released = true;
            }
            released |= r.drag_stopped() || r.lost_focus();
        }
        crate::inspector::section(ui, "Curves", "Drag a point up or down; double-click it to put it back. Master first, then each channel; Hue vs Sat raises or lowers saturation by color.");
        released |= self.curves_editor(ui, item, t);
        crate::inspector::section(ui, "HSL mixer", "Each color band's hue, saturation or luminance on its own.");
        released |= self.hsl_mixer(ui, item, t);
        if released {
            self.editor.doc.seal();
        }
    }

    /// Grade on/off, copy, paste and reset — for every selected clip.
    fn grade_actions(&mut self, ui: &mut egui::Ui, item: ItemId) {
        let seq = self.editor.seq;
        let ids = self.selected_clips();
        let ids = if ids.is_empty() { vec![item] } else { ids };
        let graded: Vec<(ItemId, oa_doc::EffectId, bool)> = ids
            .iter()
            .filter_map(|id| self.editor.item(*id).map(|it| (*id, it)))
            .flat_map(|(id, it)| it.effects.iter().filter(|e| GRADING.contains(&e.type_id.as_str())).map(move |e| (id, e.id, e.enabled)).collect::<Vec<_>>())
            .collect();
        ui.horizontal_wrapped(|ui| {
            let on = graded.iter().any(|g| g.2);
            let mut want = on;
            if ui.add_enabled(!graded.is_empty(), egui::Checkbox::new(&mut want, "Grade on")).on_hover_text("Off shows the clip as it was, to compare").changed() {
                let ops = graded.iter().map(|(item, effect, _)| Op::SetEffectEnabled { seq, item: *item, effect: *effect, enabled: want }).collect();
                if let Err(e) = self.editor.apply(if want { "Grade on" } else { "Grade off" }, ops) {
                    self.error = Some(e.to_string());
                }
            }
            if ui.add_enabled(!graded.is_empty(), egui::Button::new("Copy grade")).clicked() {
                let copied = self.editor.item(item).map(|it| it.effects.iter().filter(|e| GRADING.contains(&e.type_id.as_str())).cloned().collect::<Vec<_>>());
                self.color_tab.clipboard = copied.filter(|c| !c.is_empty());
            }
            if ui.add_enabled(self.color_tab.clipboard.is_some(), egui::Button::new("Paste grade")).on_hover_text("Onto every selected clip, replacing its grade").clicked()
                && let Some(copied) = self.color_tab.clipboard.clone()
            {
                let mut ops: Vec<Op> = graded.iter().map(|(item, effect, _)| Op::RemoveEffect { seq, item: *item, effect: *effect }).collect();
                for id in &ids {
                    for (k, fx) in copied.iter().enumerate() {
                        let mut fx = fx.clone();
                        fx.id = oa_doc::EffectId(self.editor.doc.alloc_id());
                        ops.push(Op::InsertEffect { seq, item: *id, index: k, effect: fx });
                    }
                }
                if let Err(e) = self.editor.apply("Paste grade", ops) {
                    self.error = Some(e.to_string());
                }
            }
            if ui.add_enabled(!graded.is_empty(), egui::Button::new("Reset")).on_hover_text("Takes the grade off (Ctrl+Z brings it back)").clicked() {
                let ops = graded.iter().map(|(item, effect, _)| Op::RemoveEffect { seq, item: *item, effect: *effect }).collect();
                if let Err(e) = self.editor.apply("Reset grade", ops) {
                    self.error = Some(e.to_string());
                }
            }
        });
    }

    /// One wheel and its brightness slider. True when a drag ended (seal the undo step).
    fn wheel(&mut self, ui: &mut egui::Ui, item: ItemId, name: &str, size: f32, t: Time) -> bool {
        let rgb = [0, 1, 2].map(|i| self.grade_value(item, GRADE, &format!("{name}_{}", ["r", "g", "b"][i]), t));
        let master = self.grade_value(item, GRADE, name, t);
        let mean = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
        let offset = rgb.map(|c| c - mean);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let center = rect.center();
        let radius = size * 0.5 - 4.0;
        // The ring: each direction's color.
        let steps = 72;
        for i in 0..steps {
            let a0 = i as f32 / steps as f32 * std::f32::consts::TAU;
            let a1 = (i + 1) as f32 / steps as f32 * std::f32::consts::TAU;
            let dir = wheel_rgb([(a0 as f64).cos(), (a0 as f64).sin()]);
            let color = egui::Color32::from_rgb(((0.5 + 1.6 * dir[0]).clamp(0.0, 1.0) * 255.0) as u8, ((0.5 + 1.6 * dir[1]).clamp(0.0, 1.0) * 255.0) as u8, ((0.5 + 1.6 * dir[2]).clamp(0.0, 1.0) * 255.0) as u8);
            let at = |a: f32, r: f32| center + egui::vec2(a.cos(), -a.sin()) * r;
            painter.add(egui::Shape::convex_polygon(vec![at(a0, radius), at(a1, radius), at(a1, radius - 7.0), at(a0, radius - 7.0)], color, egui::Stroke::NONE));
        }
        painter.circle_filled(center, radius - 7.0, egui::Color32::from_gray(28));
        for f in [0.33, 0.66] {
            painter.circle_stroke(center, (radius - 7.0) * f, egui::Stroke::new(1.0, egui::Color32::from_gray(50)));
        }
        painter.line_segment([center - egui::vec2(radius - 7.0, 0.0), center + egui::vec2(radius - 7.0, 0.0)], egui::Stroke::new(1.0, egui::Color32::from_gray(45)));
        painter.line_segment([center - egui::vec2(0.0, radius - 7.0), center + egui::vec2(0.0, radius - 7.0)], egui::Stroke::new(1.0, egui::Color32::from_gray(45)));
        let p = wheel_point(offset);
        let puck = center + egui::vec2(p[0] as f32, -p[1] as f32) * (radius - 7.0);
        painter.circle_filled(puck, 5.0, egui::Color32::WHITE);
        painter.circle_stroke(puck, 5.0, egui::Stroke::new(1.0, egui::Color32::BLACK));
        painter.text(rect.left_top() + egui::vec2(2.0, 2.0), egui::Align2::LEFT_TOP, name[..1].to_uppercase() + &name[1..], egui::FontId::proportional(12.0), ui.visuals().text_color());
        let mut released = false;
        if r.dragged() {
            let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
            let d = r.drag_delta() * fine / (radius - 7.0);
            let mut np = [p[0] + d.x as f64, p[1] - d.y as f64];
            let len = (np[0] * np[0] + np[1] * np[1]).sqrt();
            if len > 1.0 {
                np = [np[0] / len, np[1] / len];
            }
            let new = wheel_rgb(np);
            for (i, c) in ["r", "g", "b"].iter().enumerate() {
                self.set_grade(item, GRADE, &format!("{name}_{c}"), new[i], t);
            }
        }
        if r.double_clicked() {
            for c in ["r", "g", "b"] {
                self.set_grade(item, GRADE, &format!("{name}_{c}"), 0.0, t);
            }
            self.set_grade(item, GRADE, name, 0.0, t);
            released = true;
        }
        released |= r.drag_stopped();
        let r = r.on_hover_text(format!("{name}: R {:+.3}  G {:+.3}  B {:+.3}", rgb[0] + master, rgb[1] + master, rgb[2] + master));
        let _ = r;
        let mut m = master;
        let s = ui.add(egui::Slider::new(&mut m, -1.0..=1.0).show_value(true).fixed_decimals(3));
        if s.changed() {
            self.set_grade(item, GRADE, name, m, t);
        }
        released | s.drag_stopped() | s.lost_focus()
    }

    fn curves_editor(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) -> bool {
        ui.horizontal_wrapped(|ui| {
            for c in CurveChannel::ALL {
                if ui.selectable_label(self.color_tab.curve == c, egui::RichText::new(c.name()).color(c.color())).clicked() {
                    self.color_tab.curve = c;
                }
            }
        });
        let channel = self.color_tab.curve;
        let points: [f64; 8] = std::array::from_fn(|i| self.grade_value(item, CURVES, &format!("{}_{i}", channel.prefix()), t));
        let width = ui.available_width().max(140.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, (width * 0.62).min(220.0)), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(22));
        let hue_sat = channel == CurveChannel::HueSat;
        // Up the side: output 0..1 (or saturation 0..2); along: input (or hue).
        let (lo, hi) = if hue_sat { (0.0, 2.0) } else { (0.0, 1.0) };
        let to_screen = |x: f64, y: f64| egui::pos2(rect.left() + rect.width() * x as f32, rect.bottom() - rect.height() * ((y - lo) / (hi - lo)) as f32);
        let grid = egui::Stroke::new(1.0, egui::Color32::from_gray(45));
        for i in 1..4 {
            let f = i as f32 / 4.0;
            painter.line_segment([egui::pos2(rect.left() + rect.width() * f, rect.top()), egui::pos2(rect.left() + rect.width() * f, rect.bottom())], grid);
            painter.line_segment([egui::pos2(rect.left(), rect.top() + rect.height() * f), egui::pos2(rect.right(), rect.top() + rect.height() * f)], grid);
        }
        if hue_sat {
            // The hues along the bottom.
            for i in 0..48 {
                let h = i as f32 / 48.0;
                let c = egui::ecolor::Hsva::new(h, 0.8, 0.9, 1.0);
                let x0 = rect.left() + rect.width() * h;
                painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.bottom() - 6.0), egui::pos2(x0 + rect.width() / 48.0 + 1.0, rect.bottom())), 0.0, c);
            }
        } else {
            painter.line_segment([to_screen(0.0, 0.0), to_screen(1.0, 1.0)], egui::Stroke::new(1.0, egui::Color32::from_gray(70)));
        }
        let curve: Vec<egui::Pos2> = (0..=64)
            .map(|i| {
                let x = i as f64 / 64.0;
                let y = if hue_sat { around(&points, x) } else { curve_at(&points, x) };
                to_screen(x, y.clamp(lo, hi))
            })
            .collect();
        painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, channel.color())));
        let mut released = false;
        for (i, v) in points.iter().enumerate() {
            let pos = to_screen(i as f64 / 7.0, v.clamp(lo, hi));
            let hit = egui::Rect::from_center_size(pos, egui::vec2(16.0, 16.0));
            let r = ui.interact(hit, ui.id().with(("curve-point", channel.prefix(), i)), egui::Sense::click_and_drag());
            if r.dragged() {
                let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
                let dv = -r.drag_delta().y as f64 / rect.height() as f64 * (hi - lo) * fine;
                self.set_grade(item, CURVES, &format!("{}_{i}", channel.prefix()), (v + dv).clamp(lo - 0.5, hi + 0.5), t);
            }
            if r.double_clicked() {
                self.set_grade(item, CURVES, &format!("{}_{i}", channel.prefix()), channel.neutral(i), t);
                released = true;
            }
            released |= r.drag_stopped();
            let active = r.hovered() || r.dragged();
            painter.circle_filled(pos, if active { 5.5 } else { 4.0 }, channel.color());
            painter.circle_stroke(pos, if active { 5.5 } else { 4.0 }, egui::Stroke::new(1.0, egui::Color32::BLACK));
        }
        if ui.small_button(format!("Reset {}", channel.name())).clicked() {
            for i in 0..8 {
                self.set_grade(item, CURVES, &format!("{}_{i}", channel.prefix()), channel.neutral(i), t);
            }
            released = true;
        }
        released
    }

    fn hsl_mixer(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) -> bool {
        ui.horizontal(|ui| {
            for (part, name) in [(HslPart::Hue, "Hue"), (HslPart::Saturation, "Saturation"), (HslPart::Luminance, "Luminance")] {
                if ui.selectable_label(self.color_tab.hsl == part, name).clicked() {
                    self.color_tab.hsl = part;
                }
            }
        });
        let (suffix, lo, hi) = match self.color_tab.hsl {
            HslPart::Hue => ("hue", -60.0, 60.0),
            HslPart::Saturation => ("sat", -1.0, 1.0),
            HslPart::Luminance => ("lum", -1.0, 1.0),
        };
        let mut released = false;
        for (band, label, rgb) in BANDS {
            let param = format!("{band}_{suffix}");
            let mut v = self.grade_value(item, HSL, &param, t);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 5.5, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                let s = ui.add(egui::Slider::new(&mut v, lo..=hi).text(label).fixed_decimals(if hi > 10.0 { 0 } else { 2 }));
                if s.changed() {
                    self.set_grade(item, HSL, &param, v, t);
                }
                if s.double_clicked() {
                    self.set_grade(item, HSL, &param, 0.0, t);
                    released = true;
                }
                released |= s.drag_stopped() || s.lost_focus();
            });
        }
        released
    }

    /// The scopes, of the frame on screen: asked of the preview thread when it changes
    /// (not while playing — the viewer comes first), drawn here from its pixels.
    fn scopes_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for s in Scope::ALL {
                if ui.selectable_label(self.color_tab.scope == s, s.name()).clicked() {
                    self.color_tab.scope = s;
                }
            }
        });
        // What the frame is: the document, the playhead, the format.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (std::sync::Arc::as_ptr(&self.editor.doc.snapshot()) as usize, self.playhead.0, self.variant, self.editor.seq.0).hash(&mut h);
            h.finish()
        };
        // A finished readback.
        if let Some((asked, rx)) = &self.color_tab.job
            && let Ok(made) = rx.try_recv()
        {
            let asked = *asked;
            self.color_tab.job = None;
            if let Some(f) = made {
                self.color_tab.frame = Some((asked, f.pixels, f.size));
            }
        }
        let current = self.color_tab.frame.as_ref().is_some_and(|(k, ..)| *k == key);
        if !current && self.color_tab.job.is_none() && !self.playing {
            let (tx, rx) = std::sync::mpsc::channel();
            let canvas = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
            self.preview_worker.pixels(
                crate::preview_worker::Request {
                    slot: 0,
                    tag: key,
                    project: self.editor.doc.snapshot(),
                    registry: self.registry.clone(),
                    seq: self.editor.seq,
                    variant: self.variant_id(),
                    at: self.playhead,
                    scale: (320.0 / canvas.width.max(1) as f64).min(1.0),
                    wanted: None,
                    png: None,
                    see_through: false,
                },
                tx,
            );
            self.color_tab.job = Some((key, rx));
        }
        if self.color_tab.job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }
        // The picture, redrawn when the frame or the kind of scope changed.
        let scope = self.color_tab.scope;
        if let Some((frame_key, pixels, size)) = &self.color_tab.frame
            && self.color_tab.picture.as_ref().is_none_or(|(k, s, _)| k != frame_key || *s != scope)
        {
            let image = crate::scopes::draw(scope, pixels, *size);
            let texture = ui.ctx().load_texture("color-scope", image, egui::TextureOptions::LINEAR);
            self.color_tab.picture = Some((*frame_key, scope, texture));
        }
        let width = ui.available_width().min(420.0);
        let [w, h] = scope.size();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, width * h as f32 / w as f32), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(8, 9, 11));
        match &self.color_tab.picture {
            Some((_, s, texture)) if *s == scope => {
                painter.image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            _ => {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, if self.playing { "Scopes update when paused" } else { "…" }, egui::FontId::proportional(12.0), egui::Color32::from_gray(120));
            }
        }
        // The graticule.
        let line = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(28));
        let label = |p: egui::Pos2, s: &str| painter.text(p, egui::Align2::LEFT_CENTER, s, egui::FontId::proportional(9.0), egui::Color32::from_white_alpha(90));
        match scope {
            Scope::Waveform | Scope::Parade => {
                for pct in [0, 25, 50, 75, 100] {
                    let y = rect.bottom() - rect.height() * pct as f32 / 100.0;
                    painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], line);
                    label(egui::pos2(rect.left() + 2.0, y.clamp(rect.top() + 5.0, rect.bottom() - 5.0)), &pct.to_string());
                }
                if scope == Scope::Parade {
                    for k in 1..3 {
                        let x = rect.left() + rect.width() * k as f32 / 3.0;
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, egui::Color32::from_white_alpha(50)));
                    }
                }
            }
            Scope::Vectorscope => {
                let c = rect.center();
                for f in [0.25, 0.5] {
                    painter.circle_stroke(c, rect.width() * f, line);
                }
                painter.line_segment([egui::pos2(rect.left(), c.y), egui::pos2(rect.right(), c.y)], line);
                painter.line_segment([egui::pos2(c.x, rect.top()), egui::pos2(c.x, rect.bottom())], line);
                // The skin-tone line (about 123° on a vectorscope, towards red-yellow).
                let a = 123f32.to_radians();
                painter.line_segment([c, c + egui::vec2(a.cos(), -a.sin()) * rect.width() * 0.5], egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(230, 170, 120, 70)));
                for (name, at, color) in crate::scopes::vector_targets() {
                    let p = egui::pos2(rect.left() + rect.width() * at[0], rect.top() + rect.height() * at[1]);
                    painter.rect_stroke(egui::Rect::from_center_size(p, egui::vec2(9.0, 9.0)), 1.0, egui::Stroke::new(1.0, color.gamma_multiply(0.7)), egui::StrokeKind::Middle);
                    painter.text(p + egui::vec2(7.0, 0.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(9.0), color.gamma_multiply(0.8));
                }
            }
            Scope::Histogram => {
                for k in 1..4 {
                    let x = rect.left() + rect.width() * k as f32 / 4.0;
                    painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], line);
                }
            }
        }
    }
}

/// Eight points around a circle (hue), as the Curves shader reads them.
fn around(points: &[f64; 8], h: f64) -> f64 {
    let t = h.rem_euclid(1.0) * 8.0;
    let i = t.floor() as usize;
    let f = t - i as f64;
    let p = |k: usize| points[k % 8];
    let (p0, p1, p2, p3) = (p(i + 7), p(i), p(i + 1), p(i + 2));
    0.5 * (2.0 * p1 + (p2 - p0) * f + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * f * f + (3.0 * p1 - p0 - 3.0 * p2 + p3) * f * f * f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point on the wheel and the offset it means go both ways; the offset keeps
    /// brightness out (it sums to zero); red is to the right.
    #[test]
    fn wheels_map_both_ways() {
        for p in [[0.3, 0.1], [-0.5, 0.4], [0.0, -1.0]] {
            let rgb = wheel_rgb(p);
            assert!((rgb[0] + rgb[1] + rgb[2]).abs() < 1e-12);
            let back = wheel_point(rgb);
            assert!((back[0] - p[0]).abs() < 1e-12 && (back[1] - p[1]).abs() < 1e-12, "{p:?} → {back:?}");
        }
        let right = wheel_rgb([1.0, 0.0]);
        assert!(right[0] > 0.0 && right[1] < 0.0 && right[2] < 0.0, "{right:?}");
    }

    /// Neutral curve points draw the straight line; the curve goes through its points.
    #[test]
    fn curves_pass_through_their_points() {
        let straight: [f64; 8] = std::array::from_fn(|i| i as f64 / 7.0);
        for x in [0.0, 0.1, 0.33, 0.5, 0.9, 1.0] {
            assert!((curve_at(&straight, x) - x).abs() < 1e-9, "{x}");
        }
        let mut s = straight;
        s[3] = 0.52;
        assert!((curve_at(&s, 3.0 / 7.0) - 0.52).abs() < 1e-9);
        // Points that only go up give a curve that only goes up: no overshoot, no dip past
        // a neighbor (a smooth spline through them could swing below one).
        let mut last = f64::MIN;
        for i in 0..=200 {
            let y = curve_at(&s, i as f64 / 200.0);
            assert!(y >= last - 1e-12, "goes down at {}: {y} after {last}", i as f64 / 200.0);
            last = y;
        }
        let flat = [1.0; 8];
        assert!((around(&flat, 0.37) - 1.0).abs() < 1e-9);
    }
}
