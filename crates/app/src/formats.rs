//! The format picker (DESIGN.md §5): one project, several shapes. The top bar shows the
//! format being edited; its menu lists every format with its shape, and lets you
//! switch, turn one sideways, change its resolution, rename or remove it, and add more —
//! from presets (with the platforms each is for) or at a custom size.

use crate::App;
use eframe::egui;
use oa_doc::{AspectPreset, CanvasSize, FormatVariant, Op, VariantId};

/// Short-edge resolutions offered for a format.
const RESOLUTIONS: [(u32, &str); 4] = [(720, "720p"), (1080, "1080p"), (1440, "1440p"), (2160, "4K")];

/// A little rectangle in the format's shape.
fn shape(ui: &mut egui::Ui, size: CanvasSize, box_px: f32, on: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(box_px, box_px), egui::Sense::hover());
    let k = box_px / size.width.max(size.height) as f32;
    let s = egui::vec2(size.width as f32 * k, size.height as f32 * k).max(egui::vec2(3.0, 3.0));
    let fill = if on { crate::style::ACCENT } else { ui.visuals().widgets.inactive.bg_fill };
    ui.painter().rect(egui::Rect::from_center_size(rect.center(), s * 0.9), 2.0, fill, egui::Stroke::new(1.0, ui.visuals().widgets.inactive.fg_stroke.color), egui::StrokeKind::Inside);
    r
}

/// What the picker asked for this frame.
enum Action {
    Select(usize),
    Add(&'static str),
    AddCustom(CanvasSize),
    Rotate(VariantId),
    Resize(VariantId, CanvasSize),
    Rename(VariantId, String),
    Remove(VariantId),
}

impl App {
    /// The format being edited, with everything about formats in its menu.
    pub(crate) fn format_picker(&mut self, ui: &mut egui::Ui) {
        let variants: Vec<FormatVariant> = self.editor.sequence().variants.clone();
        let current = self.variant.min(variants.len() - 1);
        let mut action: Option<Action> = None;
        let title = format!("{}  ▾", variants[current].name);
        let custom_key = egui::Id::new("format-custom-size");
        let rename_key = egui::Id::new("format-rename");
        crate::widgets::sticky_menu(ui, &title, |ui| {
            ui.set_min_width(380.0);
            ui.label(egui::RichText::new("Formats in this project").small().weak());
            for (i, v) in variants.iter().enumerate() {
                let renaming: Option<(u64, String)> = ui.data(|d| d.get_temp(rename_key));
                ui.horizontal(|ui| {
                    shape(ui, v.size, 24.0, i == current);
                    match renaming.filter(|(id, _)| *id == v.id.0) {
                        Some((_, mut text)) => {
                            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0));
                            r.request_focus();
                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                ui.data_mut(|d| d.remove::<(u64, String)>(rename_key));
                            } else if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let name = text.trim().to_string();
                                if !name.is_empty() && name != v.name {
                                    action = Some(Action::Rename(v.id, name));
                                }
                                ui.data_mut(|d| d.remove::<(u64, String)>(rename_key));
                            } else {
                                ui.data_mut(|d| d.insert_temp(rename_key, (v.id.0, text)));
                            }
                        }
                        None => {
                            let tip = if i == 0 {
                                "The main format: transform edits here apply to every format. Double-click to rename."
                            } else {
                                "Edits here change only this format (unless “all formats” is on). Double-click to rename."
                            };
                            let r = ui.selectable_label(i == current, egui::RichText::new(&v.name).strong()).on_hover_text(tip);
                            if r.clicked() {
                                action = Some(Action::Select(i));
                            }
                            if r.double_clicked() {
                                ui.data_mut(|d| d.insert_temp(rename_key, (v.id.0, v.name.clone())));
                            }
                        }
                    }
                    ui.label(egui::RichText::new(format!("{}×{}", v.size.width, v.size.height)).small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove (never the main format).
                        let removable = variants.len() > 1 && i > 0;
                        if ui.add_enabled(removable, egui::Button::new("✕").small().frame(false)).on_hover_text("Remove this format (its per-format changes go too)").clicked() {
                            action = Some(Action::Remove(v.id));
                        }
                        // Resolution: click for the next size up, right-click for the next down.
                        let at = RESOLUTIONS.iter().position(|(e, _)| *e == v.size.short_edge());
                        let label = at.map_or_else(|| format!("{}p", v.size.short_edge()), |i| RESOLUTIONS[i].1.to_string());
                        let r = ui.add(egui::Button::new(egui::RichText::new(label).small()).min_size(egui::vec2(48.0, 0.0))).on_hover_text("Resolution — click for bigger, right-click for smaller");
                        let step = |up: bool| {
                            let i = at.unwrap_or(1) as i32 + if up { 1 } else { -1 };
                            RESOLUTIONS[i.rem_euclid(RESOLUTIONS.len() as i32) as usize].0
                        };
                        if r.clicked() {
                            action = Some(Action::Resize(v.id, scale_to(v.size, step(true))));
                        } else if r.secondary_clicked() {
                            action = Some(Action::Resize(v.id, scale_to(v.size, step(false))));
                        }
                        if v.size.width != v.size.height && ui.add(egui::Button::new("⟲").small()).on_hover_text("Turn sideways (landscape ↔ portrait)").clicked() {
                            action = Some(Action::Rotate(v.id));
                        }
                    });
                });
            }
            ui.separator();
            ui.label(egui::RichText::new("Add a format").small().weak());
            for p in AspectPreset::all() {
                let have = variants.iter().any(|v| v.size.matching_preset().is_some_and(|m| m.id == p.id));
                let row = ui
                    .scope(|ui| {
                        ui.horizontal(|ui| {
                            ui.set_min_width(ui.available_width());
                            shape(ui, p.size(1080), 20.0, false);
                            let text = if have { egui::RichText::new(p.name).weak() } else { egui::RichText::new(p.name) };
                            ui.label(text);
                            ui.label(egui::RichText::new(if have { "already added" } else { p.platforms }).small().weak());
                        })
                        .response
                    })
                    .inner
                    .interact(egui::Sense::click());
                if !have {
                    if row.hovered() {
                        ui.painter().rect_filled(row.rect.expand(1.0), 3.0, ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.35));
                    }
                    if row.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        action = Some(Action::Add(p.id));
                    }
                }
            }
            ui.separator();
            let mut custom: [u32; 2] = ui.data(|d| d.get_temp(custom_key)).unwrap_or([1920, 1080]);
            ui.horizontal(|ui| {
                ui.label("Custom size");
                ui.add(egui::DragValue::new(&mut custom[0]).range(16..=8192).suffix(" w"));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut custom[1]).range(16..=8192).suffix(" h"));
                if ui.button("Add").on_hover_text("Encoders need even sizes; odd ones are rounded up").clicked() {
                    action = Some(Action::AddCustom(CanvasSize::new(custom[0].div_ceil(2) * 2, custom[1].div_ceil(2) * 2)));
                }
            });
            ui.data_mut(|d| d.insert_temp(custom_key, custom));
        });
        if let Some(a) = action {
            self.format_action(a);
        }
        if self.variant > 0 {
            ui.checkbox(&mut self.link_formats, "all formats").on_hover_text("Transform edits apply to every format, not just this one");
        }
    }

    fn format_action(&mut self, action: Action) {
        let seq = self.editor.seq;
        let result = match action {
            Action::Select(i) => {
                self.variant = i;
                Ok(())
            }
            Action::Add(preset) => self.editor.add_variant(preset).map(|i| self.variant = i),
            Action::AddCustom(size) => {
                let id = VariantId(self.editor.doc.alloc_id());
                let name = size.matching_preset().map_or_else(|| format!("Custom {}×{}", size.width, size.height), |p| p.name.to_string());
                let index = self.editor.sequence().variants.len();
                let variant = FormatVariant { id, name, size, overrides: Default::default() };
                self.editor.apply("Add format", vec![Op::InsertVariant { seq, index, variant, make_active: false }]).map(|_| self.variant = index)
            }
            Action::Rotate(variant) => {
                let Some(v) = self.editor.sequence().variant(variant).cloned() else { return };
                let size = v.size.rotated();
                let mut ops = vec![Op::SetVariantSize { seq, variant, size }];
                // A preset's name follows its new shape ("Landscape 16:9" → "Vertical 9:16").
                if let (Some(old), Some(new)) = (v.size.matching_preset(), size.matching_preset())
                    && v.name == old.name
                {
                    ops.push(Op::SetVariantName { seq, variant, name: new.name.into() });
                }
                self.editor.apply("Turn format sideways", ops)
            }
            Action::Resize(variant, size) => self.editor.apply("Format resolution", vec![Op::SetVariantSize { seq, variant, size }]),
            Action::Rename(variant, name) => self.editor.apply("Rename format", vec![Op::SetVariantName { seq, variant, name }]),
            Action::Remove(variant) => {
                let r = self.editor.apply("Remove format", vec![Op::RemoveVariant { seq, variant }]);
                self.variant = self.variant.min(self.editor.sequence().variants.len() - 1);
                r
            }
        };
        if let Err(e) = result {
            self.report_error(e.to_string());
        }
    }
}

/// `size` with its short edge set to `edge`, keeping its shape (sides rounded to even).
fn scale_to(size: CanvasSize, edge: u32) -> CanvasSize {
    if let Some(p) = size.matching_preset() {
        return p.size(edge);
    }
    let k = edge as f64 / size.short_edge().max(1) as f64;
    let even = |x: u32| (((x as f64 * k) / 2.0).round() as u32 * 2).max(2);
    CanvasSize::new(even(size.width), even(size.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_keep_the_shape() {
        assert_eq!(scale_to(CanvasSize::new(1920, 1080), 2160), CanvasSize::new(3840, 2160));
        assert_eq!(scale_to(CanvasSize::new(1080, 1920), 720), CanvasSize::new(720, 1280));
        // A custom shape scales too, staying even.
        assert_eq!(scale_to(CanvasSize::new(1000, 600), 1080), CanvasSize::new(1800, 1080));
    }
}
