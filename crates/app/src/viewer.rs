//! The viewer: the rendered frame, plus direct manipulation on top of it.
//!
//! Click a layer to select it; drag it to move; drag a corner or edge handle to scale
//! around the anchor, the knob above the top edge to rotate, the center dot to move the
//! anchor. Shift frees the aspect ratio (or locks a move to one axis); Ctrl steps
//! rotation by 15° and turns snapping off. All geometry comes from `oa_edit` /
//! `oa_plan::scene`, i.e. the renderer's own placement math.

use crate::App;
use eframe::egui;
use oa_edit::transform::{Gesture, Guide, Handle, Handles, Modifiers, Scope};
use oa_plan::scene;

/// Screen px within which a handle can be grabbed.
const GRAB_PX: f64 = 8.0;
/// Screen px within which moves snap to the canvas edges and center.
const SNAP_PX: f64 = 6.0;
/// How far the rotate knob sits above the top edge, in screen px.
const ROTATE_KNOB_PX: f64 = 28.0;

/// How the viewer shows the canvas: zoom, pan and overlay guides.
pub struct ViewerView {
    /// Screen points per canvas pixel; `None` fits the canvas to the panel.
    pub zoom: Option<f32>,
    /// Offset of the canvas center from the panel center, in screen points.
    pub pan: egui::Vec2,
    pub thirds: bool,
    pub safe_areas: bool,
    pub center: bool,
}

impl Default for ViewerView {
    fn default() -> Self {
        ViewerView { zoom: None, pan: egui::Vec2::ZERO, thirds: false, safe_areas: false, center: false }
    }
}

/// A drag in progress on the viewer.
pub struct ViewerDrag {
    pub gesture: Gesture,
    pub guides: Vec<Guide>,
}

fn cursor_for(handle: Handle, rotation: f64) -> egui::CursorIcon {
    match handle {
        Handle::Body => egui::CursorIcon::Move,
        Handle::Rotate => egui::CursorIcon::Alias,
        Handle::Anchor => egui::CursorIcon::Crosshair,
        Handle::Corner(i) | Handle::Edge(i) => {
            // Pick the resize arrow closest to the handle's on-screen direction.
            let base = match handle {
                Handle::Corner(_) => [-135.0, -45.0, 45.0, 135.0][i],
                _ => [-90.0, 0.0, 90.0, 180.0][i],
            };
            let angle = (base + rotation).rem_euclid(180.0);
            match ((angle + 22.5) / 45.0) as i32 % 4 {
                0 => egui::CursorIcon::ResizeHorizontal,
                1 => egui::CursorIcon::ResizeNwSe,
                2 => egui::CursorIcon::ResizeVertical,
                _ => egui::CursorIcon::ResizeNeSw,
            }
        }
    }
}

impl App {
    /// Where transform edits go: the primary format edits every format; the others edit
    /// only themselves unless "link formats" is on.
    pub(crate) fn transform_scope(&self) -> Scope {
        if self.variant == 0 || self.link_formats { Scope::AllFormats } else { Scope::Variant(self.variant_id()) }
    }

    /// Zoom presets and guide toggles above the picture.
    fn viewer_toolbar(&mut self, ui: &mut egui::Ui, canvas: [f64; 2]) {
        let ppp = ui.ctx().pixels_per_point();
        ui.horizontal(|ui| {
            let view = &mut self.view;
            if ui.selectable_label(view.zoom.is_none(), "Fit").on_hover_text("Fit the whole frame (Ctrl+0)").clicked() {
                view.zoom = None;
                view.pan = egui::Vec2::ZERO;
            }
            // Percentages are canvas pixels per physical screen pixel: 100% is 1:1.
            for pct in [50.0f32, 100.0, 200.0] {
                let z = pct / 100.0 / ppp;
                let on = view.zoom.is_some_and(|v| (v - z).abs() < 1e-4);
                if ui.selectable_label(on, format!("{pct:.0}%")).clicked() {
                    view.zoom = Some(z);
                    view.pan = egui::Vec2::ZERO;
                }
            }
            if let Some(z) = view.zoom {
                ui.label(egui::RichText::new(format!("{:.0}%", z * ppp * 100.0)).weak());
            }
            ui.separator();
            ui.toggle_value(&mut view.thirds, "Thirds").on_hover_text("Rule-of-thirds grid");
            ui.toggle_value(&mut view.center, "Center").on_hover_text("Center lines");
            ui.toggle_value(&mut view.safe_areas, "Safe areas").on_hover_text("Action safe (93%) and title safe (90%); tall formats also shade where phone apps put their buttons and captions");
            ui.separator();
            let vertical = self.vertical_layout();
            if ui
                .selectable_label(vertical, "Vertical layout")
                .on_hover_text("For vertical video: the viewer takes the tall column on the right and the properties move to the middle. Remembered with the project.")
                .clicked()
            {
                self.toggle_layout();
            }
            ui.label(egui::RichText::new(format!("{}×{}", canvas[0], canvas[1])).weak().small())
                .on_hover_text("Ctrl+wheel zooms around the pointer; middle-drag (or the wheel, when zoomed) pans");
        });
        // Keyboard: Ctrl+0 fit, Ctrl+1 100%.
        if !ui.ctx().egui_wants_keyboard_input() {
            let (fit, one) = ui.input_mut(|i| {
                (i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0), i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1))
            });
            if fit {
                self.view.zoom = None;
                self.view.pan = egui::Vec2::ZERO;
            }
            if one {
                self.view.zoom = Some(1.0 / ppp);
                self.view.pan = egui::Vec2::ZERO;
            }
        }
    }

    /// Composition guides over the frame (`rect` is the canvas on screen).
    fn draw_guides(&self, painter: &egui::Painter, rect: egui::Rect) {
        let line = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(110));
        let at = |fx: f32, fy: f32| egui::pos2(rect.left() + rect.width() * fx, rect.top() + rect.height() * fy);
        if self.view.thirds {
            for f in [1.0 / 3.0, 2.0 / 3.0] {
                painter.line_segment([at(f, 0.0), at(f, 1.0)], line);
                painter.line_segment([at(0.0, f), at(1.0, f)], line);
            }
        }
        if self.view.center {
            painter.line_segment([at(0.5, 0.0), at(0.5, 1.0)], line);
            painter.line_segment([at(0.0, 0.5), at(1.0, 0.5)], line);
        }
        if self.view.safe_areas {
            for (inset, alpha) in [(0.035, 140u8), (0.05, 90)] {
                let r = egui::Rect::from_min_max(at(inset, inset), at(1.0 - inset, 1.0 - inset));
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(120, 200, 255, alpha)), egui::StrokeKind::Middle);
            }
            // Tall formats end up under a phone app's own buttons and captions (TikTok,
            // Reels, Shorts — their overlap, from the platforms' templates): shade those.
            if rect.height() > rect.width() * 1.5 {
                let shade = egui::Color32::from_rgba_unmultiplied(255, 90, 90, 38);
                let text = egui::Color32::from_rgba_unmultiplied(255, 160, 160, 200);
                let zones = [
                    (egui::Rect::from_min_max(at(0.0, 0.0), at(1.0, 0.1)), "status bar & tabs"),
                    (egui::Rect::from_min_max(at(0.0, 0.78), at(1.0, 1.0)), "caption & sound"),
                    (egui::Rect::from_min_max(at(0.86, 0.35), at(1.0, 0.78)), "buttons"),
                ];
                for (zone, label) in zones {
                    painter.rect_filled(zone, 0.0, shade);
                    if zone.height() > 14.0 && zone.width() > 40.0 {
                        painter.text(zone.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), text);
                    }
                }
            }
        }
        // A thin frame so the canvas edge reads when zoomed out over the dark panel.
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_white_alpha(30)), egui::StrokeKind::Outside);
    }

    /// Draws the preview into the available space and handles pointer interaction.
    pub(crate) fn viewer(&mut self, ui: &mut egui::Ui) {
        let Some(preview_id) = self.preview.as_ref().map(|p| p.id) else {
            ui.centered_and_justified(|ui| {
                ui.label("Import media (or drop files here) to start.");
            });
            return;
        };
        let canvas = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
        let canvas = [canvas.width as f64, canvas.height as f64];
        self.viewer_toolbar(ui, canvas);
        let available = ui.available_rect_before_wrap();
        let fit = (available.width() / canvas[0] as f32).min(available.height() / canvas[1] as f32);
        let response = ui.allocate_rect(available, egui::Sense::click_and_drag());

        // Zoom (Ctrl+wheel, around the pointer) and pan (middle drag, or the wheel when
        // zoomed in).
        let view = &mut self.view;
        if let Some(pointer) = response.hover_pos() {
            let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            if zoom_delta != 1.0 {
                let old = view.zoom.unwrap_or(fit);
                let new = (old * zoom_delta).clamp(fit * 0.1, 32.0);
                // Keep the canvas point under the pointer where it is.
                let center = available.center() + view.pan;
                view.pan += (pointer - center) * (1.0 - new / old);
                view.zoom = Some(new);
            } else if view.zoom.is_some() && scroll != egui::Vec2::ZERO {
                view.pan += scroll;
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            view.pan += response.drag_delta();
            view.zoom.get_or_insert(fit);
        }
        let scale = view.zoom.unwrap_or(fit);
        let pan = if view.zoom.is_some() { view.pan } else { egui::Vec2::ZERO };
        let size = egui::vec2(canvas[0] as f32 * scale, canvas[1] as f32 * scale);
        let rect = egui::Rect::from_center_size(available.center() + pan, size);
        let clip = ui.painter_at(available);
        clip.image(preview_id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        self.draw_guides(&clip, rect);

        self.display_scale = rect.width() / canvas[0] as f32 * ui.ctx().pixels_per_point();
        let zoom = rect.width() as f64 / canvas[0]; // screen px per canvas px
        let to_canvas = |p: egui::Pos2| [((p.x - rect.left()) as f64) / zoom, ((p.y - rect.top()) as f64) / zoom];
        let to_screen = |c: [f64; 2]| egui::pos2(rect.left() + (c[0] * zoom) as f32, rect.top() + (c[1] * zoom) as f32);
        let (seq, variant_id, t) = (self.editor.seq, self.variant_id(), self.playhead);
        let modifiers = ui.input(|i| i.modifiers);
        let mods = Modifiers { free: modifiers.shift, step: modifiers.command };

        // The track editor takes the viewer over while it's open.
        if self.track_editor.is_some() {
            self.viewer_canvas = Some((available, rect));
            self.track_overlay(ui, &response, &to_canvas, &to_screen, available);
            return;
        }

        // Selection geometry at this instant — only if the selected clip is on screen now.
        let project = self.editor.doc.snapshot();
        let variant = self.editor.sequence().variant(variant_id).cloned();
        let on_screen = |id: oa_doc::ItemId| -> Option<scene::Placement> {
            let v = variant.as_ref()?;
            scene::layers_at(&project, seq, v, t).into_iter().find(|l| l.item == id)
        };
        let selected = self.selection.and_then(on_screen);
        // A Surface effect's points stand in for the transform handles.
        let surface = selected.as_ref().and_then(|p| self.surface_of(p.item, t));
        let handles = selected.as_ref().filter(|_| surface.is_none()).map(|p| Handles::new(p, ROTATE_KNOB_PX / zoom));
        let pick = |pointer: [f64; 2]| -> Option<Handle> {
            let (p, h) = (selected.as_ref()?, handles.as_ref()?);
            h.pick(p, pointer, GRAB_PX / zoom)
        };
        let surface_point = |pointer: egui::Pos2| -> Option<usize> { surface.as_ref()?.point_near(selected.as_ref()?, &to_screen, pointer) };
        let hit = |pointer: [f64; 2]| variant.as_ref().and_then(|v| scene::hit_test(&project, seq, v, t, pointer));

        // Pointer feedback.
        let mut hovered_layer = None;
        let mut hovered_point = None;
        if let Some(pos) = response.hover_pos().filter(|_| self.viewer_drag.is_none() && self.surface_drag.is_none()) {
            let c = to_canvas(pos);
            hovered_point = surface_point(pos);
            match pick(c) {
                _ if hovered_point.is_some() => ui.ctx().set_cursor_icon(egui::CursorIcon::Grab),
                Some(h) => {
                    let rotation = selected.as_ref().map_or(0.0, |p| p.values.float(oa_doc::schema::ROTATION));
                    ui.ctx().set_cursor_icon(cursor_for(h, rotation));
                }
                None => hovered_layer = hit(c).filter(|id| Some(*id) != self.selection),
            }
        }
        // An effect dragged out of the inspector: the clip under the pointer is where a copy
        // would go; let go to copy it there.
        // Where the canvas is, for dropping effects dragged out of the inspector; while
        // one is dragged, the clip under the pointer is outlined.
        self.viewer_canvas = Some((available, rect));
        let mut effect_drop = None;
        if let Some(drag) = self.effect_drag.clone()
            && let Some(pos) = ui.input(|i| i.pointer.latest_pos()).filter(|p| available.contains(*p))
            && let Some(id) = hit(to_canvas(pos)).filter(|id| *id != drag.from)
        {
            effect_drop = Some((id, self.effect_fits(id, &drag.effect.type_id)));
        }

        // Cropping (double-click a clip): its crop handles come before everything else.
        let cropping = self.crop_mode.and_then(on_screen);
        if self.crop_mode.is_some() && cropping.is_none() {
            self.end_crop();
        }
        if let Some(p) = &cropping {
            // The press point, the moment the button goes down (not when the drag is
            // recognized a few pixels later).
            if response.is_pointer_button_down_on()
                && self.crop_drag.is_none()
                && ui.input(|i| i.pointer.primary_pressed())
                && let Some(origin) = ui.input(|i| i.pointer.press_origin())
            {
                self.begin_crop_drag(p, &to_screen, origin, to_canvas(origin));
            }
            if let (Some(drag), Some(pos)) = (self.crop_drag, ui.input(|i| i.pointer.latest_pos()))
                && ui.input(|i| i.pointer.primary_down())
            {
                if pos.distance(ui.input(|i| i.pointer.press_origin()).unwrap_or(pos)) > 0.5 {
                    self.drag_crop(drag, p, to_canvas(pos));
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if self.crop_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
                self.crop_drag = None;
                self.editor.doc.seal();
            }
            if self.crop_drag.is_none()
                && let Some(h) = response.hover_pos()
                && let Some(cursor) = Self::crop_cursor(p, &to_screen, h, to_canvas(h))
            {
                ui.ctx().set_cursor_icon(cursor);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !ui.ctx().egui_wants_keyboard_input() {
                self.end_crop();
            }
        }

        // Press: grab a handle of the selected layer, or select-and-move what's under it
        // (not while cropping: the pointer belongs to the crop then).
        if response.drag_started_by(egui::PointerButton::Primary) && self.crop_mode.is_none() {
            let origin = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
            if let (Some(origin), Some(p), Some(s)) = (origin, selected.as_ref(), surface.as_ref())
                && let Some(i) = s.point_near(p, &to_screen, origin)
            {
                self.begin_surface_drag(p, s, i, to_canvas(origin));
            } else if let Some(origin) = origin {
                let c = to_canvas(origin);
                let grabbed = match pick(c) {
                    Some(h) => self.selection.map(|id| (id, h)),
                    None => hit(c).map(|id| (id, Handle::Body)),
                };
                if let Some((item, handle)) = grabbed {
                    self.selection = Some(item);
                    self.set_playing(false);
                    let scope = self.transform_scope();
                    match Gesture::begin(&project, seq, variant_id, item, t, handle, c, scope) {
                        Ok(gesture) => self.viewer_drag = Some(ViewerDrag { gesture, guides: Vec::new() }),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let (Some(drag), Some(pos)) = (self.viewer_drag.as_mut(), response.interact_pointer_pos())
        {
            let snap = if mods.step { 0.0 } else { SNAP_PX / zoom };
            match drag.gesture.update(self.editor.doc.project(), to_canvas(pos), mods, snap) {
                Ok(update) => {
                    drag.guides = update.guides;
                    let label = match drag.gesture.handle {
                        Handle::Body => "Move layer",
                        Handle::Corner(_) | Handle::Edge(_) => "Scale layer",
                        Handle::Rotate => "Rotate layer",
                        Handle::Anchor => "Move anchor point",
                    };
                    if let Err(e) = self.editor.apply_drag(label, "viewer-drag", update.ops) {
                        self.error = Some(e.to_string());
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            if let Some(drag) = &self.viewer_drag {
                let rotation = drag.gesture.start().values.float(oa_doc::schema::ROTATION);
                ui.ctx().set_cursor_icon(match drag.gesture.handle {
                    Handle::Body => egui::CursorIcon::Grabbing,
                    h => cursor_for(h, rotation),
                });
            }
        }
        if let (Some(drag), Some(p), Some(pos)) = (self.surface_drag, selected.as_ref(), response.interact_pointer_pos())
            && response.dragged_by(egui::PointerButton::Primary)
            && drag.item == p.item
        {
            self.drag_surface(drag, p, to_canvas(pos));
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) && self.viewer_drag.take().is_some() {
            self.editor.doc.seal();
        }
        if !ui.input(|i| i.pointer.primary_down()) && self.surface_drag.take().is_some() {
            self.editor.doc.seal();
        }
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let c = to_canvas(pos);
            if pick(c).is_none() && surface_point(pos).is_none() {
                self.selection = hit(c);
            }
        }
        // Double-click a title to type into it right where it is; any other clip, to
        // crop it.
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(id) = hit(to_canvas(pos))
        {
            self.selection = Some(id);
            self.set_playing(false);
            if self.editor.item(id).is_some_and(|i| i.kind == oa_doc::ItemKind::Text) {
                self.canvas_text = Some((id, true));
            } else {
                self.start_crop(id);
            }
        }
        // Clicking away from the clip being cropped finishes.
        if self.crop_mode.is_some() && self.crop_mode != self.selection {
            self.end_crop();
        }

        // Overlay, from the (possibly just edited) document.
        let painter = ui.painter_at(available);
        let accent = egui::Color32::from_rgb(90, 170, 255);
        let now = self.editor.doc.snapshot();
        let visible = |id: oa_doc::ItemId| -> Option<scene::Placement> {
            let v = variant.as_ref()?;
            scene::layers_at(&now, seq, v, t).into_iter().find(|l| l.item == id)
        };
        if let Some(p) = hovered_layer.and_then(visible) {
            let pts = p.corners().iter().map(|c| to_screen(*c)).collect();
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.0, accent.gamma_multiply(0.6))));
        }
        if let Some((p, fits)) = effect_drop.and_then(|(id, fits)| visible(id).map(|p| (p, fits))) {
            let pts = p.corners().iter().map(|c| to_screen(*c)).collect();
            let color = if fits { crate::style::ACCENT } else { crate::style::ERROR };
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.5, color)));
        }
        let current = self.selection.and_then(visible);
        if let Some(p) = current.as_ref().filter(|p| self.crop_mode == Some(p.item)) {
            self.paint_crop(&painter, p, &to_screen);
        } else if let Some((p, s)) = current.as_ref().and_then(|p| Some((p, self.surface_of(p.item, t)?))) {
            self.paint_surface(&painter, p, &s, &to_screen, hovered_point);
        } else if let Some(p) = &current {
            let h = Handles::new(p, ROTATE_KNOB_PX / zoom);
            let pts: Vec<egui::Pos2> = h.corners.iter().map(|c| to_screen(*c)).collect();
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.5, accent)));
            let knob = to_screen(h.rotate);
            painter.line_segment([to_screen(h.edges[0]), knob], egui::Stroke::new(1.0, accent));
            painter.circle(knob, 5.0, egui::Color32::WHITE, egui::Stroke::new(1.0, accent));
            for (points, half) in [(h.corners, 4.5), (h.edges, 3.5)] {
                for c in points {
                    let r = egui::Rect::from_center_size(to_screen(c), egui::vec2(half * 2.0, half * 2.0));
                    painter.rect_filled(r, 1.0, egui::Color32::WHITE);
                    painter.rect_stroke(r, 1.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Middle);
                }
            }
            let pivot = to_screen(h.pivot);
            painter.circle_stroke(pivot, 6.0, egui::Stroke::new(1.5, accent));
            for d in [egui::vec2(9.0, 0.0), egui::vec2(0.0, 9.0)] {
                painter.line_segment([pivot - d, pivot + d], egui::Stroke::new(1.0, accent));
            }
        }
        if let Some((id, _)) = self.canvas_text {
            match visible(id) {
                Some(p) => self.canvas_text_editor(ui, &p, &to_screen, zoom),
                // Scrolled away from it, or it's gone: stop editing.
                None => {
                    self.canvas_text = None;
                    self.editor.doc.seal();
                }
            }
        }
        if let Some(drag) = &self.viewer_drag {
            let guide = egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 80, 200));
            for g in &drag.guides {
                let line = match *g {
                    Guide::Vertical(x) => [to_screen([x, 0.0]), to_screen([x, canvas[1]])],
                    Guide::Horizontal(y) => [to_screen([0.0, y]), to_screen([canvas[0], y])],
                };
                painter.add(egui::Shape::dashed_line(&line, guide, 6.0, 4.0));
            }
        }
    }

    /// Typing into a title on the canvas: a text box over the title's box, at roughly its
    /// on-screen size, writing through as you type (one undo step per editing session).
    /// Escape, Ctrl+Enter or clicking elsewhere finishes.
    fn canvas_text_editor(&mut self, ui: &mut egui::Ui, p: &scene::Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, zoom: f64) {
        use oa_doc::{schema, ParamTarget};
        use oa_params::{ParamSource, Value};
        let Some((item, first)) = self.canvas_text else { return };
        let t = self.playhead;
        let value = |app: &Self, id: &str| app.editor.param_value(item, &ParamTarget::Item, id, t);
        let mut content = value(self, schema::TEXT_CONTENT).and_then(|v| v.as_text().map(str::to_string)).unwrap_or_default();
        let em = value(self, schema::TEXT_SIZE).and_then(|v| v.as_float()).unwrap_or(96.0);
        let size = (em * zoom * p.values.vec2(schema::SCALE)[1].abs()).clamp(11.0, 72.0) as f32;
        let align = match value(self, schema::TEXT_ALIGN).and_then(|v| v.as_enum().map(str::to_string)).as_deref() {
            Some("left") => egui::Align::LEFT,
            Some("right") => egui::Align::RIGHT,
            _ => egui::Align::Center,
        };
        let corners = p.corners().map(to_screen);
        let area = egui::Rect::from_points(&corners);
        let width = area.width().max(180.0);
        let accent = crate::style::ACCENT;
        ui.painter().rect_stroke(area.expand(3.0), 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Outside);

        let inner = egui::Area::new(egui::Id::new(("canvas-text", item.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(area.center().x - width / 2.0, area.top()))
            .show(ui.ctx(), |ui| {
                egui::Frame::new().fill(egui::Color32::from_black_alpha(170)).corner_radius(4.0).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                    let edit = egui::TextEdit::multiline(&mut content)
                        .font(egui::FontId::proportional(size))
                        .horizontal_align(align)
                        .desired_width(width - 8.0)
                        .desired_rows(1)
                        .frame(egui::Frame::NONE)
                        .hint_text("Type your title");
                    let r = ui.add(edit);
                    ui.label(egui::RichText::new("Esc or Ctrl+Enter to finish").small().weak());
                    r
                })
                .inner
            });
        let r = inner.inner;
        if first {
            r.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(content.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), r.id);
            }
            self.canvas_text = Some((item, false));
        }
        if r.changed() {
            self.editor.set_param(item, ParamTarget::Item, schema::TEXT_CONTENT, ParamSource::Static(Value::Text(content)), "canvas-text");
        }
        let (escape, submit) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.modifiers.command && i.key_pressed(egui::Key::Enter)));
        if !first && (escape || submit || r.lost_focus() || !r.has_focus()) {
            self.canvas_text = None;
            self.editor.doc.seal();
        }
    }
}
