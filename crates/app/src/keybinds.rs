//! The keyboard shortcuts, listed (Settings → Shortcuts): every key and mouse combination
//! the editor answers to, by the panel it works in, General first. Each shows as keycaps
//! with a + between them ("Ctrl + Shift + S"); a row with two ways to do the same thing
//! shows both. A search box narrows the list by what a shortcut does or its keys.
//!
//! It's a list to read, kept by hand next to where the keys are handled
//! (`App::shortcuts`, and the panels' own): a key changed there should change here.

use crate::i18n::tr;
use crate::App;
use eframe::egui;

/// Each shortcut's row.
const ROW_HEIGHT: f32 = 28.0;

/// One shortcut: the ways to press it (each "Ctrl+Shift+S"), and what it does.
struct Bind {
    keys: &'static [&'static str],
    what: &'static str,
}

/// The shortcuts, by panel (General first).
fn sections() -> Vec<(&'static str, Vec<Bind>)> {
    let b = |keys: &'static [&'static str], what: &'static str| Bind { keys, what };
    vec![
        (
            tr("General"),
            vec![
                b(&["Ctrl+S"], tr("Save")),
                b(&["Ctrl+Shift+S"], tr("Save as…")),
                b(&["Ctrl+I"], tr("Import media…")),
                b(&["Ctrl+Z"], tr("Undo")),
                b(&["Ctrl+Shift+Z", "Ctrl+Y"], tr("Redo")),
                b(&["Ctrl+X"], tr("Cut")),
                b(&["Ctrl+C"], tr("Copy")),
                b(&["Ctrl+V"], tr("Paste at the playhead")),
                b(&["Ctrl+,"], tr("Settings")),
                b(&["Esc"], tr("Let go of the selection, or step out of a compound clip")),
                b(&["RightClick"], tr("The menu of what's under the pointer")),
            ],
        ),
        (
            tr("Playback"),
            vec![
                b(&["Space"], tr("Play or pause")),
                b(&["←", "→"], tr("Step one frame back or forward")),
                b(&["Shift+←", "Shift+→"], tr("Step ten frames")),
                b(&["Home", "End"], tr("Go to the start or the end")),
                b(&["Ctrl+←", "Ctrl+→"], tr("Previous or next keyframe, or the clip's start or end")),
                b(&["Ctrl+Shift+←", "Ctrl+Shift+→"], tr("Go to the start or the end of the clip")),
                b(&["Alt+←", "Alt+→"], tr("Previous or next clip on the track")),
                b(&["F"], tr("Fullscreen playback (F or Esc to leave)")),
            ],
        ),
        (
            tr("Timeline"),
            vec![
                b(&["S", "Ctrl+K"], tr("Split at the playhead")),
                b(&["T"], tr("Add a transition at the playhead")),
                b(&["Ctrl+T"], tr("Add a title")),
                b(&["Ctrl+D"], tr("Duplicate")),
                b(&["Del", "Backspace"], tr("Delete")),
                b(&["Shift+Del"], tr("Ripple delete: close the gap it leaves")),
                b(&["Ctrl+A"], tr("Select all clips")),
                b(&["Ctrl+G"], tr("Group")),
                b(&["Ctrl+Shift+G"], tr("Ungroup")),
                b(&["Alt+↑", "Alt+↓"], tr("Move the picked tracks up or down")),
                b(&["Ctrl+Click"], tr("Pick more clips or tracks")),
                b(&["Wheel"], tr("Scroll through time")),
                b(&["Shift+Wheel"], tr("Scroll up and down")),
                b(&["Ctrl+Wheel"], tr("Zoom")),
            ],
        ),
        (
            tr("Viewer"),
            vec![
                b(&["Ctrl+0"], tr("The whole frame, centered")),
                b(&["Ctrl+1"], tr("Zoom to 100%")),
                b(&["Ctrl+Wheel"], tr("Zoom around the pointer")),
                b(&["Middle-drag"], tr("Pan")),
                b(&["Alt+Arrows"], tr("Nudge the selected layer (pointer over the viewer)")),
                b(&["Alt+Shift+Arrows"], tr("Nudge it 10 pixels")),
                b(&["Double-click"], tr("Crop the clip under the pointer")),
                b(&["Enter"], tr("Finish cropping")),
                b(&["Ctrl+Enter", "Esc"], tr("Finish typing a title")),
            ],
        ),
        (
            tr("Media bin"),
            vec![
                b(&["Ctrl+Click"], tr("Pick more files")),
                b(&["Ctrl+A"], tr("Pick everything shown")),
                b(&["Del"], tr("Remove the picked files")),
                b(&["Esc"], tr("Let go of the picked files")),
            ],
        ),
        (
            tr("Effects"),
            vec![
                b(&["Click"], tr("Pick an effect (anywhere on its card)")),
                b(&["Ctrl+Click"], tr("Pick more effects")),
                b(&["Shift+Click"], tr("Pick a run of effects")),
                b(&["Ctrl+A"], tr("Pick all the effects")),
                b(&["Del"], tr("Remove the picked effects")),
                b(&["Ctrl+C", "Ctrl+V"], tr("Copy the picked effects, or paste effects")),
                b(&["Ctrl+G"], tr("Group the picked effects")),
                b(&["Ctrl+Shift+G"], tr("Take them out of their groups")),
                b(&["Alt+↑", "Alt+↓"], tr("Move the picked effects up or down")),
                b(&["Esc"], tr("Pick none")),

            ],
        ),
        (
            tr("Curve editor"),
            vec![b(&["Del"], tr("Remove the selected key"))],
        ),
        (
            tr("Quick cut"),
            vec![
                b(&["I", "O"], tr("Mark the start or the end of a part")),
                b(&["Space"], tr("Play or pause")),
                b(&["←", "→"], tr("Step through the file")),
                b(&["Del"], tr("Remove the picked part")),
                b(&["Esc"], tr("Close")),
            ],
        ),
        (
            tr("Captions"),
            vec![
                b(&["←", "→"], tr("Previous or next caption")),
                b(&["Ctrl+Z", "Ctrl+Shift+Z"], tr("Undo or redo in the captions")),
            ],
        ),
    ]
}

/// A mouse button, as a cap draws it.
#[derive(Copy, Clone, PartialEq)]
enum Button {
    Left,
    Right,
    /// The wheel, pressed (a middle click or drag).
    Middle,
    /// The wheel, turned.
    Wheel,
}

/// What a cap shows: a key's name, arrows, or a mouse with the button in use lit (and a
/// word after it: "×2", "drag").
enum Face {
    Text(String),
    /// Arrows, by the way they point (degrees: 0 right, 90 down).
    Arrows(Vec<f32>),
    Mouse(Button, Option<String>),
}

impl Face {
    /// The words for it, where the cap is a picture (its tooltip).
    fn tip(&self) -> Option<String> {
        match self {
            Face::Mouse(Button::Left, None) => Some(tr("Click").into()),
            Face::Mouse(Button::Left, Some(_)) => Some(tr("Double-click").into()),
            Face::Mouse(Button::Right, _) => Some(tr("Right-click").into()),
            Face::Mouse(Button::Middle, _) => Some(tr("Middle-drag").into()),
            Face::Mouse(Button::Wheel, _) => Some(tr("Wheel").into()),
            Face::Arrows(_) => Some(tr("Arrow keys").into()),
            Face::Text(_) => None,
        }
    }
}

/// A key as its cap shows it (the platform's names for the modifiers).
fn face(key: &str) -> Face {
    let mac = cfg!(target_os = "macos");
    let text = |s: &str| Face::Text(s.into());
    match key {
        "Ctrl" if mac => text("⌘"),
        "Alt" if mac => text("⌥"),
        "Shift" if mac => text("⇧"),
        "Space" => text(tr("Space")),
        "Del" => text(tr("Del")),
        "Home" => text(tr("Home")),
        "End" => text(tr("End")),
        "Enter" => text(tr("Enter")),
        "Esc" => text(tr("Esc")),
        "Backspace" => text(tr("Backspace")),
        "←" => Face::Arrows(vec![180.0]),
        "→" => Face::Arrows(vec![0.0]),
        "↑" => Face::Arrows(vec![270.0]),
        "↓" => Face::Arrows(vec![90.0]),
        "Arrows" => Face::Arrows(vec![180.0, 270.0, 90.0, 0.0]),
        "Click" => Face::Mouse(Button::Left, None),
        "RightClick" => Face::Mouse(Button::Right, None),
        "Double-click" => Face::Mouse(Button::Left, Some("×2".into())),
        "Middle-drag" => Face::Mouse(Button::Middle, Some(tr("drag").into())),
        "Wheel" => Face::Mouse(Button::Wheel, None),
        k => text(k),
    }
}

/// Keys printed on caps: monospace.
fn cap_font() -> egui::FontId {
    egui::FontId::monospace(crate::style::TEXT_S + 1.0)
}

/// Gaps between a combination's caps and the + between them.
const CAP_GAP: f32 = 3.0;
/// A drawn arrow's width on a cap, and a drawn mouse's.
const ARROW_W: f32 = 11.0;
const MOUSE_W: f32 = 11.0;

/// How wide a cap is.
fn cap_width(ui: &egui::Ui, face: &Face) -> f32 {
    let text = |s: &str| ui.painter().layout_no_wrap(s.to_string(), cap_font(), egui::Color32::PLACEHOLDER).size().x;
    let inner = match face {
        Face::Text(s) => text(s),
        Face::Arrows(dirs) => dirs.len() as f32 * ARROW_W + (dirs.len() as f32 - 1.0) * 2.0,
        Face::Mouse(_, word) => MOUSE_W + word.as_deref().map_or(0.0, |w| 4.0 + text(w)),
    };
    (inner + 12.0).max(22.0)
}

/// One keycap.
fn keycap(ui: &mut egui::Ui, face: &Face) {
    let visuals = ui.visuals().clone();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(cap_width(ui, face), 20.0), egui::Sense::hover());
    if let Some(tip) = face.tip() {
        response.on_hover_text(tip);
    }
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    // A key: a face over a slightly darker base, like a cap seen from the front.
    let base = visuals.widgets.noninteractive.bg_stroke.color;
    painter.rect_filled(rect, 4.0, base);
    let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 2.0));
    painter.rect(top, 4.0, visuals.faint_bg_color, egui::Stroke::new(1.0, base), egui::StrokeKind::Inside);
    let ink = visuals.strong_text_color();
    match face {
        Face::Text(s) => {
            let galley = painter.layout_no_wrap(s.clone(), cap_font(), ink);
            painter.galley(top.center() - galley.size() / 2.0, galley, ink);
        }
        Face::Arrows(dirs) => {
            let total = dirs.len() as f32 * ARROW_W + (dirs.len() as f32 - 1.0) * 2.0;
            let mut x = top.center().x - total / 2.0 + ARROW_W / 2.0;
            for d in dirs {
                arrow(painter, egui::pos2(x, top.center().y), *d, ink);
                x += ARROW_W + 2.0;
            }
        }
        Face::Mouse(button, word) => {
            let word_w = word.as_deref().map_or(0.0, |w| 4.0 + painter.layout_no_wrap(w.to_string(), cap_font(), ink).size().x);
            let left = top.center().x - (MOUSE_W + word_w) / 2.0;
            mouse(painter, egui::Rect::from_center_size(egui::pos2(left + MOUSE_W / 2.0, top.center().y), egui::vec2(MOUSE_W - 2.0, 14.0)), *button, ink);
            if let Some(w) = word {
                let galley = painter.layout_no_wrap(w.clone(), cap_font(), ink);
                painter.galley(egui::pos2(left + MOUSE_W + 4.0, top.center().y - galley.size().y / 2.0), galley, ink);
            }
        }
    }
}

/// An arrow pointing `degrees` (0 right, 90 down), centered on `at`.
fn arrow(painter: &egui::Painter, at: egui::Pos2, degrees: f32, color: egui::Color32) {
    let a = degrees.to_radians();
    let along = egui::vec2(a.cos(), a.sin());
    let across = egui::vec2(-along.y, along.x);
    let (tail, tip) = (at - along * 4.5, at + along * 4.5);
    painter.line_segment([tail, tip - along * 2.0], egui::Stroke::new(1.6, color));
    painter.add(egui::Shape::convex_polygon(vec![tip, tip - along * 4.0 + across * 3.2, tip - along * 4.0 - across * 3.2], color, egui::Stroke::NONE));
}

/// A mouse in `rect`: its outline, the line between its buttons, the wheel — and the
/// button in use filled in (the wheel with marks above and below when it turns).
fn mouse(painter: &egui::Painter, rect: egui::Rect, button: Button, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let r = rect.width() / 2.0;
    let split = rect.top() + rect.height() * 0.42;
    let mid = rect.center().x;
    // The button pressed: its half of the top, filled.
    let half = |x0: f32, x1: f32| egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, split));
    match button {
        Button::Left => {
            painter.rect_filled(half(rect.left(), mid), egui::CornerRadius { nw: r as u8, ..Default::default() }, color);
        }
        Button::Right => {
            painter.rect_filled(half(mid, rect.right()), egui::CornerRadius { ne: r as u8, ..Default::default() }, color);
        }
        _ => {}
    }
    painter.rect_stroke(rect, r, stroke, egui::StrokeKind::Middle);
    painter.line_segment([egui::pos2(rect.left(), split), egui::pos2(rect.right(), split)], stroke);
    painter.line_segment([egui::pos2(mid, rect.top()), egui::pos2(mid, split)], stroke);
    // The wheel: a small pill over the split; pressed, filled; turned, with marks.
    let wheel = egui::Rect::from_center_size(egui::pos2(mid, rect.top() + rect.height() * 0.28), egui::vec2(2.6, 5.0));
    match button {
        Button::Middle => {
            painter.rect_filled(wheel.expand(0.6), 1.5, color);
        }
        Button::Wheel => {
            painter.rect_filled(wheel.expand(0.6), 1.5, color);
            for dy in [-4.5, 4.5] {
                let y = wheel.center().y + dy;
                painter.line_segment([egui::pos2(mid - 1.0, y), egui::pos2(mid + 1.0, y)], egui::Stroke::new(1.0, color));
            }
        }
        _ => {}
    }
}

/// A combination ("Ctrl+Shift+S"): its caps with a + between them.
fn combo(ui: &mut egui::Ui, keys: &str) {
    ui.spacing_mut().item_spacing.x = CAP_GAP;
    for (i, key) in keys.split('+').filter(|k| !k.is_empty()).enumerate() {
        if i > 0 {
            ui.label(egui::RichText::new("+").weak());
        }
        // "Ctrl+," splits into "Ctrl" and ","; a lone comma is its own key.
        keycap(ui, &face(key));
    }
}

/// How wide a shortcut's keys are drawn: its combinations, a / between them.
fn keys_width(ui: &egui::Ui, keys: &[&str]) -> f32 {
    let label = |s: &str| ui.painter().layout_no_wrap(s.to_string(), egui::TextStyle::Body.resolve(ui.style()), egui::Color32::PLACEHOLDER).size().x;
    let (plus, slash) = (label("+"), label("/"));
    keys.iter()
        .enumerate()
        .map(|(i, k)| {
            let caps: Vec<&str> = k.split('+').filter(|k| !k.is_empty()).collect();
            let width: f32 = caps.iter().map(|c| cap_width(ui, &face(c))).sum::<f32>() + (caps.len() as f32 - 1.0) * (plus + 2.0 * CAP_GAP);
            width + if i > 0 { slash + 2.0 * CAP_GAP } else { 0.0 }
        })
        .sum()
}

impl App {
    /// The Shortcuts page of Settings.
    pub(crate) fn keybind_list(&mut self, ui: &mut egui::Ui) {
        let search_id = egui::Id::new("keybind-search");
        let mut needle: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut needle).hint_text(tr("Search shortcuts")).desired_width(f32::INFINITY));
        ui.data_mut(|d| d.insert_temp(search_id, needle.clone()));
        ui.add_space(4.0);
        let needle = needle.trim().to_lowercase();
        let mut any = false;
        // The keys' column: as wide as the widest keys of any shortcut (measured, so a long
        // combination never runs into its description), the same in every section.
        let keys_width = sections().iter().flat_map(|(_, binds)| binds.iter().map(|b| keys_width(ui, b.keys))).fold(0.0, f32::max) + 4.0;
        for (title, binds) in sections() {
            let shown: Vec<&Bind> = binds
                .iter()
                .filter(|b| needle.is_empty() || b.what.to_lowercase().contains(&needle) || b.keys.iter().any(|k| k.to_lowercase().contains(&needle)) || title.to_lowercase().contains(&needle))
                .collect();
            if shown.is_empty() {
                continue;
            }
            any = true;
            ui.add_space(10.0);
            ui.label(egui::RichText::new(title).strong().size(crate::style::TEXT_L));
            ui.add_space(2.0);
            // Rows of one height: the keys in a column as wide in every section (so the
            // descriptions line up down the page), the description after, every other
            // row shaded the whole width.
            let stripe = ui.visuals().faint_bg_color;
            for (n, b) in shown.iter().enumerate() {
                let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::hover());
                if n % 2 == 0 {
                    ui.painter().rect_filled(row, 4.0, stripe);
                }
                let keys = egui::Rect::from_min_size(row.min + egui::vec2(8.0, 0.0), egui::vec2(keys_width, ROW_HEIGHT));
                ui.scope_builder(egui::UiBuilder::new().max_rect(keys).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                    for (i, k) in b.keys.iter().enumerate() {
                        if i > 0 {
                            ui.label(egui::RichText::new("/").weak());
                        }
                        combo(ui, k);
                    }
                });
                let text = egui::Rect::from_min_max(egui::pos2(keys.right() + 12.0, row.top()), row.max - egui::vec2(8.0, 0.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                    ui.add(egui::Label::new(b.what).truncate());
                });
            }
        }
        if !any {
            ui.label(egui::RichText::new(tr("No shortcut matches.")).weak());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key is listed twice in one panel for different things, and every combination
    /// splits into keys (a trailing "+" would be an empty cap).
    #[test]
    fn the_list_is_consistent() {
        for (title, binds) in sections() {
            let mut seen: Vec<&str> = Vec::new();
            for b in &binds {
                for keys in b.keys {
                    assert!(!keys.ends_with('+') && !keys.is_empty(), "{title}: {keys:?}");
                    assert!(!seen.contains(keys), "{title}: {keys} is listed twice");
                    seen.push(keys);
                }
            }
        }
    }
}
