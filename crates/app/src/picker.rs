//! The effect picker: a searchable, filterable grid of live previews.
//!
//! Shared by the Intro, Outro and Effects menus. Type to narrow the list by name or
//! category, or click a category chip — "Color", "Blur", "Text"… for the built-ins,
//! the plugin's own name for anything else, so it's clear where an effect came from.
//! Each cell is the real thing rendered on this clip (see `thumbs.rs`), and hovering
//! plays it.

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
            .map(|d| (d.type_id.to_string(), d.name.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(type_id, name)| Entry { category: self.effect_category(&type_id), used: used(&type_id), name, type_id })
            .collect();

        // Search text and the chosen category live in egui's memory, so each menu keeps
        // its own and they reset when the app restarts rather than sticking in a file.
        let id = ui.id().with(("effect-picker", kind));
        let (mut search, mut category): (String, String) = ui.data(|d| d.get_temp(id)).unwrap_or_default();

        let mut categories: Vec<String> = entries.iter().map(|e| e.category.clone()).collect();
        categories.sort();
        categories.dedup();

        // The menu is as wide as three preview cells; everything inside fills it.
        let width = CELL * COLUMNS as f32 + crate::style::GAP * (COLUMNS as f32 + 1.0);
        ui.set_width(width);
        ui.horizontal(|ui| {
            let clear = !search.is_empty();
            let box_width = if clear { width - 36.0 } else { width };
            let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text("Search effects").desired_width(box_width));
            if !ui.memory(|m| m.focused().is_some()) {
                r.request_focus();
            }
            if clear && ui.small_button("✕").on_hover_text("Clear").clicked() {
                search.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            let all = category.is_empty();
            if ui.selectable_label(all, "All").clicked() {
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
        ui.data_mut(|d| d.insert_temp(id, (search, category)));

        ui.label(egui::RichText::new("Previewed on this clip at the playhead · hover to play").small().weak());
        let aspect = self.canvas_aspect();
        let mut picked = None;
        egui::ScrollArea::vertical().max_height(480.0).auto_shrink([false, true]).show(ui, |ui| {
            if entries.is_empty() {
                ui.label(egui::RichText::new("Nothing matches.").weak());
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
        let example = oa_graph::plugin::load(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/example-looks/plugin.json")).expect("loads");
        assert!(example.effects.iter().all(|d| d.category.is_none()));
    }
}
