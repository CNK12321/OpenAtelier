//! Path properties (`Value::Path`): editing the curve in the viewer, and its controls in
//! the properties panel.
//!
//! In the viewer, the selected clip's path effects show their curve, the band's edges,
//! a square at each point, an arrow where the path starts and a dot where it ends.
//! Drag the curve itself to bend it (its handles follow); click it to add a point
//! there, which keeps its shape; drag a point to move it; pick one to see its two
//! handles and drag those; drag the ticks either side of a point to widen the band
//! there; double-click a point for a sharp or a smooth corner; Delete takes the picked
//! point away. Every change keys the whole path when it's keyframed, like any property.
//!
//! In the panel: shapes to start from, the picked point ("Point 2 of 4", ‹ ›) with its
//! place and its numbers, and + / − to add a point after it or take it away.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{EffectId, ItemId, ParamTarget};
use oa_params::{BezierPath, Value};
use oa_plan::scene::Placement;
use oa_time::Time;

/// How near (screen px) the pointer has to be to grab something.
const GRAB_PX: f32 = 9.0;
/// How far out (screen px) a width tick stands past the band's edge, at least.
const TICK_PX: f32 = 7.0;

/// One path property of the selected clip at this instant.
#[derive(Clone)]
pub struct PathTarget {
    pub effect: EffectId,
    pub param: String,
    pub path: BezierPath,
    /// Its points' numbers' defaults (width, offset…).
    pub defaults: Vec<f64>,
    /// The box its fractions are of, in fractions of the clip (see `App::effect_frame`).
    pub frame: [f64; 4],
}

/// What's under the pointer on a path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathGrab {
    Point(usize),
    /// A point's handle: out (true) or in.
    Handle(usize, bool),
    /// The tick that sets the band's width at a point.
    Width(usize),
    /// The curve: segment, and how far along it.
    Curve(usize, f64),
}

/// A path being dragged in the viewer: what was grabbed, the path when it started and
/// where the pointer was (fractions of the box).
#[derive(Clone)]
pub struct PathDrag {
    pub item: ItemId,
    pub effect: EffectId,
    pub param: String,
    grab: PathGrab,
    start: [f64; 2],
    path0: BezierPath,
    defaults: Vec<f64>,
    frame: [f64; 4],
}

/// The picked point of a path: its clip, effect, property and the point's id.
#[derive(Clone, PartialEq, Debug)]
pub struct PathPick {
    pub item: ItemId,
    pub effect: EffectId,
    pub param: String,
    pub point: u32,
}

/// A point of the box (fractions of it) in the layer's own pixels.
fn layer_px(p: &Placement, frame: [f64; 4], at: [f64; 2]) -> [f64; 2] {
    [(frame[0] + at[0] * frame[2]) * p.native[0], (frame[1] + at[1] * frame[3]) * p.native[1]]
}

/// A point of the box on the canvas.
fn on_canvas(p: &Placement, frame: [f64; 4], at: [f64; 2]) -> [f64; 2] {
    p.to_canvas.apply(layer_px(p, frame, at))
}

/// A canvas point as fractions of the box.
fn in_box(p: &Placement, frame: [f64; 4], c: [f64; 2]) -> Option<[f64; 2]> {
    p.to_layer_fraction(c).map(|[u, v]| [(u - frame[0]) / frame[2], (v - frame[1]) / frame[3]])
}

/// The box's shorter side in layer px (what widths and offsets are fractions of).
fn side_px(p: &Placement, frame: [f64; 4]) -> f64 {
    (frame[2] * p.native[0]).min(frame[3] * p.native[1]).max(1e-6)
}

/// Which way the path goes at point `i` (layer px, unit length): along its handles, or
/// towards its neighbours.
fn tangent(p: &Placement, t: &PathTarget, i: usize) -> [f64; 2] {
    let pts = &t.path.points;
    let s = |v: [f64; 2]| [v[0] * t.frame[2] * p.native[0], v[1] * t.frame[3] * p.native[1]];
    let n = pts.len();
    let way = if pts[i].handle_out != [0.0; 2] {
        s(pts[i].handle_out)
    } else if pts[i].handle_in != [0.0; 2] {
        let h = s(pts[i].handle_in);
        [-h[0], -h[1]]
    } else {
        let (a, b) = (if i > 0 { i - 1 } else if t.path.closed { n - 1 } else { i }, if i + 1 < n { i + 1 } else if t.path.closed { 0 } else { i });
        let (pa, pb) = (layer_px(p, t.frame, pts[a].at), layer_px(p, t.frame, pts[b].at));
        [pb[0] - pa[0], pb[1] - pa[1]]
    };
    let len = way[0].hypot(way[1]);
    if len < 1e-9 { [1.0, 0.0] } else { [way[0] / len, way[1] / len] }
}

/// Point `i`'s place on the band's centre line (pushed sideways by its offset) and the
/// ticks either side of it (layer px).
fn band_at(p: &Placement, t: &PathTarget, i: usize) -> ([f64; 2], [f64; 2], [f64; 2]) {
    let side = side_px(p, t.frame);
    let [tx, ty] = tangent(p, t, i);
    let normal = [-ty, tx];
    let (width, offset) = (t.path.attr(i, 0, &t.defaults).max(0.0) * side, t.path.attr(i, 1, &t.defaults) * side);
    let at = layer_px(p, t.frame, t.path.points[i].at);
    let c = [at[0] + normal[0] * offset, at[1] + normal[1] * offset];
    let half = width / 2.0;
    (c, [c[0] + normal[0] * half, c[1] + normal[1] * half], [c[0] - normal[0] * half, c[1] - normal[1] * half])
}

impl App {
    /// The selected clip's path properties at `t`: its switched-on effects', in their
    /// windows.
    pub(crate) fn path_targets(&self, item: ItemId, t: Time) -> Vec<PathTarget> {
        let Some(it) = self.editor.item(item) else { return Vec::new() };
        let ctx = it.eval_context(t);
        let frame = self.effect_frame(item, t);
        let mut out = Vec::new();
        for fx in it.effects.iter().filter(|e| it.effect_on(e)) {
            let Some(d) = self.registry.effect(&fx.type_id) else { continue };
            if fx.role.clock(ctx.clip_time, it.range.duration).is_none() {
                continue;
            }
            let values = fx.params.eval(&d.params, None, &ctx);
            for s in d.params.iter().filter(|s| s.ty == oa_params::ParamType::Path) {
                if let Some(path) = values.get(s.id.as_str()).and_then(Value::as_path) {
                    out.push(PathTarget { effect: fx.id, param: s.id.as_str().to_string(), path: path.clone(), defaults: s.point_defaults(), frame });
                }
            }
        }
        out
    }

    /// What of a path is under `pointer` (screen): the picked point's handles first, then
    /// points, width ticks, and the curve.
    pub(crate) fn path_hit(pick: Option<&PathPick>, targets: &[PathTarget], p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2) -> Option<(usize, PathGrab)> {
        let near = |q: egui::Pos2| q.distance(pointer) <= GRAB_PX;
        let screen = |t: &PathTarget, at: [f64; 2]| to_screen(on_canvas(p, t.frame, at));
        let layer_screen = |v: [f64; 2]| to_screen(p.to_canvas.apply(v));
        for (k, t) in targets.iter().enumerate() {
            if let Some(i) = picked_point(pick, p.item, t) {
                let pt = &t.path.points[i];
                for (out, h) in [(true, pt.handle_out), (false, pt.handle_in)] {
                    if h != [0.0; 2] && near(screen(t, [pt.at[0] + h[0], pt.at[1] + h[1]])) {
                        return Some((k, PathGrab::Handle(i, out)));
                    }
                }
            }
        }
        for (k, t) in targets.iter().enumerate() {
            if let Some(i) = t.path.points.iter().position(|pt| near(screen(t, pt.at))) {
                return Some((k, PathGrab::Point(i)));
            }
        }
        for (k, t) in targets.iter().enumerate() {
            for i in 0..t.path.points.len() {
                let (_, a, b) = band_at(p, t, i);
                if [a, b].iter().any(|e| near(tick_end(layer_screen(*e), layer_screen(band_at(p, t, i).0)))) {
                    return Some((k, PathGrab::Width(i)));
                }
            }
        }
        // The curve: the nearest of points along each segment, then that spot refined.
        let mut best: Option<(f32, usize, usize, f64)> = None;
        for (k, t) in targets.iter().enumerate() {
            for seg in 0..t.path.segment_count() {
                for s in 0..=32 {
                    let f = s as f64 / 32.0;
                    let d = screen(t, t.path.point_on(seg, f)).distance(pointer);
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, k, seg, f));
                    }
                }
            }
        }
        let (d, k, seg, f) = best?;
        if d > GRAB_PX {
            return None;
        }
        let t = &targets[k];
        let f = (0..=20).map(|s| (f - 1.0 / 32.0 + s as f64 / 320.0).clamp(0.0, 1.0)).min_by(|a, b| screen(t, t.path.point_on(seg, *a)).distance(pointer).total_cmp(&screen(t, t.path.point_on(seg, *b)).distance(pointer))).unwrap_or(f);
        Some((k, PathGrab::Curve(seg, f)))
    }

    /// A press on a path: the drag starts.
    pub(crate) fn begin_path_drag(&mut self, p: &Placement, t: &PathTarget, grab: PathGrab, canvas: [f64; 2]) {
        let Some(start) = in_box(p, t.frame, canvas) else { return };
        if let PathGrab::Point(i) | PathGrab::Handle(i, _) | PathGrab::Width(i) = grab {
            self.path_pick = Some(PathPick { item: p.item, effect: t.effect, param: t.param.clone(), point: t.path.points[i].id });
        }
        self.path_drag = Some(PathDrag { item: p.item, effect: t.effect, param: t.param.clone(), grab, start, path0: t.path.clone(), defaults: t.defaults.clone(), frame: t.frame });
        self.set_playing(false);
    }

    /// The dragged part of the path follows the pointer (`canvas` px).
    pub(crate) fn drag_path(&mut self, drag: &PathDrag, p: &Placement, canvas: [f64; 2]) {
        let Some(q) = in_box(p, drag.frame, canvas) else { return };
        let (dx, dy) = (q[0] - drag.start[0], q[1] - drag.start[1]);
        let mut path = drag.path0.clone();
        match drag.grab {
            PathGrab::Point(i) => {
                let at = drag.path0.points[i].at;
                path.points[i].at = [at[0] + dx, at[1] + dy];
            }
            PathGrab::Handle(i, out) => {
                let pt = &drag.path0.points[i];
                let h = [q[0] - pt.at[0], q[1] - pt.at[1]];
                let other = if out { pt.handle_in } else { pt.handle_out };
                let length = other[0].hypot(other[1]);
                let hl = h[0].hypot(h[1]).max(1e-12);
                // Smooth: the other handle turns with it (keeping its length).
                let mirrored = [-h[0] / hl * length, -h[1] / hl * length];
                let p2 = &mut path.points[i];
                if out {
                    p2.handle_out = h;
                    if pt.smooth {
                        p2.handle_in = mirrored;
                    }
                } else {
                    p2.handle_in = h;
                    if pt.smooth {
                        p2.handle_out = mirrored;
                    }
                }
            }
            PathGrab::Width(i) => {
                let t = PathTarget { effect: drag.effect, param: drag.param.clone(), path: drag.path0.clone(), defaults: drag.defaults.clone(), frame: drag.frame };
                let (c, ..) = band_at(p, &t, i);
                let Some(l) = p.to_layer_fraction(canvas).map(|[u, v]| [u * p.native[0], v * p.native[1]]) else { return };
                let width = 2.0 * (l[0] - c[0]).hypot(l[1] - c[1]) / side_px(p, drag.frame);
                let attrs = &mut path.points[i].attrs;
                if attrs.is_empty() {
                    *attrs = drag.defaults.clone();
                }
                if let Some(w) = attrs.first_mut() {
                    *w = width.clamp(0.0, 4.0);
                }
            }
            PathGrab::Curve(seg, f) => {
                // Bend: the point at `f` goes with the pointer, the two handles of the
                // segment taking up the move (each as much as it pulls there).
                let (b1, b2) = (3.0 * (1.0 - f) * (1.0 - f) * f, 3.0 * (1.0 - f) * f * f);
                let den = (b1 * b1 + b2 * b2).max(1e-9);
                let n = path.points.len();
                let (a, b) = (seg % n, (seg + 1) % n);
                let (ka, kb) = (b1 / den, b2 / den);
                let pa = &drag.path0.points[a];
                let out = [pa.handle_out[0] + dx * ka, pa.handle_out[1] + dy * ka];
                let pb = &drag.path0.points[b];
                let into = [pb.handle_in[0] + dx * kb, pb.handle_in[1] + dy * kb];
                path.points[a].handle_out = out;
                path.points[b].handle_in = into;
                // A smooth point's other handle turns to match.
                for (k, h, other_out) in [(a, out, false), (b, into, true)] {
                    if drag.path0.points[k].smooth {
                        let other = if other_out { drag.path0.points[k].handle_out } else { drag.path0.points[k].handle_in };
                        let (len, hl) = (other[0].hypot(other[1]), h[0].hypot(h[1]).max(1e-12));
                        let m = [-h[0] / hl * len, -h[1] / hl * len];
                        if other_out { path.points[k].handle_out = m } else { path.points[k].handle_in = m }
                    }
                }
            }
        }
        self.set_path(drag.item, drag.effect, &drag.param, path, "viewer-path");
    }

    /// Writes a path (a key at the playhead if it's keyframed).
    fn set_path(&mut self, item: ItemId, effect: EffectId, param: &str, path: BezierPath, coalesce: &str) {
        self.editor.set_value_at(item, ParamTarget::Effect(effect), param, Value::Path(path), self.playhead, coalesce);
    }

    /// A click on a path: a point picks it; the curve gets a point there (its shape
    /// kept), picked. Whether the click was the path's.
    pub(crate) fn click_path(&mut self, p: &Placement, t: &PathTarget, grab: PathGrab) -> bool {
        match grab {
            PathGrab::Point(i) | PathGrab::Handle(i, _) | PathGrab::Width(i) => {
                self.path_pick = Some(PathPick { item: p.item, effect: t.effect, param: t.param.clone(), point: t.path.points[i].id });
                true
            }
            PathGrab::Curve(seg, f) => {
                if t.path.points.len() >= BezierPath::MAX_POINTS {
                    self.notify(trf("A path has at most {n} points.", &[("n", &BezierPath::MAX_POINTS.to_string())]));
                    return true;
                }
                let mut path = t.path.clone();
                let id = path.free_id();
                path.insert(seg, f, id, &t.defaults);
                self.set_path(p.item, t.effect, &t.param, path, "path-add");
                self.editor.doc.seal();
                self.path_pick = Some(PathPick { item: p.item, effect: t.effect, param: t.param.clone(), point: id });
                true
            }
        }
    }

    /// A double-click on a point: a sharp corner becomes smooth (handles along the way
    /// the path goes), a smooth one sharp.
    pub(crate) fn toggle_corner(&mut self, p: &Placement, t: &PathTarget, i: usize) {
        let mut path = t.path.clone();
        let n = path.points.len();
        let sharp = path.points[i].handle_in == [0.0; 2] && path.points[i].handle_out == [0.0; 2];
        if sharp {
            let (a, b) = (if i > 0 { i - 1 } else if path.closed { n - 1 } else { i }, if i + 1 < n { i + 1 } else if path.closed { 0 } else { i });
            let (pa, pb) = (path.points[a].at, path.points[b].at);
            let h = [(pb[0] - pa[0]) / 6.0, (pb[1] - pa[1]) / 6.0];
            path.points[i].handle_out = h;
            path.points[i].handle_in = [-h[0], -h[1]];
            path.points[i].smooth = true;
        } else {
            path.points[i].handle_in = [0.0; 2];
            path.points[i].handle_out = [0.0; 2];
            path.points[i].smooth = false;
        }
        self.set_path(p.item, t.effect, &t.param, path, "path-corner");
        self.editor.doc.seal();
    }

    /// Takes the picked point away (a path keeps two). Whether there was one to take.
    pub(crate) fn remove_picked_point(&mut self) -> bool {
        let Some(pick) = self.path_pick.clone() else { return false };
        let Some(target) = self.path_targets(pick.item, self.playhead).into_iter().find(|t| t.effect == pick.effect && t.param == pick.param) else {
            self.path_pick = None;
            return false;
        };
        let mut path = target.path;
        if path.points.len() <= 2 {
            self.notify(tr("A path needs at least two points."));
            return true;
        }
        path.points.retain(|pt| pt.id != pick.point);
        self.set_path(pick.item, pick.effect, &pick.param, path, "path-remove");
        self.editor.doc.seal();
        self.path_pick = None;
        true
    }

    /// Draws the selected clip's paths: the band's edges, the curve, an arrow at its
    /// start and a dot at its end, a square at each point (the picked one filled, with
    /// its handles), the width ticks; and where a click would add a point.
    pub(crate) fn paint_paths(&self, painter: &egui::Painter, p: &Placement, targets: &[PathTarget], to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, hovered: Option<(usize, PathGrab)>) {
        let accent = crate::style::GOLD;
        let shadow = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(140));
        let line = egui::Stroke::new(1.5, accent);
        let layer_screen = |v: [f64; 2]| to_screen(p.to_canvas.apply(v));
        for (k, t) in targets.iter().enumerate() {
            let screen = |at: [f64; 2]| to_screen(on_canvas(p, t.frame, at));
            // The curve, finely.
            let mut curve = Vec::new();
            for seg in 0..t.path.segment_count() {
                for s in 0..=24 {
                    if s > 0 || seg == 0 {
                        curve.push(screen(t.path.point_on(seg, s as f64 / 24.0)));
                    }
                }
            }
            // The band's edges: through each point's ticks, faint.
            let n = t.path.points.len();
            let edges: Vec<(egui::Pos2, egui::Pos2)> = (0..n).map(|i| band_at(p, t, i)).map(|(_, a, b)| (layer_screen(a), layer_screen(b))).collect();
            let faint = egui::Stroke::new(1.0, accent.gamma_multiply(0.45));
            for side in [0, 1] {
                let pts: Vec<egui::Pos2> = edges.iter().map(|e| if side == 0 { e.0 } else { e.1 }).collect();
                painter.add(egui::Shape::dashed_line(&pts, faint, 5.0, 4.0));
            }
            if curve.len() > 1 {
                painter.add(egui::Shape::line(curve.clone(), shadow));
                painter.add(egui::Shape::line(curve.clone(), line));
            }
            // Which way it runs: an arrow at the start, a dot at the end.
            if let (Some(&a), Some(&b)) = (curve.first(), curve.get(2.min(curve.len() - 1))) {
                let d = (b - a).normalized();
                if d.is_finite() {
                    let n2 = egui::vec2(-d.y, d.x);
                    let tip = a + d * 14.0;
                    painter.add(egui::Shape::convex_polygon(vec![tip, a + n2 * 6.0, a - n2 * 6.0], accent, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(160))));
                }
            }
            if let Some(&end) = curve.last() {
                painter.circle(end, 4.0, accent, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(160)));
            }
            // Width ticks.
            for i in 0..n {
                let (c, a, b) = band_at(p, t, i);
                let c = layer_screen(c);
                for e in [a, b] {
                    let tip = tick_end(layer_screen(e), c);
                    let lit = hovered == Some((k, PathGrab::Width(i)));
                    painter.line_segment([layer_screen(e), tip], egui::Stroke::new(if lit { 3.0 } else { 2.0 }, accent.gamma_multiply(if lit { 1.0 } else { 0.75 })));
                }
            }
            // Points, and the picked one's handles.
            let picked = picked_point(self.path_pick.as_ref(), p.item, t);
            for (i, pt) in t.path.points.iter().enumerate() {
                let c = screen(pt.at);
                if picked == Some(i) {
                    for h in [pt.handle_in, pt.handle_out] {
                        if h != [0.0; 2] {
                            let e = screen([pt.at[0] + h[0], pt.at[1] + h[1]]);
                            painter.line_segment([c, e], egui::Stroke::new(1.0, accent));
                            painter.circle(e, 4.0, accent, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(160)));
                        }
                    }
                }
                let lit = hovered == Some((k, PathGrab::Point(i))) || picked == Some(i);
                let r = if lit { 5.0 } else { 4.0 };
                let rect = egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 2.0));
                painter.rect_filled(rect.expand(1.0), 1.0, egui::Color32::from_black_alpha(160));
                if picked == Some(i) {
                    painter.rect_filled(rect, 1.0, accent);
                } else {
                    painter.rect(rect, 1.0, egui::Color32::from_black_alpha(90), line, egui::StrokeKind::Inside);
                }
            }
            // Where a click on the curve would add a point.
            if let Some((hk, PathGrab::Curve(seg, f))) = hovered
                && hk == k
            {
                let c = screen(t.path.point_on(seg, f));
                painter.circle(c, 6.0, egui::Color32::from_black_alpha(120), line);
                painter.line_segment([c - egui::vec2(3.0, 0.0), c + egui::vec2(3.0, 0.0)], line);
                painter.line_segment([c - egui::vec2(0.0, 3.0), c + egui::vec2(0.0, 3.0)], line);
            }
        }
    }

    /// A path property in the properties panel: its key, shapes to start from, the
    /// picked point and its numbers, + and −.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn path_widget(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, schema: &oa_params::ParamSchema, current: &Value, t: Time, label: &str) {
        let id = schema.id.as_str();
        let Some(path) = current.as_path().cloned() else { return };
        let ParamTarget::Effect(effect) = target else { return };
        let defaults = schema.point_defaults();
        let mut write: Option<(BezierPath, bool)> = None;
        ui.horizontal(|ui| {
            self.key_toggle_for(ui, item, target.clone(), id, schema.default.clone(), t);
            let r = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
            self.property_menu(&r, item, target, id, &schema.default, None, t);
            // Shapes to start from (each replaces the path, still editable).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(tr("Shape"), |ui| {
                    for (name, shape) in shapes() {
                        if ui.button(tr(name)).clicked() {
                            write = Some((shape, true));
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text(tr("Start from a shape: it replaces the path"));
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_space(24.0);
            ui.label(egui::RichText::new(tr("Drag the curve in the viewer to bend it; click it to add a point. Moving any point keys the whole path.")).small().weak());
        });
        // The picked point (else the first), stepped through with ‹ ›.
        let n = path.points.len();
        let pick = self.path_pick.clone().filter(|k| k.item == item && k.effect == *effect && k.param == id);
        let at = pick.as_ref().and_then(|k| path.points.iter().position(|pt| pt.id == k.point)).unwrap_or(0).min(n.saturating_sub(1));
        let choose = |app: &mut App, i: usize| app.path_pick = Some(PathPick { item, effect: *effect, param: id.to_string(), point: path.points[i].id });
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            if ui.add_enabled(n > 1, egui::Button::new("‹").small()).on_hover_text(tr("Previous point")).clicked() {
                choose(self, (at + n - 1) % n);
            }
            ui.label(trf("Point {i} of {n}", &[("i", &(at + 1).to_string()), ("n", &n.to_string())]));
            if ui.add_enabled(n > 1, egui::Button::new("›").small()).on_hover_text(tr("Next point")).clicked() {
                choose(self, (at + 1) % n);
            }
            if ui.add_enabled(n < BezierPath::MAX_POINTS, egui::Button::new("+").small()).on_hover_text(tr("Add a point after this one, halfway along (the curve keeps its shape)")).clicked() {
                let mut p = path.clone();
                let new = p.free_id();
                // After the picked point; after the last, the last segment's middle.
                let seg = at.min(p.segment_count().saturating_sub(1));
                p.insert(seg, 0.5, new, &defaults);
                write = Some((p, true));
                self.path_pick = Some(PathPick { item, effect: *effect, param: id.to_string(), point: new });
            }
            if ui.add_enabled(n > 2, egui::Button::new("−").small()).on_hover_text(tr("Take this point away")).clicked() {
                let mut p = path.clone();
                p.points.remove(at);
                write = Some((p, true));
                self.path_pick = None;
            }
        });
        if n > 0 {
            let mut p = path.clone();
            let mut changed = false;
            let mut finished = false;
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                for (k, axis) in ["x", "y"].iter().enumerate() {
                    let mut v = p.points[at].at[k] * 100.0;
                    let r = ui.add(egui::DragValue::new(&mut v).speed(0.5).suffix(" %").prefix(format!("{axis} ")));
                    if r.changed() {
                        p.points[at].at[k] = v / 100.0;
                        changed = true;
                    }
                    finished |= r.drag_stopped() || r.lost_focus();
                }
                let mut smooth = p.points[at].smooth;
                if ui.checkbox(&mut smooth, tr("smooth")).on_hover_text(tr("Its two handles stay in a line (double-click the point in the viewer for a sharp corner)")).changed() {
                    p.points[at].smooth = smooth;
                    changed = true;
                    finished = true;
                }
            });
            for (k, attr) in schema.point_attrs.iter().enumerate() {
                let mut v = p.attr(at, k, &defaults);
                let (lo, hi) = attr.range.unwrap_or((-1.0, 1.0));
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let r = ui.add(egui::Slider::new(&mut v, lo..=hi).text(crate::i18n::t(&attr.id.replace('_', " "))).clamping(egui::SliderClamping::Edits));
                    if r.changed() {
                        let attrs = &mut p.points[at].attrs;
                        if attrs.len() < defaults.len() {
                            let have = attrs.len();
                            attrs.extend_from_slice(&defaults[have..]);
                        }
                        if let Some(slot) = attrs.get_mut(k) {
                            *slot = v;
                        }
                        changed = true;
                    }
                    finished |= r.drag_stopped();
                });
            }
            if changed {
                write = Some((p, finished));
            } else if finished {
                self.editor.doc.seal();
            }
        }
        if let Some((p, seal)) = write {
            self.editor.set_value_at(item, target.clone(), id, Value::Path(p), t, id);
            if seal {
                self.editor.doc.seal();
            }
        }
    }
}

/// The picked point's index in `t`'s path, if it's the one picked.
fn picked_point(pick: Option<&PathPick>, item: ItemId, t: &PathTarget) -> Option<usize> {
    let pick = pick.filter(|k| k.item == item && k.effect == t.effect && k.param == t.param)?;
    t.path.points.iter().position(|pt| pt.id == pick.point)
}

/// Where a width tick ends: past the band's edge `edge`, away from its centre `c`.
fn tick_end(edge: egui::Pos2, c: egui::Pos2) -> egui::Pos2 {
    let d = edge - c;
    let len = d.length();
    if len < 1e-3 { edge + egui::vec2(0.0, -TICK_PX) } else { edge + d / len * TICK_PX }
}

/// The shapes a path can start from (fractions of the clip).
fn shapes() -> Vec<(&'static str, BezierPath)> {
    let smooth = |points: &[([f64; 2], [f64; 2])]| BezierPath {
        closed: false,
        points: points.iter().enumerate().map(|(i, &(at, h))| oa_params::PathPoint { id: i as u32 + 1, at, handle_in: [-h[0], -h[1]], handle_out: h, attrs: Vec::new(), smooth: h != [0.0; 2] }).collect(),
    };
    let mut arc = smooth(&[([0.1, 0.65], [0.0; 2]), ([0.9, 0.65], [0.0; 2])]);
    arc.points[0].handle_out = [0.25, -0.35];
    arc.points[1].handle_in = [-0.25, -0.35];
    let mut circle = smooth(&[([0.5, 0.15], [0.19, 0.0]), ([0.85, 0.5], [0.0, 0.19]), ([0.5, 0.85], [-0.19, 0.0]), ([0.15, 0.5], [0.0, -0.19])]);
    circle.closed = true;
    vec![
        ("Straight", BezierPath::through(&[[0.1, 0.5], [0.9, 0.5]])),
        ("Arc", arc),
        ("S-curve", smooth(&[([0.1, 0.7], [0.0; 2]), ([0.5, 0.5], [0.15, -0.2]), ([0.9, 0.3], [0.0; 2])])),
        ("Wave", smooth(&[([0.05, 0.5], [0.08, -0.2]), ([0.35, 0.5], [0.08, 0.2]), ([0.65, 0.5], [0.08, -0.2]), ([0.95, 0.5], [0.08, 0.2])])),
        ("Circle", circle),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape is a usable path: points with ids of their own, nothing off the clip.
    #[test]
    fn shapes_are_usable_paths() {
        for (name, p) in shapes() {
            assert!(p.points.len() >= 2, "{name}");
            assert_eq!(p.clone().repaired(), p, "{name}: ids unique, numbers finite");
            let [lo, hi] = p.bounds().unwrap();
            assert!(lo[0] >= -0.1 && lo[1] >= -0.1 && hi[0] <= 1.1 && hi[1] <= 1.1, "{name}: {lo:?} {hi:?}");
        }
    }
}
