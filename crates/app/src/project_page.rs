//! The Projects page of the start screen: every project in the projects folder, sorted
//! into folders like a file explorer's (`library`), with search, sorting, a grid or a
//! list, and a menu on each project (rename, duplicate, show in folder, move, archive,
//! trash). Unsaved work an earlier session left is shown on its project's card.
//!
//! On the left: Recent (every project that's not archived or in the trash, the ones
//! saved elsewhere too), your folders (+ makes one; drop cards on one to move them in),
//! Archived and Trash.

use crate::i18n::{t, tr, trf};
use crate::library::{self, Entry, Library, Place};
use crate::App;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Cards are this wide; their picture is 16:9.
const CARD_W: f32 = 200.0;
const SIDEBAR_W: f32 = 196.0;

/// What the page is showing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Recent,
    Folder(String),
    Archive,
    Trash,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Edited,
    Name,
    Created,
}

/// The page's own state.
#[derive(Default)]
pub struct ProjectsPage {
    pub view: View,
    search: String,
    sort: Sort,
    list: bool,
    library: Library,
    scanned: Option<Instant>,
    /// Something changed on disk: look again.
    stale: bool,
    /// A project being renamed: its file, and the name typed so far.
    renaming: Option<(PathBuf, String)>,
    /// A new folder being named.
    naming_folder: Option<String>,
    /// A folder being renamed: (its name, the name typed so far).
    renaming_folder: Option<(String, String)>,
    /// Waiting for a yes: deleting one project for good, or emptying the trash.
    confirm_delete: Option<PathBuf>,
    confirm_empty: bool,
}

/// A card dragged out (onto a folder on the left).
#[derive(Clone)]
struct ProjectDrag {
    file: PathBuf,
    place: Place,
}

/// What the page asked for this frame, done once it's drawn.
enum Act {
    Open(PathBuf),
    Rename(PathBuf, String),
    Duplicate(PathBuf),
    Show(PathBuf),
    Move(PathBuf, Place, Place),
    Restore(PathBuf),
    DeleteForever(PathBuf),
    Forget(PathBuf),
    Recover(crate::autosave::Recovery),
    NewFolder(String),
    RenameFolder(String, String),
    RemoveFolder(String),
    EmptyTrash,
}

/// "2 h ago" for a time.
fn ago(time: SystemTime) -> String {
    let secs = time.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    let n = |key: &str, n: u64| crate::i18n::args(key, &[("n", &n.to_string())]);
    match secs {
        0..=59 => t("time.moments").into(),
        60..=3599 => n("time.minutes", secs / 60),
        3600..=86_399 => n("time.hours", secs / 3600),
        86_400..=604_799 => n("time.days", secs / 86_400),
        _ => n("time.weeks", secs / 604_800),
    }
}

/// A point of a card's picture — `(u, v)`, 0–1 across and down — on the screen, the
/// card tipped toward the pointer by `tilt` (each −1–1; the side under the pointer
/// leans away) inside `rect`.
fn tilted(rect: egui::Rect, tilt: egui::Vec2) -> impl Fn(f32, f32) -> egui::Pos2 {
    let s = 0.045;
    let (c, h) = (rect.center(), rect.size() / 2.0);
    let corner = move |sx: f32, sy: f32| egui::pos2(c.x + sx * h.x * (1.0 - s * tilt.y * sy), c.y + sy * h.y * (1.0 - s * tilt.x * sx));
    let (tl, tr, bl, br) = (corner(-1.0, -1.0), corner(1.0, -1.0), corner(-1.0, 1.0), corner(1.0, 1.0));
    move |u, v| {
        let top = tl + (tr - tl) * u;
        let bottom = bl + (br - bl) * u;
        top + (bottom - top) * v
    }
}

/// The outline of a rounded rectangle in picture space (0–1 each way), corners `r`
/// (a fraction of the width; the height's follows `aspect`), as points round it.
fn rounded_outline(r: f32, aspect: f32) -> Vec<(f32, f32)> {
    let (rx, ry) = (r, r * aspect);
    let mut out = Vec::new();
    for (cx, cy, start) in [(1.0 - rx, ry, -90.0f32), (1.0 - rx, 1.0 - ry, 0.0), (rx, 1.0 - ry, 90.0), (rx, ry, 180.0)] {
        for i in 0..=6 {
            let a = (start + i as f32 * 15.0).to_radians();
            out.push((cx + rx * a.cos(), cy + ry * a.sin()));
        }
    }
    out
}

/// Paints part of a card's picture: `area` (picture space) filled with `texture`'s `uv`
/// (or the plain `tint` with no texture), rounded by `r`, through `map`.
fn paint_mapped(painter: &egui::Painter, map: &dyn Fn(f32, f32) -> egui::Pos2, area: egui::Rect, r: f32, aspect: f32, texture: Option<(egui::TextureId, egui::Rect)>, tint: egui::Color32) {
    let outline: Vec<(f32, f32)> = if r > 0.0 { rounded_outline(r, aspect) } else { vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] };
    let mut mesh = egui::Mesh::with_texture(texture.map_or(egui::TextureId::default(), |t| t.0));
    let uv_of = |u: f32, v: f32| match texture {
        Some((_, uv)) => egui::pos2(uv.left() + uv.width() * u, uv.top() + uv.height() * v),
        None => egui::epaint::WHITE_UV,
    };
    let at = |u: f32, v: f32| map(area.left() + area.width() * u, area.top() + area.height() * v);
    mesh.vertices.push(egui::epaint::Vertex { pos: at(0.5, 0.5), uv: uv_of(0.5, 0.5), color: tint });
    for &(u, v) in &outline {
        mesh.vertices.push(egui::epaint::Vertex { pos: at(u, v), uv: uv_of(u, v), color: tint });
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Shades a card's picture like a lit surface: a little darker toward its edges always
/// (a vignette), and — as it's pointed at (`h`, 0–1) — a soft light where the pointer
/// is (`light`, picture space) and shade away from it. Drawn as rings from the middle
/// out to the rounded outline, so the gradients are smooth and the corners stay round.
fn depth_overlay(painter: &egui::Painter, map: &dyn Fn(f32, f32) -> egui::Pos2, r: f32, aspect: f32, light: egui::Pos2, h: f32) {
    let outline = rounded_outline(r, aspect);
    let n = outline.len();
    let rings = [0.0f32, 0.3, 0.55, 0.75, 0.9, 1.0];
    let shade_at = |u: f32, v: f32| {
        // How far out (0 in the middle, 1 at the edge) and from the light (the picture's
        // height counting as much as its width).
        let edge = ((u - 0.5).abs() * 2.0).max((v - 0.5).abs() * 2.0);
        let d = ((u - light.x).powi(2) + ((v - light.y) / aspect.max(0.1) * 1.6).powi(2)).sqrt();
        let shine = h * 0.2 * (-(d * d) / 0.06).exp();
        let shade = 0.16 * edge.powi(3) + h * 0.2 * (d / 1.1).min(1.0);
        let a = (shine + shade).min(0.6);
        let white = (shine * 255.0) as u8;
        egui::Color32::from_rgba_premultiplied(white, white, white, (a * 255.0) as u8)
    };
    let mut mesh = egui::Mesh::default();
    for s in rings {
        for &(u, v) in &outline {
            let (u, v) = (0.5 + (u - 0.5) * s, 0.5 + (v - 0.5) * s);
            mesh.vertices.push(egui::epaint::Vertex { pos: map(u, v), uv: egui::epaint::WHITE_UV, color: shade_at(u, v) });
        }
    }
    for k in 0..rings.len() - 1 {
        for i in 0..n {
            let (a, b) = ((k * n + i) as u32, (k * n + (i + 1) % n) as u32);
            let (c, d) = (a + n as u32, b + n as u32);
            mesh.add_triangle(a, c, d);
            mesh.add_triangle(a, d, b);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A card's edge as it's pointed at (`h`, 0–1): a glow hugging `outline` (its tilted,
/// rounded shape on screen), fading outward, with the edge itself drawn over it — both
/// brightest near `light` (the pointer) and dimmer round the far side.
fn glow_edge(painter: &egui::Painter, outline: &[egui::Pos2], center: egui::Pos2, light: egui::Pos2, size: f32, h: f32) {
    let n = outline.len();
    let reach = 4.0 + 12.0 * h;
    let sigma = size * 0.35;
    let strength = |p: egui::Pos2| h * (0.3 + 0.7 * (-(p - light).length_sq() / (2.0 * sigma * sigma)).exp());
    // Outward, square to the outline at each point.
    let normal = |i: usize| {
        let along = outline[(i + 1) % n] - outline[(i + n - 1) % n];
        let mut out = egui::vec2(along.y, -along.x).normalized();
        if out.dot(outline[i] - center) < 0.0 {
            out = -out;
        }
        out
    };
    let mut mesh = egui::Mesh::default();
    for (i, &p) in outline.iter().enumerate() {
        let glow = crate::style::ACCENT.gamma_multiply(0.85 * strength(p));
        mesh.vertices.push(egui::epaint::Vertex { pos: p, uv: egui::epaint::WHITE_UV, color: glow });
        mesh.vertices.push(egui::epaint::Vertex { pos: p + normal(i) * reach, uv: egui::epaint::WHITE_UV, color: egui::Color32::TRANSPARENT });
    }
    for i in 0..n {
        let (a, b) = ((2 * i) as u32, (2 * ((i + 1) % n)) as u32);
        mesh.add_triangle(a, a + 1, b + 1);
        mesh.add_triangle(a, b + 1, b);
    }
    painter.add(egui::Shape::mesh(mesh));
    // The edge, a line per stretch of it, lit by how near the pointer it is.
    for i in 0..n {
        let (a, b) = (outline[i], outline[(i + 1) % n]);
        let k = strength(a.lerp(b, 0.5));
        let color = crate::style::ACCENT.lerp_to_gamma(egui::Color32::WHITE, (k - 0.6).max(0.0)).gamma_multiply(0.45 + 0.55 * k);
        painter.line_segment([a, b], egui::Stroke::new(1.0 + 1.2 * k, color));
    }
}

/// A color for a project with no picture yet, from its name (always the same one).
fn color_for(name: &str) -> egui::Color32 {
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.42, 1.0).into()
}

impl App {
    /// The projects folder as the page lists it, looked at again every couple of
    /// seconds (or at once after a change made here).
    fn library(&mut self) -> &Library {
        let page = &mut self.projects_page;
        if page.stale || page.scanned.is_none_or(|t| t.elapsed().as_secs_f32() > 2.0) {
            page.library = library::scan(&self.settings.projects_dir.clone().unwrap_or_else(crate::projects::default_projects_dir));
            page.scanned = Some(Instant::now());
            page.stale = false;
        }
        &self.projects_page.library
    }

    /// What a view lists, searched and sorted.
    fn listed(&mut self, view: &View) -> Vec<Entry> {
        let root = self.projects_dir();
        let mut out: Vec<Entry> = self
            .library()
            .projects
            .iter()
            .filter(|e| match view {
                View::Recent => matches!(e.place, Place::Top | Place::Folder(_)),
                View::Folder(f) => e.place == Place::Folder(f.clone()),
                View::Archive => e.place == Place::Archive,
                View::Trash => e.place == Place::Trash,
            })
            .cloned()
            .collect();
        // Projects saved somewhere else (Save as…), known from the recent list.
        if *view == View::Recent {
            for r in &self.settings.recent {
                if r.path.starts_with(&root) || out.iter().any(|e| e.file == r.path) {
                    continue;
                }
                let edited = std::fs::metadata(&r.path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                let name = if r.name.is_empty() { library::name_of(&r.path) } else { r.name.clone() };
                out.push(Entry { file: r.path.clone(), name, place: Place::Elsewhere, edited, created: edited });
            }
        }
        let page = &self.projects_page;
        let needle = page.search.trim().to_lowercase();
        out.retain(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle));
        match page.sort {
            Sort::Edited => out.sort_by_key(|e| std::cmp::Reverse(e.edited)),
            Sort::Name => out.sort_by_key(|e| e.name.to_lowercase()),
            Sort::Created => out.sort_by_key(|e| std::cmp::Reverse(e.created)),
        }
        out
    }

    /// The page, in all the room it's given: the projects in a column centered in the
    /// window (four cards wide at most), the folders just left of it — or, when the
    /// window is too narrow for that, at the left edge with the column beside them. The
    /// folders and the column's top (New project, the search and sort) stay where they
    /// are; only the projects scroll.
    pub(crate) fn projects_page(&mut self, ui: &mut egui::Ui) {
        let mut acts = Vec::new();
        let area = ui.available_rect_before_wrap();
        let margin = crate::style::GAP_XL;
        // Wider than the list's reach past the column (its glow room), so the two never meet.
        let gap = 28.0;
        // Four cards and the room between them, and a little for the scroll bar.
        let widest = 4.0 * CARD_W + 3.0 * crate::style::GAP_L + 14.0;
        let width = widest.min(area.width() - SIDEBAR_W - gap - 2.0 * margin).max(CARD_W + 14.0);
        let x0 = (area.center().x - width / 2.0).max(area.left() + margin + SIDEBAR_W + gap);
        let side = egui::Rect::from_min_max(egui::pos2(x0 - gap - SIDEBAR_W, area.top()), egui::pos2(x0 - gap, area.bottom()));
        let main = egui::Rect::from_min_max(egui::pos2(x0, area.top()), egui::pos2(x0 + width, area.bottom()));
        let column = |rect: egui::Rect| egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min));
        let mut side_ui = ui.new_child(column(side));
        egui::ScrollArea::vertical().id_salt("projects-folders").auto_shrink([false, false]).show(&mut side_ui, |ui| {
            ui.set_width(SIDEBAR_W);
            self.sidebar(ui, &mut acts);
        });
        let mut main_ui = ui.new_child(column(main));
        main_ui.set_width(width);
        self.page_content(&mut main_ui, &mut acts);
        ui.advance_cursor_after_rect(area);
        self.confirmations(ui.ctx(), &mut acts);
        for act in acts {
            self.act(act);
        }
    }

    /// The folders on the left.
    fn sidebar(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let lib = self.library().clone();
        let count = |place: &dyn Fn(&Place) -> bool| lib.projects.iter().filter(|e| place(&e.place)).count();
        let elsewhere = self.settings.recent.iter().filter(|r| !r.path.starts_with(self.projects_dir())).count();
        let recent = count(&|p| matches!(p, Place::Top | Place::Folder(_))) + elsewhere;
        let view = self.projects_page.view.clone();
        if side_row(ui, crate::icons::HISTORY, tr("Recent"), recent, view == View::Recent, Some(Place::Top), acts) {
            self.projects_page.view = View::Recent;
        }
        ui.add_space(crate::style::GAP);
        ui.label(egui::RichText::new(tr("Folders")).small().weak());
        for folder in &lib.folders {
            let here = view == View::Folder(folder.clone());
            if let Some((name, text)) = self.projects_page.renaming_folder.clone().filter(|(n, _)| n == folder) {
                let mut text = text;
                match name_field(ui, &mut text, tr("Folder name"), SIDEBAR_W - 8.0) {
                    Some(done) => {
                        if done && !text.trim().is_empty() && text.trim() != name {
                            acts.push(Act::RenameFolder(name, text.trim().to_string()));
                        }
                        self.projects_page.renaming_folder = None;
                    }
                    None => self.projects_page.renaming_folder = Some((name, text)),
                }
                continue;
            }
            let n = count(&|p| *p == Place::Folder(folder.clone()));
            let (clicked, response) = side_row_response(ui, crate::icons::FOLDER, folder, n, here, Some(Place::Folder(folder.clone())), acts);
            if clicked {
                self.projects_page.view = View::Folder(folder.clone());
            }
            crate::widgets::context_menu(&response, |ui| {
                if crate::icons::menu_item(ui, Some(crate::icons::EDITED), tr("Rename…"), "", true).clicked() {
                    self.projects_page.renaming_folder = Some((folder.clone(), folder.clone()));
                    ui.close();
                }
                if crate::icons::menu_item(ui, Some(crate::icons::DELETE), tr("Remove folder (keeps its projects)"), "", true).clicked() {
                    acts.push(Act::RemoveFolder(folder.clone()));
                    ui.close();
                }
            });
        }
        if lib.folders.is_empty() && self.projects_page.naming_folder.is_none() {
            ui.label(egui::RichText::new(tr("None yet: + makes one")).small().weak());
        }
        // A new folder, being named.
        if let Some(mut name) = self.projects_page.naming_folder.take() {
            match name_field(ui, &mut name, tr("Folder name"), SIDEBAR_W - 8.0) {
                Some(true) if !name.trim().is_empty() => acts.push(Act::NewFolder(name.trim().to_string())),
                Some(_) => {}
                None => self.projects_page.naming_folder = Some(name),
            }
        }
        ui.add_space(crate::style::GAP);
        ui.separator();
        if side_row(ui, crate::icons::ARCHIVE, tr("Archived"), count(&|p| *p == Place::Archive), view == View::Archive, Some(Place::Archive), acts) {
            self.projects_page.view = View::Archive;
        }
        if side_row(ui, crate::icons::DELETE, tr("Trash"), count(&|p| *p == Place::Trash), view == View::Trash, Some(Place::Trash), acts) {
            self.projects_page.view = View::Trash;
        }
        ui.add_space(crate::style::GAP);
        if crate::icons::flat_button(ui, crate::icons::NEW_FOLDER, tr("New folder")).on_hover_text(tr("A folder to sort projects into (drag cards onto it)")).clicked() {
            self.projects_page.naming_folder = Some(String::new());
        }
        // The view's folder went away (renamed elsewhere, removed): back to Recent.
        if let View::Folder(f) = &self.projects_page.view
            && !lib.folders.contains(f)
        {
            self.projects_page.view = View::Recent;
        }
    }

    /// The right of the page: ways to start, unsaved work, the toolbar and the projects.
    fn page_content(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let view = self.projects_page.view.clone();
        let entries = self.listed(&view);
        if view == View::Recent {
            self.starting_points(ui);
            ui.add_space(crate::style::GAP_L);
        }

        // The toolbar: what this is, then search, sort and grid or list.
        let title = match &view {
            View::Recent => tr("Recent").to_string(),
            View::Folder(f) => f.clone(),
            View::Archive => tr("Archived").to_string(),
            View::Trash => tr("Trash").to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).size(crate::style::TEXT_L).strong());
            crate::widgets::pill(ui, &entries.len().to_string(), ui.visuals().weak_text_color());
            if view == View::Trash && !entries.is_empty() && ui.button(tr("Empty trash")).clicked() {
                self.projects_page.confirm_empty = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let page = &mut self.projects_page;
                let views = [(crate::icons::GRID_VIEW, "Cards", !page.list), (crate::icons::LIST_VIEW, "List", page.list)];
                if let Some(i) = crate::widgets::segmented(ui, egui::Id::new("projects-view"), &views) {
                    page.list = i == 1;
                }
                let sort_name = |s: Sort| match s {
                    Sort::Edited => tr("Last edited"),
                    Sort::Name => tr("Name"),
                    Sort::Created => tr("Date created"),
                };
                egui::ComboBox::from_id_salt("projects-sort").selected_text(sort_name(page.sort)).show_ui(ui, |ui| {
                    for s in [Sort::Edited, Sort::Name, Sort::Created] {
                        ui.selectable_value(&mut page.sort, s, sort_name(s));
                    }
                });
                crate::widgets::search_field(ui, &mut page.search, tr("Search projects"), 200.0);
            });
        });
        ui.add_space(crate::style::GAP);
        // Everything above stays put; from here down, it scrolls. The scrolling area
        // reaches past the column by a card's glow on each side (and the cards are kept
        // that far in from its edges, lined up with what's above), so a lifted card's
        // glow and shadow aren't cut off where the scrolling stops.
        const ROOM: f32 = 20.0;
        let rest = ui.available_rect_before_wrap();
        let scroll = egui::Rect::from_min_max(egui::pos2(rest.left() - ROOM, rest.top()), egui::pos2(rest.right() + ROOM, rest.bottom()));
        let mut list = ui.new_child(egui::UiBuilder::new().max_rect(scroll).layout(egui::Layout::top_down(egui::Align::Min)));
        egui::ScrollArea::vertical().id_salt("projects-list").auto_shrink([false, false]).show(&mut list, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin { left: ROOM as i8, right: ROOM as i8, top: ROOM as i8, bottom: ROOM as i8 }).show(ui, |ui| {
                ui.set_width(rest.width());
                self.page_list(ui, &view, &entries, acts);
            });
        });
        ui.advance_cursor_after_rect(rest);
    }

    /// The scrolling part of the page: unsaved work that isn't a listed project's, the
    /// projects, and the language setting at the end.
    fn page_list(&mut self, ui: &mut egui::Ui, view: &View, entries: &[Entry], acts: &mut Vec<Act>) {
        if *view == View::Recent {
            // (Unsaved work belonging to a listed project shows on its card.)
            let all = self.listed(&View::Recent);
            let loose: Vec<crate::autosave::Recovery> = self.recoveries.iter().filter(|r| r.original.as_ref().is_none_or(|o| !all.iter().any(|e| &e.file == o))).cloned().collect();
            for r in loose {
                self.recovery_card(ui, &r);
                ui.add_space(crate::style::GAP);
            }
        }
        if entries.is_empty() {
            let empty = match (view, self.projects_page.search.trim().is_empty()) {
                (_, false) => tr("No project by that name."),
                (View::Recent, true) => t("home.recent_empty"),
                (View::Folder(_), true) => tr("Nothing here yet: drag projects onto this folder on the left."),
                (View::Archive, true) => tr("Archived projects are kept here, out of the way."),
                (View::Trash, true) => tr("The trash is empty."),
            };
            ui.add_space(crate::style::GAP_L);
            ui.label(egui::RichText::new(empty).weak());
        } else if self.projects_page.list {
            for e in entries {
                self.project_row(ui, e, acts);
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(crate::style::GAP_L, crate::style::GAP_L);
                for e in entries {
                    self.project_card(ui, e, acts);
                }
            });
        }

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
            egui::ComboBox::from_id_salt("home-language").selected_text(crate::i18n::name_of(&picked)).show_ui(ui, |ui| {
                for lang in &langs {
                    ui.selectable_value(&mut picked, lang.clone(), crate::i18n::name_of(lang));
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

    /// New project and Open or import, side by side in the middle.
    fn starting_points(&mut self, ui: &mut egui::Ui) {
        let gap = crate::style::GAP;
        let tile_w = ((ui.available_width() - gap) / 2.0).floor().min(300.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            // Centered.
            ui.add_space(((ui.available_width() - 2.0 * tile_w - gap) / 2.0).max(0.0));
            if crate::home::tile(ui, crate::icons::ADD, t("home.new_project"), t("home.new_project_hint"), true, tile_w).clicked() {
                self.ask_new_project();
            }
            if crate::home::tile(ui, crate::icons::FOLDER_OPEN, tr("Open or import…"), tr("A project file, or media to start a new one with"), false, tile_w).clicked() {
                let mut kinds: Vec<&str> = vec!["json"];
                kinds.extend(crate::MEDIA_EXTENSIONS);
                if let Some(files) = rfd::FileDialog::new().add_filter(tr("Projects and media"), &kinds).set_directory(self.projects_dir()).pick_files() {
                    self.open_paths(&files);
                    self.screen = crate::home::Screen::Editor;
                }
            }
        });
        ui.add_space(6.0);
        // The hint under them, centered too.
        let hint = egui::RichText::new(t("home.drop_hint")).small().weak();
        let width = ui.painter().layout_no_wrap(t("home.drop_hint").to_string(), egui::FontId::proportional(crate::style::TEXT_S), egui::Color32::WHITE).size().x + 20.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, crate::icons::INFO, ui.visuals().weak_text_color());
            ui.label(hint);
        });
    }

    /// The unsaved work an earlier session left for this project, if any.
    fn recovery_for(&self, file: &Path) -> Option<crate::autosave::Recovery> {
        self.recoveries.iter().find(|r| r.original.as_deref() == Some(file)).cloned()
    }

    /// One project as a card: its picture (tipping toward the pointer, its edge lit, as
    /// it's pointed at), name and when it was last edited; ⋮ for what can be done with it.
    fn project_card(&mut self, ui: &mut egui::Ui, e: &Entry, acts: &mut Vec<Act>) {
        let thumb_h = CARD_W * 0.5625;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, thumb_h + 46.0), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let exists = e.file.exists();
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, thumb_h));
        let id = response.id;
        let menu_id = id.with("menu");
        let menu_open = egui::Popup::is_id_open(ui.ctx(), menu_id);
        let lit = response.hovered() || menu_open || response.dragged();
        let h = ui.ctx().animate_bool_with_time(id.with("lift"), lit, 0.16);
        // Toward the pointer while it's over the card; eased back from where it was.
        let tilt_key = id.with("tilt");
        let mut tilt: egui::Vec2 = ui.data(|d| d.get_temp(tilt_key)).unwrap_or_default();
        if let Some(p) = response.hover_pos() {
            tilt = egui::vec2(((p.x - thumb.center().x) / (thumb.width() / 2.0)).clamp(-1.0, 1.0), ((p.y - thumb.center().y) / (thumb.height() / 2.0)).clamp(-1.0, 1.0));
            ui.data_mut(|d| d.insert_temp(tilt_key, tilt));
        }
        let lifted = egui::Rect::from_center_size(thumb.center(), thumb.size() * (1.0 + 0.035 * h));
        let map = tilted(lifted, tilt * h);
        let painter = ui.painter();
        let visuals = ui.visuals().clone();
        let radius = 8.0 / CARD_W;
        let aspect = CARD_W / thumb_h;
        // Lifted off the page: a soft shadow under it.
        if h > 0.01 {
            let shadow = egui::epaint::Shadow { offset: [0, 8], blur: 22, spread: 0, color: egui::Color32::from_black_alpha((110.0 * h) as u8) };
            painter.add(shadow.as_shape(lifted, 8));
        }
        // Where the light is: the pointer, in the picture's own terms (eased back to the
        // middle as the pointer leaves).
        let light = egui::pos2(0.5 + 0.5 * tilt.x * h, 0.5 + 0.5 * tilt.y * h);
        let recovery = self.recovery_for(&e.file);
        match self.project_thumb(ui.ctx(), &crate::thumbnail::path_for(&e.file)) {
            Some((picture, blurred)) => {
                // A blurred, darkened copy fills the card; the picture sits in it at its
                // own shape (a vertical project is a tall picture, not a cropped one).
                paint_mapped(painter, &map, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)), radius, aspect, Some((blurred.id(), cover_uv(blurred.size_vec2(), thumb.size()))), egui::Color32::from_gray(150));
                let size = picture.size_vec2();
                let k = (thumb.width() / size.x.max(1.0)).min(thumb.height() / size.y.max(1.0));
                let fitted = size * k / thumb.size();
                let area = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), fitted);
                let r = if fitted.x > 0.99 && fitted.y > 0.99 { radius } else { 0.0 };
                paint_mapped(painter, &map, area, r, aspect, Some((picture.id(), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)))), egui::Color32::WHITE);
            }
            None => {
                // No picture yet: its own color, with its initial.
                let color = if exists { color_for(&e.name) } else { visuals.extreme_bg_color };
                paint_mapped(painter, &map, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)), radius, aspect, None, color);
                let initial = e.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                let text = if exists { initial } else { "?".into() };
                painter.text(map(0.5, 0.5), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(40.0), egui::Color32::from_white_alpha(200));
            }
        }
        // Depth: the picture shaded like a lit surface.
        depth_overlay(painter, &map, radius, aspect, light, h);
        // The edge: red when the file is gone, gold with unsaved work waiting; otherwise,
        // as it's pointed at, a glow following the picture's outline, brightest by the
        // pointer.
        let outline: Vec<egui::Pos2> = rounded_outline(radius, aspect).into_iter().map(|(u, v)| map(u, v)).collect();
        let fixed = if !exists {
            Some(crate::style::ERROR)
        } else if recovery.is_some() {
            Some(crate::style::GOLD)
        } else {
            None
        };
        match fixed {
            Some(color) => {
                painter.add(egui::Shape::closed_line(outline, egui::Stroke::new(1.5, color)));
            }
            None if h > 0.01 => glow_edge(painter, &outline, map(0.5, 0.5), map(light.x, light.y), lifted.width(), h),
            None => {
                painter.add(egui::Shape::closed_line(outline, egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)));
            }
        }

        // Unsaved work from an earlier session: a band to pick it up again.
        let mut over_band = false;
        if let Some(r) = &recovery {
            let band = egui::Rect::from_min_max(egui::pos2(lifted.left(), lifted.bottom() - 24.0), lifted.max);
            over_band = response.hover_pos().is_some_and(|p| band.contains(p));
            painter.rect_filled(band, egui::CornerRadius { sw: 8, se: 8, nw: 0, ne: 0 }, if over_band { crate::style::GOLD } else { crate::style::GOLD.gamma_multiply(0.85) });
            crate::icons::paint(painter, egui::Rect::from_min_size(band.min + egui::vec2(6.0, 4.0), egui::vec2(16.0, 16.0)), crate::icons::HISTORY, egui::Color32::BLACK);
            painter.text(egui::pos2(band.left() + 26.0, band.center().y), egui::Align2::LEFT_CENTER, tr("Unsaved changes · Recover"), egui::FontId::proportional(crate::style::TEXT_S), egui::Color32::BLACK);
            if over_band && response.clicked() {
                acts.push(Act::Recover(r.clone()));
            }
        }

        // ⋮, top right, while pointed at (or its menu is open).
        let dots = egui::Rect::from_min_size(egui::pos2(lifted.right() - 30.0, lifted.top() + 6.0), egui::vec2(24.0, 24.0));
        let dots_r = ui.interact(dots, id.with("dots"), egui::Sense::click());
        if lit || dots_r.hovered() {
            let fill = if dots_r.hovered() || menu_open { egui::Color32::from_black_alpha(200) } else { egui::Color32::from_black_alpha(140) };
            ui.painter().circle_filled(dots.center(), 12.0, fill);
            crate::icons::paint(ui.painter(), dots.shrink(4.0), crate::icons::MORE, egui::Color32::WHITE);
        }
        let folders = self.projects_page.library.folders.clone();
        let open_here = self.editor.path.as_deref() == Some(e.file.as_path());
        egui::Popup::menu(&dots_r).id(menu_id).show(|ui| project_menu(ui, e, &folders, open_here, recovery.as_ref(), acts, &mut self.projects_page));

        // Name (or a box to rename it in) and when it was last edited.
        let name_at = egui::pos2(rect.left() + 2.0, thumb.bottom() + 6.0);
        if let Some((_, text)) = self.projects_page.renaming.clone().filter(|(f, _)| *f == e.file) {
            let mut text = text;
            // Over the card, in a space of its own: laid out in the row of cards, it made
            // that row taller and pushed the cards after it down.
            let field = egui::Rect::from_min_size(name_at - egui::vec2(0.0, 3.0), egui::vec2(CARD_W - 4.0, 22.0));
            let mut over = ui.new_child(egui::UiBuilder::new().max_rect(field).layout(egui::Layout::left_to_right(egui::Align::Center)));
            match name_field(&mut over, &mut text, "", CARD_W - 12.0) {
                Some(done) => {
                    if done && !text.trim().is_empty() && text.trim() != e.name {
                        acts.push(Act::Rename(e.file.clone(), text.trim().to_string()));
                    }
                    self.projects_page.renaming = None;
                }
                None => self.projects_page.renaming = Some((e.file.clone(), text)),
            }
        } else {
            let mut job = egui::text::LayoutJob::simple_singleline(e.name.clone(), egui::FontId::proportional(crate::style::TEXT), if exists { visuals.text_color() } else { crate::style::ERROR });
            job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
            ui.painter().galley(name_at, ui.painter().layout_job(job), visuals.text_color());
        }
        let detail = if exists { self.detail(e) } else { t("home.missing").to_string() };
        let mut job = egui::text::LayoutJob::simple_singleline(detail, egui::FontId::proportional(crate::style::TEXT_S), visuals.weak_text_color());
        job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
        ui.painter().galley(egui::pos2(rect.left() + 2.0, name_at.y + crate::style::TEXT + 6.0), ui.painter().layout_job(job), visuals.weak_text_color());

        self.card_response(ui, &response, e, exists && !over_band && !dots_r.hovered(), &folders, open_here, recovery.as_ref(), acts);
    }

    /// The second line of a card or row: its folder (in Recent) and when it was edited.
    fn detail(&self, e: &Entry) -> String {
        let edited = trf("Edited {when}", &[("when", &ago(e.edited))]);
        match (&self.projects_page.view, &e.place) {
            (View::Recent, Place::Folder(f)) => format!("{f} · {edited}"),
            (View::Recent, Place::Elsewhere) => format!("{} · {edited}", tr("Elsewhere")),
            _ => edited,
        }
    }

    /// What a card or row does: click opens it (not from the trash), right-click is its
    /// menu, and dragging carries it to a folder.
    #[allow(clippy::too_many_arguments)]
    fn card_response(&mut self, ui: &egui::Ui, response: &egui::Response, e: &Entry, opens: bool, folders: &[String], open_here: bool, recovery: Option<&crate::autosave::Recovery>, acts: &mut Vec<Act>) {
        let tip = if e.place == Place::Trash { format!("{}\n{}", e.file.display(), tr("In the trash: restore it to open it")) } else { e.file.display().to_string() };
        let response = response.clone().on_hover_text(tip);
        if response.clicked() && opens && e.place != Place::Trash {
            acts.push(Act::Open(e.file.clone()));
        }
        if response.hovered() && opens {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.dnd_set_drag_payload(ProjectDrag { file: e.file.clone(), place: e.place.clone() });
        // While it's dragged: its name follows the pointer, to drop on a folder.
        if response.dragged()
            && let Some(p) = ui.ctx().pointer_latest_pos()
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("project-drag")));
            let galley = painter.layout_no_wrap(e.name.clone(), egui::FontId::proportional(crate::style::TEXT), egui::Color32::WHITE);
            let chip = egui::Rect::from_min_size(p + egui::vec2(14.0, 10.0), galley.size() + egui::vec2(36.0, 12.0));
            painter.rect_filled(chip, 8.0, crate::style::ACCENT);
            crate::icons::paint(&painter, egui::Rect::from_min_size(chip.min + egui::vec2(8.0, (chip.height() - 16.0) / 2.0), egui::vec2(16.0, 16.0)), crate::icons::MOVE_TO, egui::Color32::WHITE);
            painter.galley(chip.min + egui::vec2(28.0, 6.0), galley, egui::Color32::WHITE);
        }
        crate::widgets::context_menu(&response, |ui| project_menu(ui, e, folders, open_here, recovery, acts, &mut self.projects_page));
    }

    /// One project as a row of the list view.
    fn project_row(&mut self, ui: &mut egui::Ui, e: &Entry, acts: &mut Vec<Act>) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let exists = e.file.exists();
        let visuals = ui.visuals().clone();
        let menu_id = response.id.with("menu");
        if response.hovered() || egui::Popup::is_id_open(ui.ctx(), menu_id) {
            ui.painter().rect_filled(rect, 6.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let thumb = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 5.0), egui::vec2(75.0, 42.0));
        let map = tilted(thumb, egui::Vec2::ZERO);
        let unit = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0));
        match self.project_thumb(ui.ctx(), &crate::thumbnail::path_for(&e.file)) {
            Some((picture, _)) => paint_mapped(ui.painter(), &map, unit, 0.06, 75.0 / 42.0, Some((picture.id(), cover_uv(picture.size_vec2(), thumb.size()))), egui::Color32::WHITE),
            None => paint_mapped(ui.painter(), &map, unit, 0.06, 75.0 / 42.0, None, if exists { color_for(&e.name) } else { visuals.extreme_bg_color }),
        }
        let x = thumb.right() + 12.0;
        let recovery = self.recovery_for(&e.file);
        let name_color = if !exists { crate::style::ERROR } else { visuals.text_color() };
        ui.painter().text(egui::pos2(x, rect.top() + 9.0), egui::Align2::LEFT_TOP, &e.name, egui::FontId::proportional(crate::style::TEXT), name_color);
        let mut detail = if exists { self.detail(e) } else { t("home.missing").to_string() };
        if recovery.is_some() {
            detail = format!("{detail} · {}", tr("unsaved changes waiting"));
        }
        ui.painter().text(egui::pos2(x, rect.top() + 28.0), egui::Align2::LEFT_TOP, detail, egui::FontId::proportional(crate::style::TEXT_S), visuals.weak_text_color());
        let dots = egui::Rect::from_center_size(egui::pos2(rect.right() - 20.0, rect.center().y), egui::vec2(26.0, 26.0));
        let dots_r = ui.interact(dots, response.id.with("dots"), egui::Sense::click());
        if response.hovered() || dots_r.hovered() {
            crate::icons::paint(ui.painter(), dots.shrink(4.0), crate::icons::MORE, if dots_r.hovered() { visuals.strong_text_color() } else { visuals.weak_text_color() });
        }
        let folders = self.projects_page.library.folders.clone();
        let open_here = self.editor.path.as_deref() == Some(e.file.as_path());
        egui::Popup::menu(&dots_r).id(menu_id).show(|ui| project_menu(ui, e, &folders, open_here, recovery.as_ref(), acts, &mut self.projects_page));
        self.card_response(ui, &response, e, exists && !dots_r.hovered(), &folders, open_here, recovery.as_ref(), acts);
    }

    /// The yes-or-no windows for deleting for good.
    fn confirmations(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        if let Some(file) = self.projects_page.confirm_delete.clone() {
            let mut answer = None;
            egui::Modal::new(egui::Id::new("delete-project")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading(trf("Delete {name} for good?", &[("name", &library::name_of(&file))]));
                ui.label(tr("Its folder is deleted: the project file, its picture and anything else kept in it. Its media files aren't touched. This can't be undone."));
                ui.add_space(crate::style::GAP);
                ui.horizontal(|ui| {
                    if ui.button(tr("Delete for good")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            match answer {
                Some(true) => {
                    acts.push(Act::DeleteForever(file));
                    self.projects_page.confirm_delete = None;
                }
                Some(false) => self.projects_page.confirm_delete = None,
                None => {}
            }
        }
        if self.projects_page.confirm_empty {
            let mut answer = None;
            egui::Modal::new(egui::Id::new("empty-trash")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading(tr("Empty the trash?"));
                ui.label(tr("Every project in the trash is deleted for good (their media files aren't touched). This can't be undone."));
                ui.add_space(crate::style::GAP);
                ui.horizontal(|ui| {
                    if ui.button(tr("Empty trash")).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(yes) = answer {
                if yes {
                    acts.push(Act::EmptyTrash);
                }
                self.projects_page.confirm_empty = false;
            }
        }
    }

    /// Does what the page asked for.
    fn act(&mut self, act: Act) {
        let root = self.projects_dir();
        let open_here = |app: &App, file: &Path| app.editor.path.as_deref() == Some(file);
        let busy = tr("It's open in the editor: open another project (or start a new one) first.");
        // A project's file moved: what pointed at it follows.
        let moved = |app: &mut App, old: &Path, old_key: &str, new: &Path| {
            app.settings.moved_project(old, old_key, new);
            app.project_thumbs.remove(&crate::thumbnail::path_for(old).to_string_lossy().to_string());
        };
        let result: std::io::Result<()> = match act {
            Act::Open(file) => {
                self.open_project(&file);
                self.screen = crate::home::Screen::Editor;
                Ok(())
            }
            Act::Rename(file, name) if open_here(self, &file) => {
                let _ = name;
                self.notify(busy);
                Ok(())
            }
            Act::Rename(file, name) => {
                let key = crate::layout::key(&file);
                library::rename_project(&file, &name).map(|new| moved(self, &file, &key, &new))
            }
            Act::Duplicate(file) => library::duplicate_project(&file).map(|copy| self.notify(trf("Made a copy: {name}", &[("name", &library::name_of(&copy))]))),
            Act::Show(file) => {
                library::show_in_folder(&file);
                Ok(())
            }
            Act::Move(file, _, _) if open_here(self, &file) => {
                self.notify(busy);
                Ok(())
            }
            // Saved somewhere of its own: its folder isn't ours to move.
            Act::Move(file, Place::Elsewhere, _) => {
                self.notify(trf("{name} is saved outside the projects folder, so it stays where it is (Save as… into the projects folder to sort it)", &[("name", &library::name_of(&file))]));
                Ok(())
            }
            Act::Move(file, from, to) => {
                let key = crate::layout::key(&file);
                let trashed = to == Place::Trash;
                library::move_project(&root, &file, &from, &to).map(|new| {
                    moved(self, &file, &key, &new);
                    if trashed {
                        self.notify(trf("{name} is in the trash", &[("name", &library::name_of(&new))]));
                    }
                })
            }
            Act::Restore(file) => {
                let key = crate::layout::key(&file);
                library::restore(&root, &file).map(|new| moved(self, &file, &key, &new))
            }
            Act::DeleteForever(file) => library::delete_forever(&root, &file).map(|()| self.settings.forget_project(&file)),
            Act::Forget(file) => {
                self.settings.forget_project(&file);
                Ok(())
            }
            Act::Recover(r) => {
                self.recover(r, true);
                Ok(())
            }
            Act::NewFolder(name) => library::new_folder(&root, &name).map(|_| ()),
            Act::RenameFolder(old, new) => library::rename_folder(&root, &old, &new).map(|name| {
                if self.projects_page.view == View::Folder(old) {
                    self.projects_page.view = View::Folder(name);
                }
            }),
            Act::RemoveFolder(folder) => library::remove_folder(&root, &folder),
            Act::EmptyTrash => library::empty_trash(&root).map(|n| self.notify(trf("Deleted {n} projects for good", &[("n", &n.to_string())]))),
        };
        if let Err(e) = result {
            self.report_error(e.to_string());
        }
        self.projects_page.stale = true;
    }
}

/// A box to type a name in, focused when it shows. `Some(true)` once it's done (Enter,
/// or a click elsewhere), `Some(false)` if it was let go (Esc), `None` while typing.
/// (Asking for the keyboard before seeing it went — what Enter does — kept it from ever
/// finishing.)
fn name_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> Option<bool> {
    let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(width).font(egui::FontId::proportional(crate::style::TEXT)));
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(false);
    }
    if r.lost_focus() {
        return Some(true);
    }
    if !r.has_focus() {
        r.request_focus();
    }
    None
}

/// The texture coordinates that fill `area` with a picture of `size`, cropped to it
/// (centered).
fn cover_uv(size: egui::Vec2, area: egui::Vec2) -> egui::Rect {
    let (pa, aa) = (size.x / size.y.max(1.0), area.x / area.y.max(1.0));
    if pa > aa {
        let w = aa / pa;
        egui::Rect::from_min_max(egui::pos2(0.5 - w / 2.0, 0.0), egui::pos2(0.5 + w / 2.0, 1.0))
    } else {
        let h = pa / aa;
        egui::Rect::from_min_max(egui::pos2(0.0, 0.5 - h / 2.0), egui::pos2(1.0, 0.5 + h / 2.0))
    }
}

/// What can be done with a project, as its ⋮ and right-click menu.
fn project_menu(ui: &mut egui::Ui, e: &Entry, folders: &[String], open_here: bool, recovery: Option<&crate::autosave::Recovery>, acts: &mut Vec<Act>, page: &mut ProjectsPage) {
    ui.set_min_width(220.0);
    let exists = e.file.exists();
    let item = |ui: &mut egui::Ui, acts: &mut Vec<Act>, icon: crate::icons::Icon, text: &str, enabled: bool, act: Act| {
        if crate::icons::menu_item(ui, Some(icon), text, "", enabled).clicked() {
            acts.push(act);
            ui.close();
        }
    };
    if e.place == Place::Trash {
        item(ui, acts, crate::icons::RESTORE, tr("Restore"), true, Act::Restore(e.file.clone()));
        item(ui, acts, crate::icons::FOLDER_OPEN, tr("Show in folder"), exists, Act::Show(e.file.clone()));
        ui.separator();
        if crate::icons::menu_item(ui, Some(crate::icons::DELETE), tr("Delete for good…"), "", true).clicked() {
            page.confirm_delete = Some(e.file.clone());
            ui.close();
        }
        return;
    }
    item(ui, acts, crate::icons::FOLDER_OPEN, t("home.open"), exists, Act::Open(e.file.clone()));
    if let Some(r) = recovery {
        item(ui, acts, crate::icons::HISTORY, tr("Continue previous session"), true, Act::Recover(r.clone()));
    }
    ui.separator();
    if crate::icons::menu_item(ui, Some(crate::icons::EDITED), tr("Rename…"), "", exists && !open_here).on_disabled_hover_text(tr("It's open in the editor")).clicked() {
        page.renaming = Some((e.file.clone(), e.name.clone()));
        ui.close();
    }
    item(ui, acts, crate::icons::DUPLICATE, tr("Duplicate"), exists, Act::Duplicate(e.file.clone()));
    item(ui, acts, crate::icons::FOLDER_OPEN, tr("Show in folder"), exists, Act::Show(e.file.clone()));
    if e.place == Place::Elsewhere {
        ui.separator();
        item(ui, acts, crate::icons::CLOSE, t("home.remove_from_list"), true, Act::Forget(e.file.clone()));
        return;
    }
    // Into a folder (or out of one).
    let icon = egui::RichText::new(crate::icons::MOVE_TO.code.to_string()).font(egui::FontId::new(16.0, egui::FontFamily::Name("material".into())));
    ui.add_enabled_ui(!open_here, |ui| {
        ui.menu_button((icon, tr("Move to")), |ui| {
            ui.set_min_width(180.0);
            let mut to = |ui: &mut egui::Ui, icon: crate::icons::Icon, name: &str, place: Place| {
                if crate::icons::menu_item(ui, Some(icon), name, "", e.place != place).clicked() {
                    acts.push(Act::Move(e.file.clone(), e.place.clone(), place));
                    ui.close();
                }
            };
            to(ui, crate::icons::HISTORY, tr("No folder"), Place::Top);
            for f in folders {
                to(ui, crate::icons::FOLDER, f, Place::Folder(f.clone()));
            }
        });
    });
    if e.place == Place::Archive {
        item(ui, acts, crate::icons::UNARCHIVE, tr("Unarchive"), !open_here, Act::Move(e.file.clone(), Place::Archive, Place::Top));
    } else {
        item(ui, acts, crate::icons::ARCHIVE, tr("Archive"), !open_here, Act::Move(e.file.clone(), e.place.clone(), Place::Archive));
    }
    ui.separator();
    item(ui, acts, crate::icons::DELETE, tr("Move to trash"), !open_here, Act::Move(e.file.clone(), e.place.clone(), Place::Trash));
}

/// One place on the left: its icon, name and how many are in it — lit when it's shown,
/// and when a card dragged over it would go there (`drop`). Returns whether it was clicked.
fn side_row(ui: &mut egui::Ui, icon: crate::icons::Icon, name: &str, count: usize, selected: bool, drop: Option<Place>, acts: &mut Vec<Act>) -> bool {
    side_row_response(ui, icon, name, count, selected, drop, acts).0
}

fn side_row_response(ui: &mut egui::Ui, icon: crate::icons::Icon, name: &str, count: usize, selected: bool, drop: Option<Place>, acts: &mut Vec<Act>) -> (bool, egui::Response) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    // A card dragged over it, by where the pointer is (the card being dragged keeps the
    // pointer to itself as far as egui's hovering goes).
    let dragging = drop.is_some() && egui::DragAndDrop::has_payload_of_type::<ProjectDrag>(ui.ctx());
    let taking = dragging && ui.ctx().pointer_latest_pos().is_some_and(|p| rect.contains(p));
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let (fill, color) = if taking {
            (crate::style::ACCENT.gamma_multiply(0.35), v.strong_text_color())
        } else if selected {
            (crate::style::ACCENT.gamma_multiply(0.2), v.strong_text_color())
        } else if response.hovered() {
            (v.widgets.hovered.weak_bg_fill, v.text_color())
        } else {
            (egui::Color32::TRANSPARENT, v.text_color())
        };
        ui.painter().rect_filled(rect, 6.0, fill);
        if taking {
            ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(18.0, 18.0));
        crate::icons::paint(ui.painter(), icon_rect, icon, if selected { crate::style::ACCENT } else { v.widgets.inactive.fg_stroke.color });
        let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), egui::FontId::proportional(crate::style::TEXT), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 70.0);
        ui.painter().galley(egui::pos2(icon_rect.right() + 8.0, rect.center().y - 8.0), ui.painter().layout_job(job), color);
        if count > 0 {
            ui.painter().text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, count.to_string(), egui::FontId::proportional(crate::style::TEXT_S), v.weak_text_color());
        }
    }
    if taking
        && ui.input(|i| i.pointer.any_released())
        && let (Some(place), Some(dragged)) = (drop, egui::DragAndDrop::take_payload::<ProjectDrag>(ui.ctx()))
        && dragged.place != place
    {
        acts.push(Act::Move(dragged.file.clone(), dragged.place.clone(), place));
    }
    let clicked = response.clicked();
    (clicked, response.on_hover_cursor(egui::CursorIcon::PointingHand))
}

