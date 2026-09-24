//! Editing inside a compound clip: double-click one on the timeline (or "Open compound
//! clip" in its menu) and its own timeline takes over the editor; a breadcrumb bar above
//! the tracks leads back out. Edits land in the compound's sequence, so every clip
//! using it changes with them — it's the same media.

use crate::App;
use eframe::egui;
use oa_doc::{ItemId, ItemKind, SeqId, TrackId, TrackKind};
use oa_time::Time;

/// Where the editor was before stepping into a compound clip.
pub struct Crumb {
    seq: SeqId,
    video_track: TrackId,
    audio_track: TrackId,
    variant: usize,
    playhead: Time,
    /// The compound clip that was opened (selected again on the way out).
    item: ItemId,
}

impl App {
    /// The compound clip `item` (a clip of the open timeline) as the open timeline.
    pub(crate) fn open_compound(&mut self, item: ItemId) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let ItemKind::Nested { sequence } = it.kind else { return };
        let Some(inner) = self.editor.doc.project().sequence(sequence) else { return };
        let first = |kind: TrackKind| inner.tracks.iter().find(|t| t.kind == kind).map(|t| t.id);
        let Some(video) = first(TrackKind::Video).or_else(|| inner.tracks.first().map(|t| t.id)) else {
            self.report_error("This compound clip has no tracks to edit.");
            return;
        };
        let audio = first(TrackKind::Audio).unwrap_or(video);
        // The playhead keeps its place in the picture: the moment of the compound's own
        // timeline that's showing now.
        let inside = it.eval_context(self.playhead.max(it.range.start).min(it.range.end() - Time(1))).source_time.max(Time::ZERO);
        self.set_playing(false);
        self.compound_trail.push(Crumb {
            seq: self.editor.seq,
            video_track: self.editor.video_track,
            audio_track: self.editor.audio_track,
            variant: self.variant,
            playhead: self.playhead,
            item,
        });
        self.editor.seq = sequence;
        self.editor.video_track = video;
        self.editor.audio_track = audio;
        self.variant = 0;
        self.entered_timeline(inside, None);
    }

    /// Steps out `levels` compound clips (all the way with `usize::MAX`).
    pub(crate) fn close_compound(&mut self, levels: usize) {
        let mut back = None;
        for _ in 0..levels {
            match self.compound_trail.pop() {
                Some(c) => back = Some(c),
                None => break,
            }
        }
        let Some(c) = back else { return };
        self.set_playing(false);
        self.editor.seq = c.seq;
        self.editor.video_track = c.video_track;
        self.editor.audio_track = c.audio_track;
        self.variant = c.variant;
        self.entered_timeline(c.playhead, Some(c.item));
    }

    /// Undo can take away the compound being edited (or one around it): step out to
    /// the nearest timeline that still exists.
    pub(crate) fn compound_still_there(&mut self) {
        let exists = |app: &Self, seq: SeqId| app.editor.doc.project().sequence(seq).is_some();
        if self.compound_trail.is_empty() || exists(self, self.editor.seq) && self.compound_trail.iter().all(|c| exists(self, c.seq)) {
            return;
        }
        let keep = self.compound_trail.iter().position(|c| !exists(self, c.seq)).unwrap_or(self.compound_trail.len());
        self.close_compound(self.compound_trail.len() - keep + 1);
        self.notify("The compound clip being edited was undone; back on its timeline.");
    }

    /// The project's own timeline and the format it's shown in — what's exported and
    /// pictured on the start page, even while a compound clip is open.
    pub(crate) fn main_timeline(&self) -> (SeqId, oa_doc::VariantId) {
        match self.compound_trail.first().and_then(|c| Some((c, self.editor.doc.project().sequence(c.seq)?))) {
            Some((c, s)) if !s.variants.is_empty() => (c.seq, s.variants[c.variant.min(s.variants.len() - 1)].id),
            _ => (self.editor.seq, self.variant_id()),
        }
    }

    /// Everything tied to the timeline that was open starts over for the new one.
    fn entered_timeline(&mut self, playhead: Time, select: Option<ItemId>) {
        self.selected.clear();
        self.selection = select.filter(|id| self.editor.item(*id).is_some());
        if let Some(id) = self.selection {
            self.selected.insert(id);
        }
        self.canvas_text = None;
        self.crop_mode = None;
        self.viewer_drag = None;
        self.timeline_drag = None;
        self.timeline_view.fit = true;
        self.set_playhead(playhead);
        self.rendered = None;
        self.mixed = None;
    }

    /// "Main ▸ Compound 2 ▸ …" above the tracks while inside a compound clip.
    pub(crate) fn compound_trail_bar(&mut self, ui: &mut egui::Ui) {
        if self.compound_trail.is_empty() {
            return;
        }
        let project = self.editor.doc.project();
        let name = |seq: SeqId| project.sequence(seq).map_or_else(|| "?".to_string(), |s| s.name.clone());
        let mut names: Vec<String> = self.compound_trail.iter().map(|c| name(c.seq)).collect();
        names.push(name(self.editor.seq));
        let mut go_up: Option<usize> = None;
        egui::Frame::new().fill(crate::style::ACCENT.gamma_multiply(0.18)).corner_radius(4.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("⬅ Back").on_hover_text("Close this compound clip (Esc with nothing selected)").clicked() {
                    go_up = Some(1);
                }
                let last = names.len() - 1;
                for (i, n) in names.iter().enumerate() {
                    if i > 0 {
                        ui.label(egui::RichText::new("▸").weak());
                    }
                    if i == last {
                        ui.strong(n);
                    } else if ui.link(n).clicked() {
                        go_up = Some(last - i);
                    }
                }
                ui.label(egui::RichText::new("Editing a compound clip: every use of it changes.").small().weak());
            });
        });
        if let Some(n) = go_up {
            self.close_compound(n);
        }
    }
}
