//! The media selector for an effect's picture input (a mask's matte, a displacement
//! map…): a button with the chosen picture on it, opening a searchable grid of the
//! project's pictures — videos skim under the pointer, as in the media bin — with tabs
//! for videos, pictures and compound clips.

use crate::i18n::tr;
use crate::App;
use eframe::egui;
use oa_media::MediaKind;
use std::path::PathBuf;

/// Cells across the grid, each this wide (16:9 thumbnails, a name under them).
const COLUMNS: usize = 3;
const CELL_W: f32 = 120.0;
const THUMB_H: f32 = CELL_W * 9.0 / 16.0;
const CELL_H: f32 = THUMB_H + 20.0;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    Video,
    Picture,
    Compound,
}

/// One thing that can be chosen.
struct Choice {
    /// The value's id: a media file's, or a compound clip's sequence.
    id: u64,
    name: String,
    kind: Kind,
    /// A file's picture to show: path, size, seconds.
    file: Option<(PathBuf, [u32; 2], f64)>,
    /// A compound clip's length, seconds.
    length: f64,
}

impl App {
    /// The selector: returns the new choice when one is made (`Some(None)`: none).
    pub(crate) fn media_selector(&mut self, ui: &mut egui::Ui, salt: egui::Id, chosen: Option<u64>) -> Option<Option<u64>> {
        let choices: Vec<Choice> = self
            .editor
            .pool
            .iter()
            .filter(|m| !m.missing)
            .filter_map(|m| {
                let v = m.probe.video.as_ref()?;
                let still = m.kind == MediaKind::Still;
                let seconds = if still { 0.0 } else { m.probe.duration.as_seconds_f64() };
                Some(Choice { id: m.id.0, name: m.name.clone(), kind: if still { Kind::Picture } else { Kind::Video }, file: Some((m.decode_path.clone(), [v.width, v.height], seconds)), length: seconds })
            })
            .chain(self.editor.compounds().into_iter().map(|s| Choice { id: s.id.0, name: s.name.clone(), kind: Kind::Compound, file: None, length: s.duration().as_seconds_f64() }))
            .collect();

        // The button: the chosen picture, small, and its name.
        let current = chosen.and_then(|c| choices.iter().find(|o| o.id == c));
        let text = current.map_or_else(|| tr("none — pick a picture").to_string(), |c| c.name.clone());
        let (rect, button) = ui.allocate_exact_size(egui::vec2(170.0, 26.0), egui::Sense::click());
        let visuals = *ui.style().interact(&button);
        ui.painter().rect(rect, 3.0, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        let thumb = egui::Rect::from_min_size(rect.min + egui::vec2(3.0, 3.0), egui::vec2(36.0, 20.0));
        match current {
            Some(c) => self.paint_choice(ui, c, thumb, None),
            None => {
                ui.painter().rect_filled(thumb, 2.0, ui.visuals().extreme_bg_color);
            }
        }
        let name_rect = egui::Rect::from_min_max(egui::pos2(thumb.right() + 6.0, rect.top()), rect.right_bottom() - egui::vec2(16.0, 0.0));
        let galley = ui.painter().layout(text, egui::FontId::proportional(12.5), visuals.text_color(), name_rect.width());
        let galley = if galley.rows.len() > 1 {
            // One line, cut short.
            let mut job = egui::text::LayoutJob::simple_singleline(galley.job.text.clone(), egui::FontId::proportional(12.5), visuals.text_color());
            job.wrap = egui::text::TextWrapping::truncate_at_width(name_rect.width());
            ui.fonts_mut(|f| f.layout_job(job))
        } else {
            galley
        };
        ui.painter().galley(egui::pos2(name_rect.left(), rect.center().y - galley.size().y / 2.0), galley, visuals.text_color());
        ui.painter().text(rect.right_center() - egui::vec2(9.0, 0.0), egui::Align2::CENTER_CENTER, "⏷", egui::FontId::proportional(11.0), visuals.text_color());

        // The grid, open until something's picked or you click away.
        let open_key = salt.with("open");
        let mut open: bool = ui.data(|d| d.get_temp(open_key)).unwrap_or(false);
        if button.clicked() {
            open = !open;
        }
        let mut picked = None;
        egui::Popup::from_response(&button)
            .id(salt.with("popup"))
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| picked = self.media_grid(ui, salt, &choices, chosen));
        if picked.is_some() {
            open = false;
        }
        ui.data_mut(|d| d.insert_temp(open_key, open));
        button.on_hover_text(tr("Pick the picture this effect uses")).on_hover_cursor(egui::CursorIcon::PointingHand);
        picked
    }

    /// The popup's contents: search, tabs, "none", and the grid.
    fn media_grid(&mut self, ui: &mut egui::Ui, salt: egui::Id, choices: &[Choice], chosen: Option<u64>) -> Option<Option<u64>> {
        let width = CELL_W * COLUMNS as f32 + ui.spacing().item_spacing.x * (COLUMNS as f32 - 1.0);
        ui.set_width(width);
        let key = salt.with("filter");
        let (mut search, mut tab): (String, u8) = ui.data(|d| d.get_temp(key)).unwrap_or_default();
        let before = (search.clone(), tab);
        let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text(tr("Search pictures")).desired_width(width));
        if !ui.memory(|m| m.focused().is_some()) {
            r.request_focus();
        }
        let tabs = [(0u8, tr("All"), None), (1, tr("Videos"), Some(Kind::Video)), (2, tr("Pictures"), Some(Kind::Picture)), (3, tr("Compound clips"), Some(Kind::Compound))];
        ui.horizontal_wrapped(|ui| {
            for (n, label, kind) in tabs {
                if kind.is_some_and(|k| !choices.iter().any(|c| c.kind == k)) {
                    continue;
                }
                if ui.selectable_label(tab == n, label).clicked() {
                    tab = n;
                }
            }
        });
        let wanted = tabs.iter().find(|t| t.0 == tab).and_then(|t| t.2);
        let needle = search.trim().to_lowercase();
        let shown: Vec<&Choice> = choices.iter().filter(|c| wanted.is_none_or(|k| c.kind == k) && (needle.is_empty() || c.name.to_lowercase().contains(&needle))).collect();
        let changed = before != (search.clone(), tab);
        ui.data_mut(|d| d.insert_temp(key, (search, tab)));

        let mut picked = None;
        if ui.selectable_label(chosen.is_none(), tr("None")).on_hover_text(tr("Use no picture")).clicked() {
            picked = Some(None);
        }
        let rows = shown.len().div_ceil(COLUMNS).max(1) as f32;
        let mut scroll = crate::widgets::fitted_scroll(400.0, rows * (CELL_H + ui.spacing().item_spacing.y)).id_salt(salt.with("grid"));
        if changed {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            if shown.is_empty() {
                ui.label(egui::RichText::new(if choices.is_empty() { tr("No pictures in the project yet: import one into the media bin.") } else { tr("Nothing matches.") }).weak());
            }
            for row in shown.chunks(COLUMNS) {
                ui.horizontal(|ui| {
                    for c in row {
                        let (rect, response) = ui.allocate_exact_size(egui::vec2(CELL_W, CELL_H), egui::Sense::click());
                        let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(CELL_W, THUMB_H));
                        let skim = response.hover_pos().map(|p| ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0));
                        if skim.is_some() {
                            ui.ctx().request_repaint();
                        }
                        self.paint_choice(ui, c, thumb, skim);
                        let selected = chosen == Some(c.id);
                        let stroke = if selected {
                            egui::Stroke::new(2.0, ui.visuals().selection.stroke.color)
                        } else if response.hovered() {
                            egui::Stroke::new(1.5, ui.visuals().widgets.hovered.fg_stroke.color)
                        } else {
                            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
                        };
                        ui.painter().rect_stroke(thumb, 3.0, stroke, egui::StrokeKind::Inside);
                        let mut job = egui::text::LayoutJob::simple_singleline(c.name.clone(), egui::FontId::proportional(12.0), ui.visuals().text_color());
                        job.wrap = egui::text::TextWrapping::truncate_at_width(CELL_W);
                        let galley = ui.fonts_mut(|f| f.layout_job(job));
                        ui.painter().galley(egui::pos2(rect.center().x - galley.size().x / 2.0, thumb.bottom() + 3.0), galley, ui.visuals().text_color());
                        if response.on_hover_text(&c.name).clicked() {
                            picked = Some(Some(c.id));
                        }
                    }
                });
            }
        });
        picked
    }

    /// A choice's picture fitted into `rect`: a file's frames or a compound clip's,
    /// at `skim` (0..1 through it) or a tenth of the way in.
    fn paint_choice(&mut self, ui: &egui::Ui, c: &Choice, rect: egui::Rect, skim: Option<f32>) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
        let at = skim.map_or(c.length * 0.1, |s| s as f64 * c.length);
        let strip = match &c.file {
            Some((path, size, seconds)) => self.clip_previews.strip(&ctx, oa_doc::MediaId(c.id), path, *size, *seconds).cloned(),
            None => self.compound_strip(&ctx, oa_doc::SeqId(c.id)),
        };
        match strip {
            Some(strip) => {
                let h = rect.height().min(rect.width() / strip.aspect.max(0.01));
                let dest = egui::Rect::from_center_size(rect.center(), egui::vec2(h * strip.aspect, h));
                painter.image(strip.texture.id(), dest, strip.uv(at), egui::Color32::WHITE);
            }
            None => {
                let icon = if c.kind == Kind::Compound { crate::icons::GROUP } else { crate::icons::VIDEO_TRACK };
                crate::icons::paint(&painter, egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(rect.height() * 0.5)), icon, ui.visuals().weak_text_color());
            }
        }
        if c.kind == Kind::Compound {
            painter.text(rect.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "▣", egui::FontId::proportional(11.0), egui::Color32::WHITE);
        }
    }
}
