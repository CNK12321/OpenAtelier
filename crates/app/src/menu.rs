//! The bar across the top, and the strip of verbs above the timeline.
//!
//! The top bar holds what's true of the whole session: the app menu, which project is
//! open and whether it's saved, the format being edited, and the way into everything
//! else (the palette). Editing actions aren't here — they're either on the clip, in the
//! action bar, or a keystroke away.
//!
//! The action bar is the CapCut idea: the handful of verbs that make sense for whatever
//! is selected, right above the timeline where you're already looking. It's drawn from
//! the same command list as the palette, so it can't drift out of step with it.


use crate::command::Group;
use crate::icons;
use crate::style;
use crate::App;
use eframe::egui;

impl App {
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(style::GAP_S);
            let mut run: Option<&'static str> = None;
            let (mark, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
            crate::logo::badge(ui.painter(), mark);
            ui.menu_button("OpenAtelier ▾", |ui| {
                ui.set_min_width(240.0);
                let commands = self.commands();
                let mut entry = |ui: &mut egui::Ui, id: &str| {
                    let Some(c) = commands.iter().find(|c| c.id == id) else { return };
                    if icons::menu_item(ui, c.icon, &c.title, c.shortcut, c.enabled).clicked() {
                        run = Some(c.id);
                        ui.close();
                    }
                };
                entry(ui, "project.new");
                entry(ui, "project.open");
                // egui's own submenu (it opens on hover), with room left for the icon.
                let recent = self.settings.recent.clone();
                let sub = ui.menu_button("       Recent", |ui| {
                    ui.set_min_width(220.0);
                    if recent.is_empty() {
                        ui.label(egui::RichText::new("Nothing yet").weak());
                    }
                    for p in recent {
                        if icons::menu_item(ui, Some(icons::VIDEO_TRACK), &p.name, "", p.path.exists()).on_hover_text(p.path.display().to_string()).clicked() {
                            self.open_project(&p.path);
                            ui.close();
                        }
                    }
                });
                let at = egui::Rect::from_center_size(egui::pos2(sub.response.rect.left() + 12.0, sub.response.rect.center().y), egui::vec2(16.0, 16.0));
                icons::paint(ui.painter(), at, icons::HISTORY, ui.visuals().widgets.inactive.fg_stroke.color);
                ui.separator();
                entry(ui, "project.save");
                entry(ui, "project.save_as");
                entry(ui, "project.import");
                entry(ui, "project.export");
                ui.separator();
                if icons::menu_item(ui, Some(icons::SETTINGS), "Settings…", "Ctrl+,", true).clicked() {
                    self.settings_open = true;
                    ui.close();
                }
                entry(ui, "project.home");
            });
            // The rest of the commands, grouped the way the palette used to group them,
            // so everything the editor can do is reachable from the bar.
            for group in [Group::Edit, Group::Clip, Group::Timeline, Group::View] {
                ui.menu_button(group.title(), |ui| {
                    ui.set_min_width(240.0);
                    for c in self.commands().iter().filter(|c| c.group == group) {
                        if icons::menu_item(ui, c.icon, &c.title, c.shortcut, c.enabled).clicked() {
                            run = Some(c.id);
                            ui.close();
                        }
                    }
                });
            }
            if let Some(id) = run {
                self.run_command(id);
            }

            ui.separator();
            // Undo and redo, naming what they'd undo.
            let undo = self.editor.doc.undo_label().map(str::to_string);
            let redo = self.editor.doc.redo_label().map(str::to_string);
            let tip = |label: &Option<String>, verb: &str| match label {
                Some(l) => format!("{verb} {l}"),
                None => format!("Nothing to {}", verb.to_lowercase()),
            };
            if icons::button(ui, icons::UNDO, &tip(&undo, "Undo"), "Ctrl+Z", undo.is_some()).clicked() {
                self.undo();
            }
            if icons::button(ui, icons::REDO, &tip(&redo, "Redo"), "Ctrl+Shift+Z", redo.is_some()).clicked() {
                self.redo();
            }

            ui.separator();
            // What's open, and whether it's saved.
            let dirty = self.editor.dirty();
            let name = self
                .editor
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into());
            ui.label(egui::RichText::new(name).size(style::TEXT));
            if dirty {
                ui.label(egui::RichText::new("●").color(style::WARNING)).on_hover_text("Unsaved changes (autosaved)");
            }

            ui.separator();
            self.format_picker(ui);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(style::GAP_S);
                if icons::button(ui, icons::HOME, "Home: projects and plugins", "", true).clicked() {
                    self.screen = crate::home::Screen::Home;
                }
                match &self.export {
                    Some(exporter) => {
                        let (done, total) = exporter.progress();
                        let queued = self.export_queue.len();
                        if ui.button(if queued > 0 { "Cancel exports" } else { "Cancel export" }).clicked() {
                            self.cancel_exports();
                        }
                        let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                        let text = if queued > 0 { format!("{:.0}% · {queued} more", fraction * 100.0) } else { format!("{:.0}%", fraction * 100.0) };
                        let bar = ui.add(egui::ProgressBar::new(fraction).desired_width(140.0).text(text)).interact(egui::Sense::click());
                        if bar.on_hover_text("Show the export").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            self.export_view.hidden = false;
                        }
                    }
                    None => {
                        if icons::text_button(ui, icons::EXPORT, "Export…", true).on_hover_text("Render the video to a file").clicked() {
                            self.start_export();
                        }
                    }
                }
            });
        });
    }

    /// The verbs for what's selected: split, duplicate, group, extract audio… Drawn
    /// from the command list, so it stays in step with the palette and the menus.
    pub(crate) fn action_bar(&mut self, ui: &mut egui::Ui) {
        let actions: Vec<(&'static str, icons::Icon, String, &'static str, bool)> = self
            .commands()
            .iter()
            .filter_map(|c| c.icon.map(|icon| (c.id, icon, c.title.clone(), c.shortcut, c.enabled)).filter(|_| c.contextual))
            .collect();
        let n = self.selected.len();
        ui.horizontal(|ui| {
            ui.add_space(style::GAP_S);
            if n == 0 {
                ui.label(egui::RichText::new("Select a clip to edit it — or drop media anywhere").small().weak());
                return;
            }
            ui.label(egui::RichText::new(if n == 1 { "1 clip".into() } else { format!("{n} clips") }).small().weak());
            ui.separator();
            let mut run = None;
            for (id, icon, title, shortcut, enabled) in &actions {
                if icons::button(ui, *icon, title, shortcut, *enabled).clicked() {
                    run = Some(*id);
                }
            }
            if let Some(id) = run {
                self.run_command(id);
            }
        });
    }

}
