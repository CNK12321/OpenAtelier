//! The timeline: tracks, clips and the playhead.
//!
//! Click a clip to select it (and its group); Ctrl/Shift+click adds or removes; drag on
//! empty space draws a selection box. Drag a clip's middle to move the selection (onto
//! other tracks of the same kind, too), or an edge to trim. Moves and trims snap to clip
//! edges and the playhead ("Snap"; Ctrl flips it while dragging) and land on frame
//! boundaries. Drag or click in the ruler to scrub. Right-click a clip, a track header or
//! empty space for a menu (cut/copy/paste/duplicate, split, group, enable, delete; track
//! rename/reorder/delete). Ctrl+wheel zooms, the wheel scrolls, and the view follows the
//! playhead while playing. The commands live in `oa_edit::timeline` and `clips.rs`.
//!
//! Tracks: double-click a header (or empty space on a track) to pick it — Ctrl+V and
//! Ctrl+D then put clips in its first free space after the playhead, Alt+↑/↓ moves it —
//! and drag a header up or down to reorder. **Dividers** (right-click the timeline, or
//! the track menu) cut the whole timeline, every track, into colored sections: drag a
//! divider's line to move it, drag its flag in the ruler to move the whole section among
//! the others, right-click it to name, recolor, select, clear or delete its section
//! (`oa_edit::sections`).

use crate::App;
use eframe::egui;
use oa_doc::{ItemId, TrackId, TrackKind};
use oa_edit::timeline::{self as cmd, Edge, Snapper};
use oa_time::Time;

/// Track heights the user can pick between (the ⇕ button in the timeline's tools).
pub const ROW_HEIGHTS: [f32; 4] = [22.0, 30.0, 46.0, 72.0];
const RULER_HEIGHT: f32 = 18.0;
const HEADER_WIDTH: f32 = 64.0;
/// Screen px from a clip's edge that grab the edge (trim) instead of the body (move).
const EDGE_PX: f32 = 7.0;
const SNAP_PX: f32 = 8.0;

/// What part of the timeline is visible.
pub struct TimelineView {
    /// Seconds at the left edge of the clip lane.
    pub start: f64,
    /// Seconds across the lane.
    pub span: f64,
    /// Follow the sequence length (until the user zooms or scrolls).
    pub fit: bool,
    /// What the open right-click menu is for.
    pub(crate) menu: Option<Menu>,
    /// Index into [`ROW_HEIGHTS`]: how tall each track is drawn.
    pub row_height: usize,
}

impl TimelineView {
    pub fn track_height(&self) -> f32 {
        ROW_HEIGHTS[self.row_height.min(ROW_HEIGHTS.len() - 1)]
    }
}

impl Default for TimelineView {
    fn default() -> Self {
        TimelineView { start: 0.0, span: 10.0, fit: true, menu: None, row_height: 1 }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Menu {
    /// The selected clips.
    Clips,
    Track(TrackId),
    /// Empty space at this time, on this track (if over one).
    Empty(Time, Option<TrackId>),
    /// A divider (and the section it starts).
    Divider(u64),
    /// Key `index` of a clip's keyframe line.
    Key(ItemId, usize),
}

pub enum TimelineDrag {
    /// Dragging in the ruler moves the playhead.
    Scrub,
    /// Moving the selection; `anchor` is the clip under the pointer.
    Move { anchor: ItemId, grab: Time },
    Trim { item: ItemId, edge: Edge },
    /// A selection box from `from`; `base` was selected before (Ctrl/Shift adds to it).
    Marquee { from: egui::Pos2, base: std::collections::BTreeSet<ItemId> },
    /// A track header dragged up or down to reorder the tracks.
    Track { track: TrackId },
    /// A divider line dragged along the timeline (it alone moves).
    Divider { divider: u64 },
    /// A section dragged by its divider's flag, to another place among the sections.
    Section { index: usize },
    /// The clip's keyframe line: one key, or the whole line (`key: None`).
    Band { item: ItemId, band: crate::band::Band, key: Option<usize>, grab_y: f32, original: oa_params::ParamSource, rect: egui::Rect },
}

/// A clip as drawn: where it is and what it is.
struct ClipBox {
    id: ItemId,
    name: String,
    rect: egui::Rect,
    kind: TrackKind,
    enabled: bool,
    keys: Vec<f32>,
    text: bool,
    /// Where the longest intro ends and the longest outro starts (x), if any.
    intro: Option<f32>,
    outro: Option<f32>,
    /// Media clips: the file, and where the clip's start falls in it and how fast it plays.
    media: Option<(oa_doc::MediaId, f64, f64)>,
    start: f64,
    /// An effect container (on an effect track).
    container: bool,
}

/// Clips narrower than this (px) are drawn together as runs.
const TINY_CLIP_PX: f32 = 3.0;

/// Neighboring clips too narrow to see one by one (zoomed far out), drawn as one block.
struct ClipRun {
    row: usize,
    rect: egui::Rect,
    ids: Vec<ItemId>,
    kind: TrackKind,
    enabled: bool,
}

/// A divider as drawn: its line, the tinted stretch it starts, and its flag.
struct Mark {
    /// Its section's index.
    index: usize,
    divider: oa_doc::Divider,
    x: f32,
    flag: egui::Rect,
    band: egui::Rect,
    label: Option<std::sync::Arc<egui::Galley>>,
}

type Row = (TrackId, TrackKind, String, bool);

/// The row a dragged track header would land on (among tracks of its kind), if it would
/// move at all.
fn track_drop_row(rows: &[Row], track: TrackId, y: f32, row_at: &dyn Fn(f32) -> Option<usize>, top: f32) -> Option<usize> {
    let from = rows.iter().position(|r| r.0 == track)?;
    let kind = rows[from].1;
    let same: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].1 == kind).collect();
    let (lo, hi) = (*same.first()?, *same.last()?);
    let r = row_at(y).unwrap_or(if y < top { lo } else { hi }).clamp(lo, hi);
    (r != from).then_some(r)
}

impl App {
    /// Moves `track` to row `to` of the timeline (as drawn: video top-down, then audio),
    /// in one step.
    fn reorder_track(&mut self, rows: &[Row], track: TrackId, to: usize) {
        let Some(kind) = rows.iter().find(|r| r.0 == track).map(|r| r.1) else { return };
        let mut shown: Vec<TrackId> = rows.iter().filter(|r| r.1 == kind).map(|r| r.0).collect();
        let Some(from) = shown.iter().position(|t| *t == track) else { return };
        let to = rows[..to.min(rows.len())].iter().filter(|r| r.1 == kind).count();
        shown.remove(from);
        shown.insert(to.min(shown.len()), track);
        // The document lists video bottom-up.
        if kind == TrackKind::Video {
            shown.reverse();
        }
        let s = self.editor.sequence();
        let Some(arc) = s.tracks.iter().find(|t| t.id == track).cloned() else { return };
        let rest: Vec<TrackId> = s.tracks.iter().map(|t| t.id).filter(|t| *t != track).collect();
        let at = shown.iter().position(|t| *t == track).unwrap_or(0);
        let index = match (shown.get(at + 1), at.checked_sub(1).and_then(|i| shown.get(i))) {
            (Some(next), _) => rest.iter().position(|t| t == next),
            (None, Some(prev)) => rest.iter().position(|t| t == prev).map(|i| i + 1),
            _ => None,
        };
        let Some(index) = index else { return };
        let seq = self.editor.seq;
        let ops = vec![oa_doc::Op::RemoveTrack { seq, track }, oa_doc::Op::InsertTrack { seq, index, track: arc }];
        if let Err(e) = self.editor.apply("Move track", ops) {
            self.report_error(format!("can't move the track: {e}"));
        }
    }

    pub(crate) fn timeline(&mut self, ui: &mut egui::Ui) {
        // Video tracks top-down (V2 above V1), then audio.
        let seq = self.editor.sequence();
        let mut rows: Vec<(TrackId, TrackKind, String, bool)> = seq
            .tracks
            .iter()
            .rev()
            .filter(|t| t.kind == TrackKind::Video)
            .map(|t| (t.id, t.kind, t.name.clone(), t.enabled))
            .collect();
        rows.extend(seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| (t.id, t.kind, t.name.clone(), t.enabled)));

        // A picked track that's gone (deleted, or another timeline opened) is let go.
        if self.selected_track.is_some_and(|t| seq.track(t).is_none()) {
            self.selected_track = None;
        }
        let row_height = self.timeline_view.track_height();
        // Effect tracks are half height: they hold effects, not pictures to look at.
        let fx_rows: Vec<bool> = rows.iter().map(|r| seq.track(r.0).is_some_and(|t| t.effects)).collect();
        let row_h = |r: usize| if fx_rows.get(r).copied().unwrap_or(false) { (row_height * 0.5).max(18.0) } else { row_height };
        let tops: Vec<f32> = std::iter::once(0.0).chain((0..rows.len()).scan(0.0, |y, r| {
            *y += row_h(r);
            Some(*y)
        })).collect();
        let height = RULER_HEIGHT + tops.last().copied().unwrap_or(0.0).max(row_height) + 4.0;
        let (response, painter) = ui.allocate_painter(egui::vec2(ui.available_width(), height), egui::Sense::click_and_drag());
        let full = response.rect;
        let lane = egui::Rect::from_min_max(egui::pos2(full.left() + HEADER_WIDTH, full.top()), full.max);

        // The view: fit to the sequence unless the user zoomed or scrolled. It stays put
        // while dragging, so trimming the last clip doesn't rescale under the pointer.
        let duration = self.editor.duration().as_seconds_f64();
        let view = &mut self.timeline_view;
        if view.fit && self.timeline_drag.is_none() {
            view.start = 0.0;
            view.span = (duration * 1.15).max(10.0);
        }
        if let Some(pointer) = response.hover_pos() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta()));
            if zoom != 1.0 {
                // Keep the time under the pointer where it is.
                let at = view.start + ((pointer.x - lane.left()) / lane.width()).clamp(0.0, 1.0) as f64 * view.span;
                view.span = (view.span / zoom as f64).clamp(0.2, 3600.0);
                view.start = (at - ((pointer.x - lane.left()) / lane.width()).clamp(0.0, 1.0) as f64 * view.span).max(0.0);
                view.fit = false;
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let px = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                view.start = (view.start - px as f64 / lane.width() as f64 * view.span).max(0.0);
                view.fit = false;
            }
        }
        if self.playing && !view.fit {
            let t = self.playhead.as_seconds_f64();
            if t < view.start || t > view.start + view.span * 0.95 {
                view.start = (t - view.span * 0.05).max(0.0);
            }
        }
        let (start, span) = (view.start, view.span);
        let x_of = |t: Time| lane.left() + ((t.as_seconds_f64() - start) / span) as f32 * lane.width();
        let time_at = |x: f32| Time::from_seconds_f64((start + ((x - lane.left()) / lane.width()) as f64 * span).max(0.0));
        let px_to_time = |px: f32| Time::from_seconds_f64(px as f64 / lane.width() as f64 * span);
        let row_top = |i: usize| full.top() + RULER_HEIGHT + tops[i.min(tops.len() - 1)];
        let row_at = |y: f32| {
            let y = y - full.top() - RULER_HEIGHT;
            (y >= 0.0).then(|| tops.partition_point(|top| *top <= y)).and_then(|i| i.checked_sub(1)).filter(|i| *i < rows.len())
        };

        // Lay out the clips — only the ones in view (and a little either side). A track's
        // clips are in time order and never overlap, so the first and last in view are a
        // binary search away: an hour-long edit costs what a one-minute one does.
        let (seen_from, seen_to) = (time_at(lane.left() - 64.0), time_at(lane.right() + 64.0));
        let in_view = |items: &[oa_doc::Item]| {
            let first = items.partition_point(|it| it.range.end() <= seen_from);
            let last = first + items[first..].partition_point(|it| it.range.start < seen_to);
            first..last
        };
        let mut boxes = Vec::new();
        let mut bands: Vec<(ItemId, egui::Rect)> = Vec::new();
        let mut runs: Vec<ClipRun> = Vec::new();
        for (r, (track_id, kind, _, _)) in rows.iter().enumerate() {
            let Some(track) = seq.track(*track_id) else { continue };
            let shown = in_view(&track.items);
            for item in &track.items[shown.clone()] {
                let x0 = x_of(item.range.start);
                // Clips too narrow to see (zoomed far out) join a run drawn as one block:
                // nothing to read on them, so none of the work below.
                if x_of(item.range.end()) - x0 < TINY_CLIP_PX {
                    let x1 = x_of(item.range.end()).max(x0 + 1.0);
                    match runs.last_mut().filter(|run: &&mut ClipRun| run.row == r && run.rect.right() >= x0 - 1.0) {
                        Some(run) => {
                            run.rect.max.x = run.rect.max.x.max(x1);
                            run.ids.push(item.id);
                            run.enabled |= item.enabled;
                        }
                        None => runs.push(ClipRun {
                            row: r,
                            rect: egui::Rect::from_min_max(egui::pos2(x0, row_top(r) + 2.0), egui::pos2(x1, row_top(r) + row_h(r) - 2.0)),
                            ids: vec![item.id],
                            kind: *kind,
                            enabled: item.enabled && rows[r].3,
                        }),
                    }
                    continue;
                }
                let x1 = x_of(item.range.end()).max(x0 + 2.0);
                let rect = egui::Rect::from_min_max(egui::pos2(x0, row_top(r) + 2.0), egui::pos2(x1, row_top(r) + row_h(r) - 2.0));
                let mut keys: Vec<f32> = item
                    .params
                    .0
                    .values()
                    .chain(item.effects.iter().flat_map(|e| e.params.0.values()))
                    .filter_map(|s| s.curve().filter(|c| c.anchor == oa_params::KeyframeAnchor::ClipStart))
                    .flat_map(|c| c.keys.iter().map(|k| x_of(item.range.start + k.t)))
                    .filter(|x| *x >= x0 && *x <= x1)
                    .collect();
                keys.sort_by(f32::total_cmp);
                keys.dedup_by(|a, b| (*a - *b).abs() < 1.0);
                let enabled = item.enabled && rows[r].3;
                let text = item.kind == oa_doc::ItemKind::Text;
                let container = matches!(item.kind, oa_doc::ItemKind::Adjustment);
                let name = if text {
                    let words = item.params.get(oa_doc::schema::TEXT_CONTENT).map(|s| s.eval(&item.eval_context(item.range.start)));
                    let words = words.as_ref().and_then(|v| v.as_text()).unwrap_or("Title");
                    format!("T  {}", words.lines().next().unwrap_or_default())
                } else if container {
                    // What it does, at a glance.
                    let names: Vec<String> = item.effects.iter().filter(|e| e.enabled).map(|e| self.effect_name(&e.type_id)).collect();
                    if names.is_empty() { "✦ Empty — add effects".to_string() } else { format!("✦ {}", names.join(" · ")) }
                } else {
                    item.name.clone()
                };
                let span = |intro: bool| {
                    item.active_effects()
                        .iter()
                        .filter(|e| e.enabled)
                        .filter_map(|e| match e.role {
                            oa_doc::EffectRole::In { duration } if intro => Some(duration),
                            oa_doc::EffectRole::Out { duration } | oa_doc::EffectRole::Reversed { duration } if !intro => Some(duration),
                            _ => None,
                        })
                        .max()
                        .map(|d| d.min(item.range.duration))
                };
                let intro = span(true).map(|d| x_of(item.range.start + d));
                let outro = span(false).map(|d| x_of(item.range.end() - d));
                let media = match item.kind {
                    oa_doc::ItemKind::Media { media } => {
                        let speed = item.time_map.speed.num() as f64 / item.time_map.speed.den() as f64;
                        Some((media, item.time_map.source_in.as_seconds_f64(), speed))
                    }
                    _ => None,
                };
                let start = item.range.start.as_seconds_f64();
                boxes.push(ClipBox { id: item.id, name, rect, kind: *kind, enabled, keys, text, intro, outro, media, start, container });
            }
            // Transition windows, drawn over the clips they join.
            for (i, item) in track.items.iter().enumerate().skip(shown.start).take(shown.len()) {
                for end in [oa_doc::ClipEnd::Head, oa_doc::ClipEnd::Tail] {
                    if let Some(w) = oa_plan::transitions::window(track, i, end)
                        && x_of(w.end()) - x_of(w.start) >= TINY_CLIP_PX
                    {
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(x_of(w.start), row_top(r) + row_h(r) * 0.5),
                            egui::pos2(x_of(w.end()).max(x_of(w.start) + 4.0), row_top(r) + row_h(r) - 2.0),
                        );
                        bands.push((item.id, rect));
                    }
                }
            }
        }
        let clip_at = |p: egui::Pos2| boxes.iter().rev().find(|b| b.rect.contains(p));
        let edge_at = |b: &ClipBox, x: f32| {
            let near = EDGE_PX.min(b.rect.width() / 3.0);
            if x - b.rect.left() <= near {
                Some(Edge::Head)
            } else if b.rect.right() - x <= near {
                Some(Edge::Tail)
            } else {
                None
            }
        };

        // Track header toggles: the square at the right of each header.
        let toggle_rect = |r: usize| {
            egui::Rect::from_center_size(egui::pos2(full.left() + HEADER_WIDTH - 12.0, row_top(r) + row_h(r) / 2.0), egui::vec2(12.0, 12.0))
        };

        // Dividers run across every track: the stretch each one starts is tinted its
        // color; its line can be dragged along the timeline, and the flag at its top (in
        // the ruler) drags the whole section.
        let mut marks: Vec<Mark> = Vec::new();
        let rows_bottom = row_top(rows.len().max(1));
        if !seq.dividers.is_empty() {
            let all = oa_edit::sections::sections(seq);
            for (index, sec) in all.iter().enumerate() {
                let Some(d) = &sec.divider else { continue };
                let x = x_of(sec.start);
                let x1 = if index + 1 == all.len() { lane.right() } else { x_of(sec.end) };
                let band = egui::Rect::from_min_max(egui::pos2(x, full.top()), egui::pos2(x1.max(x), rows_bottom));
                let label = if d.name.is_empty() { None } else { Some(painter.layout_no_wrap(d.name.clone(), egui::FontId::proportional(9.5), egui::Color32::BLACK)) };
                let width = label.as_ref().map_or(12.0, |g| g.size().x + 8.0);
                let flag = egui::Rect::from_min_size(egui::pos2(x, full.top() + 1.0), egui::vec2(width, 11.0));
                marks.push(Mark { index, divider: d.clone(), x, flag, band, label });
            }
        }
        let flag_at = |p: egui::Pos2| marks.iter().find(|m| m.flag.contains(p) && p.x >= lane.left());
        // The line is grabbed in the ruler, or in a track's upper half: the lower half is
        // left to the edges of clips that start or end at the divider.
        let line_at = |p: egui::Pos2| {
            let grabbable = p.y < full.top() + RULER_HEIGHT || row_at(p.y).is_some_and(|r| p.y < row_top(r) + row_h(r) * 0.5);
            marks.iter().find(|m| grabbable && (p.x - m.x).abs() <= 4.0 && p.x >= lane.left())
        };
        let header_at = |p: egui::Pos2| {
            (p.x < lane.left() && p.y >= full.top() + RULER_HEIGHT).then(|| row_at(p.y)).flatten().filter(|r| !toggle_rect(*r).expand(3.0).contains(p))
        };

        // Hover cursor.
        if self.timeline_drag.is_none()
            && let Some(pos) = response.hover_pos()
            && (flag_at(pos).is_some() || line_at(pos).is_some() || header_at(pos).is_some())
        {
            ui.ctx().set_cursor_icon(if line_at(pos).is_some() && flag_at(pos).is_none() { egui::CursorIcon::ResizeColumn } else { egui::CursorIcon::Grab });
        } else if self.timeline_drag.is_none()
            && let Some(pos) = response.hover_pos()
            && let Some(b) = clip_at(pos)
        {
            ui.ctx().set_cursor_icon(if edge_at(b, pos.x).is_some() { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grab });
        }

        let mods = ui.input(|i| i.modifiers);
        let adding = mods.command || mods.shift;
        let in_ruler = |p: egui::Pos2| p.y < full.top() + RULER_HEIGHT;
        // The keyframe line of a selected clip under `p`: its band and the key hit (or
        // `None` for the line itself).
        let band_hit = |app: &App, p: egui::Pos2| -> Option<(ItemId, crate::band::Band, Option<usize>, egui::Rect)> {
            let b = clip_at(p).filter(|b| app.selected.contains(&b.id))?;
            let band = app.clip_band(b.id)?;
            let shape = app.band_shape(b.id, &band, b.rect, &x_of, &time_at)?;
            if let Some((i, _)) = shape.keys.iter().find(|(_, k)| k.distance(p) <= 6.0) {
                return Some((b.id, band, Some(*i), b.rect));
            }
            // The clip's edges trim, even where the line runs past them (on a thin effect
            // track it covers most of the clip).
            if edge_at(b, p.x).is_some() {
                return None;
            }
            let near = shape.line.windows(2).any(|w| {
                (w[0].x..=w[1].x).contains(&p.x) && {
                    let f = (p.x - w[0].x) / (w[1].x - w[0].x).max(1e-3);
                    (w[0].y + (w[1].y - w[0].y) * f - p.y).abs() <= 4.0
                }
            });
            near.then_some((b.id, band, None, b.rect))
        };

        // Press.
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
        {
            self.timeline_drag = Some(if let Some(m) = flag_at(origin) {
                TimelineDrag::Section { index: m.index }
            } else if let Some(m) = line_at(origin) {
                TimelineDrag::Divider { divider: m.divider.id }
            } else if in_ruler(origin) {
                TimelineDrag::Scrub
            } else if let Some(r) = header_at(origin) {
                TimelineDrag::Track { track: rows[r].0 }
            } else if let Some((item, band, key, rect)) = band_hit(self, origin) {
                let original = self
                    .editor
                    .param_source(item, &band.target, &band.param)
                    .unwrap_or_else(|| oa_params::ParamSource::Static(oa_params::Value::Float(if band.param == oa_doc::schema::OPACITY { 1.0 } else { 0.0 })));
                TimelineDrag::Band { item, band, key, grab_y: origin.y, original, rect }
            } else {
                match clip_at(origin).filter(|_| origin.x >= lane.left()) {
                    Some(b) => match edge_at(b, origin.x) {
                        Some(edge) => {
                            self.select_clip(b.id, false);
                            TimelineDrag::Trim { item: b.id, edge }
                        }
                        None => {
                            // Dragging an unselected clip selects it (and its group) first;
                            // dragging a selected one moves the whole selection.
                            if !self.selected.contains(&b.id) {
                                self.select_clip(b.id, adding);
                            } else {
                                self.selection = Some(b.id);
                            }
                            let start = self.editor.item(b.id).map_or(Time::ZERO, |i| i.range.start);
                            TimelineDrag::Move { anchor: b.id, grab: time_at(origin.x) - start }
                        }
                    },
                    None if origin.x >= lane.left() => {
                        TimelineDrag::Marquee { from: origin, base: if adding { self.selected.clone() } else { Default::default() } }
                    }
                    None => TimelineDrag::Scrub,
                }
            });
            if matches!(self.timeline_drag, Some(TimelineDrag::Scrub)) {
                self.begin_scrub();
            }
        }

        // Drag.
        let mut snapped_at = None;
        let mut marquee = None;
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            // Ctrl flips snapping for the duration of the drag.
            let snapping = self.snapping != ui.input(|i| i.modifiers.command);
            let tolerance = px_to_time(SNAP_PX);
            let s = self.editor.sequence();
            let (sid, playhead) = (self.editor.seq, self.playhead);
            let result = match &self.timeline_drag {
                Some(TimelineDrag::Scrub) | None => {
                    self.set_playhead(cmd::snap_to_frame(s, time_at(pos.x)));
                    Ok(())
                }
                // Painted below; applied on release.
                Some(TimelineDrag::Track { .. }) | Some(TimelineDrag::Section { .. }) => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    Ok(())
                }
                Some(TimelineDrag::Divider { divider }) => {
                    // Along the timeline, snapped to frames, never past its neighbors.
                    let divider = *divider;
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                    let frame = cmd::min_duration(s);
                    match s.dividers.iter().position(|d| d.id == divider) {
                        Some(i) => {
                            let lo = if i == 0 { Time::ZERO } else { s.dividers[i - 1].at + frame };
                            let hi = s.dividers.get(i + 1).map(|n| n.at - frame);
                            let mut at = cmd::snap_to_frame(s, time_at(pos.x)).max(lo);
                            if let Some(hi) = hi {
                                at = at.min(hi);
                            }
                            let change = oa_doc::Divider { at, ..s.dividers[i].clone() };
                            let ops = oa_edit::sections::edit_divider(self.editor.doc.project(), sid, divider, Some(change));
                            ops.and_then(|ops| self.editor.apply_drag("Move divider", "divider-move", ops))
                        }
                        None => Ok(()),
                    }
                }
                Some(TimelineDrag::Band { item, band, key, grab_y, original, rect }) => {
                    let (item, band, key, dy, original, rect) = (*item, band.clone(), *key, pos.y - *grab_y, original.clone(), *rect);
                    self.drag_band(item, &band, key, &original, dy, pos, rect, &time_at);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    Ok(())
                }
                Some(TimelineDrag::Marquee { from, base }) => {
                    let area = egui::Rect::from_two_pos(*from, pos);
                    let mut chosen = base.clone();
                    chosen.extend(boxes.iter().filter(|b| b.rect.intersects(area)).map(|b| b.id));
                    // Zoomed far out, the clips in a run: those whose own span the box
                    // crosses.
                    for run in runs.iter().filter(|run| run.rect.intersects(area)) {
                        let (from, to) = (time_at(area.left()), time_at(area.right()));
                        chosen.extend(run.ids.iter().copied().filter(|id| {
                            self.editor.item(*id).is_some_and(|it| it.range.end() > from && it.range.start < to)
                        }));
                    }
                    self.selected = self.with_groups(chosen);
                    self.selection = self.selected.iter().next_back().copied();
                    marquee = Some(area);
                    Ok(())
                }
                Some(TimelineDrag::Move { anchor, grab }) => {
                    let (anchor, grab) = (*anchor, *grab);
                    let moving = self.selected_clips();
                    let Some(it) = self.editor.item(anchor) else { return };
                    let (current, duration) = (it.range.start, it.range.duration);
                    let mut start = time_at(pos.x) - grab;
                    if snapping {
                        let (snapped, point) = Snapper::ignoring(s, &moving, &[playhead]).snap_range(start, duration, tolerance);
                        start = snapped;
                        snapped_at = point;
                    }
                    if snapped_at.is_none() {
                        start = cmd::snap_to_frame(s, start);
                    }
                    let kind = s.find_item(anchor).map(|(t, _)| s.tracks[t].kind);
                    // Only onto a track of its kind: effect containers onto effect tracks, and
                    // everything else onto ordinary ones.
                    let fx = s.find_item(anchor).map(|(t, _)| s.tracks[t].effects);
                    let hover_track = row_at(pos.y).filter(|r| Some(fx_rows[*r]) == fx).map(|r| rows[r].clone()).filter(|(_, k, ..)| Some(*k) == kind).map(|(id, ..)| id);
                    if moving.len() <= 1 {
                        // One clip: slides up against its neighbors rather than stopping.
                        let ops = cmd::move_item(self.editor.doc.project(), sid, anchor, start.max(Time::ZERO), hover_track);
                        ops.and_then(|ops| self.editor.apply_drag("Move clip", "timeline-move", ops))
                    } else {
                        // Several: they move as one, by the anchor's change of place. A
                        // position where any would collide is skipped (they stay put).
                        let same_kind: Vec<TrackId> = s.tracks.iter().filter(|t| Some(t.kind) == kind && Some(t.effects) == fx).map(|t| t.id).collect();
                        let index = |t: TrackId| same_kind.iter().position(|x| *x == t).map_or(0, |i| i as i32);
                        let from_track = s.find_item(anchor).map(|(t, _)| s.tracks[t].id);
                        let shift = match (hover_track, from_track) {
                            (Some(to), Some(from)) => index(to) - index(from),
                            _ => 0,
                        };
                        let delta = start.max(Time::ZERO) - current;
                        match cmd::move_items(self.editor.doc.project(), sid, &moving, delta, shift) {
                            Ok(ops) => self.editor.apply_drag("Move clips", "timeline-move", ops),
                            Err(_) => Ok(()),
                        }
                    }
                }
                Some(TimelineDrag::Trim { item, edge }) => {
                    let (item, edge) = (*item, *edge);
                    let mut to = time_at(pos.x);
                    let snapper = Snapper::new(s, Some(item), &[playhead]);
                    match snapper.snap(to, tolerance).filter(|_| snapping) {
                        Some(point) => {
                            to = point;
                            snapped_at = Some(point);
                        }
                        None => to = cmd::snap_to_frame(s, to),
                    }
                    let ops = cmd::trim(self.editor.doc.project(), sid, item, edge, to);
                    ops.and_then(|ops| self.editor.apply_drag("Trim clip", "timeline-trim", ops))
                }
            };
            if let Err(e) = result {
                self.error = Some(e.to_string());
            }
            if matches!(self.timeline_drag, Some(TimelineDrag::Move { .. })) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if matches!(self.timeline_drag, Some(TimelineDrag::Trim { .. })) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        if response.drag_stopped()
            && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
        {
            match self.timeline_drag {
                Some(TimelineDrag::Track { track }) => {
                    if let Some(to) = track_drop_row(&rows, track, pos.y, &row_at, full.top() + RULER_HEIGHT) {
                        self.reorder_track(&rows, track, to);
                    }
                }
                Some(TimelineDrag::Section { index }) => {
                    let to = oa_edit::sections::section_at(self.editor.sequence(), time_at(pos.x));
                    if to != index {
                        self.section_command(crate::clips::SectionCommand::Move { from: index, to });
                    }
                }
                _ => {}
            }
        }
        if response.drag_stopped() {
            self.timeline_drag = None;
            self.editor.doc.seal();
            self.end_scrub();
        }
        let toggled = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|pos| (0..rows.len()).find(|&r| toggle_rect(r).expand(3.0).contains(pos)));
        if let Some(r) = toggled {
            let (track, enabled) = (rows[r].0, rows[r].3);
            let op = oa_doc::Op::SetTrackEnabled { seq: self.editor.seq, track, enabled: !enabled };
            if let Err(e) = self.editor.apply(if enabled { "Turn track off" } else { "Turn track on" }, vec![op]) {
                self.error = Some(e.to_string());
            }
        } else if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            // A transition band selects the clip that owns it (its settings are in the
            // inspector).
            if mods.command
                && let Some((item, band, key, _)) = band_hit(self, pos)
            {
                // Ctrl+click on a clip's keyframe line: add a key there / remove this one.
                self.toggle_band_key(item, &band, key, time_at(pos.x));
            } else if let Some((id, _)) = bands.iter().find(|(_, r)| r.contains(pos)) {
                self.select_clip(*id, false);
            } else if let Some(b) = clip_at(pos).filter(|_| !in_ruler(pos)) {
                self.select_clip(b.id, adding);
            } else if pos.x >= lane.left() {
                if !in_ruler(pos) && !adding {
                    self.selection = None;
                    self.selected.clear();
                }
                let s = self.editor.sequence();
                self.set_playhead(cmd::snap_to_frame(s, time_at(pos.x)));
            }
        }

        // Double-click a track's header (or empty space on it) to pick the track: pastes
        // and duplicates go into it, Alt+↑/↓ moves it; again (or Esc) lets go.
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && !in_ruler(pos)
            && let Some(r) = header_at(pos).or_else(|| (pos.x >= lane.left() && clip_at(pos).is_none() && flag_at(pos).is_none()).then(|| row_at(pos.y)).flatten())
        {
            let id = rows[r].0;
            self.selected_track = if self.selected_track == Some(id) { None } else { Some(id) };
        }

        // Double-click a compound clip to edit its own timeline.
        let open = response
            .double_clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .filter(|pos| !in_ruler(*pos))
            .and_then(|pos| clip_at(pos).map(|b| b.id))
            .filter(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, oa_doc::ItemKind::Nested { .. })));

        // Right-click: a menu for the clip(s), the track header, or empty space.
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.timeline_view.menu = if let Some(m) = flag_at(pos).or_else(|| line_at(pos)) {
                Some(Menu::Divider(m.divider.id))
            } else if let Some((item, _, Some(key), _)) = band_hit(self, pos) {
                Some(Menu::Key(item, key))
            } else if pos.x < lane.left() {
                row_at(pos.y).map(|r| Menu::Track(rows[r].0))
            } else if let Some(b) = clip_at(pos) {
                if !self.selected.contains(&b.id) {
                    self.select_clip(b.id, false);
                }
                Some(Menu::Clips)
            } else {
                Some(Menu::Empty(time_at(pos.x), row_at(pos.y).filter(|_| !in_ruler(pos)).map(|r| rows[r].0)))
            };
        }
        // Clicks inside don't close it (every entry closes it itself), so the name fields
        // for tracks and dividers can be clicked into and typed in.
        egui::Popup::context_menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| match self.timeline_view.menu {
            Some(Menu::Clips) => self.clip_menu(ui),
            Some(Menu::Track(track)) => self.track_menu(ui, track),
            Some(Menu::Empty(at, track)) => self.empty_menu(ui, at, track),
            Some(Menu::Divider(divider)) => self.divider_menu(ui, divider),
            Some(Menu::Key(item, key)) => self.key_menu(ui, item, key),
            None => {
                ui.close();
            }
        });

        // ---- paint (after edits, so this frame shows the result) ----
        let visuals = ui.visuals().clone();
        painter.rect_filled(full, 4.0, visuals.extreme_bg_color);
        // Ruler: a tick per second, labeled every few.
        let label_every = [1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter().find(|s| lane.width() as f64 / span * s >= 60.0).unwrap_or(120.0);
        let mut second = (start / label_every).floor() * label_every;
        while second <= start + span {
            let x = x_of(Time::from_seconds_f64(second));
            let labeled = (second / label_every).fract() == 0.0;
            let h = if labeled { 8.0 } else { 4.0 };
            painter.line_segment(
                [egui::pos2(x, full.top() + RULER_HEIGHT - h), egui::pos2(x, full.top() + RULER_HEIGHT)],
                egui::Stroke::new(1.0, visuals.weak_text_color()),
            );
            if labeled {
                painter.text(
                    egui::pos2(x + 3.0, full.top() + 1.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}:{:02}", second as i64 / 60, second as i64 % 60),
                    egui::FontId::monospace(10.0),
                    visuals.weak_text_color(),
                );
            }
            second += 1.0f64.max(label_every / 5.0);
        }
        for (r, (_, kind, name, enabled)) in rows.iter().enumerate() {
            let top = row_top(r);
            if !enabled {
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(lane.left(), top), egui::pos2(full.right(), top + row_h(r))),
                    0.0,
                    egui::Color32::from_black_alpha(90),
                );
            }
            let t = toggle_rect(r);
            let on = if *kind == TrackKind::Video { egui::Color32::from_rgb(140, 170, 220) } else { egui::Color32::from_rgb(140, 200, 160) };
            if *enabled {
                painter.rect_filled(t, 2.0, on);
            } else {
                painter.rect_stroke(t, 2.0, egui::Stroke::new(1.0, visuals.weak_text_color()), egui::StrokeKind::Inside);
            }
            painter.text(
                egui::pos2(full.left() + 6.0, top + row_h(r) / 2.0),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(11.0),
                if fx_rows[r] { egui::Color32::from_rgb(220, 170, 80) } else if *kind == TrackKind::Video { egui::Color32::from_rgb(140, 170, 220) } else { egui::Color32::from_rgb(140, 200, 160) },
            );
            painter.line_segment(
                [egui::pos2(full.left(), top), egui::pos2(full.right(), top)],
                egui::Stroke::new(1.0, visuals.faint_bg_color),
            );
        }
        let clip_painter = painter.with_clip_rect(lane);
        let rgb = |c: [u8; 3]| egui::Color32::from_rgb(c[0], c[1], c[2]);
        // The picked track: its header outlined, its lane faintly lit.
        if let Some(r) = self.selected_track.and_then(|id| rows.iter().position(|row| row.0 == id)) {
            let header = egui::Rect::from_min_max(egui::pos2(full.left(), row_top(r)), egui::pos2(lane.left(), row_top(r) + row_h(r)));
            clip_painter.rect_filled(egui::Rect::from_min_max(egui::pos2(lane.left(), row_top(r)), egui::pos2(full.right(), row_top(r) + row_h(r))), 0.0, crate::style::ACCENT.gamma_multiply(0.08));
            painter.rect_stroke(header.shrink(1.0), 3.0, egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        // Sections, under the clips.
        for m in &marks {
            clip_painter.rect_filled(m.band, 0.0, rgb(m.divider.color).gamma_multiply(0.12));
        }
        // Runs of clips too narrow to tell apart: one block each, lit if any is selected.
        for run in &runs {
            let base = match run.kind {
                TrackKind::Video => egui::Color32::from_rgb(58, 92, 140),
                TrackKind::Audio => egui::Color32::from_rgb(70, 120, 90),
            };
            let selected = run.ids.iter().any(|id| self.selected.contains(id));
            let fill = if !run.enabled { base.gamma_multiply(0.4) } else if selected { base.gamma_multiply(1.6) } else { base.gamma_multiply(0.85) };
            clip_painter.rect_filled(run.rect, 1.0, fill);
        }
        for b in &boxes {
            let selected = self.selected.contains(&b.id);
            let grouped = self.editor.item(b.id).is_some_and(|i| i.group.is_some());
            let base = match b.kind {
                _ if b.container => egui::Color32::from_rgb(150, 110, 40),
                TrackKind::Video if b.text => egui::Color32::from_rgb(120, 78, 150),
                TrackKind::Video => egui::Color32::from_rgb(58, 92, 140),
                TrackKind::Audio => egui::Color32::from_rgb(70, 120, 90),
            };
            let fill = if !b.enabled { base.gamma_multiply(0.4) } else if selected { base.gamma_multiply(1.6) } else { base };
            clip_painter.rect_filled(b.rect, 3.0, fill);
            if let Some((media, source_in, speed)) = b.media {
                self.paint_clip_preview(ui.ctx(), &clip_painter.with_clip_rect(b.rect.shrink(1.0).intersect(lane)), b, media, source_in, speed, &time_at);
            }
            // Intro and outro ramps: the clip rising from / falling to its edges.
            let ramp = egui::Color32::from_white_alpha(40);
            let r = b.rect;
            if let Some(x) = b.intro.filter(|x| *x > r.left() + 1.0) {
                clip_painter.add(egui::Shape::convex_polygon(vec![r.left_top(), egui::pos2(x.min(r.right()), r.top()), r.left_bottom()], ramp, egui::Stroke::NONE));
            }
            if let Some(x) = b.outro.filter(|x| *x < r.right() - 1.0) {
                clip_painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x.max(r.left()), r.top()), r.right_top(), r.right_bottom()], ramp, egui::Stroke::NONE));
            }
            if selected {
                clip_painter.rect_stroke(b.rect, 3.0, egui::Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
                // Trim grips.
                for x in [b.rect.left() + 2.0, b.rect.right() - 2.0] {
                    clip_painter.line_segment(
                        [egui::pos2(x, b.rect.top() + 5.0), egui::pos2(x, b.rect.bottom() - 5.0)],
                        egui::Stroke::new(2.0, egui::Color32::from_white_alpha(180)),
                    );
                }
            }
            clip_painter.with_clip_rect(b.rect.intersect(lane)).text(
                b.rect.left_center() + egui::vec2(6.0, -3.0),
                egui::Align2::LEFT_CENTER,
                &b.name,
                egui::FontId::proportional(11.0),
                egui::Color32::WHITE,
            );
            if grouped {
                // Grouped clips carry a gold line along the top.
                clip_painter.line_segment(
                    [b.rect.left_top() + egui::vec2(3.0, 1.5), b.rect.right_top() + egui::vec2(-3.0, 1.5)],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(250, 200, 90)),
                );
            }
            // The clip's keyframe line (volume / opacity / the chosen property): bright
            // and editable on selected clips, faint on the rest.
            if let Some(band) = self.clip_band(b.id)
                && let Some(shape) = self.band_shape(b.id, &band, b.rect, &x_of, &time_at)
            {
                let color = if selected { egui::Color32::from_rgb(255, 220, 110) } else { egui::Color32::from_rgba_unmultiplied(255, 220, 110, 70) };
                let clipped = clip_painter.with_clip_rect(b.rect.intersect(lane));
                clipped.add(egui::Shape::line(shape.line, egui::Stroke::new(1.2, color)));
                if selected {
                    for (_, p) in &shape.keys {
                        clipped.circle(*p, 3.2, egui::Color32::from_rgb(255, 220, 110), egui::Stroke::new(1.0, egui::Color32::BLACK));
                    }
                }
            }
            for x in &b.keys {
                let c = egui::pos2(*x, b.rect.bottom() - 5.0);
                let d = 3.0;
                clip_painter.add(egui::Shape::convex_polygon(
                    vec![c + egui::vec2(0.0, -d), c + egui::vec2(d, 0.0), c + egui::vec2(0.0, d), c + egui::vec2(-d, 0.0)],
                    crate::style::GOLD,
                    egui::Stroke::NONE,
                ));
            }
        }
        // Dividers over the clips: a line in the section's color and a flag to drag it by.
        for m in &marks {
            let color = rgb(m.divider.color);
            clip_painter.line_segment([egui::pos2(m.x, m.band.top()), egui::pos2(m.x, m.band.bottom())], egui::Stroke::new(2.0, color));
            clip_painter.rect_filled(m.flag, egui::CornerRadius { nw: 0, ne: 3, sw: 0, se: 3 }, color);
            if let Some(label) = &m.label {
                clip_painter.galley(m.flag.left_top() + egui::vec2(3.0, 0.0), label.clone(), egui::Color32::BLACK);
            }
        }
        // A track or section being dragged: where it would land.
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
            match self.timeline_drag {
                Some(TimelineDrag::Track { track }) => {
                    if let (Some(to), Some(from)) = (track_drop_row(&rows, track, pos.y, &row_at, full.top() + RULER_HEIGHT), rows.iter().position(|r| r.0 == track)) {
                        let y = if to > from { row_top(to) + row_h(to) } else { row_top(to) };
                        painter.line_segment([egui::pos2(full.left(), y), egui::pos2(full.right(), y)], egui::Stroke::new(3.0, crate::style::ACCENT));
                    }
                }
                Some(TimelineDrag::Section { index }) => {
                    let s = self.editor.sequence();
                    let to = oa_edit::sections::section_at(s, time_at(pos.x));
                    let all = oa_edit::sections::sections(s);
                    if let Some(sec) = all.get(to).filter(|_| to != index) {
                        let x1 = if to + 1 == all.len() { lane.right() } else { x_of(sec.end) };
                        let target = egui::Rect::from_min_max(egui::pos2(x_of(sec.start), full.top()), egui::pos2(x1, row_top(rows.len().max(1))));
                        clip_painter.rect_stroke(target, 2.0, egui::Stroke::new(2.0, crate::style::ACCENT), egui::StrokeKind::Inside);
                    }
                }
                _ => {}
            }
        }
        for (id, rect) in &bands {
            let selected = self.selection == Some(*id);
            clip_painter.rect_filled(*rect, 2.0, egui::Color32::from_white_alpha(if selected { 110 } else { 60 }));
            // A diagonal from the outgoing to the incoming side, like a dissolve curve.
            clip_painter.line_segment([rect.left_bottom(), rect.right_top()], egui::Stroke::new(1.0, egui::Color32::from_white_alpha(200)));
        }
        if let Some(area) = marquee {
            clip_painter.rect_filled(area, 0.0, egui::Color32::from_rgba_unmultiplied(120, 170, 255, 30));
            clip_painter.rect_stroke(area, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 170, 255)), egui::StrokeKind::Inside);
        }
        if let Some(t) = snapped_at {
            let x = x_of(t);
            clip_painter.line_segment(
                [egui::pos2(x, full.top()), egui::pos2(x, full.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 80, 200)),
            );
        }
        let x = x_of(self.playhead);
        clip_painter.line_segment([egui::pos2(x, full.top()), egui::pos2(x, full.bottom())], egui::Stroke::new(1.5, crate::style::GOLD));
        clip_painter.add(egui::Shape::convex_polygon(
            vec![egui::pos2(x - 5.0, full.top()), egui::pos2(x + 5.0, full.top()), egui::pos2(x, full.top() + 7.0)],
            crate::style::GOLD,
            egui::Stroke::NONE,
        ));

        // A media bin card dragged over the lane: where it would land; dropped: put there
        // (on another track, or a new one, if that spot is taken).
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| lane.contains(*p));
        // Over a clip showing media of the same kind (and without Shift): the card's media
        // goes into that clip instead, keeping everything done to the clip.
        let project = self.editor.doc.snapshot();
        let seq = self.editor.seq;
        let shift = ui.input(|i| i.modifiers.shift);
        let swap_target = |drop: &crate::bin::BinDrop, pos: egui::Pos2| -> Option<&ClipBox> {
            let kind = if drop.audio { TrackKind::Audio } else { TrackKind::Video };
            boxes
                .iter()
                .rev()
                .find(|b| b.rect.contains(pos))
                .filter(|b| !shift && b.kind == kind && oa_edit::swap::can_swap(&project, seq, b.id) && !drop.is_in(&project, seq, b.id))
        };
        if let (Some(drop), Some(pos)) = (response.dnd_hover_payload::<crate::bin::BinDrop>(), pointer)
            && let Some(target) = swap_target(&drop, pos)
        {
            let accent = ui.visuals().selection.stroke.color;
            clip_painter.rect_filled(target.rect, 3.0, accent.gamma_multiply(0.3));
            clip_painter.rect_stroke(target.rect, 3.0, egui::Stroke::new(2.5, accent), egui::StrokeKind::Inside);
            let label = format!("⇄ Swap for {}", drop.name);
            let galley = clip_painter.layout_no_wrap(label, egui::FontId::proportional(12.0), egui::Color32::WHITE);
            let at = egui::pos2(target.rect.left() + 6.0, target.rect.center().y - galley.size().y / 2.0);
            let bg = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(5.0, 2.0));
            clip_painter.rect_filled(bg, 3.0, accent);
            clip_painter.galley(at, galley, egui::Color32::WHITE);
            let hint = clip_painter.layout_no_wrap("Shift: add it instead".into(), egui::FontId::proportional(10.0), egui::Color32::WHITE);
            let hint_at = egui::pos2(bg.left() + 1.0, bg.bottom() + 3.0);
            if target.rect.contains(hint_at + hint.size()) {
                clip_painter.galley(hint_at, hint, egui::Color32::from_white_alpha(200));
            }
        } else if let (Some(drop), Some(pos)) = (response.dnd_hover_payload::<crate::bin::BinDrop>(), pointer) {
            let kind = if drop.audio { TrackKind::Audio } else { TrackKind::Video };
            let row = row_at(pos.y).filter(|r| rows[*r].1 == kind);
            let at = time_at(pos.x);
            let x0 = x_of(at);
            let x1 = x_of(at + drop.length).max(x0 + 4.0);
            let (y0, y1) = match row {
                Some(r) => (row_top(r) + 2.0, row_top(r) + row_h(r) - 2.0),
                None => (pos.y - row_height / 2.0 + 2.0, pos.y + row_height / 2.0 - 2.0),
            };
            let ghost = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
            let accent = ui.visuals().selection.stroke.color;
            clip_painter.rect_filled(ghost, 3.0, accent.gamma_multiply(0.25));
            clip_painter.rect_stroke(ghost, 3.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
            clip_painter.text(ghost.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, &drop.name, egui::FontId::proportional(11.0), ui.visuals().strong_text_color());
        }
        if let (Some(drop), Some(pos)) = (response.dnd_release_payload::<crate::bin::BinDrop>(), pointer) {
            match swap_target(&drop, pos).map(|b| b.id) {
                Some(clip) => self.swap_bin_entry(&drop, clip),
                None => {
                    let kind = if drop.audio { TrackKind::Audio } else { TrackKind::Video };
                    let track = row_at(pos.y).filter(|r| rows[*r].1 == kind).map(|r| rows[r].0);
                    self.drop_bin_entry(&drop, time_at(pos.x), track);
                }
            }
        }
        // Where the clips are, for dropping effects dragged out of the inspector — and
        // while one is dragged, the clip it would go onto lights up (red if it can't
        // take it).
        self.drop_targets = boxes.iter().map(|b| (b.rect.intersect(lane), b.id)).filter(|(r, _)| r.is_positive()).collect();
        // And where the effect tracks are, and the time under each point of them: an effect
        // dropped on an empty stretch starts a container there.
        self.fx_lanes = (0..rows.len())
            .filter(|r| fx_rows[*r])
            .map(|r| (egui::Rect::from_min_max(egui::pos2(lane.left(), row_top(r)), egui::pos2(lane.right(), row_top(r) + row_h(r))), rows[r].0, start, span / lane.width().max(1.0) as f64))
            .collect();
        if let Some(drag) = self.effect_drag.clone()
            && let Some(p) = ui.input(|i| i.pointer.latest_pos()).filter(|p| lane.contains(*p) && !in_ruler(*p))
            && let Some(b) = boxes.iter().rev().find(|b| b.rect.contains(p)).filter(|b| b.id != drag.from)
        {
            let color = if self.effect_fits(b.id, &drag.effect.type_id) { crate::style::ACCENT } else { crate::style::ERROR };
            clip_painter.rect_stroke(b.rect, 3.0, egui::Stroke::new(2.5, color), egui::StrokeKind::Inside);
        }
        // Last, so this frame was drawn wholly from the timeline it started with.
        if let Some(id) = open {
            self.open_compound(id);
        }
    }
}

/// A menu entry with its shortcut shown on the right.
fn item(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut)).clicked()
}

impl App {
    /// Right-click on clips: edits for the whole selection.
    fn clip_menu(&mut self, ui: &mut egui::Ui) {
        let n = self.selected.len();
        let has = n > 0;
        ui.label(egui::RichText::new(if n == 1 { "1 clip".to_string() } else { format!("{n} clips") }).small().weak());
        if item(ui, "Cut", "Ctrl+X", has) {
            self.cut_selection();
            ui.close();
        }
        if item(ui, "Copy", "Ctrl+C", has) {
            self.copy_selection();
            ui.close();
        }
        if item(ui, "Paste at playhead", "Ctrl+V", !self.clipboard.is_empty()) {
            let at = self.playhead;
            self.paste_at(at);
            ui.close();
        }
        if item(ui, "Duplicate", "Ctrl+D", has) {
            self.duplicate_selection();
            ui.close();
        }
        ui.separator();
        if item(ui, "Split at playhead", "S", has) {
            self.split_at_playhead();
            ui.close();
        }
        if n >= 2 && item(ui, "Group", "Ctrl+G", true) {
            self.group_selection();
            ui.close();
        }
        if self.selection_grouped() && item(ui, "Ungroup", "Ctrl+Shift+G", true) {
            self.ungroup_selection();
            ui.close();
        }
        if ui
            .add_enabled(has, egui::Button::new("As media"))
            .on_hover_text("Adds these clips to the media bin as one compound clip — use it as a clip, or as a mask or other effect picture")
            .clicked()
        {
            self.compound_selection(false);
            ui.close();
        }
        if ui.add_enabled(has, egui::Button::new("Nest into one clip")).on_hover_text("Replaces these clips with one compound clip").clicked() {
            self.compound_selection(true);
            ui.close();
        }
        let compound = self.selection.filter(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, oa_doc::ItemKind::Nested { .. })));
        if let Some(id) = compound
            && ui.button("Open compound clip").on_hover_text("Edit its own timeline (or double-click it)").clicked()
        {
            self.open_compound(id);
            ui.close();
        }
        if self.compound_selected()
            && ui.button("Break apart").on_hover_text("Put the clips inside back on the timeline, where they play now").clicked()
        {
            self.break_compounds();
            ui.close();
        }
        ui.separator();
        if item(ui, "Copy effects", "", self.selection.is_some_and(|id| self.editor.item(id).is_some_and(|i| !i.effects.is_empty()))) {
            if let Some(id) = self.selection {
                self.copy_effects(id, None);
            }
            ui.close();
        }
        if item(ui, "Paste effects", "", !self.effect_clipboard.is_empty() && has) {
            let targets = self.selected_clips();
            self.paste_effects(&targets);
            ui.close();
        }
        if item(ui, "Copy transform", "", self.selection.is_some()) {
            if let Some(id) = self.selection {
                self.copy_transform(id);
            }
            ui.close();
        }
        if item(ui, "Paste transform", "", self.transform_clipboard.is_some() && has) {
            let targets = self.selected_clips();
            self.paste_transform(&targets);
            ui.close();
        }
        // The file's scale mode, for a picture clip.
        let picture = self.selection.and_then(|id| self.editor.item(id)).and_then(|i| match i.kind {
            oa_doc::ItemKind::Media { media } => self.editor.pool_item(media).filter(|p| p.probe.video.is_some()).map(|_| media),
            _ => None,
        });
        if let Some(media) = picture {
            self.scaling_menu(ui, media);
            if self.settings.advanced_color {
                self.color_menu(ui, media);
            }
        }
        if !self.extractable().is_empty() && item(ui, "Extract audio", "", true) {
            self.extract_audio();
            ui.close();
        }
        let all_off = self.selected.iter().all(|id| self.editor.item(*id).is_some_and(|i| !i.enabled));
        if item(ui, if all_off { "Enable" } else { "Disable" }, "", has) {
            self.toggle_selection_enabled();
            ui.close();
        }
        ui.separator();
        if item(ui, "Delete", "Del", has) {
            self.delete_selection(false);
            ui.close();
        }
        if item(ui, "Ripple delete", "Shift+Del", has) {
            self.delete_selection(true);
            ui.close();
        }
    }

    /// Right-click on a track header.
    fn track_menu(&mut self, ui: &mut egui::Ui, track: TrackId) {
        let Some(t) = self.editor.sequence().track(track).cloned() else {
            ui.close();
            return;
        };
        // Rename in place: type and press Enter.
        let name = match &mut self.renaming {
            Some((id, name)) if *id == track => name,
            _ => {
                self.renaming = Some((track, t.name.clone()));
                &mut self.renaming.as_mut().expect("just set").1
            }
        };
        let r = ui.add(egui::TextEdit::singleline(name).desired_width(140.0).hint_text("Track name"));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some((_, name)) = self.renaming.take() {
                // Checked, not silently dropped: an empty name says so in the corner.
                match crate::notify::check_name("track", &name) {
                    Ok(name) if name != t.name => self.rename_track(track, name),
                    Ok(_) => {}
                    Err(e) => self.report_error(e),
                }
            }
            ui.close();
        }
        ui.separator();
        if item(ui, if t.enabled { "Turn off" } else { "Turn on" }, "", true) {
            let op = oa_doc::Op::SetTrackEnabled { seq: self.editor.seq, track, enabled: !t.enabled };
            if let Err(e) = self.editor.apply("Toggle track", vec![op]) {
                self.error = Some(e.to_string());
            }
            ui.close();
        }
        let picked = self.selected_track == Some(track);
        if ui.selectable_label(picked, "Pick this track").on_hover_text("Ctrl+V and Ctrl+D put clips in its first free space after the playhead; Alt+↑/↓ moves it (or double-click its header)").clicked() {
            self.selected_track = if picked { None } else { Some(track) };
            ui.close();
        }
        if item(ui, "Add divider at playhead", "", true) {
            let at = cmd::snap_to_frame(self.editor.sequence(), self.playhead);
            self.add_divider(at);
            ui.close();
        }
        if item(ui, "Select all on track", "", !t.items.is_empty()) {
            self.selected = t.items.iter().map(|i| i.id).collect();
            self.selection = t.items.last().map(|i| i.id);
            ui.close();
        }
        if item(ui, "Move up", "", true) {
            self.move_track(track, true);
            ui.close();
        }
        if item(ui, "Move down", "", true) {
            self.move_track(track, false);
            ui.close();
        }
        if item(ui, if t.kind == TrackKind::Video { "Add video track" } else { "Add audio track" }, "", true) {
            if let Err(e) = self.editor.add_track(t.kind) {
                self.error = Some(e.to_string());
            }
            ui.close();
        }
        let (label, tip) = if t.kind == TrackKind::Video {
            ("Add picture effect track above", "A thin track whose effects run over everything below it — this track and the ones under it")
        } else {
            ("Add sound effect track above", "A thin track whose sound effects run over this track, the ones below it, and the sound of picture tracks")
        };
        if ui.button(label).on_hover_text(tip).clicked() {
            match self.editor.add_effect_track(t.kind, track) {
                Ok(fx) => {
                    // With one container over the playhead to start from.
                    if let Ok(id) = self.editor.add_container(fx, cmd::snap_to_frame(self.editor.sequence(), self.playhead)) {
                        self.selection = Some(id);
                        self.sync_selection();
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            ui.close();
        }
        if t.effects && item(ui, "Add effect container at playhead", "", true) {
            self.add_container_at(track, self.playhead);
            ui.close();
        }
        ui.separator();
        if item(ui, "Delete track", "", true) {
            self.delete_track(track);
            ui.close();
        }
    }

    /// A new effect container on effect track `track` at `at`, selected.
    fn add_container_at(&mut self, track: TrackId, at: Time) {
        let at = cmd::snap_to_frame(self.editor.sequence(), at);
        match self.editor.add_container(track, at) {
            Ok(id) => {
                self.selection = Some(id);
                self.sync_selection();
            }
            Err(e) => self.error = Some(format!("can't add a container there: {e}")),
        }
    }

    /// Right-click on empty timeline space.
    fn empty_menu(&mut self, ui: &mut egui::Ui, at: Time, track: Option<TrackId>) {
        // On an effect track, that's all there is to put there.
        if let Some(track) = track.filter(|t| self.editor.sequence().track(*t).is_some_and(|t| t.effects)) {
            if ui.button("Add effect container here").on_hover_text("Its effects run over everything below this track while it lasts").clicked() {
                self.add_container_at(track, at);
                ui.close();
            }
            ui.separator();
        }
        if item(ui, "Paste here", "", !self.clipboard.is_empty()) {
            self.paste_at(at);
            ui.close();
        }
        if let Some(track) = track
            && item(ui, "Paste into this track's first gap", "", !self.clipboard.is_empty())
        {
            self.set_playhead(at);
            self.paste_into_track(track);
            ui.close();
        }
        if item(ui, "Add text here", "", true) {
            self.set_playhead(at);
            self.add_text();
            ui.close();
        }
        if item(ui, "Select all", "Ctrl+A", true) {
            self.select_all();
            ui.close();
        }
        ui.separator();
        if item(ui, "Add divider here", "", true) {
            let at = cmd::snap_to_frame(self.editor.sequence(), at);
            self.add_divider(at);
            ui.close();
        }
        let s = self.editor.sequence().clone();
        if !s.dividers.is_empty() {
            let index = oa_edit::sections::section_at(&s, at);
            self.section_items(ui, &s, index);
        }
    }

    /// Menu entries for one section of the timeline.
    fn section_items(&mut self, ui: &mut egui::Ui, s: &oa_doc::Sequence, index: usize) {
        use crate::clips::SectionCommand as C;
        let count = oa_edit::sections::sections(s).len();
        let clips = oa_edit::sections::members(s, index).len();
        ui.label(egui::RichText::new(format!("Section {} of {count} · {clips} clip{}", index + 1, if clips == 1 { "" } else { "s" })).small().weak());
        let run = |app: &mut Self, ui: &mut egui::Ui, label: &str, enabled: bool, tip: &str, command: C| {
            if ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(tip).clicked() {
                app.section_command(command);
                ui.close();
            }
        };
        run(self, ui, "Select its clips", clips > 0, "Select every clip in this section, on every track", C::Select(index));
        run(self, ui, "Move earlier", index > 0, "Swap with the section before (its clips and divider move too)", C::Move { from: index, to: index.saturating_sub(1) });
        run(self, ui, "Move later", index + 1 < count, "Swap with the section after", C::Move { from: index, to: index + 1 });
        run(self, ui, "Clear its clips", clips > 0, "Remove the clips in this section, keeping the space", C::Clear(index));
        run(self, ui, "Delete section", true, "Remove this section and its clips; everything after moves back to close the gap", C::Delete(index));
    }

    /// Right-click on a divider: its name and color, and its section's commands.
    fn divider_menu(&mut self, ui: &mut egui::Ui, divider: u64) {
        use crate::clips::SectionCommand as C;
        let s = self.editor.sequence().clone();
        let Some(d) = s.dividers.iter().find(|d| d.id == divider).cloned() else {
            ui.close();
            return;
        };
        let mut name = d.name.clone();
        let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(150.0).hint_text("Section name"));
        if r.changed() {
            let change = oa_doc::Divider { name: name.chars().take(60).collect(), ..d.clone() };
            let ops = oa_edit::sections::edit_divider(self.editor.doc.project(), self.editor.seq, divider, Some(change));
            if let Err(e) = ops.and_then(|ops| self.editor.apply_drag("Name section", "divider-name", ops)) {
                self.report_error(e.to_string());
            }
        }
        if r.lost_focus() {
            self.editor.doc.seal();
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                ui.close();
            }
        }
        ui.horizontal(|ui| {
            for c in oa_edit::sections::COLORS {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                let color = egui::Color32::from_rgb(c[0], c[1], c[2]);
                ui.painter().circle_filled(rect.center(), 6.5, color);
                if c == d.color {
                    ui.painter().circle_stroke(rect.center(), 7.5, egui::Stroke::new(1.5, ui.visuals().strong_text_color()));
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && c != d.color {
                    self.section_command(C::Edit { divider, change: Some(oa_doc::Divider { color: c, ..d.clone() }) });
                }
            }
        });
        ui.separator();
        let index = oa_edit::sections::sections(&s).iter().position(|x| x.divider.as_ref().is_some_and(|x| x.id == divider)).unwrap_or(0);
        self.section_items(ui, &s, index);
        ui.separator();
        if ui.button("Remove divider").on_hover_text("Joins this section to the one before; no clips change").clicked() {
            self.section_command(C::Edit { divider, change: None });
            ui.close();
        }
    }
}

impl App {
    /// A media clip's filmstrip (pictures) and waveform (sound), drawn under its name.
    /// Both load in the background; until then the clip is just its color.
    #[allow(clippy::too_many_arguments)]
    fn paint_clip_preview(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        b: &ClipBox,
        media: oa_doc::MediaId,
        source_in: f64,
        speed: f64,
        time_at: &dyn Fn(f32) -> Time,
    ) {
        let Some(pool) = self.editor.pool_item(media) else { return };
        if pool.missing {
            return;
        }
        let (path, video, duration, still, audible) = (
            pool.decode_path.clone(),
            pool.probe.video.clone(),
            pool.probe.duration.as_seconds_f64(),
            pool.kind == oa_media::MediaKind::Still,
            pool.probe.has_audio() && self.editor.item(b.id).is_some_and(|i| i.audio_enabled()),
        );
        // Where in the file the clip is at screen x.
        let source_at = |x: f32| source_in + (time_at(x).as_seconds_f64() - b.start) * speed;
        let r = b.rect;
        let visible = painter.clip_rect();
        let wave_h = if video.is_some() { r.height() * 0.4 } else { r.height() };
        if let Some(v) = &video
            && let Some(strip) = self.clip_previews.strip(ctx, media, &path, [v.width, v.height], if still { 0.0 } else { duration })
        {
            let pic = egui::Rect::from_min_max(r.min, egui::pos2(r.right(), if audible { r.bottom() - wave_h } else { r.bottom() }));
            let tile = (pic.height() * strip.aspect).max(8.0);
            let mut x = r.left();
            while x < r.right() {
                if x + tile >= visible.left() && x <= visible.right() {
                    let uv = strip.uv(if still { 0.0 } else { source_at(x) });
                    let dest = egui::Rect::from_min_size(egui::pos2(x, pic.top()), egui::vec2(tile, pic.height()));
                    painter.image(strip.texture.id(), dest, uv, egui::Color32::from_gray(190));
                }
                x += tile;
            }
            // Keep the name readable over the pictures.
            painter.rect_filled(egui::Rect::from_min_size(pic.min, egui::vec2(r.width(), 14.0)), 0.0, egui::Color32::from_black_alpha(90));
        }
        if audible && let Some(peaks) = self.clip_previews.peaks(ctx, media, &path) {
            let band = egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - wave_h), r.max);
            let mid = band.center().y;
            let color = egui::Color32::from_rgba_unmultiplied(220, 255, 230, 170);
            let from = r.left().max(visible.left()).floor() as i32;
            let to = r.right().min(visible.right()).ceil() as i32;
            for px in (from..to).step_by(2) {
                let i = (source_at(px as f32) * crate::previews::PEAKS_PER_SECOND) as usize;
                let Some(&(lo, hi)) = peaks.get(i) else { continue };
                let a = lo.abs().max(hi.abs()).min(1.0) * band.height() * 0.5;
                painter.line_segment([egui::pos2(px as f32, mid - a), egui::pos2(px as f32, mid + a.max(0.5))], egui::Stroke::new(1.0, color));
            }
        }
    }
}
