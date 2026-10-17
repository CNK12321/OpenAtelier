//! The bar across the top, and the strip of verbs above the timeline.
//!
//! The top bar holds what's true of the whole session, in three parts: on the left the
//! menus (the way into everything; they fold into one when the window is narrow) and
//! undo; in the middle what's open — its name (click to rename), whether it's saved and
//! the format being edited; on the right Export and Home. Editing actions aren't here —
//! they're either on the clip, in the action bar, or a keystroke away.
//!
//! The action bar is the CapCut idea: the handful of verbs that make sense for whatever
//! is selected, right above the timeline where you're already looking. It's drawn from
//! the same command list as the palette, so it can't drift out of step with it.

use crate::i18n::{tr, trf};
use crate::command::Group;
use crate::icons;
use crate::style;
use crate::App;
use eframe::egui;

/// The top bar's height.
pub const BAR_HEIGHT: f32 = 30.0;

/// How wide each part of the top bar was last frame: what decides whether the menus fold
/// (the left part as it is with them unfolded) and where the middle goes.
#[derive(Clone, Copy, Default)]
struct BarWidths {
    left_unfolded: f32,
    middle: f32,
    right: f32,
}

/// What the save state shows.
#[derive(Clone, Copy, Default)]
struct SaveSeen {
    dirty: bool,
    /// When it last went from unsaved to saved (egui's clock, seconds).
    saved_at: Option<f64>,
}

/// A menu of the top bar: its name with nothing behind it until hovered or open (the
/// menus are a way in, not something to look at), after the logo or an icon if given,
/// with a chevron if `chevron`. While one of them is open, pointing at another opens it
/// instead, as in any menu bar.
fn bar_menu<R>(ui: &mut egui::Ui, logo: bool, icon: Option<icons::Icon>, text: &str, chevron: bool, add: impl FnOnce(&mut egui::Ui) -> R) {
    let font = egui::FontId::proportional(style::TEXT);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, egui::Color32::PLACEHOLDER);
    let lead = if logo || icon.is_some() { 18.0 + if text.is_empty() { 0.0 } else { 6.0 } } else { 0.0 };
    let tail = if chevron { 16.0 } else { 0.0 };
    let size = egui::vec2(8.0 + lead + galley.size().x + tail + 8.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let popup = egui::Popup::default_response_id(&response);
    let last_key = egui::Id::new("top-bar-open-menu");
    let last: Option<egui::Id> = ui.data(|d| d.get_temp(last_key));
    let ctx = ui.ctx().clone();
    // Another of the bar's menus is open, and the pointer moved onto this one: this one.
    if response.hovered() && last.is_some_and(|l| l != popup && egui::Popup::is_id_open(&ctx, l)) && !egui::Popup::is_id_open(&ctx, popup) {
        egui::Popup::open_id(&ctx, popup);
    }
    let open = egui::Popup::is_id_open(&ctx, popup);
    if open || response.clicked() {
        ui.data_mut(|d| d.insert_temp(last_key, popup));
    }
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        if open {
            ui.painter().rect_filled(rect, style::ROUNDING, visuals.widgets.open.weak_bg_fill);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, style::ROUNDING, visuals.widgets.hovered.weak_bg_fill);
        }
        let color = if open || response.hovered() { visuals.strong_text_color() } else { visuals.text_color() };
        let mut x = rect.left() + 8.0;
        let lead_rect = egui::Rect::from_center_size(egui::pos2(x + 9.0, rect.center().y), egui::vec2(18.0, 18.0));
        if logo {
            crate::logo::badge(ui.painter(), lead_rect);
        } else if let Some(icon) = icon {
            icons::paint(ui.painter(), lead_rect, icon, color);
        }
        x += lead;
        ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley.clone(), color);
        if chevron {
            let at = egui::Rect::from_center_size(egui::pos2(x + galley.size().x + 9.0, rect.center().y), egui::vec2(15.0, 15.0));
            icons::paint(ui.painter(), at, icons::EXPAND, visuals.weak_text_color());
        }
    }
    egui::Popup::menu(&response).info(egui::UiStackInfo::new(egui::UiKind::Menu)).show(|ui| {
        ui.set_min_width(240.0);
        add(ui)
    });
}

impl App {
    /// The top bar: the menus and undo on the left, what's open in the middle (centered
    /// on the bar when there's room), Export and Home on the right. The menus fold into
    /// one when the window is too narrow for everything.
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let bar = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(ui.available_width(), BAR_HEIGHT));
        let widths_key = egui::Id::new("top-bar-widths");
        let mut widths: BarWidths = ui.data(|d| d.get_temp(widths_key)).unwrap_or_default();
        let folded = bar.width() < widths.left_unfolded + widths.middle + widths.right + 2.0 * style::GAP_XL;
        let row = |rect: egui::Rect, layout: egui::Layout| egui::UiBuilder::new().max_rect(rect).layout(layout);

        let mut right = ui.new_child(row(bar, egui::Layout::right_to_left(egui::Align::Center)));
        self.bar_right(&mut right);
        widths.right = right.min_rect().width();
        let right_edge = right.min_rect().left();

        let mut left = ui.new_child(row(bar, egui::Layout::left_to_right(egui::Align::Center)));
        self.bar_left(&mut left, folded);
        if !folded {
            widths.left_unfolded = left.min_rect().width();
        }
        let left_edge = left.min_rect().right();

        // The middle: centered on the bar if it fits between the two sides, else in the
        // room between them.
        let (lo, hi) = (left_edge + style::GAP_XL, right_edge - style::GAP_XL);
        let x = (bar.center().x - widths.middle / 2.0).clamp(lo, (hi - widths.middle).max(lo));
        let room = egui::Rect::from_min_max(egui::pos2(x, bar.top()), egui::pos2(hi.max(x + 1.0), bar.bottom()));
        let mut middle = ui.new_child(row(room, egui::Layout::left_to_right(egui::Align::Center)));
        middle.shrink_clip_rect(room);
        self.bar_middle(&mut middle);
        widths.middle = middle.min_rect().width();

        ui.data_mut(|d| d.insert_temp(widths_key, widths));
        ui.advance_cursor_after_rect(bar);
    }

    /// The left of the top bar: the app menu, the other menus (or, folded, one menu with
    /// them all), undo and redo.
    fn bar_left(&mut self, ui: &mut egui::Ui, folded: bool) {
        let mut run: Option<&'static str> = None;
        bar_menu(ui, true, None, "OpenAtelier", true, |ui| self.app_menu(ui, &mut run));
        let commands = self.commands();
        let group_menu = |ui: &mut egui::Ui, group: Group, run: &mut Option<&'static str>| {
            for c in commands.iter().filter(|c| c.group == group) {
                if icons::menu_item(ui, c.icon, &c.title, c.shortcut, c.enabled).clicked() {
                    *run = Some(c.id);
                    ui.close();
                }
            }
        };
        let groups = [Group::Edit, Group::Clip, Group::Timeline, Group::View];
        if folded {
            bar_menu(ui, false, Some(icons::MENU), "", false, |ui| {
                for group in groups {
                    ui.menu_button(group.title(), |ui| {
                        ui.set_min_width(240.0);
                        group_menu(ui, group, &mut run);
                    });
                }
                ui.menu_button(tr("Plugins"), |ui| self.plugins_menu(ui));
            });
        } else {
            for group in groups {
                bar_menu(ui, false, None, group.title(), false, |ui| group_menu(ui, group, &mut run));
            }
            bar_menu(ui, false, None, tr("Plugins"), false, |ui| self.plugins_menu(ui));
        }
        if let Some(id) = run {
            self.run_command(id);
        }

        ui.add_space(style::GAP);
        // Undo and redo, naming what they'd undo.
        let undo = self.editor.doc.undo_label().map(str::to_string);
        let redo = self.editor.doc.redo_label().map(str::to_string);
        // Whole phrases (word order differs between languages), the step's name in the
        // language in force.
        let tip = |label: &Option<String>, undo: bool| match (label, undo) {
            (Some(l), true) => trf("Undo {what}", &[("what", &crate::i18n::tx(l))]),
            (Some(l), false) => trf("Redo {what}", &[("what", &crate::i18n::tx(l))]),
            (None, true) => tr("Nothing to undo").to_string(),
            (None, false) => tr("Nothing to redo").to_string(),
        };
        if icons::button(ui, icons::UNDO, &tip(&undo, true), "Ctrl+Z", undo.is_some()).clicked() {
            self.undo();
        }
        if icons::button(ui, icons::REDO, &tip(&redo, false), "Ctrl+Shift+Z", redo.is_some()).clicked() {
            self.redo();
        }
    }

    /// The OpenAtelier menu: projects, import and export, settings.
    fn app_menu(&mut self, ui: &mut egui::Ui, run: &mut Option<&'static str>) {
        let commands = self.commands();
        let mut entry = |ui: &mut egui::Ui, id: &str| {
            let Some(c) = commands.iter().find(|c| c.id == id) else { return };
            if icons::menu_item(ui, c.icon, &c.title, c.shortcut, c.enabled).clicked() {
                *run = Some(c.id);
                ui.close();
            }
        };
        entry(ui, "project.new");
        entry(ui, "project.open");
        // egui's own submenu (it opens on hover), its icon in the Material font.
        let recent = self.settings.recent.clone();
        let icon = icons::rich(icons::HISTORY, 16.0).color(ui.visuals().widgets.inactive.fg_stroke.color);
        ui.menu_button((icon, tr("Recent")), |ui| {
            ui.set_min_width(220.0);
            if recent.is_empty() {
                ui.label(egui::RichText::new(tr("Nothing yet")).weak());
            }
            for p in recent {
                if icons::menu_item(ui, Some(icons::VIDEO_TRACK), &p.name, "", p.path.exists()).on_hover_text(p.path.display().to_string()).clicked() {
                    self.open_project(&p.path);
                    ui.close();
                }
            }
        });
        ui.separator();
        entry(ui, "project.save");
        entry(ui, "project.save_as");
        entry(ui, "project.import");
        entry(ui, "project.export");
        entry(ui, "project.export_otio");
        ui.separator();
        if icons::menu_item(ui, Some(icons::SETTINGS), tr("Settings…"), "Ctrl+,", true).clicked() {
            self.settings_open = true;
            ui.close();
        }
        entry(ui, "project.home");
    }

    /// The middle of the top bar: the project's name (click to rename it), whether it's
    /// saved, and the format being edited.
    fn bar_middle(&mut self, ui: &mut egui::Ui) {
        self.project_name(ui);
        ui.add_space(style::GAP_S);
        self.save_state(ui);
        ui.add_space(style::GAP_L);
        self.format_picker(ui);
    }

    /// The project's name; clicked, a box to rename it in (Enter keeps it, Esc doesn't).
    /// Renaming is an edit, so it undoes.
    fn project_name(&mut self, ui: &mut egui::Ui) {
        let key = egui::Id::new("top-bar-rename");
        let name = self.editor.name();
        if let Some(mut text) = ui.data(|d| d.get_temp::<String>(key)) {
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(180.0).font(egui::FontId::proportional(style::TEXT)));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                ui.data_mut(|d| d.remove::<String>(key));
            } else if r.lost_focus() {
                let new = text.trim().to_string();
                if !new.is_empty() && new != name {
                    self.rename_open_project(&new);
                }
                ui.data_mut(|d| d.remove::<String>(key));
            } else {
                ui.data_mut(|d| d.insert_temp(key, text));
            }
            return;
        }
        let galley = ui.painter().layout_no_wrap(name.clone(), egui::FontId::proportional(style::TEXT), egui::Color32::PLACEHOLDER);
        let (rect, response) = ui.allocate_exact_size(galley.size() + egui::vec2(12.0, 8.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let visuals = ui.visuals();
            if response.hovered() {
                ui.painter().rect_filled(rect, style::ROUNDING, visuals.widgets.hovered.weak_bg_fill);
            }
            ui.painter().galley(rect.min + egui::vec2(6.0, 4.0), galley, visuals.strong_text_color());
        }
        let response = response.on_hover_cursor(egui::CursorIcon::Text).on_hover_text(tr("Click to rename the project"));
        if response.clicked() {
            ui.data_mut(|d| d.insert_temp(key, name));
        }
    }

    /// Renames the open project: the name it has for itself (one undo step) and, once it's
    /// been saved, its file and picture — what the start page and the recent list call it
    /// — and its folder, unless some of its media are in there (moving them would pull
    /// them out from under the open project). A project never saved takes the name when
    /// it's first saved.
    pub(crate) fn rename_open_project(&mut self, name: &str) {
        if let Err(e) = self.editor.apply(tr("Rename project"), vec![oa_doc::Op::SetProjectName(name.to_string())]) {
            self.report_error(e.to_string());
            return;
        }
        let Some(old) = self.editor.path.clone() else { return };
        // Never two writers on one file.
        self.finish_background_save();
        let folder = old.parent().map(|d| d.to_path_buf());
        let media_inside = folder.as_ref().is_some_and(|dir| self.editor.doc.project().media.values().any(|m| std::path::Path::new(&m.path).starts_with(dir)));
        let key = crate::layout::key(&old);
        match crate::library::rename_project_with(&old, name, !media_inside) {
            Ok(new) => {
                self.project_thumbs.remove(&crate::thumbnail::path_for(&old).to_string_lossy().to_string());
                self.editor.path = Some(new.clone());
                self.settings.moved_project(&old, &key, &new);
                self.view_key = None;
                self.thumb_written = None;
            }
            Err(e) => self.report_error(trf("Renamed inside the project, but couldn't rename its file: {e}", &[("e", &e.to_string())])),
        }
    }

    /// Whether what's open is saved: "Saving…" while it's written, "Edited" with changes
    /// that will save themselves shortly, "Not saved" with changes that won't (saving as
    /// you go is off), and otherwise "Saved", saying how long ago.
    fn save_state(&mut self, ui: &mut egui::Ui) {
        let key = egui::Id::new("top-bar-saved");
        let now = ui.input(|i| i.time);
        let dirty = self.editor.dirty();
        let mut seen: SaveSeen = ui.data(|d| d.get_temp(key)).unwrap_or_default();
        if seen.dirty && !dirty {
            seen.saved_at = Some(now);
        }
        seen.dirty = dirty;
        ui.data_mut(|d| d.insert_temp(key, seen));
        let weak = ui.visuals().weak_text_color();
        let (icon, text, color, tip) = if self.saving.is_some() {
            (icons::SAVING, tr("Saving…").to_string(), weak, tr("Writing the project to its file"))
        } else if dirty && self.settings.save_as_you_go && self.editor.path.is_some() {
            (icons::EDITED, tr("Edited").to_string(), weak, tr("Saves itself after a pause in the editing (Ctrl+S saves now)"))
        } else if dirty {
            (icons::EDITED, tr("Not saved").to_string(), style::WARNING, tr("Ctrl+S saves it; meanwhile a recovery copy is kept"))
        } else {
            let text = match seen.saved_at.map(|t| ((now - t) / 60.0) as u64) {
                Some(0) => tr("Saved just now").to_string(),
                Some(m) => trf("Saved {n} min ago", &[("n", &m.to_string())]),
                None => tr("Saved").to_string(),
            };
            if seen.saved_at.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(30));
            }
            (icons::SAVED, text, weak, tr("Everything is in the project's file"))
        };
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(style::TEXT_S), color);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0 + 4.0 + galley.size().x, 20.0), egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            icons::paint(ui.painter(), egui::Rect::from_center_size(egui::pos2(rect.left() + 8.0, rect.center().y), egui::vec2(15.0, 15.0)), icon, color);
            ui.painter().galley(egui::pos2(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0), galley, color);
        }
        response.on_hover_text(tip);
    }

    /// The right of the top bar (laid out right to left): Home, then Export — the one
    /// button in the accent — or, while exporting, how far it's got.
    fn bar_right(&mut self, ui: &mut egui::Ui) {
        ui.add_space(style::GAP_S);
        if icons::button(ui, icons::HOME, tr("Home: projects and plugins"), "", true).clicked() {
            self.go_home();
        }
        ui.add_space(style::GAP_S);
        match &self.export {
            Some(exporter) => {
                let (done, total) = exporter.progress();
                let stage = exporter.stage();
                let queued = self.export_queue.len();
                if ui.button(if queued > 0 { tr("Cancel exports") } else { tr("Cancel export") }).clicked() {
                    self.cancel_exports();
                }
                let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                let status = match stage {
                    Some((crate::export_worker::Stage::Opening, _)) => tr("Starting…").to_string(),
                    Some((crate::export_worker::Stage::Finishing, _)) => tr("Finishing…").to_string(),
                    None => format!("{:.0}%", fraction * 100.0),
                };
                let text = if queued > 0 { trf("{status} · {n} more", &[("status", &status), ("n", &queued.to_string())]) } else { status };
                let bar = ui.add(egui::ProgressBar::new(fraction).desired_width(140.0).text(text)).interact(egui::Sense::click());
                if bar.on_hover_text(tr("Show the export and what's queued")).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.export_view.hidden = false;
                }
                // More can be queued while this one runs.
                if icons::button(ui, icons::EXPORT, tr("Queue another export…"), "", true).clicked() {
                    self.start_export();
                }
            }
            None => {
                if icons::text_button(ui, icons::EXPORT, tr("Export…"), true).on_hover_text(tr("Render the video to a file")).clicked() {
                    self.start_export();
                }
            }
        }
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
                ui.label(egui::RichText::new(tr("Select a clip to edit it — or drop media anywhere")).small().weak());
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
