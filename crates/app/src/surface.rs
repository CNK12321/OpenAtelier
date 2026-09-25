//! Surface on the canvas: a clip with a **Surface** effect shows the effect's points in
//! place of its transform handles. Each point drags on its own (a key at the playhead
//! when that point is keyframed), and the lines between them show the shape — they're
//! straight, as the edges of the picture's pieces are. Dragging inside still moves the
//! whole clip.

use crate::App;
use eframe::egui;
use oa_doc::{EffectId, EffectRole, ItemId, ParamTarget};
use oa_graph::registry::{surface_point_id, surface_points, EDITOR_SURFACE};
use oa_params::Value;
use oa_plan::scene::Placement;
use oa_time::Time;

/// Screen px within which a point can be grabbed.
const GRAB_PX: f32 = 10.0;

/// A clip's surface right now: the effect, points a side, and each point (fractions of
/// the layer, row by row from the top left).
pub struct Surface {
    pub effect: EffectId,
    pub n: usize,
    pub points: Vec<[f64; 2]>,
}

impl Surface {
    /// Where point `i` is on the canvas.
    fn on_canvas(&self, p: &Placement, i: usize) -> [f64; 2] {
        let [u, v] = self.points[i];
        p.to_canvas.apply([u * p.native[0], v * p.native[1]])
    }

    /// The point nearest `pointer` (screen), if one is within reach.
    pub fn point_near(&self, p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2) -> Option<usize> {
        (0..self.points.len())
            .map(|i| (i, to_screen(self.on_canvas(p, i)).distance(pointer)))
            .filter(|(_, d)| *d <= GRAB_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

/// A point being dragged: where the pointer and the point's offset were at the start.
#[derive(Clone, Copy)]
pub struct SurfaceDrag {
    pub item: ItemId,
    effect: EffectId,
    point: (usize, usize),
    start: [f64; 2],
    offset0: [f64; 2],
}

impl App {
    /// The clip's first switched-on Surface (a passive one: intros and outros animate
    /// on their own), at `t`.
    pub(crate) fn surface_of(&self, item: ItemId, t: Time) -> Option<Surface> {
        let it = self.editor.item(item)?;
        let is_surface = |type_id: &str| self.registry.effect(type_id).is_some_and(|d| d.editor.as_deref() == Some(EDITOR_SURFACE));
        let fx = it.effects.iter().find(|e| e.enabled && e.role == EffectRole::Passive && is_surface(&e.type_id))?;
        let d = self.registry.effect(&fx.type_id)?;
        let values = fx.params.eval(&d.params, None, &it.eval_context(t));
        let (n, points) = surface_points(&values);
        Some(Surface { effect: fx.id, n, points })
    }

    /// A press on point `i` of `s` at `canvas` (px).
    pub(crate) fn begin_surface_drag(&mut self, p: &Placement, s: &Surface, i: usize, canvas: [f64; 2]) {
        let Some(start) = p.to_layer_fraction(canvas) else { return };
        let (r, c) = (i / s.n, i % s.n);
        let side = (s.n - 1) as f64;
        let rest = [c as f64 / side, r as f64 / side];
        let offset0 = [s.points[i][0] - rest[0], s.points[i][1] - rest[1]];
        self.surface_drag = Some(SurfaceDrag { item: p.item, effect: s.effect, point: (r, c), start, offset0 });
        self.set_playing(false);
    }

    /// The dragged point follows the pointer (`canvas` px).
    pub(crate) fn drag_surface(&mut self, drag: SurfaceDrag, p: &Placement, canvas: [f64; 2]) {
        let Some([u, v]) = p.to_layer_fraction(canvas) else { return };
        let offset = [drag.offset0[0] + u - drag.start[0], drag.offset0[1] + v - drag.start[1]];
        let id = surface_point_id(drag.point.0, drag.point.1);
        self.editor.set_value_at(drag.item, ParamTarget::Effect(drag.effect), &id, Value::Vec2(offset), self.playhead, "viewer-surface");
    }

    /// The mesh and its points over the clip.
    pub(crate) fn paint_surface(&self, painter: &egui::Painter, p: &Placement, s: &Surface, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, hovered: Option<usize>) {
        let accent = crate::style::ACCENT;
        let at = |i: usize| to_screen(s.on_canvas(p, i));
        let n = s.n;
        for k in 0..n {
            for j in 0..n - 1 {
                // Along row k, then down column k.
                let outer = k == 0 || k == n - 1;
                let stroke = egui::Stroke::new(if outer { 1.5 } else { 1.0 }, if outer { accent } else { accent.gamma_multiply(0.6) });
                painter.line_segment([at(k * n + j), at(k * n + j + 1)], stroke);
                painter.line_segment([at(j * n + k), at((j + 1) * n + k)], stroke);
            }
        }
        let dragged = self.surface_drag.filter(|d| d.item == p.item).map(|d| d.point.0 * n + d.point.1);
        for i in 0..n * n {
            let big = hovered == Some(i) || dragged == Some(i);
            painter.circle(at(i), if big { 6.5 } else { 5.0 }, egui::Color32::WHITE, egui::Stroke::new(1.5, accent));
        }
    }
}
