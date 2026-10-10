//! Quick cut: the parts of a file worth keeping, picked in a window before any of it
//! goes on the timeline. A preview of the file as it plays (rendered on the preview
//! thread, like any frame), a strip of its frames (or its waveform) to scrub and to
//! drag parts out on, and the parts listed; "Add" puts them on the timeline one after
//! another (`Editor::append_parts`, one undo step).

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{CanvasSize, FormatVariant, Item, ItemId, ItemKind, MediaId, Project, SeqId, Sequence, Track, TrackId, TrackKind, VariantId};
use oa_media::MediaKind;
use oa_time::{Time, TimeRange};
use std::path::PathBuf;
use std::sync::Arc;

/// Parts shorter than this (seconds) aren't kept.
const SHORTEST: f64 = 0.1;
/// Screen px within which a part's edge is grabbed.
const EDGE_PX: f32 = 6.0;

/// What a drag on the strip is doing.
#[derive(Clone, Copy, PartialEq)]
enum CutDrag {
    /// Drawing a new part from `from` (seconds).
    New { from: f64 },
    /// Moving one end of part `part`.
    Edge { part: usize, start: bool },
}

/// The window's state, for one file.
pub struct QuickCut {
    media: MediaId,
    name: String,
    path: PathBuf,
    /// Seconds of the file.
    duration: f64,
    /// The picture's size (none: sound only).
    size: Option<[u32; 2]>,
    /// Where the preview is (seconds into the file).
    at: f64,
    playing: bool,
    /// The parts kept: (from, to) seconds, in order, not overlapping.
    parts: Vec<(f64, f64)>,
    /// An in point marked with I, waiting for O.
    mark_in: Option<f64>,
    picked: Option<usize>,
    drag: Option<CutDrag>,
    /// The frame on screen, and the position it was asked for.
    preview: Option<(egui::TextureId, wgpu::Texture)>,
    asked: Option<u64>,
}

/// `parts` with `(a, b)` added: kept in order, overlaps joined into one, and ones too
/// short to matter dropped. Returns where the new (or joined) part ended up.
pub fn add_part(parts: &mut Vec<(f64, f64)>, a: f64, b: f64) -> Option<usize> {
    let (mut a, mut b) = (a.min(b), a.max(b));
    if b - a < SHORTEST {
        return None;
    }
    // Swallow every part it touches.
    parts.retain(|&(x, y)| {
        let touches = x <= b && y >= a;
        if touches {
            a = a.min(x);
            b = b.max(y);
        }
        !touches
    });
    let at = parts.partition_point(|p| p.0 < a);
    parts.insert(at, (a, b));
    Some(at)
}

fn clock(s: f64) -> String {
    let s = s.max(0.0);
    format!("{}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
}

impl QuickCut {
    /// Hashes where the preview is, for knowing whether the frame on screen is it.
    fn key(&self) -> u64 {
        (self.at * 1000.0).round() as u64 ^ (self.media.0 << 40)
    }
}

impl App {
    /// Opens quick cut on a file in the bin.
    pub(crate) fn open_quick_cut(&mut self, media: MediaId) {
        let Some(m) = self.editor.pool_item(media).filter(|m| !m.missing && m.kind != MediaKind::Still) else { return };
        let size = m.probe.video.as_ref().map(|v| [v.width, v.height]);
        let (name, path, duration) = (m.name.clone(), m.decode_path.clone(), m.probe.duration.as_seconds_f64().max(0.04));
        self.set_playing(false);
        self.bin.quick_cut = Some(QuickCut {
            media,
            name,
            path,
            duration,
            size,
            at: 0.0,
            playing: false,
            parts: Vec::new(),
            mark_in: None,
            picked: None,
            drag: None,
            preview: None,
            asked: None,
        });
    }

    /// Asks the preview thread for the file's frame at the window's position: a scratch
    /// one-clip project, the file at its own size (up to `width` px across).
    fn quick_cut_frame(&mut self, width: f32, ctx: &egui::Context) {
        let Some(q) = self.bin.quick_cut.as_ref() else { return };
        let Some(size) = q.size else { return };
        let key = q.key();
        if q.asked == Some(key) {
            return;
        }
        let Some(media_ref) = self.editor.doc.project().media(q.media).cloned() else { return };
        let media_ref = Arc::new(media_ref);
        let (seq_id, variant_id) = (SeqId(u64::MAX - 20), VariantId(u64::MAX - 21));
        let variant = FormatVariant { id: variant_id, name: "Quick cut".into(), size: CanvasSize::new(size[0].max(2), size[1].max(2)), overrides: Default::default() };
        let mut seq = Sequence::new(seq_id, "Quick cut", self.editor.sequence().rate, variant);
        let mut track = Track::new(TrackId(u64::MAX - 22), "V1", TrackKind::Video);
        track.items.push(Item::new(ItemId(u64::MAX - 23), &q.name, ItemKind::Media { media: q.media }, TimeRange::new(Time::ZERO, Time::from_seconds_f64(q.duration))));
        seq.tracks.push(Arc::new(track));
        let mut project = Project::new("Quick cut");
        project.media.insert(q.media, media_ref);
        project.sequences.insert(seq_id, Arc::new(seq));
        let scale = ((width * ctx.pixels_per_point()) as f64 / size[0].max(1) as f64).min(1.0);
        // Not quite at the very end: the last frame is the one before it.
        let at = Time::from_seconds_f64(q.at.min(q.duration - 0.02).max(0.0));
        self.preview_worker.request(crate::preview_worker::Request {
            slot: crate::preview_worker::QUICK_CUT,
            tag: key,
            project: Arc::new(project),
            registry: self.registry.clone(),
            seq: seq_id,
            variant: variant_id,
            at,
            scale,
            wanted: None,
            png: None,
            see_through: false,
        });
        if let Some(q) = self.bin.quick_cut.as_mut() {
            q.asked = Some(key);
        }
    }

    /// The window, while it's open.
    pub(crate) fn quick_cut_window(&mut self, ctx: &egui::Context) {
        if self.bin.quick_cut.is_none() {
            return;
        }
        // A frame that's come back takes the preview's place.
        if let Some(done) = self.preview_worker.take(crate::preview_worker::QUICK_CUT)
            && let Some(rs) = &self.render_state
            && let Some(q) = self.bin.quick_cut.as_mut()
        {
            let view = oa_gpu::readback::display_view(&done.texture);
            let id = match q.preview.take() {
                Some((id, _)) => {
                    rs.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, id);
                    id
                }
                None => rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
            };
            q.preview = Some((id, done.texture));
        }
        // Playing moves the preview along in real time (the preview thread keeps up as
        // it can; frames it can't make in time are skipped).
        let dt = ctx.input(|i| i.stable_dt).min(0.1) as f64;
        if let Some(q) = self.bin.quick_cut.as_mut()
            && q.playing
        {
            q.at += dt;
            if q.at >= q.duration {
                q.at = q.duration;
                q.playing = false;
            }
            ctx.request_repaint();
        }

        let mut close = false;
        let mut add = false;
        let width = (ctx.content_rect().width() - 80.0).clamp(420.0, 760.0);
        self.quick_cut_frame(width, ctx);
        egui::Modal::new(egui::Id::new("quick-cut")).show(ctx, |ui| {
            ui.set_width(width);
            let Some(q) = self.bin.quick_cut.as_mut() else { return };
            // Keys: I and O mark, Space plays, Delete removes the picked part.
            let typing = ui.ctx().egui_wants_keyboard_input();
            let (key_i, key_o, space, delete, left, right) = ui.input(|i| {
                (i.key_pressed(egui::Key::I), i.key_pressed(egui::Key::O), i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace), i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight))
            });
            let frame = 1.0 / 30.0;
            if !typing {
                if key_i {
                    q.mark_in = Some(q.at);
                }
                if key_o {
                    match q.mark_in.take() {
                        Some(a) => q.picked = add_part(&mut q.parts, a, q.at),
                        None => q.picked = add_part(&mut q.parts, 0.0, q.at),
                    }
                }
                if space {
                    q.playing = !q.playing;
                    if q.playing && q.at >= q.duration - 0.01 {
                        q.at = 0.0;
                    }
                }
                if delete && let Some(i) = q.picked.take().filter(|i| *i < q.parts.len()) {
                    q.parts.remove(i);
                }
                if left {
                    q.at = (q.at - frame).max(0.0);
                }
                if right {
                    q.at = (q.at + frame).min(q.duration);
                }
            }

            // The title.
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                crate::icons::paint(ui.painter(), r, crate::icons::SPLIT, crate::style::ACCENT);
                ui.label(egui::RichText::new(trf("Quick cut — {0}", &[("0", &q.name)])).strong().size(crate::style::TEXT_L));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, crate::icons::CLOSE, tr("Close without adding"), "Esc", true).clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(crate::style::GAP_S);

            // The preview: the frame (or, for sound, its waveform big).
            let aspect = q.size.map_or(16.0 / 9.0, |s| s[0] as f32 / s[1].max(1) as f32);
            let box_h = (width / aspect).min(ctx.content_rect().height() * 0.45).max(120.0);
            let (area, _) = ui.allocate_exact_size(egui::vec2(width, box_h), egui::Sense::hover());
            ui.painter().rect_filled(area, 6.0, egui::Color32::from_gray(10));
            match (&q.preview, q.size) {
                (Some((id, _)), Some(_)) => {
                    let h = area.height().min(area.width() / aspect);
                    let shown = egui::Rect::from_center_size(area.center(), egui::vec2(h * aspect, h));
                    ui.painter().image(*id, shown, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                (None, Some(_)) => {
                    ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, tr("Preparing the preview…"), egui::FontId::proportional(12.0), egui::Color32::from_gray(140));
                }
                (_, None) => {
                    let (media, path) = (q.media, q.path.clone());
                    let duration = q.duration;
                    let at = q.at;
                    if let Some(peaks) = self.clip_previews.peaks(ui.ctx(), media, &path) {
                        paint_peaks(ui.painter(), area.shrink(12.0), &peaks, egui::Color32::from_rgb(110, 190, 150));
                    }
                    let x = area.left() + (at / duration) as f32 * area.width();
                    ui.painter().line_segment([egui::pos2(x, area.top()), egui::pos2(x, area.bottom())], egui::Stroke::new(1.5, crate::style::GOLD));
                }
            }
            let Some(q) = self.bin.quick_cut.as_mut() else { return };

            // Playback and marking.
            ui.add_space(crate::style::GAP_S);
            ui.horizontal(|ui| {
                let (icon, tip) = if q.playing { (crate::icons::PAUSE, "Pause") } else { (crate::icons::PLAY, "Play") };
                if crate::icons::button(ui, icon, tip, "Space", true).clicked() {
                    q.playing = !q.playing;
                    if q.playing && q.at >= q.duration - 0.01 {
                        q.at = 0.0;
                    }
                }
                ui.label(egui::RichText::new(format!("{} / {}", clock(q.at), clock(q.duration))).monospace());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Mark out  O")).on_hover_text(tr("End a part here (from the in mark, or from the start)")).clicked() {
                        let a = q.mark_in.take().unwrap_or(0.0);
                        q.picked = add_part(&mut q.parts, a, q.at);
                    }
                    let lit = q.mark_in.is_some();
                    if ui.add(egui::Button::new(tr("Mark in  I")).selected(lit)).on_hover_text(tr("Start a part here")).clicked() {
                        q.mark_in = Some(q.at);
                    }
                });
            });
            // The strip and the list, with the app at hand (thumbnails, peaks).
            ui.add_space(crate::style::GAP_S);
            self.quick_cut_strip(ui, width);
            self.quick_cut_parts(ui);
            ui.add_space(crate::style::GAP);
            let n = self.bin.quick_cut.as_ref().map_or(0, |q| q.parts.len());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("Drag on the strip to keep a part · I and O mark in and out · Space plays · ← → step")).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if n == 1 { tr("Add 1 part").to_string() } else { trf("Add {n} parts", &[("n", &n.to_string())]) };
                    let go = ui.add_enabled(n > 0, egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE)).fill(crate::style::ACCENT).min_size(egui::vec2(110.0, 28.0)));
                    if go.on_hover_text(tr("Put them on the timeline, one after another, at the end")).on_disabled_hover_text(tr("Drag on the strip to keep a part first")).clicked() {
                        add = true;
                    }
                    if ui.add(egui::Button::new(tr("Cancel")).min_size(egui::vec2(80.0, 28.0))).clicked() {
                        close = true;
                    }
                });
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if add && let Some(q) = self.bin.quick_cut.take() {
            let parts: Vec<(Time, Time)> = q.parts.iter().map(|&(a, b)| (Time::from_seconds_f64(a), Time::from_seconds_f64(b - a))).collect();
            match self.editor.append_parts(q.media, &parts) {
                Ok(ids) => {
                    self.selected = ids.iter().copied().collect();
                    self.selection = ids.last().copied();
                    self.sync_selection();
                    self.notify(trf("Added {n} parts of {0}.", &[("n", &ids.len().to_string()), ("0", &q.name)]));
                }
                Err(e) => self.report_error(e.to_string()),
            }
            return;
        }
        if close {
            self.bin.quick_cut = None;
        }
    }

    /// The strip under the preview: the file's frames (or waveform) across, the parts
    /// kept lit with handles, the in mark and the playhead. Click to move the playhead,
    /// drag to keep a part, drag a part's end to change it.
    fn quick_cut_strip(&mut self, ui: &mut egui::Ui, width: f32) {
        let Some(q) = self.bin.quick_cut.as_ref() else { return };
        let (media, path, size, duration) = (q.media, q.path.clone(), q.size, q.duration);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(width, 54.0), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(2.0));
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(14));
        // The file across: frames tiled along it, or its waveform.
        match size {
            Some(size) => {
                if let Some(strip) = self.clip_previews.strip(ui.ctx(), media, &path, size, duration) {
                    let tile_w = rect.height() * strip.aspect;
                    let tiles = (rect.width() / tile_w.max(8.0)).ceil().max(1.0) as usize;
                    let w = rect.width() / tiles as f32;
                    for i in 0..tiles {
                        let t = (i as f64 + 0.5) / tiles as f64 * duration;
                        let cell = egui::Rect::from_min_size(egui::pos2(rect.left() + i as f32 * w, rect.top()), egui::vec2(w, rect.height()));
                        painter.image(strip.texture.id(), cell, strip.uv(t), egui::Color32::from_gray(150));
                    }
                }
            }
            None => {
                if let Some(peaks) = self.clip_previews.peaks(ui.ctx(), media, &path) {
                    paint_peaks(&painter, rect.shrink2(egui::vec2(2.0, 6.0)), &peaks, egui::Color32::from_rgb(90, 160, 125));
                }
            }
        }
        let Some(q) = self.bin.quick_cut.as_mut() else { return };
        let x_of = |s: f64| rect.left() + (s / duration).clamp(0.0, 1.0) as f32 * rect.width();
        let s_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
        // Pointer: which end of a part it's near, if any.
        let edge_near = |q: &QuickCut, x: f32| -> Option<(usize, bool)> {
            q.parts.iter().enumerate().flat_map(|(i, &(a, b))| [(i, true, x_of(a)), (i, false, x_of(b))]).filter(|(_, _, ex)| (ex - x).abs() <= EDGE_PX).min_by(|a, b| (a.2 - x).abs().total_cmp(&(b.2 - x).abs())).map(|(i, s, _)| (i, s))
        };
        if let Some(p) = r.hover_pos()
            && edge_near(q, p.x).is_some()
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if r.drag_started()
            && let Some(p) = r.interact_pointer_pos()
        {
            q.playing = false;
            q.drag = Some(match edge_near(q, p.x) {
                Some((part, start)) => CutDrag::Edge { part, start },
                None => CutDrag::New { from: s_of(p.x) },
            });
        }
        if r.dragged()
            && let Some(p) = r.interact_pointer_pos()
        {
            let s = s_of(p.x);
            match q.drag {
                Some(CutDrag::Edge { part, start }) if part < q.parts.len() => {
                    let (a, b) = q.parts[part];
                    // Not past its other end, nor into its neighbors.
                    let lo = if part > 0 { q.parts[part - 1].1 } else { 0.0 };
                    let hi = q.parts.get(part + 1).map_or(duration, |n| n.0);
                    q.parts[part] = if start { (s.clamp(lo, b - SHORTEST), b) } else { (a, s.clamp(a + SHORTEST, hi)) };
                    q.picked = Some(part);
                }
                _ => {}
            }
            q.at = s;
        }
        if r.drag_stopped() {
            if let (Some(CutDrag::New { from }), Some(p)) = (q.drag, r.interact_pointer_pos()) {
                q.picked = add_part(&mut q.parts, from, s_of(p.x));
            }
            q.drag = None;
        }
        if r.clicked()
            && let Some(p) = r.interact_pointer_pos()
        {
            q.at = s_of(p.x);
            q.picked = q.parts.iter().position(|&(a, b)| q.at >= a && q.at <= b);
        }
        // The parts kept, a part being drawn, the in mark, the playhead.
        for (i, &(a, b)) in q.parts.iter().enumerate() {
            let part = egui::Rect::from_min_max(egui::pos2(x_of(a), rect.top()), egui::pos2(x_of(b), rect.bottom()));
            let lit = q.picked == Some(i);
            painter.rect_filled(part, 3.0, crate::style::ACCENT.gamma_multiply(if lit { 0.45 } else { 0.3 }));
            painter.rect_stroke(part, 3.0, egui::Stroke::new(if lit { 2.0 } else { 1.5 }, crate::style::ACCENT), egui::StrokeKind::Inside);
            for x in [part.left(), part.right()] {
                painter.rect_filled(egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(4.0, 18.0)), 2.0, egui::Color32::WHITE);
            }
            painter.text(part.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, (i + 1).to_string(), egui::FontId::proportional(10.0), egui::Color32::WHITE);
        }
        if let (Some(CutDrag::New { from }), Some(p)) = (q.drag, ui.input(|i| i.pointer.latest_pos())) {
            let (a, b) = (x_of(from), p.x.clamp(rect.left(), rect.right()));
            let draft = egui::Rect::from_min_max(egui::pos2(a.min(b), rect.top()), egui::pos2(a.max(b), rect.bottom()));
            painter.rect_filled(draft, 3.0, crate::style::ACCENT.gamma_multiply(0.22));
            painter.add(egui::Shape::dashed_line(&[draft.left_top(), draft.right_top(), draft.right_bottom(), draft.left_bottom(), draft.left_top()], egui::Stroke::new(1.0, crate::style::ACCENT), 4.0, 3.0));
        }
        if let Some(m) = q.mark_in {
            let x = x_of(m);
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, crate::style::ACCENT));
            painter.text(egui::pos2(x + 3.0, rect.bottom() - 2.0), egui::Align2::LEFT_BOTTOM, "I", egui::FontId::proportional(10.0), crate::style::ACCENT);
        }
        let x = x_of(q.at);
        painter.line_segment([egui::pos2(x, rect.top() - 2.0), egui::pos2(x, rect.bottom() + 2.0)], egui::Stroke::new(2.0, crate::style::GOLD));
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Outside);
    }

    /// The parts kept, as a list: where each is, how long, jump to it, take it out.
    fn quick_cut_parts(&mut self, ui: &mut egui::Ui) {
        let Some(q) = self.bin.quick_cut.as_mut() else { return };
        ui.add_space(crate::style::GAP_S);
        if q.parts.is_empty() {
            ui.label(egui::RichText::new(tr("No parts yet.")).weak());
            return;
        }
        let total: f64 = q.parts.iter().map(|(a, b)| b - a).sum();
        ui.label(egui::RichText::new(trf("{n} parts · {0} in all", &[("n", &q.parts.len().to_string()), ("0", &clock(total))])).strong());
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            for (i, &(a, b)) in q.parts.iter().enumerate() {
                let lit = q.picked == Some(i);
                let r = ui
                    .horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{}.", i + 1)).color(if lit { crate::style::ACCENT } else { ui.visuals().weak_text_color() }));
                        ui.label(egui::RichText::new(format!("{} – {}", clock(a), clock(b))).monospace());
                        ui.label(egui::RichText::new(format!("{:.2} s", b - a)).weak());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::icons::button(ui, crate::icons::DELETE, tr("Take this part out"), "Delete", true).clicked() {
                                remove = Some(i);
                            }
                            if crate::icons::button(ui, crate::icons::PLAY, tr("Play from its start"), "", true).clicked() {
                                q.at = a;
                                q.picked = Some(i);
                                q.playing = true;
                            }
                        });
                    })
                    .response;
                if r.interact(egui::Sense::click()).clicked() {
                    q.at = a;
                    q.picked = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            q.parts.remove(i);
            q.picked = None;
        }
    }
}

/// A waveform's peaks across `rect`.
fn paint_peaks(painter: &egui::Painter, rect: egui::Rect, peaks: &[(f32, f32)], color: egui::Color32) {
    let n = peaks.len().max(1);
    let (mid, half) = (rect.center().y, rect.height() / 2.0);
    let mut x = rect.left();
    while x < rect.right() {
        let i = (((x - rect.left()) / rect.width()) * n as f32) as usize;
        let j = ((((x + 2.0 - rect.left()) / rect.width()) * n as f32) as usize).clamp(i + 1, n);
        let (lo, hi) = peaks[i.min(n - 1)..j.min(n)].iter().fold((0.0f32, 0.0f32), |(a, b), (l, h)| (a.min(*l), b.max(*h)));
        painter.line_segment([egui::pos2(x, mid - hi * half), egui::pos2(x, mid - lo * half)], egui::Stroke::new(1.5, color));
        x += 2.0;
    }
}

#[cfg(test)]
mod tests {
    use super::add_part;

    /// Parts stay in order; overlapping ones join; ones too short are dropped.
    #[test]
    fn parts_join_and_keep_their_order() {
        let mut parts = Vec::new();
        assert_eq!(add_part(&mut parts, 5.0, 7.0), Some(0));
        assert_eq!(add_part(&mut parts, 3.0, 1.0), Some(0), "drawn backwards, still a part, first");
        assert_eq!(parts, [(1.0, 3.0), (5.0, 7.0)]);
        assert_eq!(add_part(&mut parts, 2.5, 5.5), Some(0), "touching both joins all three");
        assert_eq!(parts, [(1.0, 7.0)]);
        assert_eq!(add_part(&mut parts, 9.0, 9.05), None, "too short");
        assert_eq!(parts.len(), 1);
    }
}
