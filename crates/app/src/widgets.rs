//! Editors for property types that need more than a slider: a direction dial (a pinwheel
//! you spin) and a directional gradient (color stops on a bar, plus a dial).

use crate::i18n::tr;
use eframe::egui;
use oa_params::{Gradient, GradientStop};

/// Straight linear RGBA → what egui paints.
pub(crate) fn color32(c: [f64; 4]) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32).into()
}

/// A loading placeholder: a soft block with a band of light sweeping across it, so
/// waiting looks like progress instead of a frozen app. Keeps repainting while shown.
/// A window kept on the screen: never bigger than it (a window taller than the screen
/// pushed its own title bar out of reach, leaving it stuck) and never dragged off it.
pub fn on_screen<'open>(window: egui::Window<'open>, ctx: &egui::Context) -> egui::Window<'open> {
    let screen = ctx.content_rect();
    window.constrain_to(screen).max_width((screen.width() - 24.0).max(200.0)).max_height((screen.height() - 48.0).max(150.0))
}

/// `size`, no bigger than the screen allows a window to be (see [`on_screen`]).
pub fn fit_screen(ctx: &egui::Context, size: [f32; 2]) -> [f32; 2] {
    let screen = ctx.content_rect();
    [size[0].min((screen.width() - 24.0).max(200.0)), size[1].min((screen.height() - 48.0).max(150.0))]
}

/// A grey checkerboard over `rect` (behind see-through pictures): one small repeating
/// texture, made once, so it costs one quad however big the rect is.
pub fn checkerboard(painter: &egui::Painter, rect: egui::Rect) {
    const CELL: f32 = 12.0;
    let id = egui::Id::new("oa-checkerboard");
    let ctx = painter.ctx();
    let texture = ctx.data_mut(|d| d.get_temp::<egui::TextureHandle>(id)).unwrap_or_else(|| {
        let (a, b) = (egui::Color32::from_gray(58), egui::Color32::from_gray(82));
        let image = egui::ColorImage::new([2, 2], vec![a, b, b, a]);
        let options = egui::TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Nearest, wrap_mode: egui::TextureWrapMode::Repeat, ..Default::default() };
        let handle = ctx.load_texture("checkerboard", image, options);
        ctx.data_mut(|d| d.insert_temp(id, handle.clone()));
        handle
    });
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(rect.width() / (2.0 * CELL), rect.height() / (2.0 * CELL)));
    painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
}

pub fn skeleton(ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
    let base = ui.visuals().faint_bg_color;
    painter.rect_filled(rect, 3.0, base);
    let t = ui.input(|i| i.time) as f32;
    let band = rect.width().max(40.0) * 0.45;
    let x = rect.left() - band + ((t * 0.8).fract()) * (rect.width() + 2.0 * band);
    let mut mesh = egui::Mesh::default();
    let light = ui.visuals().widgets.inactive.bg_fill.gamma_multiply(1.6);
    let clear = light.gamma_multiply(0.0);
    let clip = |px: f32| px.clamp(rect.left(), rect.right());
    for (x0, x1, c0, c1) in [(x, x + band / 2.0, clear, light), (x + band / 2.0, x + band, light, clear)] {
        let (a, b) = (clip(x0), clip(x1));
        if b <= a {
            continue;
        }
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(a, rect.top()), c0);
        mesh.colored_vertex(egui::pos2(b, rect.top()), c1);
        mesh.colored_vertex(egui::pos2(b, rect.bottom()), c1);
        mesh.colored_vertex(egui::pos2(a, rect.bottom()), c0);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    painter.add(mesh);
    ui.ctx().request_repaint();
}

/// An on/off switch (the knob slides across). Returns the response, marked changed
/// when it's flipped.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(36.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let r = rect.height() / 2.0;
        let off = ui.visuals().widgets.inactive.bg_fill;
        let fill = off.lerp_to_gamma(crate::style::ACCENT, how_on);
        ui.painter().rect_filled(rect, r, if response.hovered() { fill.gamma_multiply(1.1) } else { fill });
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), how_on);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), r - 3.0, egui::Color32::WHITE);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tabs laid out in rows of at most `full` px (`widths`: each tab's narrowest, `gap`
/// between them), in order; the row holding `current` goes last, next to the page, as
/// stacked folder tabs do.
pub fn stack_rows(widths: &[f32], full: f32, gap: f32, current: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        match rows.last_mut() {
            Some(row) if used + gap + w <= full => {
                row.push(i);
                used += gap + w;
            }
            _ => {
                rows.push(vec![i]);
                used = *w;
            }
        }
    }
    if let Some(at) = rows.iter().position(|r| r.contains(&current)) {
        let open = rows.remove(at);
        rows.push(open);
    }
    rows
}

/// Folder tabs over a baseline: the open tab is raised (rounded top, an accent strip,
/// joined to the page below with no line under it); the others sit flat and dimmer.
/// Each has an icon before its title. Too many for one row, they stack in rows, the
/// open tab's row nearest the page. Returns true when the open tab changed.
pub fn tabs<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, tabs: &[(T, &str, crate::icons::Icon)]) -> bool {
    let font = egui::FontId::proportional(crate::style::TEXT);
    // Shorter when the controls are (the compact properties panel).
    let height = if ui.spacing().interact_size.y < 18.0 { 22.0 } else { 28.0 };
    let full = ui.available_width();
    let visuals = ui.visuals().clone();
    let icon = 15.0;
    const MIN_PAD: f32 = 12.0;
    const GAP: f32 = 2.0;
    let galleys: Vec<_> = tabs.iter().map(|(_, title, _)| ui.painter().layout_no_wrap(title.to_string(), font.clone(), egui::Color32::PLACEHOLDER)).collect();
    let label_w: Vec<f32> = galleys.iter().map(|g| g.size().x + icon + 4.0).collect();
    let open = tabs.iter().position(|(t, ..)| *t == *current).unwrap_or(0);
    let rows = stack_rows(&label_w.iter().map(|w| w + MIN_PAD).collect::<Vec<_>>(), full, GAP, open);
    let (area, _) = ui.allocate_exact_size(egui::vec2(full, height * rows.len().max(1) as f32), egui::Sense::hover());
    let painter = ui.painter_at(area.expand(1.0));
    let stroke = egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
    let mut changed = false;
    let mut gap = None;
    let last = rows.len().saturating_sub(1);
    for (row_i, row) in rows.iter().enumerate() {
        let top_y = area.top() + row_i as f32 * height;
        let base = top_y + height - 0.5;
        // The row's spare width shared out as padding.
        let text: f32 = row.iter().map(|i| label_w[*i]).sum();
        let pad = ((full - text - GAP * row.len() as f32) / row.len().max(1) as f32).clamp(MIN_PAD, 26.0);
        let mut x = area.left();
        for &i in row {
            let (tab, _, glyph) = &tabs[i];
            let galley = galleys[i].clone();
            let w = label_w[i] + pad;
            let rect = egui::Rect::from_min_max(egui::pos2(x, top_y + 3.0), egui::pos2(x + w, base));
            let r = ui.interact(rect, ui.id().with(("tab", i)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let on = *current == *tab;
            if r.clicked() && !on {
                *current = *tab;
                changed = true;
            }
            let corners = egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 };
            let color = if on {
                painter.rect_filled(rect, corners, visuals.faint_bg_color.lerp_to_gamma(visuals.widgets.inactive.bg_fill, 0.5));
                painter.add(egui::Shape::line(vec![rect.left_bottom(), rect.left_top() + egui::vec2(0.0, 6.0), rect.left_top() + egui::vec2(6.0, 0.0), rect.right_top() - egui::vec2(6.0, 0.0), rect.right_top() + egui::vec2(0.0, 6.0), rect.right_bottom()], stroke));
                painter.rect_filled(egui::Rect::from_min_max(rect.left_top() + egui::vec2(5.0, 0.0), egui::pos2(rect.right() - 5.0, rect.top() + 2.0)), 1.0, crate::style::ACCENT);
                gap = Some((rect.left(), rect.right()));
                visuals.strong_text_color()
            } else {
                if r.hovered() {
                    painter.rect_filled(rect.shrink2(egui::vec2(1.0, 0.0)), corners, visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
                }
                if r.hovered() { visuals.text_color() } else { visuals.weak_text_color() }
            };
            // The icon, then the title, centered together.
            let left = rect.center().x - label_w[i] / 2.0;
            let cy = rect.center().y + 1.0;
            crate::icons::paint(&painter, egui::Rect::from_min_size(egui::pos2(left, cy - icon / 2.0), egui::vec2(icon, icon)), *glyph, if on { crate::style::ACCENT } else { color });
            painter.galley(egui::pos2(left + icon + 4.0, cy - galley.size().y / 2.0), galley, color);
            x += w + GAP;
        }
        // Between stacked rows, a faint line; under the last, the baseline, open under the
        // raised tab.
        if row_i < last {
            painter.line_segment([egui::pos2(area.left(), base), egui::pos2(area.right(), base)], egui::Stroke::new(1.0, stroke.color.gamma_multiply(0.5)));
        }
    }
    let base = area.bottom() - 0.5;
    match gap {
        Some((l, r)) => {
            painter.line_segment([egui::pos2(area.left(), base), egui::pos2(l, base)], stroke);
            painter.line_segment([egui::pos2(r, base), egui::pos2(area.right(), base)], stroke);
        }
        None => {
            painter.line_segment([egui::pos2(area.left(), base), egui::pos2(area.right(), base)], stroke);
        }
    }
    changed
}

/// The same tabs standing on their side, down the left edge of a panel (the media bin):
/// labels reading upwards, the open one raised and joined to the page on its right, with
/// the accent along its outer edge. Each tab is (value, label, tooltip). Returns true when
/// the open tab changed.
pub fn side_tabs<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, tabs: &[(T, String, &str)]) -> bool {
    let font = egui::FontId::proportional(crate::style::TEXT);
    let width = crate::style::ICON + 2.0;
    let full = ui.available_rect_before_wrap();
    let strip = egui::Rect::from_min_max(full.min, egui::pos2(full.left() + width, full.bottom()));
    ui.allocate_rect(strip, egui::Sense::hover());
    let painter = ui.painter_at(strip.expand(1.0));
    let visuals = ui.visuals().clone();
    let stroke = egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color);
    let base = strip.right() - 0.5;
    let mut changed = false;
    let mut gap = None;
    let mut y = strip.top();
    for (i, (tab, label, tip)) in tabs.iter().enumerate() {
        let galley = ui.painter().layout_no_wrap(label.clone(), font.clone(), egui::Color32::PLACEHOLDER);
        let h = galley.size().x + 2.0 * crate::style::GAP_L;
        let rect = egui::Rect::from_min_max(egui::pos2(strip.left() + 3.0, y), egui::pos2(base, y + h));
        let r = ui.interact(rect, ui.id().with(("side-tab", i)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(*tip);
        let on = *current == *tab;
        if r.clicked() && !on {
            *current = *tab;
            changed = true;
        }
        let left = egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 };
        let color = if on {
            painter.rect_filled(rect, left, visuals.faint_bg_color.lerp_to_gamma(visuals.widgets.inactive.bg_fill, 0.5));
            painter.add(egui::Shape::line(
                vec![rect.right_top(), rect.left_top() + egui::vec2(6.0, 0.0), rect.left_top() + egui::vec2(0.0, 6.0), rect.left_bottom() - egui::vec2(0.0, 6.0), rect.left_bottom() + egui::vec2(6.0, 0.0), rect.right_bottom()],
                stroke,
            ));
            painter.rect_filled(egui::Rect::from_min_max(rect.left_top() + egui::vec2(0.0, 5.0), egui::pos2(rect.left() + 2.0, rect.bottom() - 5.0)), 1.0, crate::style::ACCENT);
            gap = Some((rect.top(), rect.bottom()));
            visuals.strong_text_color()
        } else {
            if r.hovered() {
                painter.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), left, visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
            }
            if r.hovered() { visuals.text_color() } else { visuals.weak_text_color() }
        };
        // Turned a quarter left: the text runs up from its start, its top facing left.
        let s = galley.size();
        let at = egui::pos2(rect.center().x - s.y * 0.5, rect.center().y + s.x * 0.5);
        painter.add(egui::epaint::TextShape::new(at, galley, color).with_angle(-std::f32::consts::FRAC_PI_2));
        y += h + 2.0;
    }
    // The edge against the page, open beside the raised tab.
    match gap {
        Some((t, b)) => {
            painter.line_segment([egui::pos2(base, strip.top()), egui::pos2(base, t)], stroke);
            painter.line_segment([egui::pos2(base, b), egui::pos2(base, strip.bottom())], stroke);
        }
        None => {
            painter.line_segment([egui::pos2(base, strip.top()), egui::pos2(base, strip.bottom())], stroke);
        }
    }
    changed
}

/// A search box `width` wide: a magnifying glass, the text, and a ✕ that clears it
/// (Esc clears it too). Returns true when the text changed.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> bool {
    let height = 26.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(80.0), height), egui::Sense::hover());
    let id = ui.id().with(("search-field", hint));
    let focused = ui.memory(|m| m.has_focus(id));
    let visuals = ui.visuals().clone();
    let stroke = if focused { egui::Stroke::new(1.5, crate::style::ACCENT) } else { egui::Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color) };
    ui.painter().rect(rect, height / 2.0, visuals.extreme_bg_color, stroke, egui::StrokeKind::Inside);
    // The glass.
    let c = egui::pos2(rect.left() + 14.0, rect.center().y - 1.0);
    let weak = visuals.weak_text_color();
    ui.painter().circle_stroke(c, 4.5, egui::Stroke::new(1.5, weak));
    ui.painter().line_segment([c + egui::vec2(3.2, 3.2), c + egui::vec2(7.0, 7.0)], egui::Stroke::new(1.8, weak));
    let clear = !text.is_empty();
    let field = egui::Rect::from_min_max(egui::pos2(rect.left() + 26.0, rect.top()), egui::pos2(rect.right() - if clear { 26.0 } else { 10.0 }, rect.bottom()));
    let before = text.clone();
    let r = ui.put(
        field,
        egui::TextEdit::singleline(text).id(id).frame(egui::Frame::NONE).hint_text(hint).vertical_align(egui::Align::Center).desired_width(field.width()),
    );
    if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        text.clear();
    }
    if clear {
        let x = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(18.0, 18.0));
        let b = ui.interact(x, id.with("clear"), egui::Sense::click()).on_hover_text(crate::i18n::tr("Clear"));
        if b.hovered() {
            ui.painter().circle_filled(x.center(), 9.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let s = 3.5;
        let color = if b.hovered() { visuals.strong_text_color() } else { weak };
        ui.painter().line_segment([x.center() + egui::vec2(-s, -s), x.center() + egui::vec2(s, s)], egui::Stroke::new(1.5, color));
        ui.painter().line_segment([x.center() + egui::vec2(-s, s), x.center() + egui::vec2(s, -s)], egui::Stroke::new(1.5, color));
        if b.clicked() {
            text.clear();
        }
    }
    *text != before
}

/// A compact button for toolbars: its text in a small rounded box, lit with the accent
/// when `on` (a toggle that's on, or a menu that's open).
pub fn chip(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(crate::style::TEXT_S + 0.5), egui::Color32::PLACEHOLDER);
    let size = egui::vec2(galley.size().x + 14.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let (fill, stroke, color) = if on {
            (crate::style::ACCENT.gamma_multiply(0.22), egui::Stroke::new(1.0, crate::style::ACCENT.gamma_multiply(0.8)), visuals.strong_text_color())
        } else if response.hovered() {
            (visuals.widgets.hovered.weak_bg_fill, egui::Stroke::new(1.0, visuals.widgets.hovered.bg_stroke.color), visuals.text_color())
        } else {
            (egui::Color32::TRANSPARENT, egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color), visuals.text_color())
        };
        ui.painter().rect(rect, 5.0, fill, stroke, egui::StrokeKind::Inside);
        ui.painter().galley(rect.center() - galley.size() * 0.5, galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small rounded label ("Built-in", "v1.2").
pub fn pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(crate::style::TEXT_S), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + egui::vec2(12.0, 4.0), egui::Sense::hover());
    ui.painter().rect(rect, rect.height() / 2.0, color.gamma_multiply(0.14), egui::Stroke::new(1.0, color.gamma_multiply(0.5)), egui::StrokeKind::Inside);
    ui.painter().galley(rect.min + egui::vec2(6.0, 2.0), galley, color);
}

/// An icon in a rounded square of its color (the start of a card or a tile).
pub fn icon_badge(ui: &mut egui::Ui, icon: crate::icons::Icon, color: egui::Color32, size: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(rect, size * 0.25, color.gamma_multiply(0.18));
    crate::icons::paint(ui.painter(), rect.shrink(size * 0.22), icon, color);
    rect
}

/// A right-click menu on `r`. It closes when you click *outside* it, so a menu can
/// hold a text box (naming a new folder, renaming a track) without vanishing the moment
/// you click into it.
pub fn context_menu(r: &egui::Response, add: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::context_menu(r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(add);
}

/// A menu button whose menu also stays open until you click away or pick something —
/// for menus with filters and search boxes in them (the effect picker).
pub fn sticky_menu<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) {
    let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    egui::containers::menu::MenuButton::new(label).config(config).ui(ui, add);
}

/// A color swatch: click to open a picker below it. Unlike egui's color button, its
/// picker's open state is kept here rather than in egui's shared popup memory, so the
/// swatch can also carry a right-click menu. Returns (response, changed).
pub fn color_swatch(ui: &mut egui::Ui, color: &mut [f64; 4]) -> (egui::Response, bool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 18.0), egui::Sense::click());
    egui::color_picker::show_color_at(ui.painter(), color32(*color), rect);
    let stroke = if response.hovered() { ui.visuals().widgets.hovered.fg_stroke } else { ui.visuals().widgets.noninteractive.bg_stroke };
    ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    let key = response.id.with("picker-open");
    let mut open: bool = ui.data(|d| d.get_temp(key)).unwrap_or(false);
    if response.clicked() {
        open = !open;
    }
    let mut changed = false;
    egui::Popup::from_response(&response)
        .id(response.id.with("picker"))
        .open_bool(&mut open)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let rgba = egui::Rgba::from_rgba_unmultiplied(color[0] as f32, color[1] as f32, color[2] as f32, color[3] as f32);
            let mut hsva = egui::ecolor::Hsva::from(rgba);
            if egui::color_picker::color_picker_hsva_2d(ui, &mut hsva, egui::color_picker::Alpha::OnlyBlend) {
                let [r, g, b, a] = egui::Rgba::from(hsva).to_rgba_unmultiplied();
                *color = [r as f64, g as f64, b as f64, a as f64];
                changed = true;
            }
        });
    ui.data_mut(|d| d.insert_temp(key, open));
    (response, changed)
}

/// A small pipette button (the eyedropper: Material's `colorize`), lit while `armed`.
pub fn eyedropper(ui: &mut egui::Ui, armed: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 18.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if armed {
            ui.painter().rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
        }
        let color = if armed { ui.visuals().selection.stroke.color } else { visuals.fg_stroke.color };
        crate::icons::paint(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(17.0)), crate::icons::COLORIZE, color);
    }
    response
}

/// How many columns a list of `n` choices is laid out in: one for a few, up to three.
pub fn choice_columns(n: usize) -> usize {
    match n {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    }
}

/// Choices as a grid of buttons (`choice_columns` across, each as wide as the widest),
/// for a dropdown's contents. `chosen` lights the ones in force. Returns the clicked one.
pub fn choice_grid(ui: &mut egui::Ui, id: egui::Id, options: &[(String, String)], chosen: &dyn Fn(&str) -> bool) -> Option<String> {
    let columns = choice_columns(options.len());
    let font = egui::TextStyle::Button.resolve(ui.style());
    let widest = options.iter().map(|(_, text)| ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), egui::Color32::WHITE).size().x)).fold(0.0f32, f32::max);
    let width = widest + ui.spacing().button_padding.x * 2.0 + 4.0;
    let mut picked = None;
    egui::Grid::new(id).num_columns(columns).spacing([4.0, 4.0]).show(ui, |ui| {
        for (i, (key, text)) in options.iter().enumerate() {
            let button = egui::Button::selectable(chosen(key), text).min_size(egui::vec2(width, 0.0));
            if ui.add(button).clicked() {
                picked = Some(key.clone());
            }
            if (i + 1) % columns == 0 {
                ui.end_row();
            }
        }
    });
    picked
}

/// A vertical scroll area for a menu whose contents change size (search, category
/// tabs): up to `max` tall, and at least as tall as `content` (up to `max`). Without
/// the floor it can only shrink: a menu is laid out within its size the frame before.
pub fn fitted_scroll(max: f32, content: f32) -> egui::ScrollArea {
    egui::ScrollArea::vertical().max_height(max).min_scrolled_height(content.min(max)).auto_shrink([false, true])
}

/// Within this many degrees of a right angle, the dial snaps to it.
const RIGHT_ANGLE_SNAP: f64 = 10.0;

/// A dial pointing along `degrees` (0 = right, 90 = down): drag it round — it snaps to
/// right angles when close, Shift snaps to 15° — or scroll over it to spin it. The
/// response is `changed()` when the angle moved.
pub fn dial(ui: &mut egui::Ui, degrees: &mut f64) -> egui::Response {
    let size = 22.0;
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let c = rect.center();
    let r = size / 2.0 - 1.5;
    let before = *degrees;
    if (response.dragged() || response.clicked())
        && let Some(p) = response.interact_pointer_pos()
    {
        let d = p - c;
        if d.length() > 2.0 {
            let mut a = (d.y as f64).atan2(d.x as f64).to_degrees();
            let right = (a / 90.0).round() * 90.0;
            if ui.input(|i| i.modifiers.shift) {
                a = (a / 15.0).round() * 15.0;
            } else if (a - right).abs() <= RIGHT_ANGLE_SNAP {
                a = right;
            }
            *degrees = a;
        }
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            *degrees += scroll as f64 * 0.25;
        }
    }
    *degrees = degrees.rem_euclid(360.0);
    if *degrees != before {
        response.mark_changed();
    }

    let v = ui.visuals();
    let active = response.hovered() || response.dragged();
    let painter = ui.painter();
    let rim = if active { v.widgets.hovered.fg_stroke.color } else { v.widgets.noninteractive.bg_stroke.color };
    painter.circle(c, r, v.extreme_bg_color, egui::Stroke::new(1.0, rim));
    // The direction: an arrow from the hub to the rim.
    let a = (*degrees as f32).to_radians();
    let at = |angle: f32, len: f32| c + egui::vec2(angle.cos(), angle.sin()) * len;
    let accent = crate::style::GOLD;
    let tip = at(a, r - 1.0);
    painter.line_segment([c, tip], egui::Stroke::new(1.5, accent));
    painter.add(egui::Shape::convex_polygon(vec![tip, at(a + 0.45, r - 5.5), at(a - 0.45, r - 5.5)], accent, egui::Stroke::NONE));
    painter.circle_filled(c, 1.8, accent);
    response.on_hover_text(tr("Drag to point it (snaps to right angles; Shift: 15° steps) · scroll to spin"))
}

/// A dial plus a 0–360° field. Returns (changed, finished) — finished when a drag or edit
/// ends, for undo grouping.
pub fn direction(ui: &mut egui::Ui, degrees: &mut f64) -> (bool, bool) {
    let d = dial(ui, degrees);
    let mut v = *degrees;
    let f = ui.add(egui::DragValue::new(&mut v).speed(1.0).range(-720.0..=720.0).suffix("°").max_decimals(1));
    if f.changed() {
        *degrees = v.rem_euclid(360.0);
    }
    (d.changed() || f.changed(), d.drag_stopped() || d.clicked() || f.drag_stopped() || f.lost_focus())
}

#[derive(Clone, Copy, Default)]
struct GradientUi {
    selected: usize,
    dragging: bool,
}

/// Edits a directional gradient: the bar shows it; click the bar to add a color there,
/// drag a marker to move one, pick the selected stop's color below; the dial sets the
/// direction. Returns (changed, finished).
pub fn gradient(ui: &mut egui::Ui, id: egui::Id, g: &mut Gradient) -> (bool, bool) {
    let mut state: GradientUi = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let (mut changed, mut finished) = (false, false);
    if g.stops.is_empty() {
        g.stops.push(GradientStop { pos: 0.0, color: [1.0; 4] });
        changed = true;
    }
    state.selected = state.selected.min(g.stops.len() - 1);

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(150.0, 26.0), egui::Sense::click_and_drag());
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 16.0));
            let t_at = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64;
            let x_at = |t: f64| bar.left() + bar.width() * t as f32;

            // Pick up a marker, or add a stop where the bar was clicked.
            if (response.drag_started() || response.clicked())
                && let Some(p) = response.interact_pointer_pos()
            {
                let near = g.stops.iter().enumerate().map(|(i, s)| (i, (x_at(s.pos) - p.x).abs())).filter(|(_, d)| *d < 7.0).min_by(|a, b| a.1.total_cmp(&b.1));
                match near {
                    Some((i, _)) => {
                        state.selected = i;
                        state.dragging = response.drag_started();
                    }
                    None if response.clicked() && g.stops.len() < Gradient::MAX_STOPS => {
                        let t = t_at(p.x);
                        g.stops.push(GradientStop { pos: t, color: g.sample(t) });
                        state.selected = g.stops.len() - 1;
                        changed = true;
                        finished = true;
                    }
                    None => {}
                }
            }
            if state.dragging
                && response.dragged()
                && let Some(p) = response.interact_pointer_pos()
            {
                g.stops[state.selected].pos = t_at(p.x);
                changed = true;
            }
            if response.drag_stopped() {
                state.dragging = false;
                finished = true;
            }

            // The bar: the gradient left to right, whatever its direction.
            let painter = ui.painter();
            painter.rect_filled(bar, 2.0, egui::Color32::from_gray(30));
            let mut mesh = egui::Mesh::default();
            let stops = g.sorted_stops();
            let mut xs: Vec<(f32, egui::Color32)> = vec![(bar.left(), color32(stops[0].color))];
            xs.extend(stops.iter().map(|s| (x_at(s.pos), color32(s.color))));
            xs.push((bar.right(), color32(stops[stops.len() - 1].color)));
            for w in xs.windows(2) {
                let i = mesh.vertices.len() as u32;
                mesh.colored_vertex(egui::pos2(w[0].0, bar.top()), w[0].1);
                mesh.colored_vertex(egui::pos2(w[1].0, bar.top()), w[1].1);
                mesh.colored_vertex(egui::pos2(w[1].0, bar.bottom()), w[1].1);
                mesh.colored_vertex(egui::pos2(w[0].0, bar.bottom()), w[0].1);
                mesh.add_triangle(i, i + 1, i + 2);
                mesh.add_triangle(i, i + 2, i + 3);
            }
            painter.add(mesh);
            painter.rect_stroke(bar, 2.0, egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
            // Markers under the bar.
            for (i, s) in g.stops.iter().enumerate() {
                let x = x_at(s.pos);
                let top = bar.bottom() + 1.0;
                let selected = i == state.selected;
                let outline = if selected { egui::Color32::WHITE } else { egui::Color32::from_gray(140) };
                painter.add(egui::Shape::convex_polygon(
                    vec![egui::pos2(x, top), egui::pos2(x + 5.0, top + 8.0), egui::pos2(x - 5.0, top + 8.0)],
                    color32(s.color),
                    egui::Stroke::new(if selected { 1.5 } else { 1.0 }, outline),
                ));
            }
            response.on_hover_text(tr("Click to add a color · drag a marker to move it"));

            let (c, f) = direction(ui, &mut g.angle);
            changed |= c;
            finished |= f;
        });

        // The selected stop.
        ui.horizontal(|ui| {
            let s = &mut g.stops[state.selected];
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(s.color[0] as f32, s.color[1] as f32, s.color[2] as f32, s.color[3] as f32);
            if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::OnlyBlend).changed() {
                let [r, gr, b, a] = rgba.to_rgba_unmultiplied();
                s.color = [r as f64, gr as f64, b as f64, a as f64];
                changed = true;
            }
            let mut pct = s.pos * 100.0;
            let p = ui.add(egui::DragValue::new(&mut pct).range(0.0..=100.0).speed(0.5).suffix(" %"));
            if p.changed() {
                s.pos = pct / 100.0;
                changed = true;
            }
            finished |= p.drag_stopped() || p.lost_focus();
            if g.stops.len() > 1 && ui.small_button(tr("✕")).on_hover_text(tr("Remove this color")).clicked() {
                g.stops.remove(state.selected);
                state.selected = state.selected.saturating_sub(1);
                changed = true;
                finished = true;
            }
            if g.stops.len() < Gradient::MAX_STOPS && ui.small_button(tr("+")).on_hover_text(tr("Add a color")).clicked() {
                // Between the selected stop and the next one along.
                let here = g.stops[state.selected].pos;
                let next = g.stops.iter().map(|s| s.pos).filter(|p| *p > here).fold(1.0f64, f64::min);
                let t = if next > here { (here + next) / 2.0 } else { (here / 2.0).max(0.0) };
                g.stops.push(GradientStop { pos: t, color: g.sample(t) });
                state.selected = g.stops.len() - 1;
                changed = true;
                finished = true;
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(id, state));
    (changed, finished)
}

/// A full-width button card: an icon in a round spot, what it does, and a line more
/// about it (the empty properties panel's choices).
pub fn action_card(ui: &mut egui::Ui, icon: crate::icons::Icon, title: &str, about: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let hovered = response.hovered();
        let edge = if hovered { crate::style::ACCENT } else { v.widgets.noninteractive.bg_stroke.color };
        let fill = if hovered { v.widgets.hovered.weak_bg_fill } else { v.faint_bg_color };
        let painter = ui.painter();
        painter.rect(rect, 8.0, fill, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        let spot = egui::pos2(rect.left() + 26.0, rect.center().y);
        painter.circle_filled(spot, 16.0, crate::style::ACCENT.gamma_multiply(0.16));
        crate::icons::paint(painter, egui::Rect::from_center_size(spot, egui::vec2(20.0, 20.0)), icon, crate::style::ACCENT);
        let x = spot.x + 26.0;
        let wrap = (rect.right() - x - 8.0).max(10.0);
        let mut title_job = egui::text::LayoutJob::simple_singleline(title.to_string(), egui::FontId::proportional(crate::style::TEXT_L), v.text_color());
        title_job.wrap = egui::text::TextWrapping::truncate_at_width(wrap);
        let title = painter.layout_job(title_job);
        let mut about_job = egui::text::LayoutJob::simple_singleline(about.to_string(), egui::FontId::proportional(crate::style::TEXT_S), v.weak_text_color());
        about_job.wrap = egui::text::TextWrapping::truncate_at_width(wrap);
        let about = painter.layout_job(about_job);
        let top = rect.center().y - (title.size().y + 2.0 + about.size().y) / 2.0;
        let title_h = title.size().y;
        painter.galley(egui::pos2(x, top), title, v.text_color());
        painter.galley(egui::pos2(x, top + title_h + 2.0), about, v.weak_text_color());
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tabs_tests {
    use super::stack_rows;

    #[test]
    fn tabs_that_dont_fit_stack_with_the_open_row_last() {
        // Five 100 px tabs, 250 px wide: two to a row.
        let w = [100.0; 5];
        assert_eq!(stack_rows(&w, 250.0, 2.0, 0), [vec![2, 3], vec![4], vec![0, 1]]);
        assert_eq!(stack_rows(&w, 250.0, 2.0, 4), [vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(stack_rows(&w, 600.0, 2.0, 3), [vec![0, 1, 2, 3, 4]], "room for all: one row");
    }
}
