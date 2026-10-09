//! Drawing masks in the viewer (the Masks tab's tools) and the tab's shape panel.
//!
//! * Rectangle, Ellipse, Brush, Eraser, Magic select and Fill draw new shapes.
//! * **Edit** reshapes what's drawn: drag a rectangle, ellipse or path to move it, a
//!   rectangle's or ellipse's corner to resize it (the opposite corner holds), a path's
//!   point or curve handle to reshape it (Alt breaks a smooth point's handles apart).
//!   With **Keyframe points** on, a moved path point gets a key at the playhead: points
//!   key one by one, which is how an outline is rotoscoped frame by frame.
//! * **Pen** draws bezier paths: click for a corner, drag for a smooth point with
//!   handles, click the first point (or press Enter) to close it — only closed paths
//!   fill. Esc drops a path of fewer than three points.

use crate::i18n::{tr, trf};
use crate::inspector::Tab;
use crate::masks::{drawing_point, framed, to_drawing, bitmap_size, Drag, Magic, Tool};
use crate::App;
use eframe::egui;
use oa_doc::mask::{self, Bitmap, Mask, MaskShape, MaskValues, PathKey, PathPoint};
use oa_doc::ItemId;
use oa_plan::scene::Placement;
use oa_time::Time;

/// Screen px within which a handle or point can be grabbed.
const GRAB_PX: f32 = 9.0;

/// What an Edit drag holds.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum Grab {
    /// Nothing (drawing a new shape).
    #[default]
    None,
    /// The whole shape.
    Body,
    /// A rectangle's or ellipse's corner: top-left, top-right, bottom-right, bottom-left.
    Corner(usize),
    Point(usize),
    HandleIn(usize),
    HandleOut(usize),
}

/// A rectangle's or ellipse's corners (drawing fractions), in [`Grab::Corner`] order.
fn corners(center: [f64; 2], size: [f64; 2]) -> [[f64; 2]; 4] {
    let (hx, hy) = (size[0] / 2.0, size[1] / 2.0);
    [[center[0] - hx, center[1] - hy], [center[0] + hx, center[1] - hy], [center[0] + hx, center[1] + hy], [center[0] - hx, center[1] + hy]]
}

/// Whether drawing point `d` is inside `shape` (as the Edit tool picks shapes).
fn inside(shape: &MaskShape, d: [f64; 2], t: Time) -> bool {
    match shape {
        MaskShape::Rect { center, size, .. } => (d[0] - center[0]).abs() <= size[0] / 2.0 && (d[1] - center[1]).abs() <= size[1] / 2.0,
        MaskShape::Ellipse { center, size, .. } => {
            let (rx, ry) = ((size[0] / 2.0).max(1e-9), (size[1] / 2.0).max(1e-9));
            ((d[0] - center[0]) / rx).powi(2) + ((d[1] - center[1]) / ry).powi(2) <= 1.0
        }
        MaskShape::Path { points, .. } => {
            let poly = MaskShape::path_outline(points, true, t);
            let mut odd = false;
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                if (a[1] > d[1]) != (b[1] > d[1]) && d[0] < a[0] + (d[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]) {
                    odd = !odd;
                }
            }
            odd
        }
        _ => false,
    }
}

/// `shape` (as it was when the drag started) with what `grab` holds moved by `delta`
/// (drawing fractions), path points keyed at clip time `t` when `animate`.
fn reshaped(shape: &MaskShape, grab: Grab, delta: [f64; 2], t: Time, animate: bool, near: Time, break_handles: bool) -> MaskShape {
    let add = |p: [f64; 2]| [p[0] + delta[0], p[1] + delta[1]];
    let mut out = shape.clone();
    match &mut out {
        MaskShape::Rect { center, size, .. } | MaskShape::Ellipse { center, size, .. } => match grab {
            Grab::Body => *center = add(*center),
            Grab::Corner(k) => {
                let c = corners(*center, *size);
                let (moved, fixed) = (add(c[k]), c[(k + 2) % 4]);
                *center = [(moved[0] + fixed[0]) / 2.0, (moved[1] + fixed[1]) / 2.0];
                *size = [(moved[0] - fixed[0]).abs(), (moved[1] - fixed[1]).abs()];
            }
            _ => {}
        },
        MaskShape::Path { points, .. } => {
            let change = |p: &mut PathPoint, f: &dyn Fn(PathKey) -> PathKey| {
                let key = f(p.at(t));
                p.set(t, key, animate, near);
            };
            match grab {
                Grab::Body => {
                    for p in points.iter_mut() {
                        change(p, &|k| PathKey { at: add(k.at), ..k });
                    }
                }
                Grab::Point(i) => {
                    if let Some(p) = points.get_mut(i) {
                        change(p, &|k| PathKey { at: add(k.at), ..k });
                    }
                }
                Grab::HandleIn(i) | Grab::HandleOut(i) => {
                    let out_side = matches!(grab, Grab::HandleOut(_));
                    if let Some(p) = points.get_mut(i) {
                        change(p, &|k| {
                            let (mine, other) = if out_side { (k.handle_out, k.handle_in) } else { (k.handle_in, k.handle_out) };
                            let mine = add(mine);
                            // A smooth point keeps its handles in line (Alt breaks them).
                            let other = if break_handles { other } else { [-mine[0], -mine[1]] };
                            if out_side { PathKey { handle_out: mine, handle_in: other, ..k } } else { PathKey { handle_in: mine, handle_out: other, ..k } }
                        });
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    out
}

impl App {
    /// Half a frame of the timeline: path keys this close to the playhead are its.
    fn half_frame(&self) -> Time {
        Time((self.editor.sequence().rate.frame_start(1).0 / 2).max(1))
    }

    /// While the Masks tab is open: the selected mask tinted over its clip, its shapes'
    /// outlines and handles, and — with a drawing tool — the viewer's pointer draws into
    /// it. Returns whether it took the pointer (the viewer's own handles stand aside).
    pub(crate) fn mask_viewer(&mut self, ui: &mut egui::Ui, response: &egui::Response, rect: egui::Rect, zoom: f64, available: egui::Rect) -> bool {
        if !self.settings.masking || self.inspector_tab != Tab::Masks {
            return false;
        }
        self.poll_mask_job();
        let (Some(item), Some(id)) = (self.selection, self.masks.selected) else { return false };
        let Some(it) = self.editor.item(item).cloned() else { return false };
        let Some(m) = it.masks.iter().find(|m| m.id == id).cloned() else { return false };
        let t = self.playhead;
        if !it.range.contains(t) {
            return false;
        }
        let local = t - it.range.start;
        let Some(p) = self.mask_placement(item, t) else { return false };
        let values = MaskValues::eval(&it, id, &it.eval_context(t));
        let to_canvas = |q: egui::Pos2| [((q.x - rect.left()) as f64) / zoom, ((q.y - rect.top()) as f64) / zoom];
        let to_screen = |c: [f64; 2]| egui::pos2(rect.left() + (c[0] * zoom) as f32, rect.top() + (c[1] * zoom) as f32);
        // A layer fraction (before the mask's move) → the screen; a drawing point too.
        let screen_of = |f: [f64; 2]| {
            let l = values.place(f, p.native);
            to_screen(p.to_canvas.apply([l[0] * p.native[0], l[1] * p.native[1]]))
        };
        let s = |d: [f64; 2]| screen_of(framed(d, m.frame));
        let painter = ui.painter_at(available);
        self.paint_mask_overlay(ui, &painter, &m, &p, local, &screen_of);
        let outline_color = egui::Color32::from_rgb(255, 110, 150);
        let frame_box: Vec<egui::Pos2> = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]].into_iter().map(s).collect();
        painter.add(egui::Shape::dashed_line(&frame_box, egui::Stroke::new(1.0, outline_color), 6.0, 4.0));

        let tool = self.masks.tool;
        let picked = self.masks.shape.filter(|i| *i < m.shapes.len());
        let picked_point = self.masks.point;
        // Outlines of what can be edited, and the picked shape's handles.
        if matches!(tool, Tool::Edit | Tool::Pen) || picked.is_some() {
            for (i, shape) in m.shapes.iter().enumerate() {
                let line = if Some(i) == picked { egui::Stroke::new(1.8, crate::style::ACCENT) } else { egui::Stroke::new(1.0, outline_color.gamma_multiply(0.8)) };
                let pts: Vec<egui::Pos2> = match shape {
                    MaskShape::Rect { center, size, .. } => corners(*center, *size).into_iter().map(s).collect(),
                    MaskShape::Ellipse { center, size, .. } => (0..48)
                        .map(|k| {
                            let a = k as f64 / 48.0 * std::f64::consts::TAU;
                            s([center[0] + size[0] / 2.0 * a.cos(), center[1] + size[1] / 2.0 * a.sin()])
                        })
                        .collect(),
                    MaskShape::Path { points, closed, .. } => MaskShape::path_outline(points, *closed, local).into_iter().map(s).collect(),
                    _ => continue,
                };
                let closed = !matches!(shape, MaskShape::Path { closed: false, .. });
                if closed {
                    painter.add(egui::Shape::closed_line(pts, line));
                } else {
                    painter.add(egui::Shape::line(pts, line));
                }
                if Some(i) != picked && tool != Tool::Pen {
                    continue;
                }
                match shape {
                    MaskShape::Rect { center, size, .. } | MaskShape::Ellipse { center, size, .. } => {
                        for c in corners(*center, *size) {
                            let r = egui::Rect::from_center_size(s(c), egui::vec2(8.0, 8.0));
                            painter.rect_filled(r, 1.0, egui::Color32::WHITE);
                            painter.rect_stroke(r, 1.0, egui::Stroke::new(1.0, crate::style::ACCENT), egui::StrokeKind::Middle);
                        }
                    }
                    MaskShape::Path { points, .. } => {
                        let near = self.half_frame();
                        for (k, point) in points.iter().enumerate() {
                            let key = point.at(local);
                            let at = s(key.at);
                            if Some(i) == picked && picked_point == Some(k) {
                                for h in [key.handle_in, key.handle_out] {
                                    if h != [0.0, 0.0] {
                                        let hp = s([key.at[0] + h[0], key.at[1] + h[1]]);
                                        painter.line_segment([at, hp], egui::Stroke::new(1.0, crate::style::ACCENT));
                                        painter.circle(hp, 4.0, egui::Color32::WHITE, egui::Stroke::new(1.0, crate::style::ACCENT));
                                    }
                                }
                            }
                            // Gold where the point has a key at the playhead.
                            let keyed = point.animated() && point.keys.iter().any(|k| (k.t - local).0.abs() <= near.0);
                            let fill = match (Some(i) == picked && picked_point == Some(k), keyed) {
                                (true, _) => crate::style::ACCENT,
                                (_, true) => crate::style::GOLD,
                                _ => egui::Color32::WHITE,
                            };
                            let r = egui::Rect::from_center_size(at, egui::vec2(7.0, 7.0));
                            painter.rect_filled(r, 0.0, fill);
                            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::BLACK), egui::StrokeKind::Middle);
                        }
                    }
                    _ => {}
                }
            }
        }
        if tool == Tool::Select {
            return false;
        }
        // Rotoscope: clicks on the clip's picture itself (not the mask's move), for SAM 2.
        if tool == Tool::Rotoscope {
            let layer_screen = |f: [f64; 2]| to_screen(p.to_canvas.apply([f[0] * p.native[0], f[1] * p.native[1]]));
            self.paint_roto_clicks(&painter, item, &layer_screen);
            if response.hover_pos().is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            let (pressed, alt) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.alt));
            // Only on the picture itself, not on what floats over it (the tool strip).
            let pressed = pressed && response.hovered();
            if pressed
                && let Some(o) = ui.input(|i| i.pointer.press_origin()).filter(|o| response.rect.contains(*o))
                && let Some(f) = p.to_layer_fraction(to_canvas(o)).filter(|f| (0.0..=1.0).contains(&f[0]) && (0.0..=1.0).contains(&f[1]))
            {
                self.set_playing(false);
                self.roto_click(item, f, !alt);
            }
            return true;
        }

        let point = |q: egui::Pos2| drawing_point(&p, &values, m.frame, to_canvas(q));
        let (pressed, down, latest, origin, alt) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.latest_pos(), i.pointer.press_origin(), i.modifiers.alt));
        // A press on what floats over the picture (the tool strip, the toolbars) isn't a
        // stroke.
        let pressed = pressed && response.hovered();
        let erase = self.masks.erase ^ alt;
        let near = self.half_frame();
        // The brush's size on screen.
        let brush_screen = (self.masks.brush * p.native[1] * m.frame[1] * values.scale * p.to_canvas.max_axis_scale() * zoom) as f32;
        if let Some(h) = response.hover_pos() {
            match tool {
                Tool::Brush | Tool::Eraser => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                    painter.circle_stroke(h, brush_screen.max(1.0), egui::Stroke::new(1.5, egui::Color32::WHITE));
                    painter.circle_stroke(h, brush_screen.max(1.0) + 1.5, egui::Stroke::new(1.0, egui::Color32::BLACK));
                }
                Tool::Edit => ui.ctx().set_cursor_icon(egui::CursorIcon::Default),
                _ => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
            }
        }
        // The Pen: a line from the last point of an open path to the pointer.
        let open = picked.filter(|i| matches!(m.shapes[*i], MaskShape::Path { closed: false, .. }));
        if tool == Tool::Pen
            && let (Some(i), Some(h)) = (open, response.hover_pos())
            && let MaskShape::Path { points, .. } = &m.shapes[i]
            && let Some(last) = points.last()
        {
            painter.line_segment([s(last.at(local).at), h], egui::Stroke::new(1.0, crate::style::ACCENT.gamma_multiply(0.6)));
        }
        if tool == Tool::Pen && !ui.ctx().egui_wants_keyboard_input() {
            let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if let Some(i) = open {
                let n = if let MaskShape::Path { points, .. } = &m.shapes[i] { points.len() } else { 0 };
                if enter && n >= 3 {
                    self.edit_mask(item, id, "Close path", None, |m| {
                        if let Some(MaskShape::Path { closed, .. }) = m.shapes.get_mut(i) {
                            *closed = true;
                        }
                    });
                } else if escape && n < 3 {
                    self.edit_mask(item, id, "Drop path", None, |m| {
                        m.shapes.remove(i);
                    });
                    self.masks.shape = None;
                }
            }
        }

        if pressed
            && let Some(o) = origin.filter(|o| response.rect.contains(*o))
            && let Some(start) = point(o)
        {
            match tool {
                Tool::Rect | Tool::Ellipse => self.masks.drag = Some(Drag { item, mask: id, start, shape: None, grab: Grab::None, before: None }),
                Tool::Brush | Tool::Eraser => {
                    let shape = MaskShape::Stroke {
                        points: vec![[start[0] as f32, start[1] as f32]],
                        radius: self.masks.brush,
                        softness: self.masks.brush_softness,
                        expand: 0.0,
                        feather: 0.0,
                        erase: tool == Tool::Eraser || alt,
                    };
                    let index = m.shapes.len();
                    self.edit_mask(item, id, "Paint mask", Some("mask-paint"), |m| m.shapes.push(shape));
                    self.masks.drag = Some(Drag { item, mask: id, start, shape: Some(index), grab: Grab::None, before: None });
                }
                Tool::Fill => self.fill_mask(item, &m, &p, framed(start, m.frame), erase, local),
                Tool::Magic => {
                    if self.masks.job.is_none() {
                        self.set_playing(false);
                        self.start_magic(Magic { item, mask: id, click: start, erase, place: p.clone(), values, frame: m.frame });
                    }
                }
                Tool::Edit => {
                    let grab = self.pick_mask_handle(&m, picked, o, local, &s);
                    match grab {
                        Some((i, grab)) => {
                            self.masks.shape = Some(i);
                            self.masks.point = match grab {
                                Grab::Point(k) | Grab::HandleIn(k) | Grab::HandleOut(k) => Some(k),
                                _ => None,
                            };
                            self.set_playing(false);
                            self.masks.drag = Some(Drag { item, mask: id, start, shape: Some(i), grab, before: Some(m.shapes[i].clone()) });
                        }
                        None => (self.masks.shape, self.masks.point) = (None, None),
                    }
                }
                Tool::Pen => {
                    let first = open.and_then(|i| if let MaskShape::Path { points, .. } = &m.shapes[i] { points.first().map(|f| (i, points.len(), f.at(local).at)) } else { None });
                    match first {
                        // Back on the first point: the path closes.
                        Some((i, n, f)) if n >= 3 && s(f).distance(o) <= GRAB_PX => {
                            self.edit_mask(item, id, "Close path", None, |m| {
                                if let Some(MaskShape::Path { closed, .. }) = m.shapes.get_mut(i) {
                                    *closed = true;
                                }
                            });
                            self.masks.point = None;
                        }
                        Some((i, n, _)) => {
                            self.edit_mask(item, id, tr("Add path point"), Some("mask-pen"), |m| {
                                if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(i) {
                                    points.push(PathPoint::new(start));
                                }
                            });
                            self.masks.point = Some(n);
                            self.masks.drag = Some(Drag { item, mask: id, start, shape: Some(i), grab: Grab::HandleOut(n), before: None });
                        }
                        None => {
                            let i = m.shapes.len();
                            let path = MaskShape::Path { points: vec![PathPoint::new(start)], closed: false, expand: 0.0, feather: 0.0, erase };
                            self.edit_mask(item, id, "Draw path", Some("mask-pen"), |m| m.shapes.push(path));
                            (self.masks.shape, self.masks.point) = (Some(i), Some(0));
                            self.masks.drag = Some(Drag { item, mask: id, start, shape: Some(i), grab: Grab::HandleOut(0), before: None });
                        }
                    }
                }
                Tool::Select | Tool::Rotoscope => {}
            }
        }
        if down
            && let (Some(drag), Some(pos)) = (self.masks.drag.as_ref(), latest)
            && drag.item == item
            && drag.mask == id
            && let Some(now) = point(pos)
        {
            let (start, shape, grab, before) = (drag.start, drag.shape, drag.grab, drag.before.clone());
            let delta = [now[0] - start[0], now[1] - start[1]];
            match tool {
                Tool::Rect | Tool::Ellipse => {
                    if delta[0].abs() > 1e-4 || delta[1].abs() > 1e-4 {
                        let new = MaskShape::boxed(tool == Tool::Ellipse, start, now, erase);
                        let index = shape.unwrap_or(m.shapes.len());
                        self.edit_mask(item, id, "Draw mask", Some("mask-shape"), |m| {
                            if index < m.shapes.len() {
                                m.shapes[index] = new;
                            } else {
                                m.shapes.push(new);
                            }
                        });
                        if let Some(d) = self.masks.drag.as_mut() {
                            d.shape = Some(index);
                        }
                    }
                }
                Tool::Brush | Tool::Eraser => {
                    if let Some(index) = shape
                        && let Some(MaskShape::Stroke { points, radius, .. }) = m.shapes.get(index)
                    {
                        // A new point once the pointer has moved a quarter of the brush.
                        let last = points.last().copied().unwrap_or([0.0; 2]);
                        let d = [(now[0] - last[0] as f64) * p.native[0] * m.frame[0], (now[1] - last[1] as f64) * p.native[1] * m.frame[1]];
                        if (d[0] * d[0] + d[1] * d[1]).sqrt() > (radius * p.native[1] * m.frame[1] * 0.25).max(0.5) {
                            self.edit_mask(item, id, "Paint mask", Some("mask-paint"), |m| {
                                if let Some(MaskShape::Stroke { points, .. }) = m.shapes.get_mut(index) {
                                    points.push([now[0] as f32, now[1] as f32]);
                                }
                            });
                        }
                    }
                }
                Tool::Edit => {
                    if let (Some(index), Some(before)) = (shape, before)
                        && (delta[0] != 0.0 || delta[1] != 0.0)
                    {
                        let new = reshaped(&before, grab, delta, local, self.masks.animate_points, near, alt);
                        self.edit_mask(item, id, "Edit mask", Some("mask-edit"), |m| {
                            if let Some(slot) = m.shapes.get_mut(index) {
                                *slot = new;
                            }
                        });
                    }
                }
                Tool::Pen => {
                    // Dragging out of a new point pulls its handles (a smooth point).
                    if let (Some(index), Grab::HandleOut(k)) = (shape, grab) {
                        let h = if (delta[0] * p.native[0]).hypot(delta[1] * p.native[1]) < 2.0 { [0.0, 0.0] } else { delta };
                        self.edit_mask(item, id, "Draw path", Some("mask-pen"), |m| {
                            if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(index)
                                && let Some(key) = points.get_mut(k).and_then(|p| p.keys.first_mut())
                            {
                                key.handle_out = h;
                                key.handle_in = [-h[0], -h[1]];
                            }
                        });
                    }
                }
                _ => {}
            }
        }
        if !down && self.masks.drag.take().is_some() {
            self.editor.doc.seal();
        }
        true
    }

    /// The mask tinted over its clip (a small raster of it, following the layer and the
    /// mask's own move).
    fn paint_mask_overlay(&mut self, ui: &egui::Ui, painter: &egui::Painter, m: &Mask, p: &Placement, local: Time, screen_of: &dyn Fn([f64; 2]) -> egui::Pos2) {
        let key = m.content_hash(local) ^ (m.invert as u64) << 63 ^ (m.enabled as u64) << 62;
        if self.masks.overlay.as_ref().is_none_or(|(k, _)| *k != key) {
            let size = {
                let k = (768.0 / p.native[0].max(p.native[1]).max(1.0)).min(1.0);
                [(p.native[0] * k).round().max(1.0) as u32, (p.native[1] * k).round().max(1.0) as u32]
            };
            let coverage = m.rasterize(size, local);
            let tint = if m.enabled { [255u8, 70, 110] } else { [150, 150, 150] };
            let pixels = coverage
                .iter()
                .map(|c| {
                    let c = if m.invert { 255 - *c } else { *c };
                    egui::Color32::from_rgba_unmultiplied(tint[0], tint[1], tint[2], (c as u32 * 110 / 255) as u8)
                })
                .collect();
            let image = egui::ColorImage { size: [size[0] as usize, size[1] as usize], pixels, source_size: egui::vec2(size[0] as f32, size[1] as f32) };
            self.masks.overlay = Some((key, ui.ctx().load_texture("mask-overlay", image, egui::TextureOptions::LINEAR)));
        }
        if let Some((_, texture)) = &self.masks.overlay {
            let mut mesh = egui::Mesh::with_texture(texture.id());
            for f in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
                mesh.vertices.push(egui::epaint::Vertex { pos: screen_of(f), uv: egui::pos2(f[0] as f32, f[1] as f32), color: egui::Color32::WHITE });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        }
    }

    /// What the Edit tool grabs at screen point `at`: the picked shape's handles and
    /// points first, then any shape under it (the topmost).
    fn pick_mask_handle(&self, m: &Mask, picked: Option<usize>, at: egui::Pos2, local: Time, s: &dyn Fn([f64; 2]) -> egui::Pos2) -> Option<(usize, Grab)> {
        let near = |d: [f64; 2]| s(d).distance(at) <= GRAB_PX;
        let order = picked.into_iter().chain((0..m.shapes.len()).rev().filter(|i| Some(*i) != picked));
        for i in order.clone() {
            match &m.shapes[i] {
                MaskShape::Rect { center, size, .. } | MaskShape::Ellipse { center, size, .. } => {
                    if let Some(k) = corners(*center, *size).into_iter().position(near) {
                        return Some((i, Grab::Corner(k)));
                    }
                }
                MaskShape::Path { points, .. } => {
                    if Some(i) == picked
                        && let Some(k) = self.masks.point.filter(|k| *k < points.len())
                    {
                        let key = points[k].at(local);
                        if key.handle_out != [0.0, 0.0] && near([key.at[0] + key.handle_out[0], key.at[1] + key.handle_out[1]]) {
                            return Some((i, Grab::HandleOut(k)));
                        }
                        if key.handle_in != [0.0, 0.0] && near([key.at[0] + key.handle_in[0], key.at[1] + key.handle_in[1]]) {
                            return Some((i, Grab::HandleIn(k)));
                        }
                    }
                    if let Some(k) = points.iter().position(|p| near(p.at(local).at)) {
                        return Some((i, Grab::Point(k)));
                    }
                }
                _ => {}
            }
        }
        // Otherwise a shape the pointer is inside.
        let d = unscreen(s, at)?;
        order.into_iter().find(|i| inside(&m.shapes[*i], d, local)).map(|i| (i, Grab::Body))
    }

    /// The fill tool at `at` (layer fractions before the mask's move): the empty area
    /// around it, bounded by what's drawn, goes into the mask (or, erasing, the drawn
    /// area around it comes out). Made at the clip's own resolution.
    fn fill_mask(&mut self, item: ItemId, m: &Mask, p: &Placement, at: [f64; 2], erase: bool, local: Time) {
        let size = bitmap_size(p.native);
        let (w, h) = (size[0] as usize, size[1] as usize);
        if !(0.0..1.0).contains(&at[0]) || !(0.0..1.0).contains(&at[1]) {
            return;
        }
        let coverage = m.rasterize(size, local);
        let start = [((at[0] * w as f64) as usize).min(w - 1), ((at[1] * h as f64) as usize).min(h - 1)];
        let region = mask::fill_region(&coverage, [w, h], start);
        let region = to_drawing(&region, size, m.frame);
        if region.iter().any(|c| *c > 0) {
            self.add_shape(item, m.id, MaskShape::Bitmap { bitmap: Bitmap::encode(size, &region), expand: 0.0, feather: 0.0, erase }, "Fill mask");
        }
    }

    // ---- the tab's shape panel ----

    /// The selected mask's shapes: pick one to edit it, its edge (expand and feather),
    /// erase or add, and for a path its points' keys.
    pub(crate) fn mask_shapes_section(&mut self, ui: &mut egui::Ui, item: ItemId, id: u64, native: [f64; 2], t: Time) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let Some(m) = it.masks.iter().find(|m| m.id == id).cloned() else { return };
        if m.shapes.is_empty() {
            return;
        }
        crate::inspector::section(ui, tr("Shapes"), tr("What's drawn in this mask, in order: each adds to it or erases from it. Pick one to reshape it (Edit) or change its edge."));
        let picked = self.masks.shape.filter(|i| *i < m.shapes.len());
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt(("mask-shapes", id)).max_height(150.0).show(ui, |ui| {
            for (i, shape) in m.shapes.iter().enumerate() {
                ui.horizontal(|ui| {
                    let label = format!("{}. {}{}", i + 1, tr(shape.kind_name()), if shape.erases() && !matches!(shape, MaskShape::Stroke { .. }) { tr(" (erases)") } else { "" });
                    if ui.selectable_label(picked == Some(i), label).clicked() {
                        (self.masks.shape, self.masks.point) = (Some(i), None);
                        if matches!(shape, MaskShape::Rect { .. } | MaskShape::Ellipse { .. } | MaskShape::Path { .. }) && self.masks.tool != Tool::Pen {
                            self.masks.tool = Tool::Edit;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(tr("✕")).on_hover_text(tr("Delete this shape")).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        if let Some(i) = remove {
            self.edit_mask(item, id, "Delete shape", None, |m| {
                m.shapes.remove(i);
            });
            (self.masks.shape, self.masks.point) = (None, None);
            return;
        }
        let Some(i) = picked else { return };
        let shape = m.shapes[i].clone();
        // The edge, in the clip's own pixels.
        let (expand, feather) = shape.edge();
        let unit = native[1].max(1.0);
        let (mut e, mut f) = (expand * unit, feather * unit);
        let er = ui.add(egui::Slider::new(&mut e, -200.0..=200.0).text(tr("expand")).suffix(" px")).on_hover_text(tr("Grow the shape outward (or, below zero, shrink it)"));
        let fr = ui.add(egui::Slider::new(&mut f, 0.0..=300.0).text(tr("feather")).suffix(" px")).on_hover_text(tr("How far its own edge fades (the mask's Softness fades every shape together)"));
        if er.changed() || fr.changed() {
            self.edit_mask(item, id, "Shape edge", Some("mask-edge"), |m| {
                if let Some(s) = m.shapes.get_mut(i) {
                    let (ex, fe) = s.edge_mut();
                    (*ex, *fe) = (e / unit, f.max(0.0) / unit);
                }
            });
        }
        if er.drag_stopped() || fr.drag_stopped() || er.lost_focus() || fr.lost_focus() {
            self.editor.doc.seal();
        }
        let mut erasing = shape.erases();
        if ui.checkbox(&mut erasing, tr("Erases")).on_hover_text(tr("Takes this away from the mask instead of adding it")).changed() {
            self.edit_mask(item, id, "Shape erases", None, |m| {
                if let Some(s) = m.shapes.get_mut(i) {
                    *s.erase_mut() = erasing;
                }
            });
        }
        let MaskShape::Path { points, closed, .. } = &shape else { return };
        let local = t - it.range.start;
        let near = self.half_frame();
        let mut close = *closed;
        if ui.checkbox(&mut close, tr("Closed")).on_hover_text(tr("Only a closed path fills")).changed() && points.len() >= 3 {
            self.edit_mask(item, id, "Close path", None, |m| {
                if let Some(MaskShape::Path { closed, .. }) = m.shapes.get_mut(i) {
                    *closed = close;
                }
            });
        }
        ui.checkbox(&mut self.masks.animate_points, tr("Keyframe points (rotoscope)"))
            .on_hover_text(tr("On: moving a point with Edit keys it at the playhead, so the outline can follow something frame by frame. Off: a point that isn't keyframed yet just moves."));
        // Every key of the path, for stepping through them.
        let mut times: Vec<Time> = points.iter().filter(|p| p.animated()).flat_map(|p| p.keys.iter().map(|k| k.t)).collect();
        times.sort();
        times.dedup_by(|a, b| (a.0 - b.0).abs() <= near.0);
        ui.horizontal(|ui| {
            let prev = times.iter().rev().find(|k| k.0 < local.0 - near.0).copied();
            let next = times.iter().find(|k| k.0 > local.0 + near.0).copied();
            if ui.add_enabled(prev.is_some(), egui::Button::new(tr("◀ key"))).clicked() {
                self.set_playhead(it.range.start + prev.expect("enabled"));
            }
            if ui.add_enabled(next.is_some(), egui::Button::new(tr("key ▶"))).clicked() {
                self.set_playhead(it.range.start + next.expect("enabled"));
            }
            ui.label(egui::RichText::new(trf("{0} points · {1} keyed moments", &[("0", &(points.len()).to_string()), ("1", &(times.len()).to_string())])).small().weak());
        });
        ui.horizontal(|ui| {
            if ui.button(tr("Key all points here")).on_hover_text(tr("A key for every point at the playhead, where they are now — the start of rotoscoping")).clicked() {
                self.edit_mask(item, id, "Key path", None, |m| {
                    if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(i) {
                        for p in points.iter_mut() {
                            let k = p.at(local);
                            p.set(local, k, true, near);
                        }
                    }
                });
            }
            if ui.button(tr("Remove keys here")).on_hover_text(tr("The points' keys at the playhead (the picked point's only, when one is picked)")).clicked() {
                let only = self.masks.point;
                self.edit_mask(item, id, tr("Remove path keys"), None, |m| {
                    if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(i) {
                        for (k, p) in points.iter_mut().enumerate() {
                            if only.is_none_or(|o| o == k) {
                                p.remove_key(local, near);
                            }
                        }
                    }
                });
            }
        });
        if let Some(k) = self.masks.point.filter(|k| *k < points.len()) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(trf("Point {0} · {1} keys", &[("0", &(k + 1).to_string()), ("1", &(points[k].keys.len()).to_string())])).small());
                let corner = points[k].at(local).handle_in == [0.0, 0.0] && points[k].at(local).handle_out == [0.0, 0.0];
                if ui.small_button(if corner { "Make smooth" } else { "Make corner" }).clicked() {
                    let animate = self.masks.animate_points;
                    self.edit_mask(item, id, "Point handles", None, |m| {
                        if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(i)
                            && let Some(p) = points.get_mut(k)
                        {
                            let n = smooth_handle();
                            let key = p.at(local);
                            let (hin, hout) = if corner { (n.map(|v| -v), n) } else { ([0.0; 2], [0.0; 2]) };
                            p.set(local, PathKey { handle_in: hin, handle_out: hout, ..key }, animate, near);
                        }
                    });
                }
                if points.len() > 3 && ui.small_button(tr("Delete point")).clicked() {
                    self.edit_mask(item, id, tr("Delete path point"), None, |m| {
                        if let Some(MaskShape::Path { points, .. }) = m.shapes.get_mut(i) {
                            points.remove(k);
                        }
                    });
                    self.masks.point = None;
                }
            });
        }
    }
}

/// A default handle for a corner made smooth: a short horizontal one.
fn smooth_handle() -> [f64; 2] {
    [0.04, 0.0]
}

/// The drawing point that `s` maps to screen point `at` (the mapping is affine: its
/// three corner images give its inverse).
fn unscreen(s: &dyn Fn([f64; 2]) -> egui::Pos2, at: egui::Pos2) -> Option<[f64; 2]> {
    let o = s([0.0, 0.0]);
    let ex = s([1.0, 0.0]) - o;
    let ey = s([0.0, 1.0]) - o;
    let det = (ex.x * ey.y - ex.y * ey.x) as f64;
    if det.abs() < 1e-9 {
        return None;
    }
    let v = at - o;
    let x = (v.x * ey.y - v.y * ey.x) as f64 / det;
    let y = (ex.x * v.y - ex.y * v.x) as f64 / det;
    Some([x, y])
}
