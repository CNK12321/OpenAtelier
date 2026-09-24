//! The media bin: imported files and compound clips (groups made into media) as cards
//! with a picture — hover a video to skim it — or a waveform for sound. Sort by when it
//! was added, name, type or length; filter by name. Double-click a card (or its ＋) to
//! add it to the timeline; right-click for more.

use crate::App;
use eframe::egui;
use oa_doc::{ItemKind, MediaId, SeqId, TrackKind};
use oa_media::MediaKind;
use oa_time::Time;
use std::path::PathBuf;

const CARD_W: f32 = 120.0;
const THUMB_H: f32 = 68.0;

#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum BinSort {
    #[default]
    Added,
    Name,
    Kind,
    Length,
}

impl BinSort {
    const ALL: [BinSort; 4] = [BinSort::Added, BinSort::Name, BinSort::Kind, BinSort::Length];

    fn label(self) -> &'static str {
        match self {
            BinSort::Added => "Added",
            BinSort::Name => "Name",
            BinSort::Kind => "Type",
            BinSort::Length => "Length",
        }
    }
}

/// Which library the bin is showing.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum BinTab {
    /// What this project has imported.
    #[default]
    Project,
    /// The things kept on this computer, for every project (see `assets.rs`).
    Assets,
}

#[derive(Default)]
pub struct BinView {
    pub tab: BinTab,
    /// Where you are in the asset library (its own folders).
    pub asset_folder: String,
    pub sort: BinSort,
    pub descending: bool,
    pub search: String,
    /// The folder being shown: "" is the top, "b-roll/day 2" is nested.
    pub folder: String,
    /// A folder being named, and what has been typed so far.
    pub naming: Option<String>,
    /// The name box was just opened and hasn't been focused yet.
    pub naming_fresh: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Entry {
    Media(MediaId),
    Compound(SeqId),
}

/// What a card being dragged carries.
#[derive(Clone)]
pub struct BinDrop {
    entry: Entry,
    pub name: String,
    pub length: Time,
    /// It goes on an audio track.
    pub audio: bool,
}

impl BinDrop {
    /// Whether clip `item` already shows what this card is.
    pub fn is_in(&self, project: &oa_doc::Project, seq: SeqId, item: oa_doc::ItemId) -> bool {
        let Some(it) = project.sequence(seq).and_then(|s| s.item(item)) else { return false };
        match (&self.entry, &it.kind) {
            (Entry::Media(a), ItemKind::Media { media }) => a == media,
            (Entry::Compound(a), ItemKind::Nested { sequence }) => a == sequence,
            _ => false,
        }
    }
}

/// One card, gathered before drawing (so drawing can borrow the app mutably).
struct Card {
    entry: Entry,
    name: String,
    detail: String,
    kind: MediaKind,
    compound: bool,
    missing: bool,
    conformed: Option<String>,
    length: Time,
    /// What the picture shows: a file (with its frame size and length) and the second
    /// into it for a still thumbnail.
    picture: Option<(MediaId, PathBuf, [u32; 2], f64, f64)>,
    /// A file whose sound is drawn.
    sound: Option<(MediaId, PathBuf)>,
    /// The bin folder it's in (compounds are always at the top).
    folder: String,
}

fn clock(t: Time) -> String {
    let s = t.as_seconds_f64().max(0.0);
    format!("{}:{:02}", (s / 60.0).floor() as u64, (s % 60.0).floor() as u64)
}

impl App {
    fn bin_cards(&self) -> Vec<Card> {
        let mut cards: Vec<Card> = self
            .editor
            .pool
            .iter()
            .map(|m| {
                let length = if m.kind == MediaKind::Still { self.still_length() } else { m.probe.duration };
                let video = m.probe.video.as_ref().filter(|_| !m.missing);
                let still = m.kind == MediaKind::Still;
                Card {
                    entry: Entry::Media(m.id),
                    name: m.name.clone(),
                    detail: m.summary(),
                    kind: m.kind,
                    compound: false,
                    missing: m.missing,
                    conformed: m.conformed.clone(),
                    length,
                    picture: video.map(|v| {
                        let d = if still { 0.0 } else { m.probe.duration.as_seconds_f64() };
                        (m.id, m.decode_path.clone(), [v.width, v.height], d, d * 0.1)
                    }),
                    sound: (m.kind == MediaKind::Audio && !m.missing).then(|| (m.id, m.decode_path.clone())),
                    folder: self.editor.doc.project().media(m.id).map(|r| r.folder.clone()).unwrap_or_default(),
                }
            })
            .collect();
        for s in self.editor.compounds() {
            // Its picture: the first file on its lowest video track.
            let first = s
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Video)
                .flat_map(|t| t.items.iter())
                .filter_map(|i| match i.kind {
                    ItemKind::Media { media } => Some((i.range.start, media, i.time_map.source_in)),
                    _ => None,
                })
                .min_by_key(|(start, _, _)| *start);
            let picture = first.and_then(|(_, media, source_in)| {
                let m = self.editor.pool_item(media).filter(|m| !m.missing)?;
                let v = m.probe.video.as_ref()?;
                let d = if m.kind == MediaKind::Still { 0.0 } else { m.probe.duration.as_seconds_f64() };
                Some((media, m.decode_path.clone(), [v.width, v.height], d, source_in.as_seconds_f64()))
            });
            let clips: usize = s.tracks.iter().map(|t| t.items.len()).sum();
            cards.push(Card {
                entry: Entry::Compound(s.id),
                name: s.name.clone(),
                detail: format!("compound · {clips} clip{} · {}", if clips == 1 { "" } else { "s" }, clock(s.duration())),
                kind: MediaKind::Video,
                compound: true,
                missing: false,
                conformed: None,
                length: s.duration(),
                picture,
                sound: None,
                folder: String::new(),
            });
        }
        // Searching looks everywhere; otherwise you see the folder you're in.
        let needle = self.bin.search.trim().to_lowercase();
        if needle.is_empty() {
            let here = self.bin.folder.clone();
            cards.retain(|c| c.folder == here);
        } else {
            cards.retain(|c| c.name.to_lowercase().contains(&needle));
        }
        let kind_rank = |c: &Card| if c.compound { 3 } else { c.kind as u8 as i32 };
        match self.bin.sort {
            BinSort::Added => {}
            BinSort::Name => cards.sort_by_key(|c| c.name.to_lowercase()),
            BinSort::Kind => cards.sort_by_key(|c| (kind_rank(c), c.name.to_lowercase())),
            BinSort::Length => cards.sort_by_key(|c| c.length),
        }
        if self.bin.descending {
            cards.reverse();
        }
        cards
    }


    /// Every folder, as full paths: the ones the project lists, plus any a file claims
    /// to be in (a project edited elsewhere, say).
    pub(crate) fn bin_folders(&self) -> Vec<String> {
        let mut out: Vec<String> = self.editor.doc.project().bin_folders.clone();
        for m in self.editor.doc.project().media.values().filter(|m| !m.folder.is_empty()) {
            // "a/b" also means "a" exists.
            let mut path = String::new();
            for part in m.folder.split('/') {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                if !out.contains(&path) {
                    out.push(path.clone());
                }
            }
        }
        out.sort();
        out
    }

    /// The folders directly inside `parent`, with how much is in each (nested included).
    fn child_folders(&self, parent: &str) -> Vec<(String, usize)> {
        let project = self.editor.doc.project();
        self.bin_folders()
            .into_iter()
            .filter(|f| match parent {
                "" => !f.contains('/'),
                p => f.strip_prefix(p).is_some_and(|rest| rest.starts_with('/') && !rest[1..].contains('/')),
            })
            .map(|f| {
                let inside = format!("{f}/");
                let n = project.media.values().filter(|m| m.folder == f || m.folder.starts_with(&inside)).count();
                (f, n)
            })
            .collect()
    }

    /// Makes a folder inside the one being shown. Returns whether it was made.
    fn new_folder(&mut self, name: &str) -> bool {
        let name = match crate::notify::check_name("folder", name) {
            Ok(n) => n,
            Err(e) => {
                self.report_error(e);
                return false;
            }
        };
        if name.contains('/') {
            self.report_error("A folder name can't contain \"/\".");
            return false;
        }
        let here = self.bin.folder.clone();
        let path = if here.is_empty() { name } else { format!("{here}/{name}") };
        let mut folders = self.editor.doc.project().bin_folders.clone();
        if self.bin_folders().contains(&path) {
            self.report_error(format!("There's already a folder called {path}."));
            return false;
        }
        folders.push(path);
        folders.sort();
        if let Err(e) = self.editor.apply("New folder", vec![oa_doc::Op::SetBinFolders(folders)]) {
            self.report_error(e.to_string());
            return false;
        }
        true
    }

    /// Removes a folder, putting whatever is inside it back at the top.
    fn remove_folder(&mut self, path: &str) {
        let inside = format!("{path}/");
        let media: Vec<MediaId> = self
            .editor
            .doc
            .project()
            .media
            .values()
            .filter(|m| m.folder == path || m.folder.starts_with(&inside))
            .map(|m| m.id)
            .collect();
        let mut ops: Vec<oa_doc::Op> =
            media.iter().map(|&media| oa_doc::Op::SetMediaFolder { media, folder: String::new() }).collect();
        let folders: Vec<String> = self
            .editor
            .doc
            .project()
            .bin_folders
            .iter()
            .filter(|f| *f != path && !f.starts_with(&inside))
            .cloned()
            .collect();
        ops.push(oa_doc::Op::SetBinFolders(folders));
        if let Err(e) = self.editor.apply("Delete folder", ops) {
            self.report_error(e.to_string());
        }
        if self.bin.folder == path || self.bin.folder.starts_with(&inside) {
            self.bin.folder.clear();
        }
    }

    /// Moves a file into `folder` ("" is the top).
    fn move_to_folder(&mut self, media: MediaId, folder: &str) {
        let op = oa_doc::Op::SetMediaFolder { media, folder: folder.to_string() };
        if let Err(e) = self.editor.apply("Move to folder", vec![op]) {
            self.report_error(e.to_string());
        }
    }

    /// Menu entries for putting `media` somewhere: the top, an existing folder, or a new
    /// one typed in (made inside the folder being shown).
    fn folder_menu(&mut self, ui: &mut egui::Ui, media: MediaId) {
        let current = self.editor.doc.project().media(media).map(|m| m.folder.clone()).unwrap_or_default();
        ui.menu_button("Move to folder", |ui| {
            if ui.radio(current.is_empty(), "Media (top)").clicked() {
                self.move_to_folder(media, "");
                ui.close();
            }
            for f in self.bin_folders() {
                if ui.radio(current == f, &f).clicked() {
                    self.move_to_folder(media, &f);
                    ui.close();
                }
            }
        });
    }

    /// Where you are, as clickable steps back up.
    fn bin_folder_row(&mut self, ui: &mut egui::Ui) {
        let here = self.bin.folder.clone();
        if here.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Media").clicked() {
                self.bin.folder.clear();
            }
            let mut path = String::new();
            for part in here.split('/') {
                ui.label("›");
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                if ui.small_button(part).clicked() {
                    self.bin.folder = path.clone();
                }
            }
        });
    }

    /// The card for a folder being named: type and press Enter, Escape to give up.
    fn naming_card(&mut self, ui: &mut egui::Ui) {
        let Some(mut name) = self.bin.naming.clone() else { return };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::hover());
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let painter = ui.painter_at(rect);
        painter.rect_filled(thumb, 4.0, ui.visuals().faint_bg_color);
        painter.rect_stroke(thumb, 4.0, egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        crate::icons::paint(&painter, thumb.shrink(THUMB_H * 0.3), crate::icons::FOLDER, ui.visuals().weak_text_color());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(rect.left(), thumb.bottom() + 2.0),
            egui::vec2(CARD_W, 24.0),
        )));
        let r = child.add(egui::TextEdit::singleline(&mut name).hint_text("Folder name").desired_width(CARD_W - 4.0));
        if std::mem::take(&mut self.bin.naming_fresh) {
            r.request_focus();
        }
        let typing = r.has_focus();
        let enter = (typing || r.lost_focus()) && child.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = child.input(|i| i.key_pressed(egui::Key::Escape));
        // Clicking elsewhere without typing anything gives up quietly.
        let abandoned = r.lost_focus() && !enter && name.trim().is_empty();
        self.bin.naming = Some(name.clone());
        if enter {
            let made = match self.bin.tab {
                BinTab::Project => self.new_folder(&name),
                BinTab::Assets => {
                    let parent = self.bin.asset_folder.clone();
                    match self.assets.new_folder(&parent, &name) {
                        Ok(()) => true,
                        Err(e) => {
                            self.report_error(e);
                            false
                        }
                    }
                }
            };
            if made {
                self.bin.naming = None;
            }
        } else if escape || abandoned {
            self.bin.naming = None;
        }
    }

    /// One folder card: click to open it, drop a card on it to move that file in.
    fn folder_card(&mut self, ui: &mut egui::Ui, path: &str, count: usize) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        let taking_drop = response.contains_pointer() && egui::DragAndDrop::has_payload_of_type::<BinDrop>(ui.ctx());
        let painter = ui.painter_at(rect);
        painter.rect_filled(thumb, 4.0, visuals.faint_bg_color);
        crate::icons::paint(&painter, thumb.shrink(THUMB_H * 0.28), crate::icons::FOLDER, visuals.weak_text_color());
        let stroke = if taking_drop || response.hovered() {
            egui::Stroke::new(1.5, visuals.selection.stroke.color)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(thumb, 4.0, stroke, egui::StrokeKind::Inside);
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), egui::Align2::LEFT_TOP, &name, egui::FontId::proportional(12.0), visuals.text_color());
        let n = format!("{count} item{}", if count == 1 { "" } else { "s" });
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 16.0), egui::Align2::LEFT_TOP, n, egui::FontId::proportional(10.0), visuals.weak_text_color());

        if response.clicked() {
            self.bin.folder = path.to_string();
        }
        if let Some(drop) = response.dnd_release_payload::<BinDrop>()
            && let Entry::Media(id) = drop.entry
        {
            self.move_to_folder(id, path);
        }
        let path = path.to_string();
        crate::widgets::context_menu(&response, |ui| {
            if ui.button("Open").clicked() {
                self.bin.folder = path.clone();
                ui.close();
            }
            if ui.button("Delete").on_hover_text("Puts everything inside it back at the top").clicked() {
                self.remove_folder(&path);
                ui.close();
            }
        });
    }

    pub(crate) fn media_panel(&mut self, ui: &mut egui::Ui) {
        // Nothing in here may ask for more width than the panel has, or the panel grows
        // to fit it and the viewer loses the room.
        ui.set_max_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.bin.tab, BinTab::Project, "Media");
            ui.selectable_value(&mut self.bin.tab, BinTab::Assets, "Assets")
                .on_hover_text("Files kept on this computer, for every project");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, crate::icons::NEW_FOLDER, "New folder", "", true).clicked() {
                    self.bin.naming = Some(String::new());
                    self.bin.naming_fresh = true;
                }
                match self.bin.tab {
                    BinTab::Project => {
                        if crate::icons::button(ui, crate::icons::SAMPLES, "Import everything in ./samples", "", true).clicked() {
                            let mut files: Vec<PathBuf> = std::fs::read_dir("samples")
                                .into_iter()
                                .flatten()
                                .flatten()
                                .map(|e| e.path())
                                .filter(|p| p.extension().is_some_and(|e| crate::MEDIA_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m))))
                                .collect();
                            files.sort();
                            self.open_paths(&files);
                        }
                        if crate::icons::button(ui, crate::icons::IMPORT, "Import media…", "Ctrl+I", true).clicked()
                            && let Some(files) = rfd::FileDialog::new().add_filter("Media", crate::MEDIA_EXTENSIONS).pick_files()
                        {
                            self.open_paths(&files);
                        }
                        if crate::icons::button(ui, crate::icons::MIC, "Record audio…", "", true).clicked() {
                            self.open_recorder();
                        }
                    }
                    BinTab::Assets => {
                        if crate::icons::button(ui, crate::icons::FOLDER, "Open the assets folder", "", true).clicked() {
                            let dir = self.assets.dir.clone();
                            let _ = std::fs::create_dir_all(&dir);
                            #[cfg(windows)]
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                            #[cfg(not(windows))]
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        }
                        if crate::icons::button(ui, crate::icons::IMPORT, "Add files to the assets library…", "", true).clicked()
                            && let Some(files) = rfd::FileDialog::new().add_filter("Media", crate::MEDIA_EXTENSIONS).pick_files()
                        {
                            let folder = self.bin.asset_folder.clone();
                            for file in files {
                                if let Err(e) = self.assets.add(&file, &folder) {
                                    self.report_error(e);
                                }
                            }
                        }
                    }
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            let room = (ui.available_width() - 110.0).clamp(80.0, 320.0);
            ui.add(egui::TextEdit::singleline(&mut self.bin.search).hint_text("Search").desired_width(room));
            egui::ComboBox::from_id_salt("bin-sort").width(64.0).selected_text(self.bin.sort.label()).show_ui(ui, |ui| {
                for s in BinSort::ALL {
                    ui.selectable_value(&mut self.bin.sort, s, s.label());
                }
            });
            let arrow = if self.bin.descending { "⬇" } else { "⬆" };
            if ui.small_button(arrow).on_hover_text("Reverse the order").clicked() {
                self.bin.descending = !self.bin.descending;
            }
        });
        ui.separator();
        if self.bin.tab == BinTab::Assets {
            self.assets_panel(ui);
            return;
        }
        self.bin_folder_row(ui);

        // Work still running on worker threads: a loading card each.
        let loading: Vec<String> = self
            .opening
            .iter()
            .map(|(p, _)| format!("Opening {}…", p.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string())))
            .chain(self.imports.iter().map(|j| format!("Importing {}…", j.name)))
            .collect();
        let cards = self.bin_cards();
        if cards.is_empty() && loading.is_empty() {
            let text = if self.bin.search.trim().is_empty() { "Nothing imported yet — drop files here, or use Import." } else { "Nothing matches." };
            ui.label(egui::RichText::new(text).weak());
        }
        // The bin fills its panel, top to bottom.
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for text in &loading {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::hover());
                    let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
                    crate::widgets::skeleton(ui, ui.painter(), thumb);
                    let painter = ui.painter_at(rect);
                    painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 4.0), egui::Align2::LEFT_TOP, text, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
                }
                // Folders first, then what is in this one.
                if self.bin.naming.is_some() {
                    self.naming_card(ui);
                }
                if self.bin.search.trim().is_empty() {
                    for (path, count) in self.child_folders(&self.bin.folder.clone()) {
                        self.folder_card(ui, &path, count);
                    }
                }
                for card in cards {
                    self.bin_card(ui, card);
                }
            });
        });
    }

    fn bin_card(&mut self, ui: &mut egui::Ui, card: Card) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        painter.rect_filled(thumb, 4.0, visuals.extreme_bg_color);

        // The picture: skims with the pointer over a video.
        let mut drawn = false;
        if let Some((media, path, size, duration, rest)) = &card.picture {
            let at = match response.hover_pos() {
                Some(p) if *duration > 0.0 => ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0) as f64 * duration,
                _ => *rest,
            };
            if let Some(strip) = self.clip_previews.strip(&ctx, *media, path, *size, *duration) {
                // Fit the frame inside the card.
                let h = thumb.height().min(thumb.width() / strip.aspect.max(0.01));
                let dest = egui::Rect::from_center_size(thumb.center(), egui::vec2(h * strip.aspect, h));
                painter.image(strip.texture.id(), dest, strip.uv(at), egui::Color32::WHITE);
                drawn = true;
            } else if self.clip_previews.strip_pending(*media) {
                crate::widgets::skeleton(ui, &painter, thumb);
                drawn = true;
            }
        }
        if let Some((media, path)) = &card.sound
            && let Some(peaks) = self.clip_previews.peaks(&ctx, *media, path)
        {
            let n = peaks.len().max(1);
            let mid = thumb.center().y;
            let half = thumb.height() * 0.42;
            let color = egui::Color32::from_rgb(110, 190, 150);
            let mut x = thumb.left();
            while x < thumb.right() {
                let i = (((x - thumb.left()) / thumb.width()) * n as f32) as usize;
                let j = ((((x + 2.0 - thumb.left()) / thumb.width()) * n as f32) as usize).clamp(i + 1, n);
                let (lo, hi) = peaks[i.min(n - 1)..j].iter().fold((0.0f32, 0.0f32), |(a, b), (l, h)| (a.min(*l), b.max(*h)));
                painter.line_segment([egui::pos2(x, mid - hi * half), egui::pos2(x, mid - lo * half)], egui::Stroke::new(1.5, color));
                x += 2.0;
            }
            drawn = true;
        }
        if !drawn {
            let icon = if card.compound {
                "▣"
            } else {
                match card.kind {
                    MediaKind::Video => "🎞",
                    MediaKind::Still => "🖼",
                    MediaKind::Audio => "🔊",
                }
            };
            painter.text(thumb.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(24.0), visuals.weak_text_color());
        }
        if card.compound {
            painter.text(thumb.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "▣", egui::FontId::proportional(12.0), crate::style::GOLD);
        }
        // Length, bottom right of the picture.
        if card.kind != MediaKind::Still || card.compound {
            let text = clock(card.length);
            let pos = thumb.right_bottom() - egui::vec2(4.0, 3.0);
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(10.0), egui::Color32::WHITE);
            let bg = egui::Rect::from_min_size(pos - galley.size() - egui::vec2(3.0, 1.0), galley.size() + egui::vec2(6.0, 2.0));
            painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(150));
            painter.galley(bg.min + egui::vec2(3.0, 1.0), galley, egui::Color32::WHITE);
        }
        let border = if card.missing {
            egui::Stroke::new(1.5, egui::Color32::LIGHT_RED)
        } else if response.hovered() {
            egui::Stroke::new(1.5, visuals.selection.stroke.color)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(thumb, 4.0, border, egui::StrokeKind::Inside);

        // Sound gets a play button, so you can hear it before using it.
        let listen = egui::Rect::from_min_size(thumb.left_top() + egui::vec2(3.0, 3.0), egui::vec2(19.0, 19.0));
        let over_listen = card.sound.is_some() && response.hover_pos().is_some_and(|p| listen.contains(p));
        if let Some((_, path)) = &card.sound {
            let playing = self.auditioning(path);
            if response.hovered() || playing {
                painter.rect_filled(listen, 4.0, if over_listen { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
                let icon = if playing { crate::icons::STOP } else { crate::icons::PLAY };
                crate::icons::paint(&painter, listen.shrink(3.0), icon, egui::Color32::WHITE);
            }
        }

        // ＋ (on hover) adds it to the timeline.
        let plus = egui::Rect::from_min_size(thumb.right_top() + egui::vec2(-22.0, 3.0), egui::vec2(19.0, 19.0));
        let over_plus = response.hover_pos().is_some_and(|p| plus.contains(p));
        if response.hovered() && !card.missing {
            painter.rect_filled(plus, 4.0, if over_plus { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
            painter.text(plus.center(), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(15.0), egui::Color32::WHITE);
        }

        // Name and details.
        let name_color = if card.missing { egui::Color32::LIGHT_RED } else { visuals.text_color() };
        let line = |text: String, size: f32, color: egui::Color32| {
            let mut job = egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(size), color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
            painter.layout_job(job)
        };
        let name = line(card.name.clone(), 12.0, name_color);
        let name_h = name.size().y;
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), name, name_color);
        let detail = if card.missing { "missing".to_string() } else { card.detail.split(" · ").take(2).collect::<Vec<_>>().join(" · ") };
        let weak = visuals.weak_text_color();
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 3.0 + name_h), line(detail, 10.0, weak), weak);
        let mut tip = format!("{}\n{}", card.name, card.detail);
        if let Some(reason) = &card.conformed {
            tip.push_str(&format!("\nconverted for playback ({reason})"));
        }
        tip.push_str("\nDouble-click or ＋ to add to the timeline; right-click for more.");
        let response = response.on_hover_text(tip);

        if response.clicked() && over_listen && let Some((_, path)) = &card.sound {
            let (path, length) = (path.clone(), card.length);
            self.audition(&path, length);
        }
        let add = !card.missing && !over_listen && ((response.clicked() && over_plus) || response.double_clicked());
        if add {
            self.add_bin_entry(card.entry, card.length);
        }
        // Drag a card onto the timeline to put it there.
        if !card.missing && response.drag_started() {
            response.dnd_set_drag_payload(BinDrop { entry: card.entry, name: card.name.clone(), length: card.length, audio: card.kind == MediaKind::Audio && !card.compound });
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        let entry = card.entry;
        let (missing, length) = (card.missing, card.length);
        let is_audio = card.kind == MediaKind::Audio;
        crate::widgets::context_menu(&response, |ui| {
            if ui.add_enabled(!missing, egui::Button::new("Add to timeline")).clicked() {
                self.add_bin_entry(entry, length);
                ui.close();
            }
            match entry {
                Entry::Media(id) => {
                    if ui.button("Save to assets").on_hover_text("Keeps a copy on this computer, for every project").clicked() {
                        let path = self.editor.pool_item(id).map(|m| m.decode_path.clone());
                        if let Some(path) = path {
                            let folder = self.bin.asset_folder.clone();
                            match self.assets.add(&path, &folder) {
                                Ok(_) => self.notify("Saved to assets."),
                                Err(e) => self.report_error(e),
                            }
                        }
                        ui.close();
                    }
                    self.folder_menu(ui, id);
                    if !is_audio {
                        self.scaling_menu(ui, id);
                        if self.settings.advanced_color {
                            self.color_menu(ui, id);
                        }
                    }
                    if missing
                        && ui.button("Relink…").clicked()
                        && let Some(file) = rfd::FileDialog::new().add_filter("Media", crate::MEDIA_EXTENSIONS).pick_file()
                    {
                        self.relink(id, &file);
                        ui.close();
                    }
                }
                Entry::Compound(seq) => {
                    let used = self.editor.doc.project().sequences.keys().any(|&s| s != seq && self.editor.doc.project().sequence_reaches(s, seq));
                    if ui.add_enabled(!used, egui::Button::new("Delete")).on_disabled_hover_text("A clip on the timeline uses it").clicked() {
                        if let Err(e) = self.editor.remove_compound(seq) {
                            self.error = Some(e.to_string());
                        }
                        ui.close();
                    }
                }
            }
        });
    }

    /// A card dropped on the timeline at `at`, over `track` (if over one).
    pub(crate) fn drop_bin_entry(&mut self, drop: &BinDrop, at: Time, track: Option<oa_doc::TrackId>) {
        let kind = match drop.entry {
            Entry::Media(media) => ItemKind::Media { media },
            Entry::Compound(sequence) => ItemKind::Nested { sequence },
        };
        match self.editor.place_clip(&drop.name, kind, drop.length, at, track) {
            Ok(item) => {
                self.selection = Some(item);
                self.sync_selection();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// A card dropped onto clip `item`: its media goes into the clip, which keeps its
    /// place, length, transform, effects and keyframes (`oa_edit::swap`). One undo step.
    pub(crate) fn swap_bin_entry(&mut self, drop: &BinDrop, item: oa_doc::ItemId) {
        let (kind, length) = match drop.entry {
            Entry::Media(media) => {
                // A still has no end; a video or a sound, its length.
                let length = self.editor.pool_item(media).filter(|p| p.kind != MediaKind::Still).map(|p| p.probe.duration);
                (ItemKind::Media { media }, length)
            }
            Entry::Compound(sequence) => (ItemKind::Nested { sequence }, self.editor.doc.project().sequence(sequence).map(|s| s.duration())),
        };
        let ops = oa_edit::swap::swap_media(self.editor.doc.project(), self.editor.seq, item, kind, &drop.name, length);
        match ops.and_then(|ops| self.editor.apply(&format!("Swap in {}", drop.name), ops)) {
            Ok(()) => {
                self.selection = Some(item);
                self.sync_selection();
                self.notify(format!("Swapped in {} — the clip kept its place, transform and effects.", drop.name));
            }
            Err(e) => self.error = Some(format!("can't swap in {}: {e}", drop.name)),
        }
    }

    fn add_bin_entry(&mut self, entry: Entry, length: Time) {
        let added = match entry {
            Entry::Media(id) => self.editor.append_clip(id, length),
            Entry::Compound(seq) => self.editor.append_compound(seq),
        };
        match added {
            Ok(item) => self.selection = Some(item),
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}


impl App {
    /// The Assets tab: what's in the library on this computer. Cards work the way the
    /// project's do — a picture, a waveform, a ＋ to use it — except that using one
    /// imports it into the project first.
    fn assets_panel(&mut self, ui: &mut egui::Ui) {
        let here = self.bin.asset_folder.clone();
        if !here.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Assets").clicked() {
                    self.bin.asset_folder.clear();
                }
                let mut path = String::new();
                for part in here.split('/') {
                    ui.label("›");
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(part);
                    if ui.small_button(part).clicked() {
                        self.bin.asset_folder = path.clone();
                    }
                }
            });
        }
        if self.assets.items.is_empty() && self.assets.folders.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Nothing here yet. Right-click a clip in Media → Save to assets, or add files with the arrow above — they stay available in every project.",
                )
                .weak(),
            );
        }

        let folders = self.assets.child_folders(&here);
        let items: Vec<crate::assets::Asset> = self.assets.in_folder(&here).cloned().collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                if self.bin.naming.is_some() {
                    self.naming_card(ui);
                }
                for (path, count) in folders {
                    self.asset_folder_card(ui, &path, count);
                }
                for asset in items {
                    self.asset_card(ui, &asset);
                }
            });
        });
    }

    fn asset_folder_card(&mut self, ui: &mut egui::Ui, path: &str, count: usize) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(thumb, 4.0, visuals.faint_bg_color);
        crate::icons::paint(&painter, thumb.shrink(THUMB_H * 0.28), crate::icons::FOLDER, visuals.weak_text_color());
        let stroke = if response.hovered() {
            egui::Stroke::new(1.5, crate::style::ACCENT)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(thumb, 4.0, stroke, egui::StrokeKind::Inside);
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(12.0), visuals.text_color());
        painter.text(
            egui::pos2(rect.left() + 2.0, thumb.bottom() + 16.0),
            egui::Align2::LEFT_TOP,
            format!("{count} item{}", if count == 1 { "" } else { "s" }),
            egui::FontId::proportional(10.0),
            visuals.weak_text_color(),
        );
        if response.clicked() {
            self.bin.asset_folder = path.to_string();
        }
    }

    /// One asset: its picture, and a ＋ to bring it into this project.
    fn asset_card(&mut self, ui: &mut egui::Ui, asset: &crate::assets::Asset) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        painter.rect_filled(thumb, 4.0, visuals.extreme_bg_color);

        // The same previews the project's cards use; the cache is keyed by a hash of
        // the path, which can't collide with a project's own media ids.
        let id = asset.preview_id();
        let mut drawn = false;
        if asset.kind == MediaKind::Audio {
            if let Some(peaks) = self.clip_previews.peaks(&ctx, id, &asset.path) {
                let n = peaks.len().max(1);
                let mid = thumb.center().y;
                let half = thumb.height() * 0.42;
                let color = egui::Color32::from_rgb(110, 190, 150);
                let mut x = thumb.left();
                while x < thumb.right() {
                    let i = (((x - thumb.left()) / thumb.width()) * n as f32) as usize;
                    let j = ((((x + 2.0 - thumb.left()) / thumb.width()) * n as f32) as usize).clamp(i + 1, n);
                    let (lo, hi) = peaks[i.min(n - 1)..j].iter().fold((0.0f32, 0.0f32), |(a, b), (l, h)| (a.min(*l), b.max(*h)));
                    painter.line_segment([egui::pos2(x, mid - hi * half), egui::pos2(x, mid - lo * half)], egui::Stroke::new(1.5, color));
                    x += 2.0;
                }
                drawn = true;
            }
        } else if let Some(strip) = self.clip_previews.strip(&ctx, id, &asset.path, [640, 360], 0.0) {
            let h = thumb.height().min(thumb.width() / strip.aspect.max(0.01));
            let dest = egui::Rect::from_center_size(thumb.center(), egui::vec2(h * strip.aspect, h));
            painter.image(strip.texture.id(), dest, strip.uv(0.0), egui::Color32::WHITE);
            drawn = true;
        } else if self.clip_previews.strip_pending(id) {
            crate::widgets::skeleton(ui, &painter, thumb);
            drawn = true;
        }
        if !drawn {
            let icon = if asset.kind == MediaKind::Audio { crate::icons::AUDIO_TRACK } else { crate::icons::VIDEO_TRACK };
            crate::icons::paint(&painter, thumb.shrink(THUMB_H * 0.3), icon, visuals.weak_text_color());
        }
        let border = if response.hovered() {
            egui::Stroke::new(1.5, crate::style::ACCENT)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(thumb, 4.0, border, egui::StrokeKind::Inside);

        let plus = egui::Rect::from_min_size(thumb.right_top() + egui::vec2(-22.0, 3.0), egui::vec2(19.0, 19.0));
        let over_plus = response.hover_pos().is_some_and(|p| plus.contains(p));
        if response.hovered() {
            painter.rect_filled(plus, 4.0, if over_plus { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
            crate::icons::paint(&painter, plus.shrink(3.0), crate::icons::ADD, egui::Color32::WHITE);
        }
        // Sound plays where it is, without joining the project first.
        let listen = egui::Rect::from_min_size(thumb.left_top() + egui::vec2(3.0, 3.0), egui::vec2(19.0, 19.0));
        let playing = self.auditioning(&asset.path);
        let over_listen = asset.kind == MediaKind::Audio && response.hover_pos().is_some_and(|p| listen.contains(p));
        if asset.kind == MediaKind::Audio && (response.hovered() || playing) {
            painter.rect_filled(listen, 4.0, if over_listen { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
            let icon = if playing { crate::icons::STOP } else { crate::icons::PLAY };
            crate::icons::paint(&painter, listen.shrink(3.0), icon, egui::Color32::WHITE);
        }
        let mut job = egui::text::LayoutJob::simple_singleline(asset.name.clone(), egui::FontId::proportional(12.0), visuals.text_color());
        job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), painter.layout_job(job), visuals.text_color());
        let what = match asset.kind {
            MediaKind::Audio => "sound",
            MediaKind::Still => "picture",
            MediaKind::Video => "video",
        };
        painter.text(
            egui::pos2(rect.left() + 2.0, thumb.bottom() + 17.0),
            egui::Align2::LEFT_TOP,
            what,
            egui::FontId::proportional(10.0),
            visuals.weak_text_color(),
        );

        let response = response.on_hover_text(format!("{}\nDouble-click or ＋ to use it in this project", asset.path.display()));
        if response.clicked() && over_listen {
            self.audition(&asset.path, Time::ZERO);
        } else if (response.clicked() && over_plus) || response.double_clicked() {
            self.use_asset(asset);
        }
        let asset = asset.clone();
        crate::widgets::context_menu(&response, |ui| {
            if ui.button("Use in this project").clicked() {
                self.use_asset(&asset);
                ui.close();
            }
            if ui.button("Show in folder").clicked() {
                let dir = asset.path.parent().unwrap_or(&asset.path).to_path_buf();
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                #[cfg(not(windows))]
                let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                ui.close();
            }
            ui.separator();
            if ui
                .button("Remove from assets")
                .on_hover_text("Deletes this copy; whatever you copied it from is untouched")
                .clicked()
            {
                if let Err(e) = self.assets.remove(&asset.path) {
                    self.report_error(e);
                }
                ui.close();
            }
        });
    }

    /// Imports an asset into the project, the same way dropping the file in would.
    fn use_asset(&mut self, asset: &crate::assets::Asset) {
        self.open_paths(std::slice::from_ref(&asset.path));
    }
}
