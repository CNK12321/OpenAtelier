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
    /// The last press on the canvas: when, where, and the title under it (for our own
    /// double-press, see [`title_double_press`]).
    pub last_press: Option<(f64, egui::Pos2, Option<oa_doc::ItemId>)>,
}

impl Default for ViewerView {
    fn default() -> Self {
        ViewerView { zoom: None, pan: egui::Vec2::ZERO, thirds: false, safe_areas: false, center: false, last_press: None }
    }
}

/// Seconds and screen points within which a second press on the same title opens it for
/// typing: roomier than egui's double-click, which also fails once the pointer has drifted
/// far enough to count as a drag.
const DOUBLE_PRESS_SECONDS: f64 = 0.5;
const DOUBLE_PRESS_PX: f32 = 10.0;

/// Whether a press at `pos`, time `now`, over `title`, is the second of a double press
/// on it (after `last`).
pub fn title_double_press(last: Option<(f64, egui::Pos2, Option<oa_doc::ItemId>)>, now: f64, pos: egui::Pos2, title: Option<oa_doc::ItemId>) -> bool {
    match (last, title) {
        (Some((then, at, Some(before))), Some(title)) => before == title && now - then <= DOUBLE_PRESS_SECONDS && at.distance(pos) <= DOUBLE_PRESS_PX,
        _ => false,
    }
}

/// A title being typed into where it's drawn (`canvas_text_editor`).
pub struct CanvasText {
    pub item: oa_doc::ItemId,
    pub caret: crate::text_edit::Caret,
    /// The pointer went down inside the title: dragging selects.
    pub selecting: bool,
    /// UI time the cursor last moved or the text changed (the cursor shows solid, then
    /// blinks).
    pub since: f64,
    /// Opened by the press still held down: until it's let go, the pointer neither moves
    /// the layer nor changes the selection.
    pub opening: bool,
}

impl CanvasText {
    /// Starts with all of `text` selected (typing replaces it).
    pub fn new(item: oa_doc::ItemId, text: &str) -> Self {
        CanvasText { item, caret: crate::text_edit::Caret::all(text), selecting: false, since: 0.0, opening: false }
    }
}

/// A drag in progress on the viewer.
pub struct ViewerDrag {
    pub gesture: Gesture,
    pub guides: Vec<Guide>,
    /// The other selected clips, which follow: moved by the same amount (their own
    /// gestures on the body), or scaled and turned as much as the grabbed clip is (their
    /// scale and rotation at the start).
    pub followers: Vec<Follower>,
}

pub struct Follower {
    pub gesture: Gesture,
    pub scale: [f64; 2],
    pub rotation: f64,
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
            if ui.button("Fullscreen").on_hover_text("Play it back on the whole screen (F; F, Esc or a double-click to leave)").clicked() {
                let ctx = ui.ctx().clone();
                self.set_fullscreen(&ctx, true);
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
        // Inside a compound clip the picture is see-through where it has nothing: a
        // checkerboard shows where.
        if !self.compound_trail.is_empty() {
            crate::widgets::checkerboard(&clip, rect);
        }
        clip.image(preview_id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        self.draw_guides(&clip, rect);

        self.display_scale = rect.width() / canvas[0] as f32 * ui.ctx().pixels_per_point();
        let zoom = rect.width() as f64 / canvas[0]; // screen px per canvas px
        let to_canvas = |p: egui::Pos2| [((p.x - rect.left()) as f64) / zoom, ((p.y - rect.top()) as f64) / zoom];
        let to_screen = |c: [f64; 2]| egui::pos2(rect.left() + (c[0] * zoom) as f32, rect.top() + (c[1] * zoom) as f32);
        // Plugins' overlays that are on, over the picture.
        if !self.settings.overlays_on.is_empty() {
            let selected = self.selection.and_then(|id| oa_edit::transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), id, self.playhead).ok()).map(|p| {
                let c = p.corners();
                let (xs, ys) = (c.map(|p| p[0]), c.map(|p| p[1]));
                [xs.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max), ys.iter().copied().fold(f64::MIN, f64::max)]
            });
            let input = oa_graph::script::OverlayInput {
                canvas: [canvas[0], canvas[1]],
                seconds: self.playhead.as_seconds_f64(),
                duration: self.editor.duration().as_seconds_f64(),
                playing: self.playing,
                selected,
            };
            self.draw_overlays(&clip, &to_screen, zoom, &input);
        }
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
        // Its effects' points (a Swirl's center…), grabbed before anything else.
        let effect_points = selected.as_ref().map(|p| self.effect_points(p.item, t)).unwrap_or_default();
        let effect_point = |pointer: egui::Pos2| -> Option<usize> { crate::points::point_near(&effect_points, selected.as_ref()?, &to_screen, pointer) };
        // A layer with a Surface is only hit inside its warped mesh (worked out up front:
        // the closure can't hold on to `self`).
        let meshes: Vec<(oa_doc::ItemId, scene::Placement, crate::surface::Surface)> = variant
            .as_ref()
            .map(|v| scene::layers_at(&project, seq, v, t).into_iter().filter_map(|l| Some((l.item, self.surface_of(l.item, t)?, l))).map(|(id, s, l)| (id, l, s)).collect())
            .unwrap_or_default();
        let hit = |pointer: [f64; 2]| {
            let v = variant.as_ref()?;
            scene::hits(&project, seq, v, t, pointer)
                .into_iter()
                .find(|id| meshes.iter().find(|(m, ..)| m == id).is_none_or(|(_, l, s)| l.to_layer_fraction(pointer).is_some_and(|q| s.covers(q))))
        };

        // Pointer feedback.
        let mut hovered_layer = None;
        let mut hovered_point = None;
        let mut hovered_effect_point = None;
        if let Some(pos) = response.hover_pos().filter(|_| self.viewer_drag.is_none() && self.surface_drag.is_none() && self.point_drag.is_none()) {
            let c = to_canvas(pos);
            hovered_effect_point = effect_point(pos);
            hovered_point = surface_point(pos);
            match pick(c) {
                _ if hovered_effect_point.is_some() => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    if let Some(e) = hovered_effect_point.and_then(|i| effect_points.get(i)) {
                        response.clone().on_hover_text(format!("{}: {} (drag it; right-click its values in the inspector to track it)", e.name, e.param.replace('_', " ")));
                    }
                }
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

        // A second press on a title opens it for typing, on the press itself: anywhere in
        // its box (its gaps too), however slightly the pointer moved between the presses,
        // and the press can't go on to drag the layer. (egui's double-click needed both
        // clicks to land on a letter and to stay clicks, which made it hit and miss.)
        if ui.input(|i| i.pointer.primary_pressed())
            && self.crop_mode.is_none()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin()).filter(|p| response.rect.contains(*p))
        {
            let now = ui.input(|i| i.time);
            let title = variant.as_ref().and_then(|v| scene::title_at(&project, seq, v, t, to_canvas(pos)));
            let editing = self.canvas_text.as_ref().map(|c| c.item);
            if editing.is_none_or(|e| Some(e) != title) && title_double_press(self.view.last_press, now, pos, title) {
                let id = title.expect("a double press is on a title");
                self.finish_canvas_text();
                self.selection = Some(id);
                self.set_playing(false);
                let text = self.title_text(id, t);
                self.canvas_text = Some(CanvasText { since: now, opening: true, ..CanvasText::new(id, &text) });
                self.view.last_press = None;
            } else {
                self.view.last_press = Some((now, pos, title));
            }
        }
        let opening = self.canvas_text.as_ref().is_some_and(|c| c.opening);
        if opening
            && !ui.input(|i| i.pointer.primary_down())
            && let Some(c) = self.canvas_text.as_mut()
        {
            c.opening = false;
        }

        // Typing into a title: inside it, the pointer places the cursor (Shift extends),
        // drags to select, double-clicks a word; pressing anywhere else on the canvas
        // finishes. While it does, the viewer's own clicks and drags stand aside.
        let mut text_pointer = opening;
        if let Some(item) = self.canvas_text.as_ref().map(|c| c.item)
            && let Some(p) = on_screen(item)
            && let Some(layout) = self.title_layout(item, t)
        {
            let index_at = |pos: egui::Pos2| p.to_layer(to_canvas(pos)).map(|l| layout.index_at(l));
            let inside = |pos: egui::Pos2| p.contains(to_canvas(pos));
            let now = ui.input(|i| i.time);
            let (pressed, down, latest, origin, shift) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.latest_pos(), i.pointer.press_origin(), i.modifiers.shift));
            if response.hover_pos().is_some_and(inside) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
            if pressed && !opening && let Some(o) = origin.filter(|o| response.rect.contains(*o)) {
                match index_at(o).filter(|_| inside(o)) {
                    Some(i) => {
                        let c = self.canvas_text.as_mut().expect("checked");
                        c.caret = if shift { crate::text_edit::Caret { cursor: i, anchor: c.caret.anchor } } else { crate::text_edit::Caret::at(i) };
                        (c.selecting, c.since) = (true, now);
                    }
                    None => self.finish_canvas_text(),
                }
            }
            if let Some(c) = self.canvas_text.as_mut().filter(|c| c.selecting) {
                text_pointer = true;
                if down && let Some(i) = latest.and_then(&index_at) {
                    c.caret.cursor = i;
                } else if !down {
                    c.selecting = false;
                }
            }
            if response.double_clicked()
                && !opening
                && let Some(pos) = response.interact_pointer_pos().filter(|pos| inside(*pos))
                && let Some(i) = index_at(pos)
            {
                let text = self.title_text(item, t);
                if let Some(c) = self.canvas_text.as_mut() {
                    (c.caret, c.selecting, c.since) = (crate::text_edit::word_at(&text, i), false, now);
                }
                text_pointer = true;
            }
        }

        // Press: grab a handle of the selected layer, or select-and-move what's under it
        // (not while cropping: the pointer belongs to the crop then).
        if response.drag_started_by(egui::PointerButton::Primary) && self.crop_mode.is_none() && !text_pointer {
            let origin = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
            if let (Some(origin), Some(p)) = (origin, selected.as_ref())
                && let Some(i) = effect_point(origin)
            {
                self.begin_point_drag(p, &effect_points[i], to_canvas(origin));
            } else if let (Some(origin), Some(p), Some(s)) = (origin, selected.as_ref(), surface.as_ref())
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
                    // Grabbing one of several selected clips drags them all.
                    let others: Vec<oa_doc::ItemId> = if self.selected.contains(&item) && handle != Handle::Anchor { self.selected_clips().into_iter().filter(|o| *o != item).collect() } else { Vec::new() };
                    self.selection = Some(item);
                    self.set_playing(false);
                    let scope = self.transform_scope();
                    match Gesture::begin(&project, seq, variant_id, item, t, handle, c, scope) {
                        Ok(gesture) => {
                            let followers = others
                                .into_iter()
                                .filter_map(|o| {
                                    let gesture = Gesture::begin(&project, seq, variant_id, o, t, Handle::Body, c, scope).ok()?;
                                    let v = &gesture.start().values;
                                    let (scale, rotation) = (v.vec2(oa_doc::schema::SCALE), v.float(oa_doc::schema::ROTATION));
                                    Some(Follower { gesture, scale, rotation })
                                })
                                .collect();
                            self.viewer_drag = Some(ViewerDrag { gesture, guides: Vec::new(), followers });
                        }
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
                Ok(mut update) => {
                    drag.guides = update.guides;
                    let project = self.editor.doc.project();
                    match drag.gesture.handle {
                        // The others move with the pointer too (snapping is the grabbed
                        // clip's).
                        Handle::Body => {
                            for f in &drag.followers {
                                if let Ok(u) = f.gesture.update(project, to_canvas(pos), mods, 0.0) {
                                    update.ops.extend(u.ops);
                                }
                            }
                        }
                        // The others scale and turn by as much as the grabbed clip does.
                        _ if !drag.followers.is_empty() => {
                            let v0 = &drag.gesture.start().values;
                            let (s0, r0) = (v0.vec2(oa_doc::schema::SCALE), v0.float(oa_doc::schema::ROTATION));
                            let (mut s1, mut r1) = (s0, r0);
                            for (param, value) in &update.values {
                                match (*param, value) {
                                    (oa_doc::schema::SCALE, oa_params::Value::Vec2(s)) => s1 = *s,
                                    (oa_doc::schema::ROTATION, oa_params::Value::Float(r)) => r1 = *r,
                                    _ => {}
                                }
                            }
                            let ratio = [s1[0] / s0[0].max(1e-9), s1[1] / s0[1].max(1e-9)];
                            for f in &drag.followers {
                                let g = &f.gesture;
                                let write = |param: &str, value: oa_params::Value| oa_edit::transform::write_param(project, g.seq, g.item, g.variant, g.scope, g.t, param, value);
                                if (ratio[0] - 1.0).abs() > 1e-9 || (ratio[1] - 1.0).abs() > 1e-9 {
                                    update.ops.extend(write(oa_doc::schema::SCALE, oa_params::Value::Vec2([f.scale[0] * ratio[0], f.scale[1] * ratio[1]])).ok());
                                }
                                if (r1 - r0).abs() > 1e-9 {
                                    update.ops.extend(write(oa_doc::schema::ROTATION, oa_params::Value::Float(f.rotation + r1 - r0)).ok());
                                }
                            }
                        }
                        _ => {}
                    }
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
        if let (Some(drag), Some(p), Some(pos)) = (self.point_drag.clone(), selected.as_ref(), response.interact_pointer_pos())
            && response.dragged_by(egui::PointerButton::Primary)
            && drag.item == p.item
        {
            self.drag_point(&drag, p, to_canvas(pos));
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if !ui.input(|i| i.pointer.primary_down()) && self.surface_drag.take().is_some() {
            self.editor.doc.seal();
        }
        if !ui.input(|i| i.pointer.primary_down()) && self.point_drag.take().is_some() {
            self.editor.doc.seal();
        }
        if response.clicked()
            && !text_pointer
            && let Some(pos) = response.interact_pointer_pos()
        {
            let c = to_canvas(pos);
            if pick(c).is_none() && surface_point(pos).is_none() && effect_point(pos).is_none() {
                self.selection = hit(c);
            }
        }
        // Double-click a title to type into it right where it is; any other clip, to
        // crop it.
        if response.double_clicked()
            && !text_pointer
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(id) = hit(to_canvas(pos))
        {
            self.selection = Some(id);
            self.set_playing(false);
            if self.editor.item(id).is_some_and(|i| i.kind == oa_doc::ItemKind::Text) {
                let text = self.title_text(id, t);
                self.canvas_text = Some(CanvasText { since: ui.input(|i| i.time), ..CanvasText::new(id, &text) });
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
        // The selected clip's effect points, where they are after this frame's edits.
        if let Some(p) = current.as_ref().filter(|p| self.crop_mode != Some(p.item)) {
            let points = self.effect_points(p.item, t);
            self.paint_points(&painter, p, &points, &to_screen, hovered_effect_point);
        }
        if let Some(id) = self.canvas_text.as_ref().map(|c| c.item) {
            match visible(id) {
                Some(p) => self.canvas_text_editor(ui, &p, &to_screen),
                // Scrolled away from it, or it's gone: stop editing.
                None => self.finish_canvas_text(),
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

    /// The words of title `item` at `t`.
    pub(crate) fn title_text(&self, item: oa_doc::ItemId, t: oa_time::Time) -> String {
        self.editor.param_value(item, &oa_doc::ParamTarget::Item, oa_doc::schema::TEXT_CONTENT, t).and_then(|v| v.as_text().map(str::to_string)).unwrap_or_default()
    }

    /// Title `item`'s layout at `t` — the same one it's drawn from (format overrides
    /// included), in the layer's own px.
    fn title_layout(&self, item: oa_doc::ItemId, t: oa_time::Time) -> Option<std::sync::Arc<oa_text::Layout>> {
        let it = self.editor.item(item)?;
        let variant = self.editor.sequence().variant(self.variant_id())?;
        let ctx = it.eval_context(t);
        let values = it.params.eval(oa_doc::schema::text(), variant.overrides.get(&item), &ctx);
        Some(oa_text::layout(&scene::text_spec(&values)))
    }

    /// Stops typing into the title (one undo step for the whole session).
    pub(crate) fn finish_canvas_text(&mut self) {
        if self.canvas_text.take().is_some() {
            self.editor.doc.seal();
        }
    }

    /// Typing into a title where it's drawn: the keys, typing, copy, cut and paste go
    /// straight into its words (`text_edit`), written through as you type, and the real
    /// render — effects and all — is what you see, with the cursor and the selection
    /// drawn over it. Escape, Ctrl+Enter, or pressing elsewhere finishes.
    fn canvas_text_editor(&mut self, ui: &mut egui::Ui, p: &scene::Placement, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2) {
        use crate::text_edit;
        use oa_doc::{schema, ParamTarget};
        use oa_params::{ParamSource, Value};
        let Some(item) = self.canvas_text.as_ref().map(|c| c.item) else { return };
        // Another text field took the keyboard (the inspector's, say): done here.
        if ui.ctx().text_edit_focused() {
            self.finish_canvas_text();
            return;
        }
        let t = self.playhead;
        let now = ui.input(|i| i.time);
        let (escape, submit) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.modifiers.command && i.key_pressed(egui::Key::Enter)));
        if escape || submit {
            self.finish_canvas_text();
            return;
        }

        // Edits, in the order they came.
        let mut text = self.title_text(item, t);
        let events = ui.input(|i| i.events.clone());
        let mut changed = false;
        if !events.is_empty()
            && let Some(layout) = self.title_layout(item, t)
            && let Some(c) = self.canvas_text.as_mut()
        {
            // Up and down: the same x on the line above or below (the layout as it was
            // before this frame's typing — close enough for a keypress).
            let vertical = |i: usize, down: bool| -> Option<usize> {
                let (line, x) = layout.caret(i)?;
                let n = layout.carets.iter().position(|l| std::ptr::eq(l, line))?;
                let to = layout.carets.get(if down { n + 1 } else { n.checked_sub(1)? })?;
                let k = to.xs.iter().enumerate().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))?.0;
                Some(to.first_char + k)
            };
            for e in &events {
                let before = c.caret;
                let out = text_edit::apply(&mut text, &mut c.caret, e, &vertical);
                if let Some(copied) = out.copied {
                    ui.ctx().copy_text(copied);
                }
                changed |= out.changed;
                if out.changed || c.caret != before {
                    c.since = now;
                }
            }
        }
        if changed {
            self.editor.set_param(item, ParamTarget::Item, schema::TEXT_CONTENT, ParamSource::Static(Value::Text(text.clone())), "canvas-text");
        }

        // The title's box, marked as being edited; the selection and the cursor over the
        // letters, where they are after this frame's typing.
        let accent = crate::style::ACCENT;
        let corners = p.corners().map(to_screen);
        let outline: Vec<egui::Pos2> = corners.iter().copied().chain(std::iter::once(corners[0])).collect();
        ui.painter().add(egui::Shape::dashed_line(&outline, egui::Stroke::new(1.0, accent), 5.0, 4.0));
        let (Some(layout), Some(c)) = (self.title_layout(item, t), self.canvas_text.as_ref()) else { return };
        let screen = |x: f64, y: f64| to_screen(p.to_canvas.apply([x, y]));
        let (a, b) = c.caret.range();
        for line in &layout.carets {
            let last = line.first_char + line.xs.len() - 1;
            let (from, to) = (a.max(line.first_char), b.min(last));
            // Selected through the line's end: a sliver for its line break.
            let past_end = b > last && a <= last;
            if from < to || (past_end && from <= to) {
                let x0 = line.xs[from - line.first_char];
                let x1 = line.xs[to - line.first_char] + if past_end { layout.em * 0.25 } else { 0.0 };
                let quad = vec![screen(x0, line.top), screen(x1, line.top), screen(x1, line.bottom), screen(x0, line.bottom)];
                ui.painter().add(egui::Shape::convex_polygon(quad, accent.gamma_multiply(0.35), egui::Stroke::NONE));
            }
        }
        // Solid for a moment after each change, then blinking.
        let shown = now - c.since < 0.5 || (now - c.since) % 1.0 < 0.6;
        if shown && let Some((line, x)) = layout.caret(c.caret.cursor) {
            let (top, bottom) = (screen(x, line.top), screen(x, line.bottom));
            ui.painter().line_segment([top, bottom], egui::Stroke::new(3.0, egui::Color32::from_black_alpha(160)));
            ui.painter().line_segment([top, bottom], egui::Stroke::new(1.5, egui::Color32::WHITE));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        if text.is_empty() {
            let at = to_screen(p.to_canvas.apply([layout.size[0] / 2.0, layout.size[1] / 2.0]));
            ui.painter().text(at, egui::Align2::CENTER_CENTER, "Type your title", egui::FontId::proportional(14.0), egui::Color32::from_white_alpha(140));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::title_double_press;
    use eframe::egui::pos2;
    use oa_doc::ItemId;

    /// A second press opens a title when it's soon, near, and on the same title — even a
    /// few points off, which egui's double-click counted as a drag.
    #[test]
    fn a_second_press_on_the_same_title_opens_it() {
        let title = Some(ItemId(7));
        let first = Some((1.0, pos2(100.0, 100.0), title));
        assert!(title_double_press(first, 1.3, pos2(106.0, 103.0), title));
        assert!(!title_double_press(first, 1.7, pos2(100.0, 100.0), title), "too late");
        assert!(!title_double_press(first, 1.2, pos2(130.0, 100.0), title), "too far");
        assert!(!title_double_press(first, 1.2, pos2(100.0, 100.0), Some(ItemId(8))), "another title");
        assert!(!title_double_press(Some((1.0, pos2(100.0, 100.0), None)), 1.2, pos2(100.0, 100.0), title), "the first press missed");
        assert!(!title_double_press(None, 1.2, pos2(100.0, 100.0), title));
    }
}
