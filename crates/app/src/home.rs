//! The start page: the projects you've been working on, and the plugins you have.
//!
//! It's what opens when you launch OpenAtelier with nothing to edit (pass files on the
//! command line or drop them on the window and it goes straight to the editor). "Home"
//! in the editor's top bar comes back here; nothing is closed in the meantime, so the
//! project you had is still there.

use crate::i18n::{tr, trf};
use crate::i18n::{args, t};
use crate::App;
use eframe::egui;
use std::path::Path;

/// Reads one of our own thumbnails: (size, pixels).
fn load_png(path: &Path) -> Option<([usize; 2], Vec<egui::Color32>)> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let pixels = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, a]| egui::Color32::from_rgba_unmultiplied(r, g, b, a))
            .collect(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .map(|&[r, g, b]| egui::Color32::from_rgb(r, g, b))
            .collect(),
        _ => return None,
    };
    Some(([info.width as usize, info.height as usize], pixels))
}

/// A project's picture, and a copy of it shrunk to a few pixels (drawn large, it's the
/// picture blurred).
pub type ProjectPicture = (egui::TextureHandle, egui::TextureHandle);

/// A picture shrunk to `across` pixels wide (its shape kept), each pixel the average of
/// the ones it covers.
fn shrink(size: [usize; 2], pixels: &[egui::Color32], across: usize) -> egui::ColorImage {
    let w = across.clamp(1, size[0].max(1));
    let h = (size[1] * w / size[0].max(1)).max(1);
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x * size[0] / w, ((x + 1) * size[0] / w).max(x * size[0] / w + 1));
            let (y0, y1) = (y * size[1] / h, ((y + 1) * size[1] / h).max(y * size[1] / h + 1));
            let mut sum = [0u32; 4];
            for yy in y0..y1.min(size[1]) {
                for xx in x0..x1.min(size[0]) {
                    let c = pixels[yy * size[0] + xx];
                    for (s, v) in sum.iter_mut().zip([c.r(), c.g(), c.b(), c.a()]) {
                        *s += v as u32;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)).max(1) as u32;
            out.push(egui::Color32::from_rgba_premultiplied((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8));
        }
    }
    egui::ColorImage { size: [w, h], pixels: out, source_size: egui::vec2(w as f32, h as f32) }
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
    /// It carries scripts (not built in): whether they're allowed.
    scripts: Option<bool>,
}

impl App {
    pub(crate) fn home(&mut self, ui: &mut egui::Ui) {
        // Behind it all: a ghost of a timeline playing (an interface effect).
        if self.settings.ui_effects {
            crate::backdrop::ghost_timeline(ui, ui.max_rect());
            // And a soft backing under the name and the tabs.
            let top = ui.max_rect();
            crate::backdrop::backing(ui, egui::Rect::from_center_size(egui::pos2(top.center().x, top.top() + 78.0), egui::vec2(480.0, 120.0)));
        }
        // Settings, in the top right corner (as in the editor: Ctrl+, too).
        {
            let page = ui.max_rect();
            let corner = egui::Rect::from_min_max(egui::pos2(page.right() - 180.0, page.top() + 12.0), egui::pos2(page.right() - 12.0, page.top() + 12.0 + crate::style::ICON));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(corner).layout(egui::Layout::right_to_left(egui::Align::Center)));
            if crate::icons::flat_button(&mut ui, crate::icons::SETTINGS, tr("Settings")).on_hover_text(tr("Settings (Ctrl+,)")).clicked() {
                self.settings_open = true;
            }
        }
        ui.add_space(22.0);
        // The mark and the name, side by side, centered.
        let title = egui::RichText::new(tr("OpenAtelier")).size(crate::style::TITLE * 1.5).strong();
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
        // The Projects page lays itself out in the whole window (its sidebar and top stay
        // put while the projects scroll).
        if self.home_tab == HomeTab::Projects {
            self.projects_page(ui);
            return;
        }
        // One centered column, at most 760 px wide. The inner ui keeps a top-down layout
        // of its own — inheriting the centering row's would lay everything out sideways.
        if self.settings.ui_effects {
            let area = ui.available_rect_before_wrap();
            let width = area.width().min(760.0);
            crate::backdrop::backing(ui, egui::Rect::from_center_size(egui::pos2(area.center().x, area.center().y), egui::vec2(width + 28.0, area.height())));
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            let left = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.add_space(left);
                ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(width);
                    self.home_plugins(ui);
                    ui.add_space(crate::style::GAP_XL);
                });
            });
        });
    }

    /// The picture saved beside a project, loaded once and kept. It's read and decoded on
    /// a thread of its own (a start page of big PNGs would otherwise hitch as it opens);
    /// `None` until it's in, or if there's none.
    /// With it, a copy shrunk to a few pixels across: drawn large (smoothly), it's the
    /// picture blurred, behind it where its shape leaves room.
    pub(crate) fn project_thumb(&mut self, ctx: &egui::Context, path: &Path) -> Option<(egui::TextureHandle, egui::TextureHandle)> {
        let path = path.to_path_buf();
        let key = path.to_string_lossy().to_string();
        let slot = self.project_thumbs.entry(key.clone()).or_insert_with(|| {
            let slot = std::sync::Arc::new(std::sync::OnceLock::new());
            let (fill, ctx) = (slot.clone(), ctx.clone());
            let spawned = std::thread::Builder::new().name("oa-project-picture".into()).spawn(move || {
                // egui takes textures from any thread; it uploads them with the next frame.
                let loaded = load_png(&path).map(|(size, pixels)| {
                    let small = shrink(size, &pixels, 12);
                    let image = egui::ColorImage { size, pixels, source_size: egui::vec2(size[0] as f32, size[1] as f32) };
                    let picture = ctx.load_texture(format!("project-{key}"), image, egui::TextureOptions::LINEAR);
                    let blurred = ctx.load_texture(format!("project-blur-{key}"), small, egui::TextureOptions::LINEAR);
                    (picture, blurred)
                });
                let _ = fill.set(loaded);
                ctx.request_repaint();
            });
            if spawned.is_err() {
                let _ = slot.set(None);
            }
            slot
        });
        slot.get().cloned().flatten()
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
                            (Some(_), _) => trf("Unsaved changes to {0}", &[("0", &(r.name()).to_string())]),
                            (None, true) => trf("Unsaved work from {0} (its file is gone)", &[("0", &(r.name()).to_string())]),
                            (None, false) => tr("Unsaved work from an untitled project").to_string(),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            ui.label(egui::RichText::new(trf("autosaved {0}", &[("0", &(r.age()).to_string())])).small().weak());
                        });
                        if let Some(p) = saved {
                            ui.label(egui::RichText::new(p.display().to_string()).small().monospace().weak());
                            ui.label(tr("Would you like to continue with the previous session, discard it, or move it to a new project and continue?"));
                        } else {
                            ui.label(tr("The app didn't close normally. Continue with it, or discard it?"));
                        }
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if saved.is_some() {
                                if crate::icons::text_button(ui, crate::icons::HISTORY, tr("Continue previous session"), true)
                                    .on_hover_text(tr("Open it as the project's unsaved changes; saving writes the project's file"))
                                    .clicked()
                                {
                                    self.recover(r.clone(), true);
                                }
                                if crate::icons::text_button(ui, crate::icons::ADD, tr("Move to new project"), false)
                                    .on_hover_text(tr("Continue with it as a new, untitled project; the saved file stays as it is"))
                                    .clicked()
                                {
                                    self.recover(r.clone(), false);
                                }
                            } else if crate::icons::text_button(ui, crate::icons::HISTORY, tr("Continue"), true).on_hover_text(tr("Open it as a new, untitled project")).clicked() {
                                self.recover(r.clone(), false);
                            }
                            if crate::icons::text_button(ui, crate::icons::DELETE, t("action.discard"), false).on_hover_text(tr("Delete the autosave")).clicked() {
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
                scripts: (!p.builtin && p.has_scripts()).then(|| crate::plugins::Plugins::scripts_allowed(p, &self.settings.trusted_scripts)),
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
                                if ui.add(egui::Button::new(egui::RichText::new(&card.name).size(crate::style::TEXT_L).strong()).frame(false)).on_hover_text(tr("Open it: its effects, with previews")).clicked() {
                                    self.plugin_previews.open = Some(card.id.clone());
                                }
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
                                // Opens the plugin's window: what it is, and its effects with previews.
                                if ui.add(egui::Button::new(egui::RichText::new(trf("{0} ▸", &[("0", &(card.summary).to_string())])).small()).frame(false)).on_hover_text(tr("Open it: its effects, with previews")).clicked() {
                                    self.plugin_previews.open = Some(card.id.clone());
                                }
                                let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                crate::icons::paint(ui.painter(), r, crate::icons::FOLDER, ui.visuals().weak_text_color());
                                let source = if card.builtin { t("plugins.built_in_source").to_string() } else { card.where_.clone() };
                                ui.label(egui::RichText::new(source).small().weak());
                            });
                            if !card.enabled {
                                ui.label(egui::RichText::new(t("plugins.off_note")).small().weak());
                            }
                            // Scripts: nothing of them runs until allowed (plugin_scripts.rs).
                            match card.scripts {
                                Some(false) => {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(egui::RichText::new(tr("⚠ Carries scripts — none run until you allow them")).small().color(crate::style::WARNING));
                                        if ui.small_button(tr("Review and allow…")).clicked() {
                                            self.ask_script_consent(&card.id);
                                        }
                                    });
                                }
                                Some(true) => {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(egui::RichText::new(tr("Scripts allowed")).small().weak());
                                        if ui.small_button(tr("Stop allowing")).on_hover_text(tr("Its scripts stop at once; it asks again next time")).clicked() {
                                            self.revoke_scripts(&card.id);
                                        }
                                    });
                                }
                                None => {}
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
        // A plugin opened: its window over the page.
        let ctx = ui.ctx().clone();
        self.plugin_window(&ctx);
    }
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
pub(crate) fn tile(ui: &mut egui::Ui, icon: crate::icons::Icon, title: &str, about: &str, primary: bool, width: f32) -> egui::Response {
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
