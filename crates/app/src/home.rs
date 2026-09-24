//! The start page: the projects you've been working on, and the plugins you have.
//!
//! It's what opens when you launch OpenAtelier with nothing to edit (pass files on the
//! command line or drop them on the window and it goes straight to the editor). "Home"
//! in the editor's top bar comes back here; nothing is closed in the meantime, so the
//! project you had is still there.

use crate::i18n::{args, t};
use crate::App;
use eframe::egui;
use std::path::Path;

/// Project cards are this wide; their picture is 16:9.
const CARD_W: f32 = 176.0;

/// Reads one of our own thumbnails: (size, pixels).
fn load_png(path: &Path) -> Option<([usize; 2], Vec<egui::Color32>)> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let pixels = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()]
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks_exact(3)
            .map(|p| egui::Color32::from_rgb(p[0], p[1], p[2]))
            .collect(),
        _ => return None,
    };
    Some(([info.width as usize, info.height as usize], pixels))
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Screen {
    Home,
    Editor,
}

#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum HomeTab {
    #[default]
    Projects,
    Plugins,
}

/// One plugin as the list shows it.
struct Card {
    id: String,
    name: String,
    version: String,
    author: String,
    description: String,
    summary: String,
    builtin: bool,
    /// The folder it came from (empty for the built-in one).
    where_: String,
    enabled: bool,
}

/// "3 min ago" for a file's last change.
fn when(path: &Path) -> String {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else { return t("home.missing").into() };
    let secs = modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let ago = |key: &str, n: u64| args(key, &[("n", &n.to_string())]);
    match secs {
        0..=59 => t("time.moments").into(),
        60..=3599 => ago("time.minutes", secs / 60),
        3600..=86_399 => ago("time.hours", secs / 3600),
        86_400..=604_799 => ago("time.days", secs / 86_400),
        _ => ago("time.weeks", secs / 604_800),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string())
}

impl App {
    pub(crate) fn home(&mut self, ui: &mut egui::Ui) {
        ui.add_space(22.0);
        // The mark and the name, side by side, centered.
        let title = egui::RichText::new("OpenAtelier").size(crate::style::TITLE * 1.5).strong();
        let title_width = ui.painter().layout_no_wrap("OpenAtelier".into(), egui::FontId::proportional(crate::style::TITLE * 1.5), egui::Color32::WHITE).size().x;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - title_width - 60.0) / 2.0).max(0.0));
            let (mark, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
            crate::logo::badge(ui.painter(), mark);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(title);
                ui.label(egui::RichText::new(t("app.tagline")).weak());
            });
        });
        ui.add_space(crate::style::GAP_XL);
        // The two tabs, as one segmented control.
        let plugins = self.plugins.list.len();
        let tabs = [
            (HomeTab::Projects, crate::icons::FOLDER, t("home.projects").to_string()),
            (HomeTab::Plugins, crate::icons::PLUGINS, format!("{}  {plugins}", t("home.plugins"))),
        ];
        let widths: f32 = tabs.iter().map(|(_, _, s)| tab_width(ui, s)).sum::<f32>() + 8.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - widths) / 2.0).max(0.0));
            egui::Frame::NONE.fill(ui.visuals().extreme_bg_color).corner_radius(10.0).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (tab, icon, text) in tabs {
                    if tab_button(ui, icon, &text, self.home_tab == tab).clicked() {
                        self.home_tab = tab;
                    }
                }
            });
        });
        ui.add_space(crate::style::GAP_L);
        // One centered column, at most 760 px wide. The inner ui keeps a top-down layout
        // of its own — inheriting the centering row's would lay everything out sideways.
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            let left = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.add_space(left);
                ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(width);
                    match self.home_tab {
                        HomeTab::Projects => self.home_projects(ui),
                        HomeTab::Plugins => self.home_plugins(ui),
                    }
                    ui.add_space(crate::style::GAP_XL);
                });
            });
        });
    }

    fn home_projects(&mut self, ui: &mut egui::Ui) {
        // Three ways in, as tiles.
        let gap = crate::style::GAP;
        let tile_w = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if tile(ui, crate::icons::ADD, t("home.new_project"), t("home.new_project_hint"), true, tile_w).clicked() {
                self.new_project();
                self.screen = Screen::Editor;
            }
            if tile(ui, crate::icons::FOLDER_OPEN, t("home.open"), t("home.open_hint"), false, tile_w).clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("OpenAtelier project", &["json"]).pick_file()
            {
                self.open_project(&path);
                self.screen = Screen::Editor;
            }
            if tile(ui, crate::icons::IMPORT, t("home.import"), t("home.import_hint"), false, tile_w).clicked()
                && let Some(files) = rfd::FileDialog::new().add_filter("Media", crate::MEDIA_EXTENSIONS).pick_files()
            {
                self.open_paths(&files);
                self.screen = Screen::Editor;
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, crate::icons::INFO, ui.visuals().weak_text_color());
            ui.label(egui::RichText::new(t("home.drop_hint")).small().weak());
        });

        // Sessions that ended without saving.
        if !self.recoveries.is_empty() {
            ui.add_space(crate::style::GAP_L);
            section(ui, crate::icons::WARNING, t("home.unsaved"), None);
            for r in self.recoveries.clone() {
                self.recovery_card(ui, &r);
                ui.add_space(4.0);
            }
        }

        ui.add_space(crate::style::GAP_L);
        let count = self.settings.recent.len();
        section(ui, crate::icons::HISTORY, t("home.recent"), (count > 0).then(|| count.to_string()));
        if self.settings.recent.is_empty() {
            ui.label(egui::RichText::new(t("home.recent_empty")).weak());
        }
        // Cards: a frame from the project, what it's called, where it is and when it
        // was last touched. Far easier to pick out than a list of file names.
        let recent = self.settings.recent.clone();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(crate::style::GAP, crate::style::GAP);
            for r in recent {
                let exists = r.path.exists();
                let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_W * 0.5625 + 44.0), egui::Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, CARD_W * 0.5625));
                let painter = ui.painter_at(rect);
                let visuals = ui.visuals().clone();
                painter.rect_filled(thumb, crate::style::ROUNDING, visuals.extreme_bg_color);
                match self.project_thumb(ui.ctx(), &r) {
                    Some(texture) => {
                        painter.image(
                            texture.id(),
                            thumb,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    None => {
                        let icon = if exists { crate::icons::VIDEO_TRACK } else { crate::icons::WARNING };
                        let color = if exists { visuals.weak_text_color() } else { crate::style::ERROR };
                        crate::icons::paint(&painter, egui::Rect::from_center_size(thumb.center(), egui::vec2(34.0, 34.0)), icon, color);
                    }
                }
                let border = if !exists {
                    egui::Stroke::new(1.5, crate::style::ERROR)
                } else if response.hovered() {
                    egui::Stroke::new(1.5, crate::style::ACCENT)
                } else {
                    egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
                };
                painter.rect_stroke(thumb, crate::style::ROUNDING, border, egui::StrokeKind::Inside);
                // Flagged when an earlier session left unsaved changes to it (above).
                let unsaved = self.recoveries.iter().any(|x| x.original.as_ref() == Some(&r.path));
                if unsaved {
                    let tag = egui::Rect::from_min_size(thumb.right_top() + egui::vec2(-30.0, 6.0), egui::vec2(24.0, 24.0));
                    painter.rect_filled(tag, 6.0, crate::style::GOLD);
                    crate::icons::paint(&painter, tag.shrink(4.0), crate::icons::WARNING, egui::Color32::BLACK);
                    painter.rect_stroke(thumb, crate::style::ROUNDING, egui::Stroke::new(1.5, crate::style::GOLD), egui::StrokeKind::Inside);
                }

                let name = if r.name.is_empty() { name_of(&r.path) } else { r.name.clone() };
                let mut job = egui::text::LayoutJob::simple_singleline(
                    name,
                    egui::FontId::proportional(crate::style::TEXT),
                    if exists { visuals.text_color() } else { crate::style::ERROR },
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
                let galley = painter.layout_job(job);
                painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 4.0), galley, visuals.text_color());
                let when_text = if exists { when(&r.path) } else { t("home.missing").to_string() };
                painter.text(
                    egui::pos2(rect.left() + 2.0, thumb.bottom() + 6.0 + crate::style::TEXT + 2.0),
                    egui::Align2::LEFT_TOP,
                    when_text,
                    egui::FontId::proportional(crate::style::TEXT_S),
                    visuals.weak_text_color(),
                );
                let tip = if unsaved { format!("{}
Has unsaved changes from an earlier session — see Unsaved work above", r.path.display()) } else { r.path.display().to_string() };
                let response = response.on_hover_text(tip);
                if response.clicked() && exists {
                    self.open_project(&r.path);
                    self.screen = Screen::Editor;
                }
                if response.hovered() && exists {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                crate::widgets::context_menu(&response, |ui| {
                    if crate::icons::menu_item(ui, Some(crate::icons::FOLDER_OPEN), t("home.open"), "", exists).clicked() {
                        self.open_project(&r.path);
                        self.screen = Screen::Editor;
                        ui.close();
                    }
                    if crate::icons::menu_item(ui, Some(crate::icons::DELETE), t("home.remove_from_list"), "", true).clicked() {
                        self.settings.forget_project(&r.path);
                        ui.close();
                    }
                });
            }
        });

        // The language the app is shown in. Changing it takes effect next launch (the
        // strings are picked once, at startup).
        ui.add_space(crate::style::GAP_XL);
        ui.separator();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, crate::icons::LANGUAGE, ui.visuals().weak_text_color());
            ui.label(egui::RichText::new(t("home.language")).weak());
            let dir = crate::settings::config_dir().join("locales");
            let langs = crate::i18n::available(&dir);
            let mut picked = self.settings.language.clone().unwrap_or_else(|| crate::i18n::language().to_string());
            let was = picked.clone();
            egui::ComboBox::from_id_salt("home-language").selected_text(&picked).show_ui(ui, |ui| {
                for lang in &langs {
                    ui.selectable_value(&mut picked, lang.clone(), lang);
                }
            });
            if picked != was {
                self.settings.set_language(Some(picked));
                self.notify(t("home.language_hint"));
            }
        })
        .response
        .on_hover_text(t("home.language_hint"));
    }

    /// The picture saved beside a project, loaded once and kept.
    fn project_thumb(&mut self, ctx: &egui::Context, r: &crate::settings::RecentProject) -> Option<egui::TextureHandle> {
        let path = r.thumb.clone().unwrap_or_else(|| crate::thumbnail::path_for(&r.path));
        let key = path.to_string_lossy().to_string();
        if let Some(found) = self.project_thumbs.get(&key) {
            return found.clone();
        }
        let loaded = load_png(&path).map(|(size, pixels)| {
            let image = egui::ColorImage { size, pixels, source_size: egui::vec2(size[0] as f32, size[1] as f32) };
            ctx.load_texture(format!("project-{key}"), image, egui::TextureOptions::LINEAR)
        });
        self.project_thumbs.insert(key, loaded.clone());
        loaded
    }

    /// Work an earlier session autosaved but never saved, with what can be done with it.
    /// When it belongs to a project file that's still there, it's flagged: carry on with
    /// it as that project's changes, throw it away, or carry on with it as a new project
    /// (leaving the saved file as it was).
    pub(crate) fn recovery_card(&mut self, ui: &mut egui::Ui, r: &crate::autosave::Recovery) {
        let saved = r.original.as_ref().filter(|p| p.exists());
        let gone = r.original.is_some() && saved.is_none();
        let flag = crate::style::GOLD;
        egui::Frame::NONE
            .fill(flag.gamma_multiply(0.08))
            .stroke(egui::Stroke::new(1.0, flag.gamma_multiply(0.45)))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    crate::widgets::icon_badge(ui, if saved.is_some() { crate::icons::WARNING } else { crate::icons::HISTORY }, flag, 28.0);
                    ui.vertical(|ui| {
                        let title = match (saved, gone) {
                            (Some(_), _) => format!("Unsaved changes to {}", r.name()),
                            (None, true) => format!("Unsaved work from {} (its file is gone)", r.name()),
                            (None, false) => "Unsaved work from an untitled project".to_string(),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            ui.label(egui::RichText::new(format!("autosaved {}", r.age())).small().weak());
                        });
                        if let Some(p) = saved {
                            ui.label(egui::RichText::new(p.display().to_string()).small().monospace().weak());
                            ui.label("Would you like to continue with the previous session, discard it, or move it to a new project and continue?");
                        } else {
                            ui.label("The app didn't close normally. Continue with it, or discard it?");
                        }
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if saved.is_some() {
                                if crate::icons::text_button(ui, crate::icons::HISTORY, "Continue previous session", true)
                                    .on_hover_text("Open it as the project's unsaved changes; saving writes the project's file")
                                    .clicked()
                                {
                                    self.recover(r.clone(), true);
                                }
                                if crate::icons::text_button(ui, crate::icons::ADD, "Move to new project", false)
                                    .on_hover_text("Continue with it as a new, untitled project; the saved file stays as it is")
                                    .clicked()
                                {
                                    self.recover(r.clone(), false);
                                }
                            } else if crate::icons::text_button(ui, crate::icons::HISTORY, "Continue", true).on_hover_text("Open it as a new, untitled project").clicked() {
                                self.recover(r.clone(), false);
                            }
                            if crate::icons::text_button(ui, crate::icons::DELETE, t("action.discard"), false).on_hover_text("Delete the autosave").clicked() {
                                self.discard_recovery(r);
                            }
                        });
                    });
                });
            });
    }

    fn home_plugins(&mut self, ui: &mut egui::Ui) {
        let on = self.plugins.list.iter().filter(|p| self.plugins.is_enabled(&p.id)).count();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(t("home.plugins")).size(crate::style::TITLE).strong());
                ui.label(egui::RichText::new(args("plugins.count", &[("n", &self.plugins.list.len().to_string()), ("on", &on.to_string())])).weak());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::text_button(ui, crate::icons::FOLDER_OPEN, t("plugins.open_folder"), false).clicked() {
                    let dir = self.plugins.dir.clone();
                    let _ = std::fs::create_dir_all(&dir);
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    #[cfg(not(windows))]
                    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                }
                if crate::icons::text_button(ui, crate::icons::REFRESH, t("plugins.reload"), false).on_hover_text(t("plugins.reload_hint")).clicked() {
                    self.reload_plugins();
                }
            });
        });
        ui.add_space(crate::style::GAP);

        // What a plugin is, and where they're read from (so two plugins showing with an
        // empty plugins folder makes sense).
        egui::Frame::NONE.fill(ui.visuals().faint_bg_color).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                crate::widgets::icon_badge(ui, crate::icons::INFO, crate::style::ACCENT, 28.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(t("plugins.what_is")).small());
                    ui.add_space(4.0);
                    let folder = |ui: &mut egui::Ui, what: &str, path: &Path| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(what).small().weak());
                            ui.label(egui::RichText::new(path.display().to_string()).small().monospace());
                        });
                    };
                    folder(ui, t("plugins.your_folder"), &self.plugins.dir);
                    for extra in &self.plugins.extra_dirs {
                        folder(ui, t("plugins.also_scanning"), extra);
                    }
                });
            });
        });

        let issues = self.plugins.issues();
        if !issues.is_empty() {
            ui.add_space(crate::style::GAP);
            egui::Frame::NONE
                .fill(crate::style::ERROR.gamma_multiply(0.08))
                .stroke(egui::Stroke::new(1.0, crate::style::ERROR.gamma_multiply(0.45)))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        crate::widgets::icon_badge(ui, crate::icons::WARNING, crate::style::ERROR, 28.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(t("plugins.problems")).strong().color(crate::style::ERROR));
                            for i in &issues {
                                ui.label(egui::RichText::new(i).small());
                            }
                        });
                    });
                });
        }
        ui.add_space(crate::style::GAP_L);

        // Gathered first: drawing a card can toggle a plugin, which reloads the list.
        let cards: Vec<Card> = self
            .plugins
            .list
            .iter()
            .map(|p| Card {
                id: p.id.clone(),
                name: p.name.clone(),
                version: p.version.clone(),
                author: p.author.clone(),
                description: p.description.clone(),
                summary: p.summary(),
                builtin: p.builtin,
                where_: p.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default(),
                enabled: self.plugins.is_enabled(&p.id),
            })
            .collect();
        for card in cards {
            let stroke = if card.enabled { crate::style::ACCENT.gamma_multiply(0.35) } else { ui.visuals().widgets.noninteractive.bg_stroke.color };
            egui::Frame::NONE
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(1.0, stroke))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        let color = if card.enabled { crate::style::ACCENT } else { ui.visuals().weak_text_color() };
                        crate::widgets::icon_badge(ui, crate::icons::PLUGINS, color, 40.0);
                        ui.add_space(4.0);
                        // The switch on the right; the words take the rest.
                        let text_width = ui.available_width() - 52.0;
                        ui.allocate_ui_with_layout(egui::vec2(text_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(&card.name).size(crate::style::TEXT_L).strong());
                                crate::widgets::pill(ui, &format!("v{}", card.version), ui.visuals().weak_text_color());
                                if card.builtin {
                                    crate::widgets::pill(ui, t("plugins.builtin"), crate::style::ACCENT);
                                }
                                if !card.enabled {
                                    crate::widgets::pill(ui, t("plugins.off"), crate::style::WARNING);
                                }
                            });
                            if !card.author.is_empty() {
                                ui.label(egui::RichText::new(args("plugins.by", &[("author", &card.author)])).small().weak());
                            }
                            if !card.description.is_empty() {
                                ui.add_space(2.0);
                                ui.label(&card.description);
                            }
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                crate::icons::paint(ui.painter(), r, crate::icons::EFFECTS, ui.visuals().weak_text_color());
                                ui.label(egui::RichText::new(&card.summary).small().weak());
                                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                crate::icons::paint(ui.painter(), r, crate::icons::FOLDER, ui.visuals().weak_text_color());
                                let source = if card.builtin { t("plugins.built_in_source").to_string() } else { card.where_.clone() };
                                ui.label(egui::RichText::new(source).small().weak());
                            });
                            if !card.enabled {
                                ui.label(egui::RichText::new(t("plugins.off_note")).small().weak());
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            let mut on = card.enabled;
                            if crate::widgets::toggle(ui, &mut on).on_hover_text(if card.enabled { t("plugins.turn_off") } else { t("plugins.turn_on") }).changed() {
                                self.set_plugin_enabled(&card.id, on);
                            }
                        });
                    });
                });
            ui.add_space(crate::style::GAP);
        }
    }
}

/// A section's heading: its icon, its name and (when there's one) how many.
fn section(ui: &mut egui::Ui, icon: crate::icons::Icon, title: &str, count: Option<String>) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        crate::icons::paint(ui.painter(), r, icon, ui.visuals().text_color());
        ui.label(egui::RichText::new(title).size(crate::style::TEXT_L).strong());
        if let Some(n) = count {
            crate::widgets::pill(ui, &n, ui.visuals().weak_text_color());
        }
    });
    ui.add_space(4.0);
}

/// How wide a tab of the segmented control is.
fn tab_width(ui: &egui::Ui, text: &str) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(crate::style::TEXT_L), egui::Color32::WHITE).size().x + 18.0 + 6.0 + 32.0
}

/// One tab of the segmented control: filled with the accent when it's the one shown.
fn tab_button(ui: &mut egui::Ui, icon: crate::icons::Icon, text: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(tab_width(ui, text), 34.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if selected {
            egui::Color32::WHITE
        } else if response.hovered() {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        if selected {
            ui.painter().rect_filled(rect, 7.0, crate::style::ACCENT);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 7.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(crate::style::TEXT_L), color);
        let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 16.0 + 9.0, rect.center().y), egui::vec2(18.0, 18.0));
        crate::icons::paint(ui.painter(), icon_rect, icon, color);
        ui.painter().galley(egui::pos2(icon_rect.right() + 6.0, rect.center().y - galley.size().y / 2.0), galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A big button on the start page: an icon, what it does, and a line more about it.
fn tile(ui: &mut egui::Ui, icon: crate::icons::Icon, title: &str, about: &str, primary: bool, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 76.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let hovered = response.hovered();
        let (fill, stroke) = if primary {
            (if hovered { crate::style::ACCENT.gamma_multiply(1.12) } else { crate::style::ACCENT }, egui::Stroke::NONE)
        } else {
            let edge = if hovered { crate::style::ACCENT } else { v.widgets.noninteractive.bg_stroke.color };
            (if hovered { v.widgets.hovered.weak_bg_fill } else { v.faint_bg_color }, egui::Stroke::new(1.0, edge))
        };
        let painter = ui.painter();
        painter.rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
        let fg = if primary { egui::Color32::WHITE } else { v.text_color() };
        let weak = if primary { egui::Color32::from_white_alpha(200) } else { v.weak_text_color() };
        // The icon in a round spot on the left.
        let spot = egui::Rect::from_center_size(egui::pos2(rect.left() + 34.0, rect.center().y), egui::vec2(40.0, 40.0));
        let spot_fill = if primary { egui::Color32::from_white_alpha(40) } else { crate::style::ACCENT.gamma_multiply(0.16) };
        painter.circle_filled(spot.center(), 20.0, spot_fill);
        crate::icons::paint(painter, spot.shrink(9.0), icon, if primary { egui::Color32::WHITE } else { crate::style::ACCENT });
        let x = spot.right() + 12.0;
        let wrap = rect.right() - x - 10.0;
        let mut title_job = egui::text::LayoutJob::simple_singleline(title.to_string(), egui::FontId::proportional(crate::style::TEXT_L), fg);
        title_job.wrap = egui::text::TextWrapping::truncate_at_width(wrap);
        let title = painter.layout_job(title_job);
        let about = painter.layout(about.to_string(), egui::FontId::proportional(crate::style::TEXT_S), weak, wrap);
        let top = rect.center().y - (title.size().y + 3.0 + about.size().y) / 2.0;
        let title_h = title.size().y;
        painter.galley(egui::pos2(x, top), title, fg);
        painter.galley(egui::pos2(x, top + title_h + 3.0), about, weak);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
