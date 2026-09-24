//! The font menu: every installed family, searchable, each name drawn in its own font.
//!
//! The samples are rasterized by `oa-text` from the family's own outlines and uploaded
//! as small textures, a few per frame and only for rows on screen, so opening the menu
//! on a machine with a thousand fonts doesn't stall. A family whose sample isn't ready
//! (or which can't draw its own name — symbol fonts) shows as plain text.

use crate::App;
use eframe::egui;
use std::collections::HashMap;

/// Sample height in points.
const SAMPLE_PX: f32 = 17.0;
/// Samples rasterized per frame.
const PER_FRAME: usize = 8;

#[derive(Default)]
pub struct FontSamples {
    /// `None`: this family can't draw its own name, don't try again.
    images: HashMap<String, Option<egui::TextureHandle>>,
    made_this_frame: usize,
}

impl FontSamples {
    pub fn frame_start(&mut self) {
        self.made_this_frame = 0;
    }

    /// The family's name drawn in the family, if it's ready (or can be made now).
    fn sample(&mut self, ctx: &egui::Context, family: &str) -> Option<&egui::TextureHandle> {
        if !self.images.contains_key(family) {
            if self.made_this_frame >= PER_FRAME {
                ctx.request_repaint();
                return None;
            }
            self.made_this_frame += 1;
            let made = oa_text::fonts::sample_image(family, family, SAMPLE_PX).map(|(w, h, alpha)| {
                // White text, alpha from the outline coverage: it tints with the theme.
                let pixels: Vec<egui::Color32> = alpha.iter().map(|a| egui::Color32::from_white_alpha(*a)).collect();
                let image = egui::ColorImage { size: [w as usize, h as usize], pixels, source_size: egui::vec2(w as f32, h as f32) };
                ctx.load_texture(format!("font-{family}"), image, egui::TextureOptions::LINEAR)
            });
            self.images.insert(family.to_string(), made);
        }
        self.images.get(family).and_then(|t| t.as_ref())
    }
}

impl App {
    /// The font picker. `family` is the chosen family ("" = the default one); returns the
    /// family picked, if it changed.
    pub(crate) fn font_menu(&mut self, ui: &mut egui::Ui, id: egui::Id, family: &str) -> Option<String> {
        let default = oa_text::default_family();
        let shown = if family.is_empty() { format!("{default} (default)") } else { family.to_string() };
        let mut picked = None;
        egui::ComboBox::from_id_salt(id).selected_text(shown).width(190.0).height(420.0).show_ui(ui, |ui| {
            let search_id = id.with("search");
            let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text("🔍 Search fonts").desired_width(170.0));
            r.request_focus();
            let needle = search.trim().to_lowercase();
            ui.data_mut(|d| d.insert_temp(search_id, search));
            ui.separator();

            if needle.is_empty() && ui.selectable_label(family.is_empty(), format!("{default} (default)")).clicked() {
                picked = Some(String::new());
                ui.close();
            }
            let families: Vec<&String> =
                oa_text::families().iter().filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle)).collect();
            if families.is_empty() {
                ui.label(egui::RichText::new("No font by that name.").weak());
            }
            // Only rows in view are drawn, so the samples are made for those.
            let row_height = SAMPLE_PX + 10.0;
            egui::ScrollArea::vertical().max_height(360.0).show_rows(ui, row_height, families.len(), |ui, range| {
                for f in &families[range] {
                    let selected = *f == family;
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_height), egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        let visuals = ui.style().interact_selectable(&response, selected);
                        if selected || response.hovered() {
                            ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
                        }
                        let at = egui::pos2(rect.left() + 6.0, rect.center().y);
                        match self.font_samples.sample(ui.ctx(), f) {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let size = size * (SAMPLE_PX / size.y.max(1.0)).min(1.0);
                                let dest = egui::Rect::from_min_size(egui::pos2(at.x, at.y - size.y / 2.0), size);
                                ui.painter().image(
                                    texture.id(),
                                    dest,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    visuals.text_color(),
                                );
                            }
                            None => {
                                ui.painter().text(at, egui::Align2::LEFT_CENTER, f, egui::FontId::proportional(13.0), visuals.text_color());
                            }
                        }
                    }
                    if response.clicked() {
                        picked = Some((*f).clone());
                        ui.close();
                    }
                }
            });
        });
        picked.filter(|p| p != family)
    }
}
