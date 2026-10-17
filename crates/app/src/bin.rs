//! The media bin: imported files and compound clips (groups made into media) as cards
//! with a picture — hover a video to skim it — or a waveform for sound, or as a list with
//! details. Search by words, narrow with smart filters (kind, unused, missing),
//! sort by when it was added, name, type or length. Click to pick (Ctrl adds, Shift picks
//! a run); double-click or ＋ adds to the timeline, ＋ with scissors opens quick cut
//! (`quick_cut.rs`); right-click for more. Missing files show a badge and relink; folders
//! show a mosaic of what's inside and skim through it.

use crate::i18n::{tr, trf};
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
            BinSort::Added => tr("Added"),
            BinSort::Name => tr("Name"),
            BinSort::Kind => tr("Type"),
            BinSort::Length => tr("Length"),
        }
    }
}

/// Which library the bin is showing.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum BinTab {
    /// What this project has imported.
    #[default]
    Project,
    /// The project's compound clips.
    Compounds,
    /// The things kept on this computer, for every project (see `assets.rs`).
    Assets,
}

/// A smart filter: what kind of thing, or what state it's in.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum BinFilter {
    #[default]
    All,
    Video,
    Audio,
    Pictures,
    /// On no timeline yet.
    Unused,
    /// Its file can't be found.
    Missing,
}

impl BinFilter {
    const ALL: [BinFilter; 6] = [BinFilter::All, BinFilter::Video, BinFilter::Audio, BinFilter::Pictures, BinFilter::Unused, BinFilter::Missing];

    fn label(self) -> &'static str {
        match self {
            BinFilter::All => tr("All"),
            BinFilter::Video => tr("Video"),
            BinFilter::Audio => tr("Audio"),
            BinFilter::Pictures => tr("Pictures"),
            BinFilter::Unused => tr("Unused"),
            BinFilter::Missing => tr("Missing"),
        }
    }

    fn icon(self) -> crate::icons::Icon {
        use crate::icons;
        match self {
            BinFilter::All => icons::EVERYTHING,
            BinFilter::Video => icons::VIDEO_TRACK,
            BinFilter::Audio => icons::AUDIO_TRACK,
            BinFilter::Pictures => icons::PICTURE,
            BinFilter::Unused => icons::UNUSED,
            BinFilter::Missing => icons::WARNING,
        }
    }

    fn tip(self) -> &'static str {
        match self {
            BinFilter::All => tr("Everything in this folder"),
            BinFilter::Video => tr("Video files, everywhere in the bin"),
            BinFilter::Audio => tr("Sound files, everywhere in the bin"),
            BinFilter::Pictures => tr("Pictures, everywhere in the bin"),
            BinFilter::Unused => tr("What isn't on any timeline yet"),
            BinFilter::Missing => tr("Files that can't be found: relink them"),
        }
    }

    /// Whether `card` passes.
    fn keeps(self, card: &Card) -> bool {
        match self {
            BinFilter::All => true,
            BinFilter::Video => !card.compound && card.kind == MediaKind::Video,
            BinFilter::Audio => !card.compound && card.kind == MediaKind::Audio,
            BinFilter::Pictures => !card.compound && card.kind == MediaKind::Still,
            BinFilter::Unused => card.uses == 0,
            BinFilter::Missing => card.missing,
        }
    }
}

#[derive(Default)]
pub struct BinView {
    pub tab: BinTab,
    /// The smart filter in use (anything but All looks in every folder).
    pub filter: BinFilter,
    /// Rows with details instead of picture cards.
    pub list: bool,
    /// The cards picked (Ctrl-click adds, Shift-click a run), in the order picked.
    pub(crate) picked: Vec<Entry>,
    /// Where a Shift-click run starts.
    pub(crate) anchor: Option<Entry>,
    /// The quick cut window, while it's open.
    pub(crate) quick_cut: Option<crate::quick_cut::QuickCut>,
    /// The pointer was over the bin last frame: its keys (Delete, Ctrl+A, Esc) are its own.
    pub(crate) hovered: bool,
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

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Entry {
    Media(MediaId),
    Compound(SeqId),
}

/// What a search for missing files found (where each is now), and how many it looked for.
pub(crate) type Found = (Vec<(MediaId, std::path::PathBuf)>, usize);

/// What a card being dragged carries.
#[derive(Clone)]
pub struct BinDrop {
    entry: Entry,
    pub name: String,
    pub length: Time,
    /// It goes on an audio track.
    pub audio: bool,
    /// The other cards picked with it, dragged along: they go after it.
    pub rest: Vec<BinDrop>,
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

/// What the pointer is over on a card or row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    None,
    Listen,
    Plus,
    Cut,
    Relink,
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
    /// How many clips use it, on every timeline.
    uses: usize,
}

fn clock(t: Time) -> String {
    let s = t.as_seconds_f64().max(0.0);
    format!("{}:{:02}", (s / 60.0).floor() as u64, (s % 60.0).floor() as u64)
}

impl App {
    /// Every card of the tab being shown, wherever it is.
    fn tab_cards(&self) -> Vec<Card> {
        let (uses, compound_uses) = self.editor.media_uses();
        let project = self.editor.doc.project();
        let mut cards: Vec<Card> = self
            .editor
            .pool
            .iter()
            // Taken out of the project (undo brings it back): not shown.
            .filter(|m| project.media(m.id).is_some())
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
                    uses: uses.get(&m.id).copied().unwrap_or(0),
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
                uses: compound_uses.get(&s.id).copied().unwrap_or(0),
            });
        }
        // Media and compound clips each have their own tab.
        let compounds = self.bin.tab == BinTab::Compounds;
        cards.retain(|c| c.compound == compounds);
        cards
    }

    /// The cards shown: this tab's, through the smart filter and the search, in this
    /// folder (unless searching or filtering), in the order chosen.
    fn bin_cards(&self) -> Vec<Card> {
        let compounds = self.bin.tab == BinTab::Compounds;
        let mut cards = self.tab_cards();
        // A smart filter keeps what passes it.
        let filter = self.bin.filter;
        cards.retain(|c| filter.keeps(c));
        // Searching or filtering looks everywhere; otherwise you see the folder you're in.
        let needle = self.bin.search.trim().to_lowercase();
        if needle.is_empty() && filter == BinFilter::All && !compounds {
            let here = self.bin.folder.clone();
            cards.retain(|c| c.folder == here);
        } else {
            // Every word somewhere in its name or what it is ("mp4", "4K", "0:12").
            cards.retain(|c| matches(&needle, &[&c.name, &c.detail]));
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
            self.report_error(tr("A folder name can't contain \"/\"."));
            return false;
        }
        let here = self.bin.folder.clone();
        let path = if here.is_empty() { name } else { format!("{here}/{name}") };
        let mut folders = self.editor.doc.project().bin_folders.clone();
        if self.bin_folders().contains(&path) {
            self.report_error(trf("There's already a folder called {path}.", &[("path", &path.to_string())]));
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

    /// Moves files into `folder` ("" is the top), as one undo step.
    fn move_all_to_folder(&mut self, media: &[MediaId], folder: &str) {
        let ops = media.iter().map(|&media| oa_doc::Op::SetMediaFolder { media, folder: folder.to_string() }).collect();
        if let Err(e) = self.editor.apply(tr("Move to folder"), ops) {
            self.report_error(e.to_string());
        }
    }

    /// Menu entries for putting `media` somewhere: the top, an existing folder, or a new
    /// one typed in (made inside the folder being shown).
    fn folder_menu(&mut self, ui: &mut egui::Ui, media: &[MediaId]) {
        let Some(&first) = media.first() else { return };
        let current = self.editor.doc.project().media(first).map(|m| m.folder.clone()).unwrap_or_default();
        ui.menu_button(tr("Move to folder"), |ui| {
            if ui.radio(current.is_empty(), "Media (top)").clicked() {
                self.move_all_to_folder(media, "");
                ui.close();
            }
            for f in self.bin_folders() {
                if ui.radio(current == f, &f).clicked() {
                    self.move_all_to_folder(media, &f);
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
            if ui.small_button(tr("Media")).clicked() {
                self.bin.folder.clear();
            }
            let mut path = String::new();
            for part in here.split('/') {
                ui.label(tr("›"));
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
        let r = child.add(egui::TextEdit::singleline(&mut name).hint_text(tr("Folder name")).desired_width(CARD_W - 4.0));
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
                BinTab::Project | BinTab::Compounds => self.new_folder(&name),
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

    /// Pictures of what's in folder `path` (folders inside it too), at most `max`: each
    /// file with a picture, and where in it to show.
    fn folder_pictures(&self, path: &str, max: usize) -> Vec<(MediaId, PathBuf, [u32; 2], f64, f64)> {
        let inside = format!("{path}/");
        let project = self.editor.doc.project();
        self.editor
            .pool
            .iter()
            .filter(|m| !m.missing)
            .filter(|m| project.media(m.id).is_some_and(|r| r.folder == path || r.folder.starts_with(&inside)))
            .filter_map(|m| {
                let v = m.probe.video.as_ref()?;
                let d = if m.kind == MediaKind::Still { 0.0 } else { m.probe.duration.as_seconds_f64() };
                Some((m.id, m.decode_path.clone(), [v.width, v.height], d, d * 0.1))
            })
            .take(max)
            .collect()
    }

    /// A folder's face in `thumb`: a mosaic of what's in it (up to four), or — with the
    /// pointer over it — one of them at a time, chosen by where the pointer is across, with
    /// dots for which. Empty, or nothing with a picture: the folder icon.
    fn paint_folder_face(&mut self, ui: &egui::Ui, painter: &egui::Painter, thumb: egui::Rect, path: &str, hover: Option<egui::Pos2>) {
        let ctx = ui.ctx().clone();
        let visuals = ui.visuals().clone();
        painter.rect_filled(thumb, 4.0, visuals.faint_bg_color);
        let pictures = self.folder_pictures(path, 12);
        let draw = |app: &mut Self, cell: egui::Rect, p: &(MediaId, PathBuf, [u32; 2], f64, f64)| -> bool {
            let scaling = app.media_scaling(p.0);
            let Some(strip) = app.clip_previews.strip(&ctx, p.0, &p.1, p.2, p.3, scaling) else { return false };
            // Filled (cropped to the cell), like a cover.
            let (cw, ch) = (cell.width(), cell.height());
            let uv = strip.uv(p.4);
            let (aspect, cell_aspect) = (strip.aspect, cw / ch.max(1.0));
            let uv = if aspect > cell_aspect {
                let keep = cell_aspect / aspect;
                let w = uv.width() * keep;
                egui::Rect::from_min_size(egui::pos2(uv.center().x - w / 2.0, uv.top()), egui::vec2(w, uv.height()))
            } else {
                let keep = aspect / cell_aspect;
                let h = uv.height() * keep;
                egui::Rect::from_min_size(egui::pos2(uv.left(), uv.center().y - h / 2.0), egui::vec2(uv.width(), h))
            };
            painter.image(strip.texture.id(), cell, uv, egui::Color32::WHITE);
            true
        };
        let hovering = hover.filter(|p| thumb.contains(*p));
        if pictures.is_empty() {
            crate::icons::paint(painter, thumb.shrink(thumb.height() * 0.28), crate::icons::FOLDER, visuals.weak_text_color());
        } else if let Some(p) = hovering {
            // Skimming: one at a time, by where the pointer is.
            let n = pictures.len();
            let i = (((p.x - thumb.left()) / thumb.width()) * n as f32).floor().clamp(0.0, (n - 1) as f32) as usize;
            draw(self, thumb, &pictures[i]);
            if n > 1 {
                let dot = 5.0f32.min((thumb.width() - 16.0) / n as f32 - 2.0).max(2.0);
                let total = n as f32 * (dot + 3.0) - 3.0;
                let mut x = thumb.center().x - total / 2.0 + dot / 2.0;
                for k in 0..n {
                    let c = egui::pos2(x, thumb.bottom() - 7.0);
                    painter.circle_filled(c, dot / 2.0 + 1.0, egui::Color32::from_black_alpha(120));
                    painter.circle_filled(c, dot / 2.0, if k == i { egui::Color32::WHITE } else { egui::Color32::from_white_alpha(110) });
                    x += dot + 3.0;
                }
            }
        } else {
            // A mosaic: 1 fills it, 2 side by side, 3 or 4 in a square.
            let cells: Vec<egui::Rect> = match pictures.len() {
                1 => vec![thumb],
                2 => {
                    let w = thumb.width() / 2.0;
                    (0..2).map(|k| egui::Rect::from_min_size(thumb.min + egui::vec2(k as f32 * w, 0.0), egui::vec2(w, thumb.height()))).collect()
                }
                _ => {
                    let (w, h) = (thumb.width() / 2.0, thumb.height() / 2.0);
                    (0..4).map(|k| egui::Rect::from_min_size(thumb.min + egui::vec2((k % 2) as f32 * w, (k / 2) as f32 * h), egui::vec2(w, h))).collect()
                }
            };
            for (cell, p) in cells.iter().zip(&pictures) {
                draw(self, cell.shrink(0.5), p);
            }
            painter.rect_filled(thumb, 4.0, egui::Color32::from_black_alpha(40));
        }
        // The folder sign, so it reads as a folder over its pictures.
        let tag = egui::Rect::from_min_size(thumb.left_top() + egui::vec2(4.0, 4.0), egui::vec2(20.0, 18.0));
        if !pictures.is_empty() {
            painter.rect_filled(tag, 4.0, egui::Color32::from_black_alpha(170));
            crate::icons::paint(painter, tag.shrink(2.0), crate::icons::FOLDER, egui::Color32::WHITE);
        }
        if hovering.is_some() {
            ui.ctx().request_repaint();
        }
    }

    /// One folder card: click to open it, drop cards on it to move those files in.
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
        self.paint_folder_face(ui, &painter, thumb, path, response.hover_pos());
        let stroke = if taking_drop {
            egui::Stroke::new(2.0, crate::style::ACCENT)
        } else if response.hovered() {
            egui::Stroke::new(1.5, visuals.selection.stroke.color)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(thumb, 4.0, stroke, egui::StrokeKind::Inside);
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), egui::Align2::LEFT_TOP, &name, egui::FontId::proportional(12.0), visuals.text_color());
        let n = format!("{count} item{}", if count == 1 { "" } else { "s" });
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 16.0), egui::Align2::LEFT_TOP, n, egui::FontId::proportional(10.0), visuals.weak_text_color());
        self.folder_interact(&response, path);
    }

    /// One folder as a row of the list view.
    fn folder_row(&mut self, ui: &mut egui::Ui, path: &str, count: usize) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        let taking_drop = response.contains_pointer() && egui::DragAndDrop::has_payload_of_type::<BinDrop>(ui.ctx());
        if taking_drop {
            painter.rect(rect, 5.0, crate::style::ACCENT.gamma_multiply(0.16), egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        } else if response.hovered() {
            painter.rect_filled(rect, 5.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let thumb = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 3.0), egui::vec2(56.0, rect.height() - 6.0));
        self.paint_folder_face(ui, &painter, thumb, path, response.hover_pos());
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        painter.text(egui::pos2(thumb.right() + 8.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, &name, egui::FontId::proportional(crate::style::TEXT), visuals.text_color());
        let n = format!("{count} item{}", if count == 1 { "" } else { "s" });
        painter.text(egui::pos2(thumb.right() + 8.0, rect.bottom() - 4.0), egui::Align2::LEFT_BOTTOM, n, egui::FontId::proportional(crate::style::TEXT_S), visuals.weak_text_color());
        self.folder_interact(&response, path);
    }

    /// A folder card's or row's clicks, drops and menu.
    fn folder_interact(&mut self, response: &egui::Response, path: &str) {
        if response.clicked() {
            self.bin.folder = path.to_string();
            self.bin.picked.clear();
        }
        if let Some(drop) = response.dnd_release_payload::<BinDrop>() {
            let media: Vec<MediaId> = std::iter::once(&*drop).chain(drop.rest.iter()).filter_map(|d| if let Entry::Media(id) = d.entry { Some(id) } else { None }).collect();
            self.move_all_to_folder(&media, path);
        }
        let path = path.to_string();
        crate::widgets::context_menu(response, |ui| {
            if ui.button(tr("Open")).clicked() {
                self.bin.folder = path.clone();
                ui.close();
            }
            if ui.button(tr("Delete")).on_hover_text(tr("Puts everything inside it back at the top")).clicked() {
                self.remove_folder(&path);
                ui.close();
            }
        });
    }

    pub(crate) fn media_panel(&mut self, ui: &mut egui::Ui) {
        // Nothing in here may ask for more width than the panel has, or the panel grows
        // to fit it and the viewer loses the room.
        ui.set_max_width(ui.available_width());
        // The tabs run down the left edge, so nothing on the top row can run into them.
        let full = ui.available_rect_before_wrap();
        // An effect or group dragged over the bin: it turns to Assets, and letting go
        // there keeps it as a preset (the inspector takes the drop).
        if self.effect_drag.is_some() && ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| full.contains(p)) {
            self.bin.tab = BinTab::Assets;
            self.assets_drop = Some((full, ui.ctx().cumulative_pass_nr()));
            ui.painter().rect_stroke(full, 4.0, egui::Stroke::new(2.0, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        let strip = egui::Rect::from_min_max(full.min, egui::pos2(full.left() + TAB_STRIP, full.bottom()));
        let body = egui::Rect::from_min_max(egui::pos2(strip.right() + crate::style::GAP_S, full.top()), full.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(strip).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
            let compounds = self.editor.compounds().len();
            let tabs = [
                (BinTab::Project, tr("Media").to_string(), tr("What this project has imported")),
                (BinTab::Compounds, trf("Compounds ({n})", &[("n", &compounds.to_string())]), tr("This project's compound clips")),
                (BinTab::Assets, tr("Assets").to_string(), tr("Files kept on this computer, for every project")),
            ];
            crate::widgets::side_tabs(ui, &mut self.bin.tab, &tabs);
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)), |ui| self.media_body(ui));
    }

    /// The bin beside its tabs: the tab's buttons, search and sorting, and its cards.
    fn media_body(&mut self, ui: &mut egui::Ui) {
        self.bin.hovered = false;
        ui.set_max_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Folders: a new one, or (in Media) a folder from disk with its folders.
                if self.bin.tab != BinTab::Compounds {
                    let r = crate::icons::button(ui, crate::icons::NEW_FOLDER, tr("Folders"), "", true);
                    egui::Popup::menu(&r).show(|ui| {
                        if crate::icons::menu_item(ui, Some(crate::icons::NEW_FOLDER), tr("New folder"), "", true).clicked() {
                            self.bin.naming = Some(String::new());
                            self.bin.naming_fresh = true;
                            ui.close();
                        }
                        match self.bin.tab {
                            BinTab::Project => {
                                if crate::icons::menu_item(ui, Some(crate::icons::FOLDER_OPEN), tr("Import a folder (its folders come in as bin folders)…"), "", true).clicked() {
                                    ui.close();
                                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                        self.open_paths(&[dir]);
                                    }
                                }
                            }
                            BinTab::Assets => {
                                if crate::icons::menu_item(ui, Some(crate::icons::FOLDER), tr("Open the assets folder"), "", true).clicked() {
                                    ui.close();
                                    let dir = self.assets.dir.clone();
                                    let _ = std::fs::create_dir_all(&dir);
                                    #[cfg(windows)]
                                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                                    #[cfg(not(windows))]
                                    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                                }
                            }
                            BinTab::Compounds => {}
                        }
                    });
                }
                match self.bin.tab {
                    BinTab::Compounds => {}
                    BinTab::Project => {
                        if crate::icons::button(ui, crate::icons::IMPORT, "Import media…", "Ctrl+I", true).clicked()
                            && let Some(files) = rfd::FileDialog::new().add_filter(tr("Media"), crate::MEDIA_EXTENSIONS).pick_files()
                        {
                            self.open_paths(&files);
                        }
                        if crate::icons::button(ui, crate::icons::MIC, "Record audio…", "", true).clicked() {
                            self.open_recorder();
                        }
                        if crate::icons::button(ui, crate::icons::TITLE, tr("Add a title"), "Ctrl+T", true).clicked() {
                            self.add_text();
                        }
                    }
                    BinTab::Assets => {
                        if crate::icons::button(ui, crate::icons::IMPORT, tr("Add files to the assets library…"), "", true).clicked()
                            && let Some(files) = rfd::FileDialog::new().add_filter(tr("Media"), crate::MEDIA_EXTENSIONS).pick_files()
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
        // One row: the search filling what the order (on the right) leaves.
        ui.add_space(crate::style::GAP_S);
        ui.horizontal(|ui| {
            let views = self.bin.tab != BinTab::Assets;
            let buttons = if views { 3.0 } else { 2.0 };
            let tools = buttons * (crate::style::ICON + ui.spacing().item_spacing.x);
            let hint = match self.bin.tab {
                BinTab::Project => tr("Search media"),
                BinTab::Compounds => tr("Search compound clips"),
                BinTab::Assets => tr("Search assets"),
            };
            crate::widgets::search_field(ui, &mut self.bin.search, hint, ui.available_width() - tools);
            // The order, from a menu; which way, a button; cards or rows, a button.
            let r = crate::icons::button(ui, crate::icons::SORT, &trf("Order: {0}", &[("0", self.bin.sort.label())]), "", true);
            egui::Popup::menu(&r).show(|ui| {
                for s in BinSort::ALL {
                    if ui.radio(self.bin.sort == s, s.label()).clicked() {
                        self.bin.sort = s;
                        ui.close();
                    }
                }
            });
            let (arrow, tip) = if self.bin.descending { (crate::icons::ARROW_DOWN, tr("Last first: click for first first")) } else { (crate::icons::ARROW_UP, tr("First first: click for last first")) };
            if crate::icons::button(ui, arrow, tip, "", true).clicked() {
                self.bin.descending = !self.bin.descending;
            }
            if views {
                let (icon, tip) = if self.bin.list { (crate::icons::GRID_VIEW, tr("Show as cards")) } else { (crate::icons::LIST_VIEW, tr("Show as a list, with details")) };
                if crate::icons::button(ui, icon, tip, "", true).clicked() {
                    self.bin.list = !self.bin.list;
                }
            }
        });
        // Smart filters: what kind, and what state (unused, missing).
        if self.bin.tab == BinTab::Project {
            ui.add_space(4.0);
            self.bin_filters(ui);
        }
        ui.add_space(crate::style::GAP_S);
        if self.bin.tab == BinTab::Assets {
            self.assets_panel(ui);
            return;
        }
        // Compound clips have a tab of their own, without folders.
        let compounds = self.bin.tab == BinTab::Compounds;
        if !compounds {
            self.bin_folder_row(ui);
        }

        // Work still running on worker threads: a loading card each.
        let loading: Vec<String> = if compounds {
            Vec::new()
        } else {
            self.opening
                .iter()
                .map(|(p, _)| format!("Opening {}…", p.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string())))
                .chain(self.imports.iter().map(|j| format!("Importing {}…", j.name)))
                .collect()
        };
        let cards = self.bin_cards();
        if cards.is_empty() && loading.is_empty() {
            let text = match (self.bin.search.trim().is_empty(), compounds) {
                (false, _) => tr("Nothing matches."),
                (true, true) => tr("No compound clips yet — select clips on the timeline and group them into one."),
                (true, false) => tr("Nothing imported yet — drop files or folders here, or use Import."),
            };
            ui.label(egui::RichText::new(text).weak());
        }
        // What a Shift-click run spans: the cards as shown.
        let order: Vec<Entry> = cards.iter().map(|c| c.entry).collect();
        // Keys over the bin: Delete removes what's picked (undoable), Esc lets go of it,
        // Ctrl+A picks everything shown.
        self.bin.hovered = ui.rect_contains_pointer(ui.max_rect());
        if self.bin.hovered && !ui.ctx().egui_wants_keyboard_input() {
            let (delete, escape, all) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::Delete), i.consume_key(egui::Modifiers::NONE, egui::Key::Escape), i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)));
            if delete && !self.bin.picked.is_empty() {
                let picked = self.bin.picked.clone();
                self.remove_entries(&picked);
            }
            if escape {
                self.bin.picked.clear();
            }
            if all {
                self.bin.picked = order.clone();
            }
        }
        self.bin.picked.retain(|e| order.contains(e) || matches!(e, Entry::Media(id) if self.editor.doc.project().media(*id).is_some()));
        // As a list: folders, then a row per file.
        if self.bin.list && self.bin.tab != BinTab::Assets {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.bin_background(ui);
                ui.spacing_mut().item_spacing.y = 2.0;
                for text in &loading {
                    ui.label(egui::RichText::new(text).weak());
                }
                if self.bin.naming.is_some() && !compounds {
                    self.naming_card(ui);
                }
                if self.bin.search.trim().is_empty() && self.bin.filter == BinFilter::All && !compounds {
                    for (path, count) in self.child_folders(&self.bin.folder.clone()) {
                        self.folder_row(ui, &path, count);
                    }
                }
                for card in cards {
                    self.bin_row(ui, card, &order);
                }
            });
            return;
        }
        // The bin fills its panel, top to bottom.
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.bin_background(ui);
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
                if self.bin.naming.is_some() && !compounds {
                    self.naming_card(ui);
                }
                if self.bin.search.trim().is_empty() && self.bin.filter == BinFilter::All && !compounds {
                    for (path, count) in self.child_folders(&self.bin.folder.clone()) {
                        self.folder_card(ui, &path, count);
                    }
                }
                for card in cards {
                    self.bin_card(ui, card, &order);
                }
            });
        });
    }

    /// The bin's empty space: a click there lets go of the picked files. Taken first, so
    /// the cards and folders drawn over it get their own clicks.
    fn bin_background(&mut self, ui: &mut egui::Ui) {
        // The cards' own area only: the clip rect reaches over the tabs beside it.
        let area = ui.max_rect().intersect(ui.clip_rect());
        let bg = ui.interact(area, ui.id().with("bin-background"), egui::Sense::click());
        if bg.clicked() && !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
            self.bin.picked.clear();
        }
    }

    /// How a file's pixels are scaled (its Scale mode in the bin's right-click menu).
    pub(crate) fn media_scaling(&self, media: MediaId) -> oa_doc::MediaScaling {
        self.editor.doc.project().media(media).map_or(oa_doc::MediaScaling::Auto, |m| m.scaling)
    }

    /// A card's picture in `thumb`: a compound clip's frames, a file's (skimmed with the
    /// pointer at `hover`), a waveform for sound, or an icon.
    fn paint_card_picture(&mut self, ui: &egui::Ui, painter: &egui::Painter, thumb: egui::Rect, card: &Card, hover: Option<egui::Pos2>) {
        let ctx = ui.ctx().clone();
        let visuals = ui.visuals().clone();
        painter.rect_filled(thumb, 4.0, visuals.extreme_bg_color);
        let skim = |duration: f64, rest: f64| match hover.filter(|p| thumb.contains(*p)) {
            Some(p) if duration > 0.0 => ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0) as f64 * duration,
            _ => rest,
        };
        let mut drawn = false;
        // A compound clip: its own frames, rendered from the timeline inside it.
        if let Entry::Compound(seq) = card.entry {
            let duration = card.length.as_seconds_f64();
            let (picture, sound) = self.compound_look(seq);
            if !picture {
                // Sound alone: its waveform across the card, and a note.
                let band = thumb.shrink2(egui::vec2(6.0, (thumb.height() * 0.2).min(14.0)));
                self.paint_compound_sound(&ctx, painter, &sound, band, |x| ((x - band.left()) / band.width().max(1.0)) as f64 * duration);
                drawn = true;
            } else if let Some(strip) = self.compound_strip(&ctx, seq) {
                let at = skim(duration, duration * 0.1);
                let h = thumb.height().min(thumb.width() / strip.aspect.max(0.01));
                let dest = egui::Rect::from_center_size(thumb.center(), egui::vec2(h * strip.aspect, h));
                painter.image(strip.texture.id(), dest, strip.uv(at), egui::Color32::WHITE);
                drawn = true;
            } else if self.clip_previews.compound_pending(seq) {
                crate::widgets::skeleton(ui, painter, thumb);
                drawn = true;
            }
        }
        if let Some((media, path, size, duration, rest)) = card.picture.as_ref().filter(|_| !drawn) {
            let at = skim(*duration, *rest);
            let scaling = self.media_scaling(*media);
            if let Some(strip) = self.clip_previews.strip(&ctx, *media, path, *size, *duration, scaling) {
                let h = thumb.height().min(thumb.width() / strip.aspect.max(0.01));
                let dest = egui::Rect::from_center_size(thumb.center(), egui::vec2(h * strip.aspect, h));
                painter.image(strip.texture.id(), dest, strip.uv(at), egui::Color32::WHITE);
                drawn = true;
            } else if self.clip_previews.strip_pending(*media) {
                crate::widgets::skeleton(ui, painter, thumb);
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
            let icon = match (card.compound, card.kind) {
                (true, _) => crate::icons::GROUP,
                (_, MediaKind::Video) => crate::icons::VIDEO_TRACK,
                (_, MediaKind::Still) => crate::icons::PICTURE,
                (_, MediaKind::Audio) => crate::icons::AUDIO_TRACK,
            };
            let side = (thumb.height() * 0.4).min(28.0);
            crate::icons::paint(painter, egui::Rect::from_center_size(thumb.center(), egui::vec2(side, side)), icon, visuals.weak_text_color());
        }
        if card.missing {
            painter.rect_filled(thumb, 4.0, egui::Color32::from_black_alpha(150));
        }
    }

    /// The Missing badge in a picture's corner: a red pill with a warning sign.
    fn paint_missing_badge(painter: &egui::Painter, corner: egui::Pos2, small: bool) {
        let text = if small { String::new() } else { tr("Missing").to_string() };
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(10.0), egui::Color32::WHITE);
        let w = 16.0 + if small { 0.0 } else { galley.size().x + 4.0 };
        let r = egui::Rect::from_min_size(corner, egui::vec2(w + 4.0, 16.0));
        painter.rect_filled(r, 8.0, crate::style::ERROR);
        crate::icons::paint(painter, egui::Rect::from_min_size(r.min + egui::vec2(3.0, 2.0), egui::vec2(12.0, 12.0)), crate::icons::WARNING, egui::Color32::WHITE);
        if !small {
            painter.galley(egui::pos2(r.left() + 17.0, r.center().y - galley.size().y / 2.0), galley, egui::Color32::WHITE);
        }
    }

    /// What clicks on a card or row do, given what was under the pointer.
    fn card_clicks(&mut self, ui: &egui::Ui, response: &egui::Response, card: &Card, order: &[Entry], hit: Hit) {
        let entry = card.entry;
        if response.clicked() {
            match hit {
                Hit::Listen => {
                    if let Some((_, path)) = &card.sound {
                        let (path, length) = (path.clone(), card.length);
                        self.audition(&path, length);
                    }
                }
                Hit::Relink => {
                    if let Entry::Media(id) = entry {
                        self.ask_relink(id);
                    }
                }
                Hit::Cut => {
                    if let Entry::Media(id) = entry {
                        self.open_quick_cut(id);
                    }
                }
                Hit::Plus => {
                    let acting = self.acting_on(entry, order);
                    self.add_entries(&acting);
                }
                // The second click of a double-click adds it (below), leaving it picked.
                Hit::None if !response.double_clicked() => {
                    let mods = ui.input(|i| i.modifiers);
                    self.pick_card(entry, order, mods);
                }
                Hit::None => {}
            }
        }
        if response.double_clicked() && hit == Hit::None {
            let acting = self.acting_on(entry, order);
            self.add_entries(&acting);
        }
        // Right-clicking a card that isn't picked picks it alone.
        if response.secondary_clicked() && !self.bin.picked.contains(&entry) {
            self.bin.picked = vec![entry];
            self.bin.anchor = Some(entry);
        }
        // Dragged onto the timeline, the viewer or a folder: it, and every other card
        // picked if it's one of them. Dragging doesn't pick it (picking is a click, on
        // release).
        if response.drag_started() {
            let acting = self.acting_on(entry, order);
            if let Some(payload) = self.drag_payload(&acting) {
                response.dnd_set_drag_payload(payload);
            }
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }

    /// The tooltip for a card or row.
    fn card_tip(&self, card: &Card) -> String {
        let mut tip = format!("{}\n{}", card.name, card.detail);
        if card.missing {
            tip.push_str(&format!("\n{}", tr("Missing: its file can't be found. Click Relink to point it at it.")));
        }
        match card.uses {
            0 => tip.push_str(&format!("\n{}", tr("Not on any timeline yet"))),
            1 => tip.push_str(&format!("\n{}", tr("On the timeline once"))),
            n => tip.push_str(&format!("\n{}", trf("On the timeline {n} times", &[("n", &n.to_string())]))),
        }
        if let Some(reason) = &card.conformed {
            tip.push_str(&format!("\nconverted for playback ({reason})"));
        }
        tip.push_str(&format!("\n{}", tr("Click to pick (Ctrl adds, Shift picks a run); double-click or ＋ adds to the timeline; right-click for more.")));
        tip
    }

    /// Whether a card can be quick cut: a video or a sound file that's there.
    fn cuttable(card: &Card) -> bool {
        !card.compound && !card.missing && card.kind != MediaKind::Still
    }

    /// A ＋ button (with a scissors in its corner when `cut`) in `rect`.
    fn paint_add_button(painter: &egui::Painter, rect: egui::Rect, hot: bool, cut: bool, visuals: &egui::Visuals) {
        painter.rect_filled(rect, 4.0, if hot { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(170) });
        let plus = if cut { egui::Rect::from_min_size(rect.min + egui::vec2(1.0, 1.0), rect.size() * 0.72) } else { rect.shrink(3.0) };
        crate::icons::paint(painter, plus, crate::icons::ADD, egui::Color32::WHITE);
        if cut {
            let small = egui::Rect::from_min_max(rect.center() + egui::vec2(-0.5, -0.5), rect.max - egui::vec2(1.0, 1.0));
            painter.circle_filled(small.center(), small.width() * 0.62, if hot { visuals.selection.bg_fill } else { egui::Color32::from_rgb(40, 40, 44) });
            crate::icons::paint(painter, small, crate::icons::SPLIT, egui::Color32::WHITE);
        }
    }

    fn bin_card(&mut self, ui: &mut egui::Ui, card: Card, order: &[Entry]) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter_at(rect.expand(2.0));
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        let picked = self.bin.picked.contains(&card.entry);
        let hover = response.hover_pos();
        self.paint_card_picture(ui, &painter, thumb, &card, hover);
        if card.compound {
            crate::icons::paint(&painter, egui::Rect::from_min_size(thumb.left_top() + egui::vec2(4.0, 4.0), egui::vec2(13.0, 13.0)), crate::icons::GROUP, crate::style::GOLD);
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
        // Missing: the badge, and (on hover) the way to fix it.
        let relink = egui::Rect::from_center_size(thumb.center() + egui::vec2(0.0, 6.0), egui::vec2(78.0, 22.0));
        let over_relink = card.missing && hover.is_some_and(|p| relink.contains(p));
        if card.missing {
            Self::paint_missing_badge(&painter, thumb.left_top() + egui::vec2(3.0, 3.0), false);
            if response.hovered() {
                painter.rect_filled(relink, 11.0, if over_relink { crate::style::ACCENT } else { egui::Color32::from_black_alpha(200) });
                crate::icons::paint(&painter, egui::Rect::from_min_size(relink.min + egui::vec2(8.0, 4.0), egui::vec2(14.0, 14.0)), crate::icons::RELINK, egui::Color32::WHITE);
                painter.text(egui::pos2(relink.left() + 26.0, relink.center().y), egui::Align2::LEFT_CENTER, tr("Relink…"), egui::FontId::proportional(11.0), egui::Color32::WHITE);
            }
        }
        // Picked: lit, with a check.
        let border = if picked {
            egui::Stroke::new(2.0, crate::style::ACCENT)
        } else if card.missing {
            egui::Stroke::new(1.5, crate::style::ERROR)
        } else if response.hovered() {
            egui::Stroke::new(1.5, visuals.selection.stroke.color)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        if picked {
            painter.rect_filled(thumb, 4.0, crate::style::ACCENT.gamma_multiply(0.12));
            let check = egui::Rect::from_min_size(egui::pos2(thumb.right() - 18.0, thumb.bottom() - 34.0), egui::vec2(15.0, 15.0));
            painter.circle_filled(check.center(), 8.0, egui::Color32::WHITE);
            crate::icons::paint(&painter, check, crate::icons::CHECK, crate::style::ACCENT);
        }
        painter.rect_stroke(thumb, 4.0, border, egui::StrokeKind::Inside);

        // Sound gets a play button, so you can hear it before using it.
        let listen = egui::Rect::from_min_size(thumb.left_top() + egui::vec2(3.0, 3.0), egui::vec2(19.0, 19.0));
        let over_listen = card.sound.is_some() && hover.is_some_and(|p| listen.contains(p));
        if let Some((_, path)) = &card.sound {
            let playing = self.auditioning(path);
            if response.hovered() || playing {
                painter.rect_filled(listen, 4.0, if over_listen { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
                let icon = if playing { crate::icons::STOP } else { crate::icons::PLAY };
                crate::icons::paint(&painter, listen.shrink(3.0), icon, egui::Color32::WHITE);
            }
        }
        // ＋ adds it to the timeline; the one with scissors beside it, quick cut.
        let plus = egui::Rect::from_min_size(thumb.right_top() + egui::vec2(-22.0, 3.0), egui::vec2(19.0, 19.0));
        let cut = plus.translate(egui::vec2(-22.0, 0.0));
        let cuttable = Self::cuttable(&card);
        let over_plus = !card.missing && hover.is_some_and(|p| plus.contains(p));
        let over_cut = cuttable && hover.is_some_and(|p| cut.contains(p));
        if response.hovered() && !card.missing {
            Self::paint_add_button(&painter, plus, over_plus, false, &visuals);
            if cuttable {
                Self::paint_add_button(&painter, cut, over_cut, true, &visuals);
            }
        }

        // Name and details.
        let name_color = if card.missing { crate::style::ERROR } else { visuals.text_color() };
        let line = |text: String, size: f32, color: egui::Color32| {
            let mut job = egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(size), color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
            painter.layout_job(job)
        };
        let name = line(card.name.clone(), 12.0, name_color);
        let name_h = name.size().y;
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), name, name_color);
        let detail = if card.missing { tr("missing — relink it").to_string() } else { card.detail.split(" · ").take(2).collect::<Vec<_>>().join(" · ") };
        let weak = visuals.weak_text_color();
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 3.0 + name_h), line(detail, 10.0, weak), weak);

        let tip = self.card_tip(&card);
        let hit = if over_listen {
            Hit::Listen
        } else if over_relink {
            Hit::Relink
        } else if over_cut && response.hovered() {
            Hit::Cut
        } else if over_plus && response.hovered() {
            Hit::Plus
        } else {
            Hit::None
        };
        let response = match hit {
            Hit::Cut => response.on_hover_text(tr("Quick cut: pick the parts to add, with a preview")),
            Hit::Plus => response.on_hover_text(tr("Add to the timeline")),
            Hit::Relink => response.on_hover_text(tr("Point it at its file")),
            _ => response.on_hover_text(tip),
        };
        self.card_clicks(ui, &response, &card, order, hit);
        let entry = card.entry;
        crate::widgets::context_menu(&response, |ui| self.card_menu(ui, entry, order));
    }

    /// One file as a row of the list view: a small picture, its name and what it is, how
    /// long, whether it's used, and the same buttons, clicks and menu as a card.
    fn bin_row(&mut self, ui: &mut egui::Ui, card: Card, order: &[Entry]) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 38.0), egui::Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        let picked = self.bin.picked.contains(&card.entry);
        let hover = response.hover_pos();
        if picked {
            painter.rect(rect, 5.0, crate::style::ACCENT.gamma_multiply(0.16), egui::Stroke::new(1.0, crate::style::ACCENT), egui::StrokeKind::Inside);
        } else if response.hovered() {
            painter.rect_filled(rect, 5.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let thumb = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 3.0), egui::vec2(56.0, rect.height() - 6.0));
        self.paint_card_picture(ui, &painter, thumb, &card, hover);
        if card.missing {
            Self::paint_missing_badge(&painter, thumb.left_top() + egui::vec2(2.0, 2.0), true);
        }
        // Right side, right to left: the buttons (on hover), then length and use.
        let mut x = rect.right() - 6.0;
        let plus = egui::Rect::from_min_size(egui::pos2(x - 20.0, rect.center().y - 10.0), egui::vec2(20.0, 20.0));
        let cut = plus.translate(egui::vec2(-24.0, 0.0));
        let relink = egui::Rect::from_min_size(egui::pos2(x - 74.0, rect.center().y - 10.0), egui::vec2(74.0, 20.0));
        let cuttable = Self::cuttable(&card);
        let over_plus = !card.missing && hover.is_some_and(|p| plus.contains(p));
        let over_cut = cuttable && hover.is_some_and(|p| cut.contains(p));
        let over_relink = card.missing && hover.is_some_and(|p| relink.contains(p));
        if card.missing {
            painter.rect_filled(relink, 10.0, if over_relink { crate::style::ACCENT } else { crate::style::ERROR.gamma_multiply(0.85) });
            crate::icons::paint(&painter, egui::Rect::from_min_size(relink.min + egui::vec2(7.0, 3.0), egui::vec2(14.0, 14.0)), crate::icons::RELINK, egui::Color32::WHITE);
            painter.text(egui::pos2(relink.left() + 25.0, relink.center().y), egui::Align2::LEFT_CENTER, tr("Relink…"), egui::FontId::proportional(11.0), egui::Color32::WHITE);
            x = relink.left() - 10.0;
        } else if response.hovered() {
            Self::paint_add_button(&painter, plus, over_plus, false, &visuals);
            if cuttable {
                Self::paint_add_button(&painter, cut, over_cut, true, &visuals);
            }
            x = if cuttable { cut.left() } else { plus.left() } - 10.0;
        } else {
            x -= 50.0;
        }
        let weak = visuals.weak_text_color();
        let small = egui::FontId::proportional(crate::style::TEXT_S);
        let used = match card.uses {
            0 => tr("unused").to_string(),
            n => trf("used {n}×", &[("n", &n.to_string())]),
        };
        let used_color = if card.uses == 0 { weak } else { crate::style::ACCENT };
        let used_g = painter.layout_no_wrap(used, small.clone(), used_color);
        painter.galley(egui::pos2(x - used_g.size().x, rect.center().y - used_g.size().y / 2.0), used_g.clone(), used_color);
        x -= used_g.size().x + 12.0;
        if card.kind != MediaKind::Still || card.compound {
            let len = painter.layout_no_wrap(clock(card.length), egui::FontId::monospace(crate::style::TEXT_S), visuals.text_color());
            painter.galley(egui::pos2(x - len.size().x, rect.center().y - len.size().y / 2.0), len.clone(), visuals.text_color());
            x -= len.size().x + 12.0;
        }
        // Name over what it is.
        let left = thumb.right() + 8.0;
        let room = (x - left).max(20.0);
        let line = |text: String, font: egui::FontId, color: egui::Color32| {
            let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            painter.layout_job(job)
        };
        let name_color = if card.missing { crate::style::ERROR } else { visuals.text_color() };
        let name = line(card.name.clone(), egui::FontId::proportional(crate::style::TEXT), name_color);
        let name_h = name.size().y;
        painter.galley(egui::pos2(left, rect.top() + 3.0), name, name_color);
        let detail = if card.missing { tr("missing — relink it").to_string() } else { card.detail.clone() };
        painter.galley(egui::pos2(left, rect.top() + 4.0 + name_h), line(detail, small, weak), weak);

        let hit = if over_relink {
            Hit::Relink
        } else if over_cut && response.hovered() {
            Hit::Cut
        } else if over_plus && response.hovered() {
            Hit::Plus
        } else {
            Hit::None
        };
        let tip = self.card_tip(&card);
        let response = match hit {
            Hit::Cut => response.on_hover_text(tr("Quick cut: pick the parts to add, with a preview")),
            Hit::Plus => response.on_hover_text(tr("Add to the timeline")),
            Hit::Relink => response.on_hover_text(tr("Point it at its file")),
            _ => response.on_hover_text(tip),
        };
        self.card_clicks(ui, &response, &card, order, hit);
        let entry = card.entry;
        crate::widgets::context_menu(&response, |ui| self.card_menu(ui, entry, order));
    }

    /// A card dropped on the timeline at `at`, over `track` (if over one).
    pub(crate) fn drop_bin_entry(&mut self, drop: &BinDrop, at: Time, track: Option<oa_doc::TrackId>) {
        let kind = match drop.entry {
            Entry::Media(media) => ItemKind::Media { media },
            Entry::Compound(sequence) => ItemKind::Nested { sequence },
        };
        match self.editor.place_clip(&drop.name, kind, drop.length, at, track) {
            Ok(item) => {
                // The other cards dragged with it follow it, one after another.
                let mut placed = vec![item];
                let mut next = self.editor.item(item).map_or(at, |i| i.range.end());
                let mut track = self.editor.sequence().find_item(item).map(|(t, _)| self.editor.sequence().tracks[t].id).or(track);
                for more in &drop.rest {
                    let kind = match more.entry {
                        Entry::Media(media) => ItemKind::Media { media },
                        Entry::Compound(sequence) => ItemKind::Nested { sequence },
                    };
                    match self.editor.place_clip(&more.name, kind, more.length, next, track) {
                        Ok(id) => {
                            next = self.editor.item(id).map_or(next, |i| i.range.end());
                            track = self.editor.sequence().find_item(id).map(|(t, _)| self.editor.sequence().tracks[t].id).or(track);
                            placed.push(id);
                        }
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                self.selected = placed.iter().copied().collect();
                self.selection = placed.last().copied();
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
        match ops.and_then(|ops| self.editor.apply(&trf("Swap in {0}", &[("0", &(drop.name).to_string())]), ops)) {
            Ok(()) => {
                self.selection = Some(item);
                self.sync_selection();
                self.notify(trf("Swapped in {0} — the clip kept its place, transform and effects.", &[("0", &(drop.name).to_string())]));
            }
            Err(e) => self.error = Some(format!("can't swap in {}: {e}", drop.name)),
        }
    }

    /// A card's ＋ (or a double-click): onto the timeline at the playhead, on the first
    /// track of its kind with room there — or a temp layer when none has.
    fn add_bin_entry(&mut self, entry: Entry, length: Time) {
        let (name, kind, length) = match entry {
            Entry::Media(id) => (self.editor.pool_item(id).map(|p| p.name.clone()).unwrap_or_default(), ItemKind::Media { media: id }, length),
            Entry::Compound(sequence) => {
                let s = self.editor.doc.project().sequence(sequence);
                (s.map(|s| s.name.clone()).unwrap_or_default(), ItemKind::Nested { sequence }, s.map_or(length, |s| s.duration()))
            }
        };
        let added = self.editor.place_clip(&name, kind, length, self.playhead, None);
        match added {
            Ok(item) => self.selection = Some(item),
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

/// One smart filter chip: its icon, its name and how many pass it; lit when on, red when
/// it's the Missing one and something is.
fn filter_chip(ui: &mut egui::Ui, filter: BinFilter, count: usize, on: bool) -> egui::Response {
    let v = ui.visuals().clone();
    let warn = filter == BinFilter::Missing && count > 0;
    let label = if filter == BinFilter::All { filter.label().to_string() } else { format!("{}  {count}", filter.label()) };
    let galley = ui.painter().layout_no_wrap(label, egui::FontId::proportional(crate::style::TEXT_S + 0.5), egui::Color32::PLACEHOLDER);
    let size = egui::vec2(galley.size().x + 14.0 + 4.0 + 16.0, 22.0);
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
    let accent = if warn { crate::style::ERROR } else { crate::style::ACCENT };
    let (fill, stroke, color) = if on {
        (accent.gamma_multiply(0.22), egui::Stroke::new(1.0, accent), v.strong_text_color())
    } else if r.hovered() {
        (v.widgets.hovered.weak_bg_fill, egui::Stroke::new(1.0, v.widgets.hovered.bg_stroke.color), v.text_color())
    } else {
        (egui::Color32::TRANSPARENT, egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color), if warn { crate::style::ERROR } else { v.text_color() })
    };
    ui.painter().rect(rect, 11.0, fill, stroke, egui::StrokeKind::Inside);
    let icon = egui::Rect::from_center_size(egui::pos2(rect.left() + 7.0 + 7.0, rect.center().y), egui::vec2(14.0, 14.0));
    crate::icons::paint(ui.painter(), icon, filter.icon(), if on || warn { accent } else { v.weak_text_color() });
    ui.painter().galley(egui::pos2(icon.right() + 4.0, rect.center().y - galley.size().y / 2.0), galley, color);
    r.on_hover_text(filter.tip()).on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl App {
    /// The smart filter chips, each with how many pass it. Missing shows only when
    /// something is.
    fn bin_filters(&mut self, ui: &mut egui::Ui) {
        let cards = self.tab_cards();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for f in BinFilter::ALL {
                let count = cards.iter().filter(|c| f.keeps(c)).count();
                if f == BinFilter::Missing && count == 0 && self.bin.filter != f {
                    continue;
                }
                let on = self.bin.filter == f;
                if filter_chip(ui, f, count, on).clicked() {
                    // Clicking the one on goes back to everything.
                    self.bin.filter = if on { BinFilter::All } else { f };
                }
            }
        });
    }

    /// A click on a card: picks it alone, or with Ctrl adds or drops it, or with Shift
    /// picks the run from the last one picked (`order`: the cards as shown).
    fn pick_card(&mut self, entry: Entry, order: &[Entry], mods: egui::Modifiers) {
        let b = &mut self.bin;
        if mods.command {
            match b.picked.iter().position(|e| *e == entry) {
                Some(i) => {
                    b.picked.remove(i);
                }
                None => b.picked.push(entry),
            }
            b.anchor = Some(entry);
        } else if mods.shift
            && let Some(from) = b.anchor.and_then(|a| order.iter().position(|e| *e == a))
            && let Some(to) = order.iter().position(|e| *e == entry)
        {
            let (lo, hi) = (from.min(to), from.max(to));
            b.picked = order[lo..=hi].to_vec();
        } else if b.picked == [entry] {
            // Clicked again: unpicked.
            b.picked.clear();
            b.anchor = None;
        } else {
            b.picked = vec![entry];
            b.anchor = Some(entry);
        }
    }

    /// What a card's action works on: every picked card (in the order shown) when it's
    /// one of them, otherwise just it.
    fn acting_on(&self, entry: Entry, order: &[Entry]) -> Vec<Entry> {
        if self.bin.picked.len() > 1 && self.bin.picked.contains(&entry) {
            order.iter().copied().filter(|e| self.bin.picked.contains(e)).collect()
        } else {
            vec![entry]
        }
    }

    /// The length a card adds to the timeline with.
    fn entry_length(&self, entry: Entry) -> Time {
        match entry {
            Entry::Media(id) => self.editor.pool_item(id).map_or(Time::from_seconds(5), |m| if m.kind == MediaKind::Still { self.still_length() } else { m.probe.duration }),
            Entry::Compound(seq) => self.editor.doc.project().sequence(seq).map_or(Time::from_seconds(5), |s| s.duration()),
        }
    }

    /// Adds cards to the timeline, one after another (missing files left out).
    fn add_entries(&mut self, entries: &[Entry]) {
        for &e in entries {
            if let Entry::Media(id) = e
                && self.editor.pool_item(id).is_none_or(|m| m.missing)
            {
                continue;
            }
            let length = self.entry_length(e);
            self.add_bin_entry(e, length);
        }
    }

    /// What a card dragged out carries: it, and every other picked card with it.
    fn drag_payload(&self, entries: &[Entry]) -> Option<BinDrop> {
        let mut drops: Vec<BinDrop> = entries
            .iter()
            .filter_map(|&e| {
                let (name, audio, missing) = match e {
                    Entry::Media(id) => {
                        let m = self.editor.pool_item(id)?;
                        (m.name.clone(), m.kind == MediaKind::Audio, m.missing)
                    }
                    Entry::Compound(seq) => (self.editor.doc.project().sequence(seq)?.name.clone(), false, false),
                };
                (!missing).then(|| BinDrop { entry: e, name, length: self.entry_length(e), audio, rest: Vec::new() })
            })
            .collect();
        if drops.is_empty() {
            return None;
        }
        let mut first = drops.remove(0);
        first.rest = drops;
        Some(first)
    }

    /// Takes files out of the project — with the clips that use them — as one undo step.
    fn remove_entries(&mut self, entries: &[Entry]) {
        let media: Vec<MediaId> = entries.iter().filter_map(|e| if let Entry::Media(id) = e { Some(*id) } else { None }).collect();
        if media.is_empty() {
            return;
        }
        let clips: usize = {
            let (uses, _) = self.editor.media_uses();
            media.iter().map(|m| uses.get(m).copied().unwrap_or(0)).sum()
        };
        match self.editor.remove_media(&media) {
            Ok(()) => {
                self.bin.picked.retain(|e| !entries.contains(e));
                self.selected.retain(|id| self.editor.item(*id).is_some());
                if self.selection.is_some_and(|id| self.editor.item(id).is_none()) {
                    self.selection = None;
                }
                let what = if media.len() == 1 { tr("Removed it").to_string() } else { trf("Removed {n} files", &[("n", &media.len().to_string())]) };
                let note = if clips > 0 { trf("{what} and the {n} clips using it. Ctrl+Z brings them back.", &[("what", &what), ("n", &clips.to_string())]) } else { trf("{what}. Ctrl+Z brings it back.", &[("what", &what)]) };
                self.notify(note);
            }
            Err(e) => self.report_error(e.to_string()),
        }
    }

    /// Looks for every missing file — in `dir` (and the folders in it), or with `None`
    /// all around this computer — on a thread of its own: the same name and content, or
    /// the same content renamed (`oa_media::find`). What's found is relinked as it comes
    /// back (`poll_finding`).
    pub(crate) fn find_missing(&mut self, dir: Option<&std::path::Path>) {
        let project = self.editor.doc.project();
        let missing: Vec<(MediaId, oa_media::find::Missing)> = self
            .editor
            .pool
            .iter()
            .filter(|m| m.missing)
            .filter_map(|m| {
                let r = project.media(m.id)?;
                Some((m.id, oa_media::find::Missing { path: std::path::PathBuf::from(&r.path), fingerprint: r.fingerprint.clone() }))
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        let roots = match dir {
            Some(d) => vec![d.to_path_buf()],
            None => {
                let paths: Vec<std::path::PathBuf> = missing.iter().map(|(_, m)| m.path.clone()).collect();
                crate::projects::relink_roots(self.editor.path.as_deref().and_then(|p| p.parent()), &paths)
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let wanted: Vec<oa_media::find::Missing> = missing.iter().map(|(_, m)| m.clone()).collect();
            let hits = oa_media::find::find_moved(&wanted, &roots, std::time::Duration::from_secs(30));
            let found: Vec<(MediaId, std::path::PathBuf)> = missing.iter().zip(hits).filter_map(|((id, _), hit)| hit.map(|p| (*id, p))).collect();
            let _ = tx.send((found, missing.len()));
        });
        self.finding = Some(rx);
        self.notify(tr("Looking for the missing files…"));
    }

    /// Takes in what `find_missing` found: each file relinked (probed in the background;
    /// the cards update as each comes in).
    pub(crate) fn poll_finding(&mut self) {
        let Some(rx) = &self.finding else { return };
        let Ok((found, looked_for)) = rx.try_recv() else { return };
        self.finding = None;
        for (id, path) in &found {
            self.relink(*id, path);
        }
        let left = looked_for - found.len();
        if left == 0 {
            self.notify(trf("Relinking {n} files…", &[("n", &found.len().to_string())]));
        } else {
            self.notify(trf("Relinking {n} files… {left} weren't found there.", &[("n", &found.len().to_string()), ("left", &left.to_string())]));
        }
    }

    /// Asks for a file to relink `media` to.
    fn ask_relink(&mut self, media: MediaId) {
        if let Some(file) = rfd::FileDialog::new().add_filter(tr("Media"), crate::MEDIA_EXTENSIONS).pick_file() {
            self.relink(media, &file);
        }
    }

    /// A card's right-click menu: adding, assets, folders, scaling and color, relinking
    /// and removing — for every picked card when it's one of them.
    fn card_menu(&mut self, ui: &mut egui::Ui, entry: Entry, order: &[Entry]) {
        let acting = self.acting_on(entry, order);
        let many = acting.len() > 1;
        if many {
            ui.label(egui::RichText::new(trf("{n} picked", &[("n", &acting.len().to_string())])).small().weak());
        }
        let missing = matches!(entry, Entry::Media(id) if self.editor.pool_item(id).is_some_and(|m| m.missing));
        if crate::icons::menu_item(ui, Some(crate::icons::ADD), if many { tr("Add them to the timeline") } else { tr("Add to timeline") }, "", !missing || many).clicked() {
            self.add_entries(&acting);
            ui.close();
        }
        if let Entry::Media(id) = entry
            && !many
            && self.editor.pool_item(id).is_some_and(|m| !m.missing && m.kind != MediaKind::Still)
            && crate::icons::menu_item(ui, Some(crate::icons::SPLIT), tr("Quick cut…"), "", true).clicked()
        {
            self.open_quick_cut(id);
            ui.close();
        }
        match entry {
            Entry::Media(id) => {
                if !many && ui.button(tr("Save to assets")).on_hover_text(tr("Keeps a copy on this computer, for every project")).clicked() {
                    let path = self.editor.pool_item(id).map(|m| m.decode_path.clone());
                    if let Some(path) = path {
                        let folder = self.bin.asset_folder.clone();
                        match self.assets.add(&path, &folder) {
                            Ok(_) => self.notify(tr("Saved to assets.")),
                            Err(e) => self.report_error(e),
                        }
                    }
                    ui.close();
                }
                let media: Vec<MediaId> = acting.iter().filter_map(|e| if let Entry::Media(id) = e { Some(*id) } else { None }).collect();
                self.folder_menu(ui, &media);
                let is_audio = self.editor.pool_item(id).is_some_and(|m| m.kind == MediaKind::Audio);
                if !is_audio && !many {
                    self.scaling_menu(ui, id);
                    if self.settings.advanced_color {
                        self.color_menu(ui, id);
                    }
                }
                ui.separator();
                if crate::icons::menu_item(ui, Some(crate::icons::RELINK), tr("Relink…"), "", !many).on_hover_text(tr("Point it at its file (moved or renamed)")).clicked() {
                    ui.close();
                    self.ask_relink(id);
                }
                if self.editor.pool.iter().any(|m| m.missing)
                    && crate::icons::menu_item(ui, Some(crate::icons::FOLDER_OPEN), tr("Find missing files in a folder…"), "", true).clicked()
                {
                    ui.close();
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.find_missing(Some(&dir));
                    }
                }
                if self.editor.pool.iter().any(|m| m.missing)
                    && self.finding.is_none()
                    && crate::icons::menu_item(ui, Some(crate::icons::SEARCH), tr("Find missing files on this computer"), "", true)
                        .on_hover_text(tr("Around where they were, by the project, in your usual folders and on your drives: by name and content, or by content if renamed"))
                        .clicked()
                {
                    ui.close();
                    self.find_missing(None);
                }
                let (uses, _) = self.editor.media_uses();
                let clips: usize = media.iter().map(|m| uses.get(m).copied().unwrap_or(0)).sum();
                let label = match (many, clips) {
                    (false, 0) => tr("Remove from project").to_string(),
                    (false, n) => trf("Remove from project (and its {n} clips)", &[("n", &n.to_string())]),
                    (true, 0) => trf("Remove {n} files from project", &[("n", &media.len().to_string())]),
                    (true, n) => trf("Remove {0} files from project (and {n} clips)", &[("0", &media.len().to_string()), ("n", &n.to_string())]),
                };
                if crate::icons::menu_item(ui, Some(crate::icons::DELETE), &label, "", true).on_hover_text(tr("Takes it out of the bin (the file on disk stays). Ctrl+Z brings it back.")).clicked() {
                    self.remove_entries(&acting);
                    ui.close();
                }
            }
            Entry::Compound(seq) => {
                let used = self.editor.doc.project().sequences.keys().any(|&s| s != seq && self.editor.doc.project().sequence_reaches(s, seq));
                if ui.add_enabled(!used, egui::Button::new(tr("Delete"))).on_disabled_hover_text(tr("A clip on the timeline uses it")).clicked() {
                    if let Err(e) = self.editor.remove_compound(seq) {
                        self.error = Some(e.to_string());
                    }
                    ui.close();
                }
            }
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
                if ui.small_button(tr("Assets")).clicked() {
                    self.bin.asset_folder.clear();
                }
                let mut path = String::new();
                for part in here.split('/') {
                    ui.label(tr("›"));
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
        if self.assets.items.is_empty() && self.assets.presets.is_empty() && self.assets.folders.is_empty() {
            ui.label(
                egui::RichText::new(
                    tr("Nothing here yet. Right-click a clip in Media → Save to assets, or add files with the arrow above — they stay available in every project."),
                )
                .weak(),
            );
        }

        // Searching looks through the whole library; otherwise the folder you're in.
        let needle = self.bin.search.trim().to_lowercase();
        let (folders, items): (Vec<(String, usize)>, Vec<crate::assets::Asset>) = if needle.is_empty() {
            (self.assets.child_folders(&here), self.assets.in_folder(&here).cloned().collect())
        } else {
            (Vec::new(), self.assets.items.iter().filter(|a| matches(&needle, &[&a.name, &a.folder])).cloned().collect())
        };
        let presets: Vec<crate::assets::PresetAsset> = if needle.is_empty() {
            self.assets.presets_in(&here).cloned().collect()
        } else {
            self.assets.presets.iter().filter(|a| matches(&needle, &[&a.name, &a.folder])).cloned().collect()
        };
        if !needle.is_empty() && items.is_empty() && presets.is_empty() {
            ui.label(egui::RichText::new(tr("Nothing matches.")).weak());
        }
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
                for preset in presets {
                    self.preset_card(ui, &preset);
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
        } else if let Some(strip) = self.clip_previews.strip(&ctx, id, &asset.path, [0, 0], 0.0, oa_doc::MediaScaling::Auto) {
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

        let response = response.on_hover_text(trf("{0}\nDouble-click or ＋ to use it in this project", &[("0", &(asset.path.display()).to_string())]));
        if response.clicked() && over_listen {
            self.audition(&asset.path, Time::ZERO);
        } else if (response.clicked() && over_plus) || response.double_clicked() {
            self.use_asset(asset);
        }
        let asset = asset.clone();
        crate::widgets::context_menu(&response, |ui| {
            if ui.button(tr("Use in this project")).clicked() {
                self.use_asset(&asset);
                ui.close();
            }
            if ui.button(tr("Show in folder")).clicked() {
                let dir = asset.path.parent().unwrap_or(&asset.path).to_path_buf();
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                #[cfg(not(windows))]
                let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                ui.close();
            }
            ui.separator();
            if ui
                .button(tr("Remove from assets"))
                .on_hover_text(tr("Deletes this copy; whatever you copied it from is untouched"))
                .clicked()
            {
                if let Err(e) = self.assets.remove(&asset.path) {
                    self.report_error(e);
                }
                ui.close();
            }
        });
    }

    /// An effects preset (`presets.rs`): its effects previewed on the sample picture (a
    /// sound preset shows its icon). Drag it onto a clip or the effect list, or
    /// double-click or ＋ it, to put it on the selected clips.
    fn preset_card(&mut self, ui: &mut egui::Ui, preset: &crate::assets::PresetAsset) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(CARD_W, THUMB_H + 30.0), egui::Sense::click_and_drag());
        if response.drag_started() {
            self.preset_drag = Some(crate::presets::PresetDrag { path: preset.path.clone(), name: preset.name.clone() });
        }
        if !ui.is_rect_visible(rect) {
            return;
        }
        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_W, THUMB_H));
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(thumb, 4.0, visuals.faint_bg_color);
        let picture = if preset.sound { None } else { self.preset_still(preset) };
        match picture {
            Some(id) => {
                // The sample is 3:4: its middle, filling the card.
                let aspect = thumb.width() / thumb.height() * 0.75;
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.5 - 0.5 * aspect), egui::pos2(1.0, 0.5 + 0.5 * aspect));
                painter.image(id, thumb, uv, egui::Color32::WHITE);
            }
            None => {
                let icon = if preset.sound { crate::icons::SOUND } else { crate::icons::EFFECTS };
                crate::icons::paint(&painter, thumb.shrink(THUMB_H * 0.3), icon, visuals.weak_text_color());
            }
        }
        // A badge: it's effects, not a file to import.
        let badge = egui::Rect::from_min_size(thumb.left_top() + egui::vec2(3.0, 3.0), egui::vec2(19.0, 19.0));
        painter.rect_filled(badge, 4.0, egui::Color32::from_black_alpha(160));
        crate::icons::paint(&painter, badge.shrink(3.0), crate::icons::EFFECTS, egui::Color32::WHITE);
        let border = if response.hovered() { egui::Stroke::new(1.5, crate::style::ACCENT) } else { egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color) };
        painter.rect_stroke(thumb, 4.0, border, egui::StrokeKind::Inside);
        let plus = egui::Rect::from_min_size(thumb.right_top() + egui::vec2(-22.0, 3.0), egui::vec2(19.0, 19.0));
        let over_plus = response.hover_pos().is_some_and(|p| plus.contains(p));
        if response.hovered() {
            painter.rect_filled(plus, 4.0, if over_plus { visuals.selection.bg_fill } else { egui::Color32::from_black_alpha(160) });
            crate::icons::paint(&painter, plus.shrink(3.0), crate::icons::ADD, egui::Color32::WHITE);
        }
        let mut job = egui::text::LayoutJob::simple_singleline(preset.name.clone(), egui::FontId::proportional(12.0), visuals.text_color());
        job.wrap = egui::text::TextWrapping::truncate_at_width(CARD_W - 4.0);
        painter.galley(egui::pos2(rect.left() + 2.0, thumb.bottom() + 2.0), painter.layout_job(job), visuals.text_color());
        let n = preset.effects;
        let what = if preset.sound {
            trf("{n} sound effect{0}", &[("n", &n.to_string()), ("0", if n == 1 { "" } else { "s" })])
        } else {
            trf("{n} effect{0}", &[("n", &n.to_string()), ("0", if n == 1 { "" } else { "s" })])
        };
        painter.text(egui::pos2(rect.left() + 2.0, thumb.bottom() + 17.0), egui::Align2::LEFT_TOP, what, egui::FontId::proportional(10.0), visuals.weak_text_color());

        let response = response.on_hover_text(tr("An effects preset: drag it onto a clip, or double-click or ＋ to put it on the selected clips"));
        let apply = |app: &mut App, path: &std::path::Path| {
            let targets = app.selected_clips();
            if targets.is_empty() {
                app.notify(tr("Select a clip first, or drag the preset onto one."));
            } else {
                app.apply_preset(path, &targets);
            }
        };
        if (response.clicked() && over_plus) || response.double_clicked() {
            apply(self, &preset.path);
        }
        let preset = preset.clone();
        crate::widgets::context_menu(&response, |ui| {
            if ui.button(tr("Put on the selected clips")).clicked() {
                apply(self, &preset.path);
                ui.close();
            }
            if ui.button(tr("Show in folder")).clicked() {
                let dir = preset.path.parent().unwrap_or(&preset.path).to_path_buf();
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                #[cfg(not(windows))]
                let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                ui.close();
            }
            ui.separator();
            if ui.button(tr("Remove from assets")).clicked() {
                if let Err(e) = self.assets.remove(&preset.path) {
                    self.report_error(e);
                }
                ui.close();
            }
        });
    }

    /// Uses an asset (its ＋, or a double-click): imported into the project and put on the
    /// timeline — asked for by name, unlike a plain import.
    fn use_asset(&mut self, asset: &crate::assets::Asset) {
        self.import_paths(std::slice::from_ref(&asset.path), true, "");
    }
}

/// How wide the bin's column of tabs is.
const TAB_STRIP: f32 = crate::style::ICON + 2.0;

/// Whether every word of `needle` (already lowercase) is in one of `fields`.
fn matches(needle: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    needle.split_whitespace().all(|word| fields.iter().any(|f| f.contains(word)))
}

/// The media in `dir` and the folders inside it, to import into the bin folder
/// `parent`: (bin folder, its files), the folder named after `dir` first. Hidden files
/// and folders are skipped, and it goes no more than eight folders deep.
pub(crate) fn media_in_folder(dir: &std::path::Path, parent: &str) -> Vec<(String, Vec<PathBuf>)> {
    fn walk(dir: &std::path::Path, folder: String, depth: usize, out: &mut Vec<(String, Vec<PathBuf>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))).collect();
        paths.sort();
        let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| p.is_dir());
        let files: Vec<PathBuf> = files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| crate::MEDIA_EXTENSIONS.iter().any(|m| e.eq_ignore_ascii_case(m))))
            .collect();
        if !files.is_empty() {
            out.push((folder.clone(), files));
        }
        if depth < 8 {
            for d in dirs {
                let name = d.file_name().map_or_else(String::new, |n| n.to_string_lossy().replace('/', "-"));
                walk(&d, format!("{folder}/{name}"), depth + 1, out);
            }
        }
    }
    let name = dir.file_name().map_or_else(|| "Imported".to_string(), |n| n.to_string_lossy().replace('/', "-"));
    let folder = if parent.is_empty() { name } else { format!("{parent}/{name}") };
    let mut out = Vec::new();
    walk(dir, folder, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    /// Every word typed has to be somewhere: in the name, or in what the file is.
    #[test]
    fn search_matches_every_word_anywhere() {
        let card = ["Beach Trip.mp4", "video · 3840×2160 · 0:12"];
        assert!(super::matches("beach", &card));
        assert!(super::matches("trip 3840", &card), "words in different places");
        assert!(super::matches("", &card));
        assert!(!super::matches("beach wav", &card));
    }

    /// A folder comes in as a bin folder of its name, its folders inside it; only media
    /// is taken, and hidden things are left alone.
    #[test]
    fn folders_import_as_bin_folders() {
        let root = std::env::temp_dir().join(format!("oa-bin-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shots = root.join("Shots");
        std::fs::create_dir_all(shots.join("Day 1")).unwrap();
        std::fs::create_dir_all(shots.join(".cache")).unwrap();
        for f in ["a.mp4", "notes.txt", "Day 1/b.PNG", ".cache/c.mp4"] {
            std::fs::write(shots.join(f), b"x").unwrap();
        }
        let found = super::media_in_folder(&shots, "Project");
        let _ = std::fs::remove_dir_all(&root);
        let names: Vec<(String, Vec<String>)> = found
            .into_iter()
            .map(|(folder, files)| (folder, files.iter().map(|f| f.file_name().unwrap().to_string_lossy().to_string()).collect()))
            .collect();
        assert_eq!(names, vec![("Project/Shots".to_string(), vec!["a.mp4".to_string()]), ("Project/Shots/Day 1".to_string(), vec!["b.PNG".to_string()])]);
    }
}
