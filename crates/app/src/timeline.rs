//! The timeline: tracks, clips and the playhead.
//!
//! Click a clip to select it (and its group); Ctrl/Shift+click adds or removes; drag on
//! empty space draws a selection box. Drag a clip's middle to move the selection (onto
//! other tracks of the same kind, too), or an edge to trim. Moves and trims snap to clip
//! edges and the playhead ("Snap"; hold Shift to drag without it, Ctrl flips it) and land on frame
//! boundaries. Drag or click in the ruler to scrub; held against an edge, the view
//! scrolls that way, faster the longer it's held. Right-click a clip, a track header or
//! empty space for a menu (cut/copy/paste/duplicate, split, group, enable, delete; track
//! rename/reorder/delete). Ctrl+wheel zooms, the wheel scrolls, and the view follows the
//! playhead while playing. The commands live in `oa_edit::timeline` and `clips.rs`.
//!
//! Tracks: click a header to pick it (Ctrl+click for more, Shift+click for a run) —
//! Ctrl+V and Ctrl+D then put clips in the first one's free space after the playhead,
//! Alt+↑/↓ moves them — and drag a header up or down to reorder.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{ItemId, TrackId, TrackKind};
use oa_edit::timeline::{self as cmd, Edge, Snapper};
use oa_time::Time;
use std::sync::Arc;

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
    /// Scrolled or zoomed away from the playhead while playing: the view stays where it
    /// was put until the playhead comes back into it.
    pub looked_away: bool,
    /// Seconds the playhead has been dragged against an edge of the lane: the view
    /// scrolls that way, faster the longer it's held (and the further past the edge).
    pub edge_held: f64,
    /// What the open right-click menu is for.
    pub(crate) menu: Option<Menu>,
    /// Index into [`ROW_HEIGHTS`]: how tall each track is drawn.
    pub row_height: usize,
    /// Milliseconds the timeline takes to draw per clip in view (learned while each clip
    /// is drawn on its own), and whether clips too narrow to see are being joined into
    /// blocks — only when drawing them one by one would be slow (see [`join_tiny_clips`]).
    pub per_clip_ms: f32,
    pub joining: bool,
}

/// Whether to join clips too narrow to see into blocks, given how many are in view and
/// what one costs to draw: only once drawing them one by one would take more than
/// `JOIN_ABOVE_MS`, and back to one by one below `SPLIT_BELOW_MS` (a gap between the two,
/// so it doesn't flicker on the edge).
pub fn join_tiny_clips(joining: bool, per_clip_ms: f32, clips_in_view: usize) -> bool {
    const JOIN_ABOVE_MS: f32 = 6.0;
    const SPLIT_BELOW_MS: f32 = 3.0;
    let predicted = per_clip_ms * clips_in_view as f32;
    if joining { predicted > SPLIT_BELOW_MS } else { predicted > JOIN_ABOVE_MS }
}

impl TimelineView {
    pub fn track_height(&self) -> f32 {
        ROW_HEIGHTS[self.row_height.min(ROW_HEIGHTS.len() - 1)]
    }
}

impl Default for TimelineView {
    fn default() -> Self {
        TimelineView { start: 0.0, span: 10.0, fit: true, looked_away: false, edge_held: 0.0, menu: None, row_height: 1, per_clip_ms: 0.0, joining: false }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Menu {
    /// The selected clips.
    Clips,
    Track(TrackId),
    /// Empty space at this time, on this track (if over one).
    Empty(Time, Option<TrackId>),
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
    /// Compound clips: the same for the timeline inside.
    compound: Option<(oa_doc::SeqId, f64, f64)>,
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

type Row = (TrackId, TrackKind, String, bool);

/// How fast the view scrolls, in views per second (negative: back), while the playhead
/// is dragged against an edge: `push` is how deep into the edge the pointer is (1 = at
/// the lane's edge, up to 3 well past it), `held` how long it's been there. Gentle at
/// first, it gathers speed — up to 8 views a second.
fn edge_scroll_speed(push: f32, held: f64) -> f64 {
    (push as f64 * 0.6 * (1.0 + 2.5 * held)).clamp(-8.0, 8.0)
}

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
            self.report_error(trf("can't move the track: {e}", &[("e", &e.to_string())]));
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
        self.selected_tracks.retain(|t| seq.track(*t).is_some());
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
            // Ctrl+wheel zooms; the wheel scrolls through time; Shift+wheel scrolls the
            // tracks up and down (egui hands Shift+wheel over as sideways scrolling, so
            // it's turned back for the tracks' scroll area, which reads it after this).
            let (zoom, scroll, shift) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta(), i.modifiers.shift));
            if shift && zoom == 1.0 {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::vec2(0.0, scroll.x + scroll.y));
            } else if zoom == 1.0 && (scroll.x != 0.0 || scroll.y != 0.0) {
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
            if shift {
            } else if zoom != 1.0 {
                // Keep the time under the pointer where it is.
                let at = view.start + ((pointer.x - lane.left()) / lane.width()).clamp(0.0, 1.0) as f64 * view.span;
                view.span = (view.span / zoom as f64).clamp(0.2, 3600.0);
                view.start = (at - ((pointer.x - lane.left()) / lane.width()).clamp(0.0, 1.0) as f64 * view.span).max(0.0);
                view.fit = false;
                view.looked_away = self.playing;
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let px = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                view.start = (view.start - px as f64 / lane.width() as f64 * view.span).max(0.0);
                view.fit = false;
                view.looked_away = self.playing;
            }
        }
        // While playing, the view glides along with the playhead — it stays put until the
        // playhead is three quarters across, then scrolls with it every frame (no page
        // jumps). Scrolled or zoomed away: left alone until the playhead is back in the
        // part of the view where following wouldn't move it.
        const FOLLOW_AT: f64 = 0.75;
        if !self.playing {
            view.looked_away = false;
        } else if !view.fit && self.timeline_drag.is_none() {
            let t = self.playhead.as_seconds_f64();
            // Back where following wouldn't move the view: follow again.
            if view.looked_away && t >= view.start && t <= view.start + view.span * FOLLOW_AT {
                view.looked_away = false;
            }
            if !view.looked_away {
                if t < view.start {
                    // Looped back to the start (or jumped behind the view).
                    view.start = (t - view.span * 0.05).max(0.0);
                } else if t > view.start + view.span * FOLLOW_AT {
                    view.start = t - view.span * FOLLOW_AT;
                }
            }
        }
        // Scrubbing against an edge of the lane (or past it) scrolls the view that way,
        // speeding up the longer it's held and the deeper into the edge the pointer is —
        // so a long timeline can be crossed in one drag, and a short nudge moves a little.
        const SCROLL_EDGE_PX: f32 = 40.0;
        let edge_push = match (&self.timeline_drag, response.interact_pointer_pos()) {
            (Some(TimelineDrag::Scrub), Some(pos)) if response.dragged() => {
                if pos.x < lane.left() + SCROLL_EDGE_PX && view.start > 0.0 {
                    -((lane.left() + SCROLL_EDGE_PX - pos.x) / SCROLL_EDGE_PX).min(3.0)
                } else if pos.x > lane.right() - SCROLL_EDGE_PX && view.start + view.span < duration + view.span * 0.05 {
                    ((pos.x - (lane.right() - SCROLL_EDGE_PX)) / SCROLL_EDGE_PX).min(3.0)
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        if edge_push != 0.0 {
            let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
            view.edge_held += dt;
            let speed = edge_scroll_speed(edge_push, view.edge_held);
            let end = (duration + view.span * 0.05 - view.span).max(0.0);
            view.start = (view.start + speed * view.span * dt).clamp(0.0, end);
            view.fit = false;
            ui.ctx().request_repaint();
        } else {
            view.edge_held = 0.0;
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
        // Only when drawing each clip would be slow: clips too narrow to see join blocks.
        let clips_in_view: usize = rows.iter().filter_map(|(id, ..)| seq.track(*id)).map(|t| in_view(&t.items).len()).sum();
        let joining = join_tiny_clips(self.timeline_view.joining, self.timeline_view.per_clip_ms, clips_in_view);
        self.timeline_view.joining = joining;
        let layout_clock = std::time::Instant::now();
        for (r, (track_id, kind, _, _)) in rows.iter().enumerate() {
            let Some(track) = seq.track(*track_id) else { continue };
            let shown = in_view(&track.items);
            for item in &track.items[shown.clone()] {
                let x0 = x_of(item.range.start);
                // Clips too narrow to see (zoomed far out) join a run drawn as one block:
                // nothing to read on them, so none of the work below.
                if joining && x_of(item.range.end()) - x0 < TINY_CLIP_PX {
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
                    // In the clip, and on screen (a tracked clip can carry thousands).
                    .filter(|x| *x >= x0.max(lane.left() - 8.0) && *x <= x1.min(lane.right() + 8.0))
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
                    if names.is_empty() { tr("✦ Empty — add effects").to_string() } else { format!("✦ {}", names.join(" · ")) }
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
                let compound = match item.kind {
                    oa_doc::ItemKind::Nested { sequence } => {
                        let speed = item.time_map.speed.num() as f64 / item.time_map.speed.den() as f64;
                        Some((sequence, item.time_map.source_in.as_seconds_f64(), speed))
                    }
                    _ => None,
                };
                let start = item.range.start.as_seconds_f64();
                boxes.push(ClipBox { id: item.id, name, rect, kind: *kind, enabled, keys, text, intro, outro, media, compound, start, container });
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
        let layout_ms = layout_clock.elapsed().as_secs_f32() * 1000.0;
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

        let header_at = |p: egui::Pos2| {
            (p.x < lane.left() && p.y >= full.top() + RULER_HEIGHT).then(|| row_at(p.y)).flatten().filter(|r| !toggle_rect(*r).expand(3.0).contains(p))
        };

        // Hover cursor.
        if self.timeline_drag.is_none()
            && let Some(pos) = response.hover_pos()
            && header_at(pos).is_some()
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
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
            let shape = app.band_shape(b.id, &band, b.rect, lane, &x_of, &time_at)?;
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
            self.timeline_drag = Some(if in_ruler(origin) {
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
            // Shift turns snapping off for the duration of the drag; Ctrl flips it.
            let snapping = self.settings.snapping != mods.command && !mods.shift;
            let tolerance = px_to_time(SNAP_PX);
            let s = self.editor.sequence();
            let (sid, playhead) = (self.editor.seq, self.playhead);
            let result = match &self.timeline_drag {
                Some(TimelineDrag::Scrub) | None => {
                    // Past the lane's edge, the playhead rides the edge as the view scrolls.
                    self.set_playhead(cmd::snap_to_frame(s, time_at(pos.x.clamp(lane.left(), lane.right()))));
                    Ok(())
                }
                // Painted below; applied on release.
                Some(TimelineDrag::Track { .. }) => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    Ok(())
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
            && let Some(TimelineDrag::Track { track }) = self.timeline_drag
                && let Some(to) = track_drop_row(&rows, track, pos.y, &row_at, full.top() + RULER_HEIGHT) {
                    self.reorder_track(&rows, track, to);
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
            if let Err(e) = self.editor.apply(if enabled { tr("Turn track off") } else { tr("Turn track on") }, vec![op]) {
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
            } else if let Some(r) = header_at(pos) {
                // A track's header picks it: Ctrl+click adds or drops it, Shift+click
                // picks the run of tracks from the last one picked.
                let id = rows[r].0;
                if mods.command {
                    if let Some(i) = self.selected_tracks.iter().position(|t| *t == id) {
                        self.selected_tracks.remove(i);
                    } else {
                        self.selected_tracks.push(id);
                    }
                } else if mods.shift
                    && let Some(from) = self.selected_tracks.last().and_then(|last| rows.iter().position(|row| row.0 == *last))
                {
                    let (a, b) = (from.min(r), from.max(r));
                    for row in &rows[a..=b] {
                        if !self.selected_tracks.contains(&row.0) {
                            self.selected_tracks.push(row.0);
                        }
                    }
                } else {
                    self.selected_tracks = vec![id];
                }
            } else if pos.x >= lane.left() {
                if !in_ruler(pos) && !adding {
                    self.selection = None;
                    self.selected.clear();
                }
                let s = self.editor.sequence();
                self.set_playhead(cmd::snap_to_frame(s, time_at(pos.x)));
            }
        }

        // Double-click empty space on a track to pick just that track (pastes and
        // duplicates go into it, Alt+↑/↓ moves it); again (or Esc) lets go.
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && !in_ruler(pos)
            && let Some(r) = (pos.x >= lane.left() && clip_at(pos).is_none()).then(|| row_at(pos.y)).flatten()
        {
            let id = rows[r].0;
            self.selected_tracks = if self.selected_tracks == [id] { Vec::new() } else { vec![id] };
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
            self.timeline_view.menu = if let Some((item, _, Some(key), _)) = band_hit(self, pos) {
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
        // for tracks can be clicked into and typed in.
        egui::Popup::context_menu(&response).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| match self.timeline_view.menu {
            Some(Menu::Clips) => self.clip_menu(ui),
            Some(Menu::Track(track)) => self.track_menu(ui, track),
            Some(Menu::Empty(at, track)) => self.empty_menu(ui, at, track),
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
        // The picked tracks: their headers outlined, their lanes faintly lit.
        for r in self.selected_tracks.iter().filter_map(|id| rows.iter().position(|row| row.0 == *id)) {
            let header = egui::Rect::from_min_max(egui::pos2(full.left(), row_top(r)), egui::pos2(lane.left(), row_top(r) + row_h(r)));
            clip_painter.rect_filled(egui::Rect::from_min_max(egui::pos2(lane.left(), row_top(r)), egui::pos2(full.right(), row_top(r) + row_h(r))), 0.0, crate::style::ACCENT.gamma_multiply(0.08));
            painter.rect_stroke(header.shrink(1.0), 3.0, egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        let draw_clock = std::time::Instant::now();
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
            // A compound clip: its own filmstrip, rendered from the timeline inside, over
            // its sound — or, made of sound alone, just its sound.
            if let Some((seq, source_in, speed)) = b.compound {
                let (picture, sound) = self.compound_look(seq);
                let painter = clip_painter.with_clip_rect(b.rect.shrink(1.0).intersect(lane));
                let r = b.rect;
                let audible = !sound.is_empty() && self.editor.item(b.id).is_some_and(|i| i.audio_enabled());
                let wave_h = match (picture, audible) {
                    (true, true) => r.height() * 0.4,
                    (false, _) => r.height(),
                    (true, false) => 0.0,
                };
                if picture && let Some(strip) = self.compound_strip(ui.ctx(), seq) {
                    let pic = egui::Rect::from_min_max(r.min, egui::pos2(r.right(), r.bottom() - wave_h));
                    let tile = (pic.height() * strip.aspect).max(8.0);
                    let visible = painter.clip_rect();
                    for x in shown_tiles([r.left(), r.right()], tile, [visible.left(), visible.right()]) {
                        let at = source_in + (time_at(x).as_seconds_f64() - b.start) * speed;
                        let dest = egui::Rect::from_min_size(egui::pos2(x, pic.top()), egui::vec2(tile, pic.height()));
                        painter.image(strip.texture.id(), dest, strip.uv(at), egui::Color32::from_gray(190));
                    }
                    painter.rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(r.width(), 14.0)), 0.0, egui::Color32::from_black_alpha(90));
                }
                if audible {
                    let band = egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - wave_h), r.max);
                    self.paint_compound_sound(ui.ctx(), &painter, &sound, band, |x| source_in + (time_at(x).as_seconds_f64() - b.start) * speed);
                }
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
                && let Some(shape) = self.band_shape(b.id, &band, b.rect, lane, &x_of, &time_at)
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
        // What a clip costs to lay out and draw, learned while each is drawn on its own
        // (smoothed, so one slow frame doesn't flip the joining on).
        if !joining && clips_in_view > 0 {
            let ms = (layout_ms + draw_clock.elapsed().as_secs_f32() * 1000.0) / clips_in_view as f32;
            let v = &mut self.timeline_view.per_clip_ms;
            *v = if *v == 0.0 { ms } else { *v * 0.9 + ms * 0.1 };
        }
        // A track being dragged: where it would land.
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos())
            && let Some(TimelineDrag::Track { track }) = self.timeline_drag
                && let (Some(to), Some(from)) = (track_drop_row(&rows, track, pos.y, &row_at, full.top() + RULER_HEIGHT), rows.iter().position(|r| r.0 == track)) {
                    let y = if to > from { row_top(to) + row_h(to) } else { row_top(to) };
                    painter.line_segment([egui::pos2(full.left(), y), egui::pos2(full.right(), y)], egui::Stroke::new(3.0, crate::style::ACCENT));
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
            let label = trf("⇄ Swap for {0}", &[("0", &(drop.name).to_string())]);
            let galley = clip_painter.layout_no_wrap(label, egui::FontId::proportional(12.0), egui::Color32::WHITE);
            let at = egui::pos2(target.rect.left() + 6.0, target.rect.center().y - galley.size().y / 2.0);
            let bg = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(5.0, 2.0));
            clip_painter.rect_filled(bg, 3.0, accent);
            clip_painter.galley(at, galley, egui::Color32::WHITE);
            let hint = clip_painter.layout_no_wrap(tr("Shift: add it instead").into(), egui::FontId::proportional(10.0), egui::Color32::WHITE);
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
        // Files (or a picture from a browser) dragged in from outside: a line where they'd
        // land, on the track under the pointer; dropped here, they go there.
        if let Some(pos) = self.file_hover.filter(|p| lane.contains(*p) && !in_ruler(*p)) {
            let x = x_of(time_at(pos.x));
            let (y0, y1) = match row_at(pos.y) {
                Some(r) => (row_top(r), row_top(r) + row_h(r)),
                None => (full.top() + RULER_HEIGHT, full.bottom()),
            };
            let accent = ui.visuals().selection.stroke.color;
            clip_painter.line_segment([egui::pos2(x, y0), egui::pos2(x, y1)], egui::Stroke::new(2.5, accent));
            let label = clip_painter.layout_no_wrap(tr("Drop to add it here").into(), egui::FontId::proportional(11.0), egui::Color32::WHITE);
            let bg = egui::Rect::from_min_size(egui::pos2(x + 4.0, y0 + 2.0), label.size()).expand2(egui::vec2(4.0, 2.0)).translate(egui::vec2(4.0, 2.0));
            clip_painter.rect_filled(bg, 3.0, accent);
            clip_painter.galley(bg.min + egui::vec2(4.0, 2.0), label, egui::Color32::WHITE);
            ui.ctx().request_repaint();
        }
        if self.file_drop.as_ref().is_some_and(|(_, p)| lane.contains(*p) && !in_ruler(*p))
            && let Some((items, pos)) = self.file_drop.take()
        {
            let track = row_at(pos.y).map(|r| rows[r].0);
            let at = time_at(pos.x);
            self.take_dropped(items, Some((at, track)));
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
    /// Whether compound `seq` shows a picture, and its sound: the clips heard inside it,
    /// in its own time (compound clips inside it included). Worked out once per edit.
    pub(crate) fn compound_look(&mut self, seq: oa_doc::SeqId) -> (bool, Arc<Vec<oa_audio::AudioClip>>) {
        let version = Arc::as_ptr(&self.editor.doc.snapshot()) as usize;
        if let Some((v, picture, sound)) = self.compound_looks.get(&seq)
            && *v == version
        {
            return (*picture, sound.clone());
        }
        let picture = self.editor.sequence_has_picture(seq);
        let sound = Arc::new(self.audio_clips_of(seq));
        self.compound_looks.retain(|_, (v, ..)| *v == version);
        self.compound_looks.insert(seq, (version, picture, sound.clone()));
        (picture, sound)
    }

    /// A compound clip's sound in `band`: at each x, the loudest of the clips inside it
    /// playing then (`inside(x)`: the compound's own time at screen x, in seconds), from
    /// each file's waveform.
    pub(crate) fn paint_compound_sound(&mut self, ctx: &egui::Context, painter: &egui::Painter, sound: &[oa_audio::AudioClip], band: egui::Rect, inside: impl Fn(f32) -> f64) {
        let peaks: Vec<Option<crate::previews::Peaks>> = sound.iter().map(|c| self.clip_previews.peaks(ctx, oa_doc::MediaId(c.media), &c.path)).collect();
        let visible = painter.clip_rect();
        let mid = band.center().y;
        let color = egui::Color32::from_rgba_unmultiplied(220, 255, 230, 170);
        let from = band.left().max(visible.left()).floor() as i32;
        let to = band.right().min(visible.right()).ceil() as i32;
        for px in (from..to).step_by(2) {
            let t = Time::from_seconds_f64(inside(px as f32));
            let mut level = 0f32;
            for (c, peaks) in sound.iter().zip(&peaks) {
                let Some(peaks) = peaks.as_ref().filter(|_| c.range.contains(t)) else { continue };
                // Where in the file (a reversed clip reads down from its pivot).
                let into = c.source_at(t).as_seconds_f64();
                let at = match c.reverse {
                    Some(pivot) => pivot.as_seconds_f64() - into,
                    None => into,
                };
                if let Some(&(lo, hi)) = peaks.get((at.max(0.0) * crate::previews::PEAKS_PER_SECOND) as usize) {
                    level = level.max(lo.abs().max(hi.abs()));
                }
            }
            let a = level.min(1.0) * band.height() * 0.5;
            painter.line_segment([egui::pos2(px as f32, mid - a), egui::pos2(px as f32, mid + a.max(0.5))], egui::Stroke::new(1.0, color));
        }
    }

    /// A compound clip's filmstrip — rendered from what's in it, on the preview thread,
    /// and made again after it's edited (the last one stays up meanwhile).
    pub(crate) fn compound_strip(&mut self, ctx: &egui::Context, seq: oa_doc::SeqId) -> Option<crate::previews::Strip> {
        let project = self.editor.doc.snapshot();
        let sequence = project.sequences.get(&seq)?;
        // What it is now: a new sequence value (an edit inside it) is a new version.
        let version = std::sync::Arc::as_ptr(sequence) as u64;
        let frames = (sequence.duration().as_seconds_f64().ceil() as usize).clamp(1, crate::previews::COMPOUND_FRAMES);
        let (worker, registry) = (&self.preview_worker, self.registry.clone());
        self.clip_previews
            .compound_strip(ctx, seq, version, |reply| {
                worker.strip(crate::preview_worker::StripRequest {
                    seq,
                    version,
                    project: project.clone(),
                    registry,
                    frames,
                    frame_w: crate::previews::FRAME_W,
                    cols: crate::previews::COLS,
                    reply,
                })
            })
            .cloned()
    }

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
        if item(ui, tr("Paste at playhead"), "Ctrl+V", !self.clipboard.is_empty()) {
            let at = self.playhead;
            self.paste_at(at);
            ui.close();
        }
        if item(ui, "Duplicate", "Ctrl+D", has) {
            self.duplicate_selection();
            ui.close();
        }
        ui.separator();
        if item(ui, tr("Split at playhead"), "S", has) {
            self.split_at_playhead();
            ui.close();
        }
        if n >= 2 {
            ui.menu_button(tr("Arrange"), |ui| {
                use oa_edit::timeline::Arrange;
                let choices: [(&str, &str, Arrange); 4] = [
                    ("Move together", tr("Close the gaps between them: on each track, each starts where the one before ends"), Arrange::Together),
                    (tr("Line up starts"), tr("Every one starts where the earliest does"), Arrange::LineUpStarts),
                    (tr("Move to playhead"), tr("The earliest starts at the playhead; the rest keep their spacing"), Arrange::StartAt(self.playhead)),
                    ("Space evenly", tr("The first and last stay; the ones between are spread out evenly"), Arrange::SpaceEvenly),
                ];
                for (label, hint, how) in choices {
                    if ui.button(label).on_hover_text(hint).clicked() {
                        self.arrange_selection(how, label);
                        ui.close();
                    }
                }
            });
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
            .add_enabled(has, egui::Button::new(tr("As media")))
            .on_hover_text(tr("Adds these clips to the media bin as one compound clip — use it as a clip, or as a mask or other effect picture"))
            .clicked()
        {
            self.compound_selection(false);
            ui.close();
        }
        if ui.add_enabled(has, egui::Button::new(tr("Nest into one clip"))).on_hover_text(tr("Replaces these clips with one compound clip")).clicked() {
            self.compound_selection(true);
            ui.close();
        }
        let compound = self.selection.filter(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, oa_doc::ItemKind::Nested { .. })));
        if let Some(id) = compound
            && ui.button(tr("Open compound clip")).on_hover_text(tr("Edit its own timeline (or double-click it)")).clicked()
        {
            self.open_compound(id);
            ui.close();
        }
        if self.compound_selected()
            && ui.button(tr("Break apart")).on_hover_text(tr("Put the clips inside back on the timeline, where they play now")).clicked()
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
        let r = ui.add(egui::TextEdit::singleline(name).desired_width(140.0).hint_text(tr("Track name")));
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
        let picked = self.selected_tracks.contains(&track);
        if ui.selectable_label(picked, tr("Pick this track")).on_hover_text(tr("Ctrl+V and Ctrl+D put clips in its first free space after the playhead; Alt+↑/↓ moves it (or click its header; Ctrl+click picks more)")).clicked() {
            if picked {
                self.selected_tracks.retain(|t| *t != track);
            } else {
                self.selected_tracks.push(track);
            }
            ui.close();
        }
        if item(ui, tr("Select all on track"), "", !t.items.is_empty()) {
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
        if item(ui, if t.kind == TrackKind::Video { tr("Add video track") } else { tr("Add audio track") }, "", true) {
            if let Err(e) = self.editor.add_track(t.kind) {
                self.error = Some(e.to_string());
            }
            ui.close();
        }
        let (label, tip) = if t.kind == TrackKind::Video {
            (tr("Add picture effect track above"), tr("A thin track whose effects run over everything below it — this track and the ones under it"))
        } else {
            (tr("Add sound effect track above"), tr("A thin track whose sound effects run over this track, the ones below it, and the sound of picture tracks"))
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
        if t.effects && item(ui, tr("Add effect container at playhead"), "", true) {
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
            if ui.button(tr("Add effect container here")).on_hover_text(tr("Its effects run over everything below this track while it lasts")).clicked() {
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
            && item(ui, tr("Paste into this track's first gap"), "", !self.clipboard.is_empty())
        {
            self.set_playhead(at);
            self.paste_into_track(track);
            ui.close();
        }
        if item(ui, tr("Add text here"), "", true) {
            self.set_playhead(at);
            self.add_text();
            ui.close();
        }
        if item(ui, "Select all", "Ctrl+A", true) {
            self.select_all();
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
        // Where in the file the clip is at screen x (following its speed ramp, if it has
        // one — only the ramp is copied, not the clip).
        let ramp = self.editor.item(b.id).and_then(|i| i.speed_ramp().cloned().map(|r| (r, speed.signum())));
        let source_at = |x: f32| {
            let local = time_at(x).as_seconds_f64() - b.start;
            match &ramp {
                Some((r, dir)) => source_in + dir * oa_doc::ramp_integral(r, local),
                None => source_in + local * speed,
            }
        };
        let r = b.rect;
        let visible = painter.clip_rect();
        let wave_h = if video.is_some() { r.height() * 0.4 } else { r.height() };
        if let Some(v) = &video
            && let Some(strip) = self.clip_previews.strip(ctx, media, &path, [v.width, v.height], if still { 0.0 } else { duration })
        {
            let pic = egui::Rect::from_min_max(r.min, egui::pos2(r.right(), if audible { r.bottom() - wave_h } else { r.bottom() }));
            let tile = (pic.height() * strip.aspect).max(8.0);
            for x in shown_tiles([r.left(), r.right()], tile, [visible.left(), visible.right()]) {
                let uv = strip.uv(if still { 0.0 } else { source_at(x) });
                let dest = egui::Rect::from_min_size(egui::pos2(x, pic.top()), egui::vec2(tile, pic.height()));
                painter.image(strip.texture.id(), dest, uv, egui::Color32::from_gray(190));
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

/// The left edges of a filmstrip's tiles (`tile` px apart from the clip's start,
/// `clip` = [left, right]) that reach into `view` = [left, right]: only the ones on
/// screen, found without walking from the clip's start — a long clip zoomed in is
/// millions of px wide.
fn shown_tiles(clip: [f32; 2], tile: f32, view: [f32; 2]) -> impl Iterator<Item = f32> {
    let first = ((view[0] - clip[0]).max(0.0) / tile).floor();
    let end = clip[1].min(view[1]);
    (0..).map(move |i| clip[0] + (first + i as f32) * tile).take_while(move |x| *x < end)
}

#[cfg(test)]
mod tests {
    use super::{edge_scroll_speed, join_tiny_clips, shown_tiles};

    #[test]
    fn only_tiles_on_screen_are_visited() {
        // A clip ten million px wide, a 1000 px view in the middle of it: ~20 tiles, all
        // on the clip's own grid, covering the view.
        let tiles: Vec<f32> = shown_tiles([-5_000_000.0, 5_000_000.0], 50.0, [0.0, 1000.0]).collect();
        assert!((20..=21).contains(&tiles.len()), "{}", tiles.len());
        assert!(tiles[0] <= 0.0 && tiles[0] > -50.0);
        assert!(tiles.iter().all(|x| ((x + 5_000_000.0) / 50.0).fract() == 0.0));
        assert!(*tiles.last().unwrap() + 50.0 >= 1000.0);
        // A clip starting inside the view begins at its own start; one off screen has none.
        assert_eq!(shown_tiles([300.0, 420.0], 50.0, [0.0, 1000.0]).collect::<Vec<_>>(), vec![300.0, 350.0, 400.0]);
        assert_eq!(shown_tiles([2000.0, 3000.0], 50.0, [0.0, 1000.0]).count(), 0);
    }

    #[test]
    fn tiny_clips_join_only_when_drawing_them_is_slow() {
        // 0.002 ms a clip: 1000 in view is 2 ms — drawn one by one.
        assert!(!join_tiny_clips(false, 0.002, 1000));
        // 5000 is 10 ms: joined.
        assert!(join_tiny_clips(false, 0.002, 5000));
        // Once joined, it stays so until well under the line (no flicker at 6 ms).
        assert!(join_tiny_clips(true, 0.002, 2000));
        assert!(!join_tiny_clips(true, 0.002, 1000));
    }

    #[test]
    fn edge_scrolling_speeds_up() {
        // Its direction is the edge's; deeper and longer both go faster; it tops out.
        assert!(edge_scroll_speed(-1.0, 0.0) < 0.0 && edge_scroll_speed(1.0, 0.0) > 0.0);
        assert!(edge_scroll_speed(1.0, 1.0) > edge_scroll_speed(1.0, 0.0));
        assert!(edge_scroll_speed(2.0, 0.5) > edge_scroll_speed(1.0, 0.5));
        assert!(edge_scroll_speed(0.2, 0.0) < 0.2, "a nudge barely moves");
        assert_eq!(edge_scroll_speed(3.0, 60.0), 8.0);
        assert_eq!(edge_scroll_speed(-3.0, 60.0), -8.0);
    }
}
