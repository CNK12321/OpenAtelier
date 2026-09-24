//! Cropping on the canvas: double-click a clip in the viewer and it shows its crop —
//! the cut-away part dimmed, rounded squares on the corners and pills on the edges.
//! Drag a corner or an edge to crop; drag inside to slide the crop window over the
//! picture (the clip itself doesn't move). Enter, Esc or clicking outside finishes.
//! While cropping, the pointer belongs to the crop: nothing else grabs it.
//!
//! It writes the same crop properties as the Transform rows (Settings → Advanced
//! transformations), keyframes and per-format scope included.

use crate::App;
use eframe::egui;
use oa_doc::{schema, ItemId};
use oa_edit::transform;
use oa_params::Value;
use oa_plan::scene::Placement;

/// Screen px within which a crop handle can be grabbed.
const GRAB_PX: f32 = 10.0;
/// The least of the picture a crop leaves (a share of each side).
const MIN_KEEP: f64 = 0.02;

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    /// Which edges follow the pointer: left, right, top, bottom.
    Edges([bool; 4]),
    /// The whole crop window slides.
    Pan,
}

/// A crop drag, with where it started (layer px) and the crop then.
#[derive(Clone, Copy)]
pub struct CropDrag {
    pub item: ItemId,
    grab: Grab,
    start: [f64; 2],
    crop0: [f64; 4],
}

/// The eight handles: (which edges, where in layer px, corner?).
fn handles(p: &Placement) -> [([bool; 4], [f64; 2], bool); 8] {
    let [x0, y0, x1, y1] = p.visible_box();
    let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    [
        ([true, false, true, false], [x0, y0], true),
        ([false, true, true, false], [x1, y0], true),
        ([false, true, false, true], [x1, y1], true),
        ([true, false, false, true], [x0, y1], true),
        ([true, false, false, false], [x0, ym], false),
        ([false, true, false, false], [x1, ym], false),
        ([false, false, true, false], [xm, y0], false),
        ([false, false, false, true], [xm, y1], false),
    ]
}

/// A stadium (pill) centered at `c`, `len` long along `dir`, `thick` across.
fn pill(c: egui::Pos2, dir: egui::Vec2, len: f32, thick: f32) -> Vec<egui::Pos2> {
    let d = dir.normalized();
    let n = egui::vec2(-d.y, d.x);
    let r = thick / 2.0;
    let half = (len / 2.0 - r).max(0.0);
    let mut pts = Vec::new();
    for (end, sign) in [(c + d * half, 1.0f32), (c - d * half, -1.0)] {
        for i in 0..=8 {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 8.0;
            pts.push(end + (d * a.cos() + n * a.sin()) * r * sign);
        }
    }
    pts
}

impl App {
    /// Starts cropping `item` on the canvas.
    pub(crate) fn start_crop(&mut self, item: ItemId) {
        self.crop_mode = Some(item);
        self.crop_drag = None;
        self.canvas_text = None;
        self.viewer_drag = None;
    }

    pub(crate) fn end_crop(&mut self) {
        if self.crop_mode.take().is_some() {
            self.crop_drag = None;
            self.editor.doc.seal();
        }
    }

    fn crop_handle_near(p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2) -> Option<[bool; 4]> {
        handles(p)
            .into_iter()
            .map(|(edges, at, _)| (edges, to_screen(p.to_canvas.apply(at)).distance(pointer)))
            .filter(|(_, d)| *d <= GRAB_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(edges, _)| edges)
    }

    /// The cursor for the crop overlay at `pointer` (screen), if it's over something.
    pub(crate) fn crop_cursor(p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2, canvas: [f64; 2]) -> Option<egui::CursorIcon> {
        if let Some(e) = Self::crop_handle_near(p, to_screen, pointer) {
            return Some(match e {
                [true, false, true, false] | [false, true, false, true] => egui::CursorIcon::ResizeNwSe,
                [false, true, true, false] | [true, false, false, true] => egui::CursorIcon::ResizeNeSw,
                [true, false, false, false] | [false, true, false, false] => egui::CursorIcon::ResizeHorizontal,
                _ => egui::CursorIcon::ResizeVertical,
            });
        }
        p.contains(canvas).then_some(egui::CursorIcon::Grab)
    }

    /// A press at `pointer` (screen; `canvas` px): grabs a handle, or the crop window.
    pub(crate) fn begin_crop_drag(&mut self, p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, pointer: egui::Pos2, canvas: [f64; 2]) {
        let grab = match Self::crop_handle_near(p, to_screen, pointer) {
            Some(edges) => Some(Grab::Edges(edges)),
            None if p.contains(canvas) => Some(Grab::Pan),
            None => None,
        };
        self.crop_drag = grab.zip(p.to_layer(canvas)).map(|(grab, start)| CropDrag { item: p.item, grab, start, crop0: p.crop() });
        if self.crop_drag.is_some() {
            self.set_playing(false);
        }
    }

    /// Follows the pointer (`canvas` px) with the grabbed edges or window.
    pub(crate) fn drag_crop(&mut self, drag: CropDrag, p: &Placement, canvas: [f64; 2]) {
        let Some([x, y]) = p.to_layer(canvas) else { return };
        let [w, h] = p.native;
        let [l0, r0, t0, b0] = drag.crop0;
        let (mut l, mut r, mut t, mut b) = (l0, r0, t0, b0);
        match drag.grab {
            Grab::Edges([el, er, et, eb]) => {
                if el {
                    l = (x / w).clamp(0.0, 1.0 - r - MIN_KEEP);
                }
                if er {
                    r = (1.0 - x / w).clamp(0.0, 1.0 - l - MIN_KEEP);
                }
                if et {
                    t = (y / h).clamp(0.0, 1.0 - b - MIN_KEEP);
                }
                if eb {
                    b = (1.0 - y / h).clamp(0.0, 1.0 - t - MIN_KEEP);
                }
            }
            Grab::Pan => {
                // Same size, moved; stopped at the picture's edges.
                let du = ((x - drag.start[0]) / w).clamp(-l0, r0);
                let dv = ((y - drag.start[1]) / h).clamp(-t0, b0);
                (l, r, t, b) = (l0 + du, r0 - du, t0 + dv, b0 - dv);
            }
        }
        let scope = self.transform_scope();
        let (project, seq, variant, at) = (self.editor.doc.snapshot(), self.editor.seq, self.variant_id(), self.playhead);
        let now = p.crop();
        let mut ops = Vec::new();
        for (i, (param, v)) in [(schema::CROP_LEFT, l), (schema::CROP_RIGHT, r), (schema::CROP_TOP, t), (schema::CROP_BOTTOM, b)].into_iter().enumerate() {
            if (v - now[i]).abs() > 1e-9 {
                match transform::write_param(&project, seq, drag.item, variant, scope, at, param, Value::Float(v)) {
                    Ok(op) => ops.push(op),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
        }
        if let Err(e) = self.editor.apply_drag("Crop", "viewer-crop", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// The crop overlay: the cut-away part dimmed, the kept part outlined, handles.
    pub(crate) fn paint_crop(&self, painter: &egui::Painter, p: &Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2) {
        let [w, h] = p.native;
        let [x0, y0, x1, y1] = p.visible_box();
        let s = |x: f64, y: f64| to_screen(p.to_canvas.apply([x, y]));
        let dim = egui::Color32::from_black_alpha(150);
        // Four bands between the whole picture and the kept part.
        for band in [
            [s(0.0, 0.0), s(w, 0.0), s(w, y0), s(0.0, y0)],
            [s(0.0, y1), s(w, y1), s(w, h), s(0.0, h)],
            [s(0.0, y0), s(x0, y0), s(x0, y1), s(0.0, y1)],
            [s(x1, y0), s(w, y0), s(w, y1), s(x1, y1)],
        ] {
            painter.add(egui::Shape::convex_polygon(band.to_vec(), dim, egui::Stroke::NONE));
        }
        let accent = crate::style::ACCENT;
        let whole: Vec<egui::Pos2> = p.full_corners().map(to_screen).to_vec();
        painter.add(egui::Shape::closed_line(whole, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60))));
        let kept: Vec<egui::Pos2> = p.corners().map(to_screen).to_vec();
        painter.add(egui::Shape::closed_line(kept.clone(), egui::Stroke::new(1.5, accent)));

        let outline = egui::Stroke::new(1.5, accent);
        for (edges, at, corner) in handles(p) {
            let c = s(at[0], at[1]);
            if corner {
                let r = egui::Rect::from_center_size(c, egui::vec2(12.0, 12.0));
                painter.rect(r, 3.0, egui::Color32::WHITE, outline, egui::StrokeKind::Middle);
            } else {
                // Along its edge: top/bottom pills lie flat, side pills stand up.
                let along = if edges[0] || edges[1] { kept[3] - kept[0] } else { kept[1] - kept[0] };
                painter.add(egui::Shape::convex_polygon(pill(c, along, 22.0, 7.0), egui::Color32::WHITE, outline));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pills_are_long_along_their_direction() {
        let pts = pill(egui::pos2(0.0, 0.0), egui::vec2(1.0, 0.0), 22.0, 7.0);
        let (xs, ys): (Vec<f32>, Vec<f32>) = pts.iter().map(|p| (p.x, p.y)).unzip();
        let span = |v: &[f32]| v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min);
        assert!((span(&xs) - 22.0).abs() < 0.1 && (span(&ys) - 7.0).abs() < 0.1, "{xs:?} {ys:?}");
    }
}
