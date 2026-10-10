//! The font menu: every installed family, searchable, each name drawn in its own font.
//!
//! The samples are rasterized by `oa-text` from the family's own outlines and uploaded
//! as small textures, a few per frame and only for rows on screen, so opening the menu
//! on a machine with a thousand fonts doesn't stall. A family whose sample isn't ready
//! (or which can't draw its own name — symbol fonts) shows as plain text. Starred fonts
//! and the ones used lately (`Settings::favorite_fonts`, `recent_fonts`) come first.

use crate::i18n::tr;
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
    /// family picked, if it changed. Without a search it lists the default font, then the
    /// starred fonts, the ones used lately, and every font; a search looks through them
    /// all. Every row has a star (shown on hover, gold when on) to keep a font at the top.
    pub(crate) fn font_menu(&mut self, ui: &mut egui::Ui, id: egui::Id, family: &str) -> Option<String> {
        /// A row of the list: a heading, or a font ("" the default).
        enum Row {
            Heading(&'static str),
            Font(String),
        }
        let default = oa_text::default_family();
        let default_name = format!("{default} ({})", tr("default"));
        let shown = if family.is_empty() { default_name.clone() } else { family.to_string() };
        let mut picked = None;
        let width = ui.available_width().max(120.0);
        // It stays open while you star fonts; picking one closes it.
        let combo = egui::ComboBox::from_id_salt(id).selected_text(shown).width(width).height(460.0);
        combo.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(ui, |ui| {
            let search_id = id.with("search");
            let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
            let hint = tr("Search fonts");
            let field = ui.id().with(("search-field", hint));
            if ui.memory(|m| m.focused().is_none()) {
                ui.memory_mut(|m| m.request_focus(field));
            }
            crate::widgets::search_field(ui, &mut search, hint, ui.available_width());
            let needle = search.trim().to_lowercase();
            ui.data_mut(|d| d.insert_temp(search_id, search));
            ui.add_space(4.0);

            let installed = oa_text::families();
            let mut rows = Vec::new();
            if needle.is_empty() {
                rows.push(Row::Font(String::new()));
                let favorites: Vec<&String> = self.settings.favorite_fonts.iter().filter(|f| installed.contains(f)).collect();
                if !favorites.is_empty() {
                    rows.push(Row::Heading("Favorites"));
                    rows.extend(favorites.into_iter().map(|f| Row::Font(f.clone())));
                }
                let recent: Vec<&String> = self.settings.recent_fonts.iter().filter(|f| installed.contains(f) && !self.settings.favorite_fonts.contains(f)).collect();
                if !recent.is_empty() {
                    rows.push(Row::Heading("Recent"));
                    rows.extend(recent.into_iter().map(|f| Row::Font(f.clone())));
                }
                rows.push(Row::Heading("All fonts"));
            }
            rows.extend(installed.iter().filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle)).map(|f| Row::Font(f.clone())));
            if rows.is_empty() {
                ui.label(egui::RichText::new(tr("No font by that name.")).weak());
            }
            // Only rows in view are drawn, so the samples are made for those.
            let row_height = SAMPLE_PX + 10.0;
            let mut star: Option<String> = None;
            egui::ScrollArea::vertical().max_height(400.0).show_rows(ui, row_height, rows.len(), |ui, range| {
                let first = range.start;
                for (n, row) in rows[range].iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_height), egui::Sense::click());
                    let f = match row {
                        Row::Heading(title) => {
                            let at = egui::pos2(rect.left() + 6.0, rect.bottom() - 4.0);
                            ui.painter().text(at, egui::Align2::LEFT_BOTTOM, tr(title), egui::FontId::proportional(crate::style::TEXT_S), ui.visuals().weak_text_color());
                            continue;
                        }
                        Row::Font(f) => f,
                    };
                    let selected = f == family;
                    // The star, on the right: registered after the row, so it takes the
                    // pointer from it. Keyed by its row: a starred font is listed twice
                    // (under Favorites and under All fonts).
                    let star_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(20.0, 20.0));
                    let starred = self.settings.favorite_fonts.contains(f);
                    let star_r = (!f.is_empty()).then(|| ui.interact(star_rect, id.with(("star", first + n)), egui::Sense::click()));
                    if ui.is_rect_visible(rect) {
                        let visuals = ui.style().interact_selectable(&response, selected);
                        let lit = selected || response.hovered() || star_r.as_ref().is_some_and(|r| r.hovered());
                        if lit {
                            ui.painter().rect_filled(rect, 3.0, if selected { visuals.bg_fill } else { ui.visuals().widgets.hovered.weak_bg_fill });
                        }
                        let at = egui::pos2(rect.left() + 6.0, rect.center().y);
                        let sample = if f.is_empty() { None } else { self.font_samples.sample(ui.ctx(), f) };
                        match sample {
                            Some(texture) => {
                                let size = texture.size_vec2();
                                let size = size * (SAMPLE_PX / size.y.max(1.0)).min(1.0);
                                let size = size * ((rect.width() - 40.0) / size.x.max(1.0)).min(1.0);
                                let dest = egui::Rect::from_min_size(egui::pos2(at.x, at.y - size.y / 2.0), size);
                                ui.painter().image(texture.id(), dest, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), visuals.text_color());
                            }
                            None => {
                                let name = if f.is_empty() { default_name.as_str() } else { f.as_str() };
                                ui.painter().text(at, egui::Align2::LEFT_CENTER, name, egui::FontId::proportional(13.0), visuals.text_color());
                            }
                        }
                        if let Some(r) = &star_r
                            && (starred || lit)
                        {
                            let color = if starred {
                                crate::style::GOLD
                            } else if r.hovered() {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            crate::icons::paint(ui.painter(), star_rect.shrink(2.0), crate::icons::STAR, color);
                        }
                    }
                    if let Some(r) = star_r
                        && r.on_hover_text(if starred { tr("Remove from favorites") } else { tr("Add to favorites: kept at the top") }).clicked()
                    {
                        star = Some(f.clone());
                    }
                    if response.clicked() {
                        picked = Some(f.clone());
                        ui.close();
                    }
                }
            });
            if let Some(f) = star {
                self.settings.toggle_favorite_font(&f);
                self.settings.save();
            }
        });
        if let Some(p) = &picked {
            self.settings.used_font(p);
            self.settings.save();
        }
        picked.filter(|p| p != family)
    }
}
