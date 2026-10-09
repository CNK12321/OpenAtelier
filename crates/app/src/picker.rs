//! The effect picker: a searchable, filterable grid of live previews.
//!
//! Shared by the Intro, Outro and Effects menus. Type to narrow the list by name or
//! category, or click a category chip — "Color", "Blur", "Text"… for the built-ins,
//! the plugin's own name for anything else, so it's clear where an effect came from.
//! Each cell is the real thing rendered on this clip (see `thumbs.rs`), and hovering
//! plays it.

use crate::i18n::tr;
use crate::thumbs::ThumbKind;
use crate::App;
use eframe::egui;
use oa_doc::ItemId;
use oa_graph::registry::EffectUsage;

/// Preview cells across the picker, and how wide each is.
const COLUMNS: usize = 3;
pub const CELL: f32 = 150.0;

/// What the menu shows, gathered before drawing.
struct Entry {
    type_id: String,
    name: String,
    category: String,
    used: bool,
    /// Made for what's being added (an intro/outro proper), not an effect offered for
    /// it because it has an off state: these come first.
    own: bool,
}

impl App {
    /// What an effect is for, as the picker groups them: its manifest's `category`
    /// ("Color", "Blur"…), or the plugin it came from.
    pub(crate) fn effect_category(&self, type_id: &str) -> String {
        if let Some(c) = self.registry.effect(type_id).and_then(|d| d.category.clone()) {
            return c;
        }
        self.registry
            .plugin_of(type_id)
            .and_then(|id| self.plugins.list.iter().find(|p| p.id == **id))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Other".into())
    }

    /// The contents of an "+ Add …" menu: search box, category chips and the preview
    /// grid. Returns the effect picked, if one was.
    pub(crate) fn effect_picker(
        &mut self,
        ui: &mut egui::Ui,
        item: ItemId,
        kind: ThumbKind,
        usage: EffectUsage,
        is_text: bool,
        used: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        let mut entries: Vec<Entry> = self
            .registry
            .offered_for(usage, is_text)
            .iter()
            .map(|d| (d.type_id.to_string(), d.name.clone(), d.usage == usage))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(type_id, name, own)| Entry { category: self.effect_category(&type_id), used: used(&type_id), name, type_id, own })
            .collect();

        // Search text and the chosen category live in egui's memory, so each menu keeps
        // its own and they reset when the app restarts rather than sticking in a file.
        let id = ui.id().with(("effect-picker", kind));
        let (mut search, mut category): (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let before = (search.clone(), category.clone());

        // Categories with effects made for this first (an intro picker's Animation and
        // Text before the effects that ease to off), each lot in alphabetical order.
        let mut categories: Vec<(bool, String)> = entries.iter().map(|e| (!entries.iter().any(|o| o.own && o.category == e.category), e.category.clone())).collect();
        categories.sort();
        categories.dedup();
        let categories: Vec<String> = categories.into_iter().map(|(_, c)| c).collect();

        // The menu is as wide as three preview cells; everything inside fills it.
        let width = CELL * COLUMNS as f32 + crate::style::GAP * (COLUMNS as f32 + 1.0);
        ui.set_width(width);
        ui.horizontal(|ui| {
            let clear = !search.is_empty();
            let box_width = if clear { width - 36.0 } else { width };
            let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text(tr("Search effects")).desired_width(box_width));
            if !ui.memory(|m| m.focused().is_some()) {
                r.request_focus();
            }
            if clear && ui.small_button(tr("✕")).on_hover_text(tr("Clear")).clicked() {
                search.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            let all = category.is_empty();
            if ui.selectable_label(all, tr("All")).clicked() {
                category.clear();
            }
            for c in &categories {
                if ui.selectable_label(category == *c, c).clicked() {
                    category = if category == *c { String::new() } else { c.clone() };
                }
            }
        });
        let needle = search.trim().to_lowercase();
        entries.retain(|e| {
            (category.is_empty() || e.category == category)
                && (needle.is_empty() || e.name.to_lowercase().contains(&needle) || e.category.to_lowercase().contains(&needle))
        });
        // Another tab or search starts the grid at its top.
        let changed = before != (search.clone(), category.clone());
        ui.data_mut(|d| d.insert_temp(id, (search, category)));

        ui.label(egui::RichText::new(tr("Previewed on this clip at the playhead · hover to play")).small().weak());
        let aspect = self.canvas_aspect();
        let mut picked = None;
        // As tall as this category's previews need (up to 480): switching tabs resizes it.
        let rows = entries.len().div_ceil(COLUMNS).max(1) as f32;
        let row = CELL / aspect.max(0.1) + 20.0 + ui.spacing().item_spacing.y;
        let mut scroll = crate::widgets::fitted_scroll(480.0, rows * row).id_salt(("effect-picker-grid", kind));
        if changed {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            if entries.is_empty() {
                ui.label(egui::RichText::new(tr("Nothing matches.")).weak());
            }
            for row in entries.chunks(COLUMNS) {
                ui.horizontal(|ui| {
                    for e in row {
                        if self.effect_cell(ui, item, kind, &e.type_id, &e.name, aspect, e.used) {
                            picked = Some(e.type_id.clone());
                        }
                    }
                });
            }
        });
        picked
    }
}

/// Sound effect cards across the sound picker, and each one's size.
const SOUND_COLUMNS: usize = 2;
const SOUND_CARD: egui::Vec2 = egui::vec2(230.0, 62.0);

impl App {
    /// The "+ Add sound effect" menu: search, a tab for each group (Dynamics, Space…),
    /// and a grid of cards saying what each does. `offered` is (id, name, description).
    pub(crate) fn sound_picker(&mut self, ui: &mut egui::Ui, offered: &[(String, String, String)], used: &dyn Fn(&str) -> bool) -> Option<String> {
        let entries: Vec<(&(String, String, String), String)> = offered.iter().map(|o| (o, self.effect_category(&o.0))).collect();
        let id = ui.id().with("sound-picker");
        let (mut search, mut category): (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let before = (search.clone(), category.clone());
        let mut categories: Vec<String> = entries.iter().map(|e| e.1.clone()).collect();
        categories.sort();
        categories.dedup();

        let width = SOUND_CARD.x * SOUND_COLUMNS as f32 + ui.spacing().item_spacing.x * (SOUND_COLUMNS as f32 - 1.0);
        ui.set_width(width);
        ui.horizontal(|ui| {
            let clear = !search.is_empty();
            let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text(tr("Search sound effects")).desired_width(if clear { width - 36.0 } else { width }));
            if !ui.memory(|m| m.focused().is_some()) {
                r.request_focus();
            }
            if clear && ui.small_button(tr("✕")).on_hover_text(tr("Clear")).clicked() {
                search.clear();
            }
        });
        if categories.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(category.is_empty(), tr("All")).clicked() {
                    category.clear();
                }
                for c in &categories {
                    if ui.selectable_label(category == *c, crate::i18n::t(c)).clicked() {
                        category = if category == *c { String::new() } else { c.clone() };
                    }
                }
            });
        }
        let needle = search.trim().to_lowercase();
        let shown: Vec<&(&(String, String, String), String)> = entries
            .iter()
            .filter(|((_, name, about), c)| {
                (category.is_empty() || *c == category)
                    && (needle.is_empty() || [name, about, c].iter().any(|s| s.to_lowercase().contains(&needle)))
            })
            .collect();
        let changed = before != (search.clone(), category.clone());
        ui.data_mut(|d| d.insert_temp(id, (search, category)));

        let rows = shown.len().div_ceil(SOUND_COLUMNS).max(1) as f32;
        let mut scroll = crate::widgets::fitted_scroll(420.0, rows * (SOUND_CARD.y + ui.spacing().item_spacing.y)).id_salt("sound-picker-grid");
        if changed {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        let mut picked = None;
        scroll.show(ui, |ui| {
            if shown.is_empty() {
                ui.label(egui::RichText::new(tr("Nothing matches.")).weak());
            }
            for row in shown.chunks(SOUND_COLUMNS) {
                ui.horizontal(|ui| {
                    for ((type_id, name, about), category) in row.iter().map(|e| (e.0, &e.1)) {
                        if sound_card(ui, name, about, category, used(type_id)).clicked() {
                            picked = Some(type_id.clone());
                        }
                    }
                });
            }
        });
        picked
    }
}

/// A sound effect's card: its name, its group and (in two lines at most) what it does;
/// marked when the clip already has it.
fn sound_card(ui: &mut egui::Ui, name: &str, about: &str, category: &str, used: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(SOUND_CARD, egui::Sense::click());
    let visuals = ui.visuals().clone();
    let fill = if response.hovered() { visuals.widgets.hovered.weak_bg_fill } else { visuals.faint_bg_color };
    let stroke = if used {
        egui::Stroke::new(1.5, visuals.selection.stroke.color)
    } else if response.hovered() {
        egui::Stroke::new(1.0, visuals.widgets.hovered.fg_stroke.color)
    } else {
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
    };
    ui.painter().rect(rect, 4.0, fill, stroke, egui::StrokeKind::Inside);
    let inner = rect.shrink2(egui::vec2(8.0, 6.0));
    let painter = ui.painter_at(rect);
    painter.text(inner.left_top(), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(13.5), visuals.strong_text_color());
    let tag = if used { format!("{} · {}", crate::i18n::t(category), tr("on this clip")) } else { crate::i18n::t(category).to_string() };
    painter.text(inner.right_top(), egui::Align2::RIGHT_TOP, tag, egui::FontId::proportional(10.5), visuals.weak_text_color());
    let mut job = egui::text::LayoutJob::single_section(about.to_string(), egui::TextFormat::simple(egui::FontId::proportional(11.0), visuals.weak_text_color()));
    job.wrap = egui::text::TextWrapping { max_width: inner.width(), max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(inner.left_top() + egui::vec2(0.0, 19.0), galley, visuals.weak_text_color());
    response.on_hover_text(about).on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    /// Atelier Core's effects say what they're for; a plugin's without one group under
    /// the plugin's name.
    #[test]
    fn builtins_are_grouped_by_what_they_do() {
        let r = oa_graph::registry::Registry::with_builtins();
        let category = |id: &str| r.effect(id).and_then(|d| d.category.clone());
        assert_eq!(category("oa.color.tint").as_deref(), Some("Color"));
        assert_eq!(category("oa.depth.slab").as_deref(), Some("Depth"));
        assert_eq!(category("oa.text.wave").as_deref(), Some("Text"));
        // Sound effects are grouped by what they do, for the sound picker's tabs.
        let groups = ["Dynamics", "EQ & Tone", "Space", "Character", "Cleanup", "Fades"];
        for d in r.sounds() {
            let c = d.category.clone().unwrap_or_default();
            assert!(groups.contains(&c.as_str()), "{} is in {c:?}", d.type_id);
        }
        assert_eq!(category("oa.audio.reverb").as_deref(), Some("Space"));
        assert!(crate::widgets::choice_columns(14) == 3 && crate::widgets::choice_columns(3) == 1);
        let example = oa_graph::plugin::load(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/example-looks/plugin.json")).expect("loads");
        assert!(example.effects.iter().all(|d| d.category.is_none()));
    }
}
