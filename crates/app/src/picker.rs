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

/// The categories built-in effects fall into, from the middle of their id.
fn builtin_category(type_id: &str) -> Option<&'static str> {
    let part = type_id.strip_prefix("oa.")?.split('.').next()?;
    Some(match part {
        "color" => "Color",
        "blur" => "Blur",
        "light" => "Light",
        "key" => "Keying",
        "mask" => "Masking",
        "warp" => "Warp",
        "stylize" => "Stylize",
        "depth" => "Depth",
        "motion" => "Motion",
        "anim" => "Animation",
        "text" => "Text",
        "transition" => "Transition",
        "audio" => "Sound",
        _ => "Other",
    })
}

impl App {
    /// Where an effect came from, as the picker groups them.
    pub(crate) fn effect_category(&self, type_id: &str) -> String {
        if let Some(c) = builtin_category(type_id) {
            return c.to_string();
        }
        // A plugin's effects are grouped under the plugin.
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
    use super::*;

    #[test]
    fn builtins_are_grouped_by_what_they_do() {
        assert_eq!(builtin_category("oa.color.tint"), Some("Color"));
        assert_eq!(builtin_category("oa.depth.slab"), Some("Depth"));
        assert_eq!(builtin_category("oa.text.wave"), Some("Text"));
        assert_eq!(builtin_category("com.example.vignette"), None, "plugins group under their own name");
    }
}
