//! Where projects live, and starting one.
//!
//! Every project gets a folder of its own in the projects folder (Documents/OpenAtelier
//! Projects unless Settings say otherwise): `<name>/<name>.oaproj.json`, with its
//! picture beside it. New project asks for a name, a format and a frame rate, and saves
//! it there straight away — nobody has to pick where a project goes. (Save as… still
//! puts one anywhere.)

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::AspectPreset;
use oa_time::FrameRate;
use std::path::{Path, PathBuf};

/// The frame rates offered, as (label, num, den).
pub const FRAME_RATES: &[(&str, u32, u32)] =
    &[("23.976", 24000, 1001), ("24", 24, 1), ("25", 25, 1), ("29.97", 30000, 1001), ("30", 30, 1), ("50", 50, 1), ("59.94", 60000, 1001), ("60", 60, 1)];

/// What a project is called when it's given no name.
pub const UNTITLED: &str = "Untitled project";

/// The user's Documents folder.
fn documents_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{FOLDERID_Documents, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
        // Where Windows really keeps it (moved to OneDrive, another drive…).
        // SAFETY: the returned string is read once and freed with the allocator it came from.
        if let Ok(p) = unsafe { SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None) } {
            let path = unsafe { p.to_string() }.ok().map(PathBuf::from);
            unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
            if path.is_some() {
                return path;
            }
        }
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Documents"))
    }
    #[cfg(not(windows))]
    {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        var("XDG_DOCUMENTS_DIR").or_else(|| var("HOME").map(|h| h.join("Documents")))
    }
}

/// The projects folder unless Settings name another (`OA_PROJECTS_DIR` overrides both,
/// for tests and portable installs).
pub fn default_projects_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OA_PROJECTS_DIR") {
        return PathBuf::from(dir);
    }
    documents_dir().unwrap_or_else(crate::settings::config_dir).join("OpenAtelier Projects")
}

/// A name that's fine as a folder and a file name everywhere.
pub fn folder_name(name: &str) -> String {
    let mut out: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    out = out.trim().trim_end_matches(['.', ' ']).chars().take(80).collect::<String>().trim().to_string();
    // Names Windows keeps for devices.
    let stem = out.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        out.push('_');
    }
    if out.is_empty() { UNTITLED.into() } else { out }
}

/// A new project's file in `dir`: `<name>/<name>.oaproj.json`, in a folder made for it —
/// "<name> 2", "<name> 3"… when one by that name is there already.
pub fn fresh_project_file(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let base = folder_name(name);
    for n in 1.. {
        let folder = if n == 1 { base.clone() } else { format!("{base} {n}") };
        let at = dir.join(&folder);
        if at.exists() {
            continue;
        }
        std::fs::create_dir_all(&at)?;
        return Ok(at.join(format!("{folder}.oaproj.json")));
    }
    unreachable!()
}

/// The New project window, while it's open.
pub struct NewProjectForm {
    pub name: String,
    /// An `AspectPreset` id.
    pub layout: String,
    pub fps: [u32; 2],
    focus: bool,
}

impl App {
    pub(crate) fn projects_dir(&self) -> PathBuf {
        self.settings.projects_dir.clone().unwrap_or_else(default_projects_dir)
    }

    /// Opens the New project window, with the format and frame rate picked last time.
    pub(crate) fn ask_new_project(&mut self) {
        let last = &self.settings.new_project;
        self.new_project_form = Some(NewProjectForm { name: String::new(), layout: last.layout.clone(), fps: last.fps, focus: true });
    }

    /// Makes the project the window describes, saves it into its own folder and opens it.
    fn create_project(&mut self, form: &NewProjectForm) {
        let name = match form.name.trim() {
            "" => UNTITLED,
            n => n,
        };
        let file = match fresh_project_file(&self.projects_dir(), name) {
            Ok(f) => f,
            Err(e) => {
                self.report_error(trf("couldn't make the project's folder: {e}", &[("e", &e.to_string())]));
                return;
            }
        };
        let rate = FrameRate::new(form.fps[0], form.fps[1].max(1));
        self.set_aside_project();
        self.editor = crate::editor::Editor::with_setup(name, &form.layout, rate);
        self.reset_workspace();
        self.viewer_render.set_media(Vec::new(), true);
        match self.editor.save(&file) {
            Ok(()) => self.settings.remember_project(&file, name, None),
            Err(e) => self.report_error(e),
        }
        self.settings.new_project = crate::settings::NewProjectPrefs { layout: form.layout.clone(), fps: form.fps };
        self.settings.save();
        self.screen = crate::home::Screen::Editor;
    }

    /// The New project window: a name, the format, the frame rate.
    pub(crate) fn new_project_window(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.new_project_form.take() else { return };
        let mut done: Option<bool> = None;
        let folder = self.projects_dir();
        let modal = egui::Modal::new(egui::Id::new("new-project")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(tr("New project"));
            ui.add_space(crate::style::GAP);

            ui.label(egui::RichText::new(tr("Name")).strong());
            let r = ui.add(egui::TextEdit::singleline(&mut form.name).hint_text(tr(UNTITLED)).desired_width(f32::INFINITY));
            if std::mem::take(&mut form.focus) {
                r.request_focus();
            }
            let shown = match form.name.trim() {
                "" => UNTITLED.to_string(),
                n => folder_name(n),
            };
            ui.label(egui::RichText::new(trf("Saved in {0}", &[("0", &folder.join(&shown).display().to_string())])).small().weak());
            ui.add_space(crate::style::GAP);

            ui.label(egui::RichText::new(tr("Format")).strong());
            let gap = 6.0;
            let w = ((ui.available_width() - 3.0 * gap) / 4.0).floor();
            for row in AspectPreset::all().chunks(4) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for p in row {
                        if format_card(ui, p, form.layout == p.id, w).clicked() {
                            form.layout = p.id.to_string();
                        }
                    }
                });
                ui.add_space(gap);
            }
            ui.add_space(crate::style::GAP_S);

            ui.label(egui::RichText::new(tr("Frame rate")).strong());
            ui.horizontal_wrapped(|ui| {
                for (label, num, den) in FRAME_RATES {
                    let on = form.fps == [*num, *den];
                    if ui.add(egui::Button::new(*label).selected(on).min_size(egui::vec2(52.0, 26.0))).on_hover_text(trf("{0} frames per second", &[("0", label)])).clicked() {
                        form.fps = [*num, *den];
                    }
                }
                ui.label(egui::RichText::new(tr("fps")).weak());
            });
            ui.add_space(crate::style::GAP_L);

            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let create = ui.add(egui::Button::new(egui::RichText::new(tr("Create")).strong().color(egui::Color32::WHITE)).fill(crate::style::ACCENT).min_size(egui::vec2(96.0, 30.0)));
                    if create.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.add(egui::Button::new(tr("Cancel")).min_size(egui::vec2(80.0, 30.0))).clicked() {
                        done = Some(false);
                    }
                });
            });
        });
        if modal.should_close() {
            done.get_or_insert(false);
        }
        match done {
            Some(true) => self.create_project(&form),
            Some(false) => {}
            None => self.new_project_form = Some(form),
        }
    }

    /// Over the workspace while a project opens: what's being opened, and that it's
    /// being worked on (its media is found and read in before it shows).
    pub(crate) fn loading_overlay(&self, ui: &mut egui::Ui) {
        let Some((path, _)) = &self.opening else { return };
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let v = ui.visuals();
        painter.rect_filled(rect, 0.0, v.extreme_bg_color);
        let (title, name) = if self.recovering.is_some() {
            (tr("Recovering unsaved work…").to_string(), String::new())
        } else {
            (tr("Loading project…").to_string(), crate::thumbnail::name_from_path(path))
        };
        let c = rect.center();
        // A ring turning, and a short bar sweeping under the words.
        let time = ui.input(|i| i.time) as f32;
        let r = 18.0;
        let ring = c + egui::vec2(0.0, -34.0);
        painter.circle_stroke(ring, r, egui::Stroke::new(3.0, v.widgets.noninteractive.bg_stroke.color));
        let start = time * 4.5;
        let arc: Vec<egui::Pos2> = (0..=24).map(|i| start + i as f32 / 24.0 * 1.6).map(|a| ring + egui::vec2(a.cos(), a.sin()) * r).collect();
        painter.add(egui::Shape::line(arc, egui::Stroke::new(3.0, crate::style::ACCENT)));
        painter.text(c + egui::vec2(0.0, 2.0), egui::Align2::CENTER_CENTER, title, egui::FontId::proportional(crate::style::TEXT_L), v.strong_text_color());
        if !name.is_empty() {
            painter.text(c + egui::vec2(0.0, 22.0), egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(crate::style::TEXT_S), v.weak_text_color());
        }
        let track = egui::Rect::from_center_size(c + egui::vec2(0.0, 42.0), egui::vec2(160.0, 3.0));
        painter.rect_filled(track, 1.5, v.widgets.noninteractive.bg_stroke.color);
        let k = (time * 0.8).fract();
        let x0 = track.left() + (track.width() + 50.0) * k - 50.0;
        let bar = egui::Rect::from_x_y_ranges(x0.max(track.left())..=(x0 + 50.0).min(track.right()), track.y_range());
        if bar.width() > 0.0 {
            painter.rect_filled(bar, 1.5, crate::style::ACCENT);
        }
        ui.ctx().request_repaint();
    }
}

/// One format to pick: its shape drawn to scale, its name and where it's used.
fn format_card(ui: &mut egui::Ui, p: &AspectPreset, selected: bool, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 96.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let hovered = response.hovered();
        let edge = if selected { crate::style::ACCENT } else if hovered { v.widgets.hovered.bg_stroke.color } else { v.widgets.noninteractive.bg_stroke.color };
        let fill = if selected { crate::style::ACCENT.gamma_multiply(0.14) } else if hovered { v.widgets.hovered.weak_bg_fill } else { v.faint_bg_color };
        let painter = ui.painter();
        painter.rect(rect, 8.0, fill, egui::Stroke::new(if selected { 1.5 } else { 1.0 }, edge), egui::StrokeKind::Inside);
        // The shape, in a 40 px box.
        let (rw, rh) = (p.ratio.0 as f32, p.ratio.1 as f32);
        let k = 40.0 / rw.max(rh);
        let shape = egui::Rect::from_center_size(egui::pos2(rect.center().x, rect.top() + 28.0), egui::vec2(rw * k, rh * k));
        painter.rect(shape, 3.0, if selected { crate::style::ACCENT.gamma_multiply(0.35) } else { v.extreme_bg_color }, egui::Stroke::new(1.5, if selected { crate::style::ACCENT } else { v.weak_text_color() }), egui::StrokeKind::Inside);
        let size = p.size(1080);
        painter.text(egui::pos2(rect.center().x, rect.top() + 60.0), egui::Align2::CENTER_CENTER, tr(p.name), egui::FontId::proportional(crate::style::TEXT_S + 0.5), v.text_color());
        painter.text(egui::pos2(rect.center().x, rect.top() + 78.0), egui::Align2::CENTER_CENTER, format!("{}×{}", size.width, size.height), egui::FontId::proportional(crate::style::TEXT_S), v.weak_text_color());
    }
    response.on_hover_text(tr(p.platforms)).on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_make_safe_folders() {
        assert_eq!(folder_name("  My trip: day 1?  "), "My trip_ day 1_");
        assert_eq!(folder_name("..."), UNTITLED);
        assert_eq!(folder_name("con"), "con_");
        assert_eq!(folder_name("COM3.final"), "COM3.final_");
        assert_eq!(folder_name("Combo"), "Combo");
    }

    #[test]
    fn each_new_project_gets_a_folder_of_its_own() {
        let dir = std::env::temp_dir().join(format!("oa-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = fresh_project_file(&dir, "Trip").unwrap();
        let b = fresh_project_file(&dir, "Trip").unwrap();
        assert_eq!(a, dir.join("Trip").join("Trip.oaproj.json"));
        assert_eq!(b, dir.join("Trip 2").join("Trip 2.oaproj.json"));
        assert!(b.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
