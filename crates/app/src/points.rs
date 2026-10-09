//! Effect points on the canvas: a setting that's a place on the clip — a Swirl's or a
//! Vignette's center, any point relative to the clip (`vec2`, `source_fraction`) — shows
//! as a crosshair over the selected clip, dragged there (a key at the playhead when the
//! point is keyframed). Right-click its values in the inspector to make it follow a
//! track, like the clip's own position.
//!
//! An effect with a cut line (`EDITOR_LINE`: Slice) shows the line through its point at
//! its `angle`, and a handle along it that turns it.

use crate::App;
use eframe::egui;
use oa_doc::{EffectId, ItemId, ParamTarget};
use oa_params::{Unit, Value};
use oa_plan::scene::Placement;
use oa_time::Time;

/// Screen px within which a point can be grabbed.
const GRAB_PX: f32 = 9.0;

/// How far along its line (screen px) a line's turning handle sits.
const TURN_PX: f32 = 70.0;

/// One effect's point on a clip right now.
pub struct EffectPoint {
    pub effect: EffectId,
    pub param: String,
    /// Where it is, in fractions of the clip.
    pub at: [f64; 2],
    /// The effect's name, for the pointer's tooltip.
    pub name: String,
    /// On a cut line: its angle (degrees, 0 to the right, 90 down). The point is the
    /// line's own, drawn with the line through it; with `turn`, it's the handle that
    /// turns it (`param` is then the angle, `at` still the line's point).
    pub line: Option<f64>,
    pub turn: bool,
}

impl EffectPoint {
    fn on_canvas(&self, p: &Placement) -> [f64; 2] {
        p.to_canvas.apply([self.at[0] * p.native[0], self.at[1] * p.native[1]])
    }

    /// The line's direction on screen (a unit vector), for a point on a line.
    fn direction(&self, p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2) -> Option<egui::Vec2> {
        let a = self.line?.to_radians();
        let c = [self.at[0] * p.native[0], self.at[1] * p.native[1]];
        let ahead = p.to_canvas.apply([c[0] + a.cos(), c[1] + a.sin()]);
        let d = to_screen(ahead) - to_screen(p.to_canvas.apply(c));
        (d.length() > 1e-6).then(|| d.normalized())
    }

    /// Where it is on screen: the point, or (a turning handle) along its line.
    pub fn on_screen(&self, p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2) -> egui::Pos2 {
        let c = to_screen(self.on_canvas(p));
        match (self.turn, self.direction(p, to_screen)) {
            (true, Some(d)) => c + d * TURN_PX,
            _ => c,
        }
    }
}

/// The point nearest `pointer` (screen), if one is within reach.
pub fn point_near(points: &[EffectPoint], p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.on_screen(p, to_screen).distance(pointer)))
        .filter(|(_, d)| *d <= GRAB_PX)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// A point being dragged: where the pointer and the point were at the start.
#[derive(Clone)]
pub struct PointDrag {
    pub item: ItemId,
    effect: EffectId,
    param: String,
    start: [f64; 2],
    at0: [f64; 2],
    /// Turning a line about `at0`.
    turn: bool,
}

impl App {
    /// The points of the clip's switched-on effects at `t` (not a Surface's: it has its
    /// own editor), and each cut line's point and turning handle.
    pub(crate) fn effect_points(&self, item: ItemId, t: Time) -> Vec<EffectPoint> {
        let Some(it) = self.editor.item(item) else { return Vec::new() };
        let ctx = it.eval_context(t);
        let mut out = Vec::new();
        for fx in it.effects.iter().filter(|e| e.enabled) {
            let Some(d) = self.registry.effect(&fx.type_id) else { continue };
            let line = d.editor.as_deref() == Some(oa_graph::registry::EDITOR_LINE);
            if (d.editor.is_some() && !line) || fx.role.clock(ctx.clip_time, it.range.duration).is_none() {
                continue;
            }
            let values = fx.params.eval(&d.params, None, &ctx);
            let angle = line.then(|| values.float("angle"));
            for s in d.params.iter().filter(|s| s.unit == Unit::SourceFraction && matches!(s.default, Value::Vec2(_))) {
                let at = values.vec2(s.id.as_str());
                out.push(EffectPoint { effect: fx.id, param: s.id.as_str().to_string(), at, name: d.name.clone(), line: angle, turn: false });
                if line {
                    out.push(EffectPoint { effect: fx.id, param: "angle".into(), at, name: d.name.clone(), line: angle, turn: true });
                }
            }
        }
        out
    }

    pub(crate) fn begin_point_drag(&mut self, p: &Placement, point: &EffectPoint, canvas: [f64; 2]) {
        let Some(start) = p.to_layer_fraction(canvas) else { return };
        self.point_drag = Some(PointDrag { item: p.item, effect: point.effect, param: point.param.clone(), start, at0: point.at, turn: point.turn });
        self.set_playing(false);
    }

    /// The dragged point follows the pointer (`canvas` px); a line's handle turns the
    /// line to point at it (Shift: in 15° steps).
    pub(crate) fn drag_point(&mut self, drag: &PointDrag, p: &Placement, canvas: [f64; 2], shift: bool) {
        let Some([u, v]) = p.to_layer_fraction(canvas) else { return };
        if drag.turn {
            let (dx, dy) = ((u - drag.at0[0]) * p.native[0], (v - drag.at0[1]) * p.native[1]);
            if dx.hypot(dy) < 1e-6 {
                return;
            }
            let mut degrees = dy.atan2(dx).to_degrees().rem_euclid(360.0);
            if shift {
                degrees = ((degrees / 15.0).round() * 15.0).rem_euclid(360.0);
            }
            self.editor.set_value_at(drag.item, ParamTarget::Effect(drag.effect), &drag.param, Value::Float(degrees), self.playhead, "viewer-point");
            return;
        }
        let at = [drag.at0[0] + u - drag.start[0], drag.at0[1] + v - drag.start[1]];
        self.editor.set_value_at(drag.item, ParamTarget::Effect(drag.effect), &drag.param, Value::Vec2(at), self.playhead, "viewer-point");
    }

    /// A crosshair at each point; a cut line through a line's point, and a round handle
    /// on it that turns it.
    pub(crate) fn paint_points(&self, painter: &egui::Painter, p: &Placement, points: &[EffectPoint], to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, hovered: Option<usize>) {
        let accent = crate::style::GOLD;
        let shadow = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(140));
        for (i, e) in points.iter().enumerate() {
            let c = to_screen(e.on_canvas(p));
            let dragged = self.point_drag.as_ref().is_some_and(|d| d.effect == e.effect && d.param == e.param);
            let lit = hovered == Some(i) || dragged;
            let stroke = egui::Stroke::new(1.5, accent);
            if e.turn {
                // The handle on the line, joined to its point.
                let h = e.on_screen(p, to_screen);
                painter.line_segment([c, h], egui::Stroke::new(1.0, accent.gamma_multiply(0.8)));
                let r = if lit { 6.5 } else { 5.0 };
                painter.circle(h, r, accent.gamma_multiply(if lit { 1.0 } else { 0.7 }), egui::Stroke::new(1.5, egui::Color32::from_black_alpha(160)));
                continue;
            }
            if let Some(d) = e.direction(p, to_screen) {
                // The cut: across the whole view, dashed so the picture shows through.
                let reach = 4000.0;
                let (a, b) = (c - d * reach, c + d * reach);
                painter.line_segment([a, b], shadow);
                painter.add(egui::Shape::dashed_line(&[a, b], egui::Stroke::new(1.5, accent), 8.0, 5.0));
            }
            let r = if lit { 7.5 } else { 6.0 };
            painter.circle_stroke(c, r, shadow);
            painter.circle_stroke(c, r, stroke);
            for d in [egui::vec2(1.0, 0.0), egui::vec2(0.0, 1.0)] {
                painter.line_segment([c - d * (r + 4.0), c - d * (r - 3.0)], stroke);
                painter.line_segment([c + d * (r - 3.0), c + d * (r + 4.0)], stroke);
            }
        }
    }
}
