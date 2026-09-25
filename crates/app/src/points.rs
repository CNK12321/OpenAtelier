//! Effect points on the canvas: a setting that's a place on the clip — a Swirl's or a
//! Vignette's center, any point relative to the clip (`vec2`, `source_fraction`) — shows
//! as a crosshair over the selected clip, dragged there (a key at the playhead when the
//! point is keyframed). Right-click its values in the inspector to make it follow a
//! track, like the clip's own position.

use crate::App;
use eframe::egui;
use oa_doc::{EffectId, ItemId, ParamTarget};
use oa_params::{Unit, Value};
use oa_plan::scene::Placement;
use oa_time::Time;

/// Screen px within which a point can be grabbed.
const GRAB_PX: f32 = 9.0;

/// One effect's point on a clip right now.
pub struct EffectPoint {
    pub effect: EffectId,
    pub param: String,
    /// Where it is, in fractions of the clip.
    pub at: [f64; 2],
    /// The effect's name, for the pointer's tooltip.
    pub name: String,
}

impl EffectPoint {
    fn on_canvas(&self, p: &Placement) -> [f64; 2] {
        p.to_canvas.apply([self.at[0] * p.native[0], self.at[1] * p.native[1]])
    }
}

/// The point nearest `pointer` (screen), if one is within reach.
pub fn point_near(points: &[EffectPoint], p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, e)| (i, to_screen(e.on_canvas(p)).distance(pointer)))
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
}

impl App {
    /// The points of the clip's switched-on effects at `t` (not a Surface's: it has its
    /// own editor).
    pub(crate) fn effect_points(&self, item: ItemId, t: Time) -> Vec<EffectPoint> {
        let Some(it) = self.editor.item(item) else { return Vec::new() };
        let ctx = it.eval_context(t);
        let mut out = Vec::new();
        for fx in it.effects.iter().filter(|e| e.enabled) {
            let Some(d) = self.registry.effect(&fx.type_id) else { continue };
            if d.editor.is_some() || fx.role.clock(ctx.clip_time, it.range.duration).is_none() {
                continue;
            }
            let values = fx.params.eval(&d.params, None, &ctx);
            for s in d.params.iter().filter(|s| s.unit == Unit::SourceFraction && matches!(s.default, Value::Vec2(_))) {
                let at = values.vec2(s.id.as_str());
                out.push(EffectPoint { effect: fx.id, param: s.id.as_str().to_string(), at, name: d.name.clone() });
            }
        }
        out
    }

    pub(crate) fn begin_point_drag(&mut self, p: &Placement, point: &EffectPoint, canvas: [f64; 2]) {
        let Some(start) = p.to_layer_fraction(canvas) else { return };
        self.point_drag = Some(PointDrag { item: p.item, effect: point.effect, param: point.param.clone(), start, at0: point.at });
        self.set_playing(false);
    }

    /// The dragged point follows the pointer (`canvas` px).
    pub(crate) fn drag_point(&mut self, drag: &PointDrag, p: &Placement, canvas: [f64; 2]) {
        let Some([u, v]) = p.to_layer_fraction(canvas) else { return };
        let at = [drag.at0[0] + u - drag.start[0], drag.at0[1] + v - drag.start[1]];
        self.editor.set_value_at(drag.item, ParamTarget::Effect(drag.effect), &drag.param, Value::Vec2(at), self.playhead, "viewer-point");
    }

    /// A crosshair at each point.
    pub(crate) fn paint_points(&self, painter: &egui::Painter, p: &Placement, points: &[EffectPoint], to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, hovered: Option<usize>) {
        let accent = crate::style::GOLD;
        for (i, e) in points.iter().enumerate() {
            let c = to_screen(e.on_canvas(p));
            let dragged = self.point_drag.as_ref().is_some_and(|d| d.effect == e.effect && d.param == e.param);
            let r = if hovered == Some(i) || dragged { 7.5 } else { 6.0 };
            let stroke = egui::Stroke::new(1.5, accent);
            painter.circle_stroke(c, r, egui::Stroke::new(3.0, egui::Color32::from_black_alpha(140)));
            painter.circle_stroke(c, r, stroke);
            for d in [egui::vec2(1.0, 0.0), egui::vec2(0.0, 1.0)] {
                painter.line_segment([c - d * (r + 4.0), c - d * (r - 3.0)], stroke);
                painter.line_segment([c + d * (r - 3.0), c + d * (r + 4.0)], stroke);
            }
        }
    }
}
