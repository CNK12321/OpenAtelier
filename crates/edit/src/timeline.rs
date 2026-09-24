//! Timeline commands: trim, split (razor), ripple delete, move, slip, snapping.
//!
//! Every command is a pure function from a project snapshot to the [`Op`]s that perform
//! it, so a command is one undo step, commands compose, and they're testable without a
//! UI. Interactive commands (trim, move) **clamp** instead of failing: dragging a clip
//! edge past its media or into a neighbor stops at the limit, like any editor.

use oa_doc::{ClipEnd, EditError, Item, ItemId, ItemKind, Op, ParamTarget, Project, SeqId, Sequence, Track, TrackId, Transition};
use oa_time::{Time, TimeRange};

type R<T> = Result<T, EditError>;

/// Which end of a clip a trim moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    Head,
    Tail,
}

fn sequence(p: &Project, seq: SeqId) -> R<&Sequence> {
    p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))
}

fn locate(p: &Project, seq: SeqId, item: ItemId) -> R<(&Sequence, &Track, &Item)> {
    let s = sequence(p, seq)?;
    let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
    Ok((s, &s.tracks[ti], &s.tracks[ti].items[ii]))
}

/// The shortest clip an edit may leave: one frame of the sequence.
pub fn min_duration(s: &Sequence) -> Time {
    s.rate.frame_start(1).max(Time(1))
}

/// Rounds `t` to the nearest frame boundary of the sequence.
pub fn snap_to_frame(s: &Sequence, t: Time) -> Time {
    let rate = s.rate;
    let n = rate.frame_at(t);
    let (a, b) = (rate.frame_start(n), rate.frame_start(n + 1));
    if t - a <= b - t { a } else { b }
}

/// Timeline span the item's media allows: `(earliest start, latest end)`. `None` means
/// unbounded on that side (stills, solids, freeze frames, media of unknown length).
pub fn media_limits(p: &Project, item: &Item) -> (Option<Time>, Option<Time>) {
    let ItemKind::Media { media } = item.kind else { return (None, None) };
    let Some(duration) = p.media(media).and_then(|m| m.info.as_ref()).and_then(|i| i.playable_duration()) else {
        return (None, None);
    };
    let speed = item.time_map.speed;
    if speed.is_zero() {
        return (None, None);
    }
    // Clip-local times where the source clock hits 0 and the end of the media.
    let local_at = |source: Time| Time::from_rational_floor((source - item.time_map.source_in).as_rational() * speed.recip());
    let (at_zero, at_end) = (local_at(Time::ZERO), local_at(duration));
    let (lo, hi) = if speed.num() > 0 { (at_zero, at_end) } else { (at_end, at_zero) };
    (Some(item.range.start + lo), Some(item.range.start + hi))
}

/// The free span around `item` on its track: `(end of the previous clip, start of the
/// next)`. `None` = open (timeline start / nothing after).
fn neighbors(track: &Track, item: &Item) -> (Time, Option<Time>) {
    let prev = track.items.iter().filter(|i| i.id != item.id && i.range.start < item.range.start).map(|i| i.range.end()).max();
    let next = track.items.iter().filter(|i| i.id != item.id && i.range.start >= item.range.start).map(|i| i.range.start).min();
    (prev.unwrap_or(Time::ZERO).max(Time::ZERO), next)
}

/// Moves one edge of a clip to `to`, clamped so the clip stays at least a frame long,
/// doesn't run past its media, and doesn't overlap its neighbors. Head trims keep the
/// footage in place (the source in-point moves with the edge).
pub fn trim(p: &Project, seq: SeqId, item: ItemId, edge: Edge, to: Time) -> R<Vec<Op>> {
    let (s, track, it) = locate(p, seq, item)?;
    let (media_lo, media_hi) = media_limits(p, it);
    let (prev_end, next_start) = neighbors(track, it);
    let min = min_duration(s);
    let (range, time_map) = match edge {
        Edge::Head => {
            let lo = media_lo.map_or(prev_end, |m| m.max(prev_end));
            let hi = it.range.end() - min;
            let start = to.clamp(lo.min(hi), hi);
            it.trimmed_head(start).ok_or(EditError::InvalidRange)?
        }
        Edge::Tail => {
            let hi = match (media_hi, next_start) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let lo = it.range.start + min;
            let end = hi.map_or(to.max(lo), |hi| to.clamp(lo, hi.max(lo)));
            (TimeRange::new(it.range.start, end - it.range.start), it.time_map)
        }
    };
    if range == it.range && time_map == it.time_map {
        return Ok(Vec::new());
    }
    Ok(vec![Op::SetItemTiming { seq, item, range, time_map }])
}

/// Slides the footage inside a clip without moving the clip (a slip edit): the source
/// in-point moves by `delta`, clamped to the media.
pub fn slip(p: &Project, seq: SeqId, item: ItemId, delta: Time) -> R<Vec<Op>> {
    let (_, _, it) = locate(p, seq, item)?;
    let mut map = it.time_map;
    map.source_in += delta;
    let duration = match it.kind {
        ItemKind::Media { media } => p.media(media).and_then(|m| m.info.as_ref()).and_then(|i| i.playable_duration()),
        _ => None,
    };
    if let Some(duration) = duration {
        // The span of source the clip shows must stay inside [0, duration].
        let travel = map.source_time(it.range.duration) - map.source_in;
        let lo = Time::ZERO - travel.min(Time::ZERO);
        let hi = duration - travel.max(Time::ZERO);
        map.source_in = map.source_in.clamp(lo, hi.max(lo));
    }
    if map == it.time_map {
        return Ok(Vec::new());
    }
    Ok(vec![Op::SetItemTiming { seq, item, range: it.range, time_map: map }])
}

/// Cuts a clip in two at `at` (the razor). The picture and sound don't change: the back
/// half continues the footage, keyframes and procedural motion exactly where the front
/// half stops, and format-variant overrides are carried over. Returns the ops and the
/// new (back half) item's id. `alloc` hands out fresh ids (see `Document::alloc_id`).
pub fn split(p: &Project, seq: SeqId, item: ItemId, at: Time, alloc: &mut dyn FnMut() -> u64) -> R<(Vec<Op>, ItemId)> {
    let (s, track, it) = locate(p, seq, item)?;
    let min = min_duration(s);
    if at < it.range.start + min || at > it.range.end() - min {
        return Err(EditError::InvalidRange);
    }
    let head = TimeRange::new(it.range.start, at - it.range.start);
    let (tail_range, tail_map) = it.trimmed_head(at).ok_or(EditError::InvalidRange)?;
    let delta = at - it.range.start;

    let mut back = it.clone();
    back.id = ItemId(alloc());
    back.range = tail_range;
    back.time_map = tail_map;
    for src in back.params.0.values_mut() {
        src.shift_clip_clock(delta);
    }
    for fx in &mut back.effects {
        fx.id = oa_doc::EffectId(alloc());
        for src in fx.params.0.values_mut() {
            src.shift_clip_clock(delta);
        }
    }
    // The head transition stays on the front half, the tail one moves to the back half.
    back.transition_in = None;
    if let Some(tr) = &mut back.transition_out {
        for src in tr.params.0.values_mut() {
            src.shift_clip_clock(delta);
        }
    }
    let new_id = back.id;
    let mut ops = vec![
        Op::SetItemTiming { seq, item, range: head, time_map: it.time_map },
        Op::InsertItem { seq, track: track.id, item: back },
    ];
    if it.transition_out.is_some() {
        ops.push(Op::SetTransition { seq, item, end: ClipEnd::Tail, transition: None });
    }
    for v in &s.variants {
        for (param, src) in v.overrides.get(&item).into_iter().flat_map(|o| o.0.iter()) {
            let mut src = src.clone();
            src.shift_clip_clock(delta);
            ops.push(Op::SetParam {
                seq,
                item: new_id,
                target: ParamTarget::VariantOverride(v.id),
                param: param.clone(),
                source: Some(src),
            });
        }
    }
    Ok((ops, new_id))
}

/// Puts a transition of `type_id` on `end` of a clip (replacing any there, and keeping
/// its settings if the type is the same). The duration is clamped to what the clips
/// allow: a cut transition can use at most each neighbor's full length on its side.
pub fn set_transition(p: &Project, seq: SeqId, item: ItemId, end: ClipEnd, type_id: &str, duration: Time) -> R<Vec<Op>> {
    let (s, track, it) = locate(p, seq, item)?;
    let min = min_duration(s);
    let i = track.items.iter().position(|x| x.id == item).expect("located");
    let prev = i.checked_sub(1).map(|k| &track.items[k]).filter(|x| x.range.end() == it.range.start);
    let limit = match (end, prev) {
        (ClipEnd::Head, Some(prev)) => Time(2 * prev.range.duration.0.min(it.range.duration.0)),
        _ => it.range.duration,
    };
    let duration = duration.clamp(min, limit.max(min));
    let mut transition = match it.transition(end) {
        Some(existing) if existing.type_id == type_id => existing.clone(),
        _ => Transition::new(type_id, duration),
    };
    transition.duration = duration;
    Ok(vec![Op::SetTransition { seq, item, end, transition: Some(transition) }])
}

/// The cut nearest `t` on a track, as the clip that starts there — where a cut
/// transition would go. Only cuts within `tolerance` count.
pub fn nearest_cut(track: &Track, t: Time, tolerance: Time) -> Option<ItemId> {
    track
        .items
        .windows(2)
        .filter(|w| w[0].range.end() == w[1].range.start)
        .map(|w| &w[1])
        .filter(|b| (b.range.start - t).0.abs() <= tolerance.0)
        .min_by_key(|b| (b.range.start - t).0.abs())
        .map(|b| b.id)
}

/// Splits every clip under `at` on the given tracks (all tracks if empty) — the razor
/// across the whole timeline. Clips whose edge is within a frame of `at` are skipped.
pub fn split_all(p: &Project, seq: SeqId, at: Time, tracks: &[TrackId], alloc: &mut dyn FnMut() -> u64) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let mut ops = Vec::new();
    for track in s.tracks.iter().filter(|t| tracks.is_empty() || tracks.contains(&t.id)) {
        if let Some(it) = track.item_at(at)
            && let Ok((mut more, _)) = split(p, seq, it.id, at, alloc)
        {
            ops.append(&mut more);
        }
    }
    Ok(ops)
}

/// Removes a clip and pulls everything after it on the same track left to close the gap.
pub fn ripple_delete(p: &Project, seq: SeqId, item: ItemId) -> R<Vec<Op>> {
    let (_, track, it) = locate(p, seq, item)?;
    let shift = it.range.duration;
    let mut ops = vec![Op::RemoveItem { seq, item }];
    // Left to right, so each move lands in space the previous one vacated.
    for later in track.items.iter().filter(|i| i.range.start >= it.range.end()) {
        let range = TimeRange::new(later.range.start - shift, later.range.duration);
        ops.push(Op::SetItemTiming { seq, item: later.id, range, time_map: later.time_map });
    }
    Ok(ops)
}

/// Removes the empty space containing `at` on a track, pulling later clips left.
pub fn close_gap(p: &Project, seq: SeqId, track: TrackId, at: Time) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let t = s.track(track).ok_or(EditError::NotFound("track", track.0))?;
    if t.item_at(at).is_some() {
        return Ok(Vec::new());
    }
    let gap_start = t.items.iter().map(|i| i.range.end()).filter(|e| *e <= at).max().unwrap_or(Time::ZERO);
    let Some(gap_end) = t.items.iter().map(|i| i.range.start).filter(|s| *s > at).min() else { return Ok(Vec::new()) };
    let shift = gap_end - gap_start;
    Ok(t.items
        .iter()
        .filter(|i| i.range.start >= gap_end)
        .map(|i| Op::SetItemTiming {
            seq,
            item: i.id,
            range: TimeRange::new(i.range.start - shift, i.range.duration),
            time_map: i.time_map,
        })
        .collect())
}

/// The start nearest to `want` where a clip of `duration` fits on `track` without
/// overlapping anything (ignoring `ignore`), never before zero.
pub fn nearest_free_start(track: &Track, duration: Time, want: Time, ignore: Option<ItemId>) -> Time {
    let want = want.max(Time::ZERO);
    let others: Vec<TimeRange> = track.items.iter().filter(|i| Some(i.id) != ignore).map(|i| i.range).collect();
    let fits = |start: Time| start >= Time::ZERO && !others.iter().any(|r| r.overlaps(TimeRange::new(start, duration)));
    if fits(want) {
        return want;
    }
    // Candidates: flush against either side of every other clip.
    let mut best: Option<Time> = None;
    for r in &others {
        for c in [r.end(), r.start - duration] {
            if fits(c) && best.is_none_or(|b| (c - want).0.abs() < (b - want).0.abs()) {
                best = Some(c);
            }
        }
    }
    best.unwrap_or_else(|| others.iter().map(|r| r.end()).max().unwrap_or(Time::ZERO))
}

/// Moves a clip to start at `start` (clamped to the nearest free spot), optionally onto
/// another track of a compatible kind.
pub fn move_item(p: &Project, seq: SeqId, item: ItemId, start: Time, to_track: Option<TrackId>) -> R<Vec<Op>> {
    let (s, track, it) = locate(p, seq, item)?;
    let dest = match to_track {
        Some(id) => s.track(id).ok_or(EditError::NotFound("track", id.0))?,
        None => track,
    };
    let start = nearest_free_start(dest, it.range.duration, start, Some(item));
    let range = TimeRange::new(start, it.range.duration);
    if dest.id == track.id {
        if range == it.range {
            return Ok(Vec::new());
        }
        return Ok(vec![Op::SetItemTiming { seq, item, range, time_map: it.time_map }]);
    }
    let mut moved = it.clone();
    moved.range = range;
    Ok(vec![Op::RemoveItem { seq, item }, Op::InsertItem { seq, track: dest.id, item: moved }])
}

/// Moves several clips together: `delta` later in time and `track_shift` tracks up
/// (among tracks of each clip's kind, top = positive). All are lifted out first and put
/// back at their new places, so they can move through each other's old spots; the whole
/// move fails (and changes nothing) if any would land on another clip, before zero or
/// off the list of tracks.
pub fn move_items(p: &Project, seq: SeqId, items: &[ItemId], delta: Time, track_shift: i32) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let mut removes = Vec::new();
    let mut inserts = Vec::new();
    for &id in items {
        let (ti, ii) = s.find_item(id).ok_or(EditError::NotFound("item", id.0))?;
        let (track, it) = (&s.tracks[ti], &s.tracks[ti].items[ii]);
        let start = it.range.start + delta;
        if start < Time::ZERO {
            return Err(EditError::InvalidRange);
        }
        let same_kind: Vec<&Track> = s.tracks.iter().filter(|t| t.kind == track.kind).map(|t| &**t).collect();
        let at = same_kind.iter().position(|t| t.id == track.id).expect("own track") as i32 + track_shift;
        let dest = usize::try_from(at).ok().and_then(|i| same_kind.get(i)).ok_or(EditError::InvalidRange)?;
        let mut moved = it.clone();
        moved.range = TimeRange::new(start, it.range.duration);
        removes.push(Op::RemoveItem { seq, item: id });
        inserts.push(Op::InsertItem { seq, track: dest.id, item: moved });
    }
    if delta == Time::ZERO && track_shift == 0 {
        return Ok(Vec::new());
    }
    // Nothing may land on a clip that stays put, or on another moved clip.
    let landed: Vec<(TrackId, TimeRange)> = inserts
        .iter()
        .filter_map(|op| match op {
            Op::InsertItem { track, item, .. } => Some((*track, item.range)),
            _ => None,
        })
        .collect();
    for (i, (track, range)) in landed.iter().enumerate() {
        let staying = s.track(*track).into_iter().flat_map(|t| t.items.iter()).filter(|it| !items.contains(&it.id));
        let hits_staying = staying.into_iter().any(|it| it.range.overlaps(*range));
        let hits_moved = landed.iter().enumerate().any(|(j, (t, r))| j != i && t == track && r.overlaps(*range));
        if hits_staying || hits_moved {
            return Err(EditError::Overlap);
        }
    }
    removes.extend(inserts);
    Ok(removes)
}

/// Pastes copies of `clips` (each with the track it came from) so the earliest starts at
/// `at`, keeping their spacing. Each goes back on its own track when there's room there,
/// otherwise on a new track of its kind added on top. Copies get fresh ids (clips,
/// effects, groups — pasted clips that were grouped are grouped again, separately).
/// Returns the ops and the new clips' ids.
pub fn paste(p: &Project, seq: SeqId, clips: &[(TrackId, Item)], at: Time, alloc: &mut dyn FnMut() -> u64) -> R<(Vec<Op>, Vec<ItemId>)> {
    let s = sequence(p, seq)?;
    let Some(first) = clips.iter().map(|(_, i)| i.range.start).min() else { return Ok((Vec::new(), Vec::new())) };
    let mut ops = Vec::new();
    let mut ids = Vec::new();
    let mut groups = std::collections::HashMap::new();
    // Placed so far, per track (including new tracks), so pasted clips don't overlap
    // each other either.
    let mut placed: std::collections::HashMap<TrackId, Vec<TimeRange>> = std::collections::HashMap::new();
    let mut new_tracks: Vec<(TrackId, oa_doc::TrackKind, bool)> = Vec::new();
    let fits = |track: Option<&Track>, extra: &[TimeRange], range: TimeRange| {
        track.is_none_or(|t| !t.items.iter().any(|i| i.range.overlaps(range))) && !extra.iter().any(|r| r.overlaps(range))
    };
    for (from, item) in clips {
        let range = TimeRange::new(at + (item.range.start - first), item.range.duration);
        let kind = s.track(*from).map_or(oa_doc::TrackKind::Video, |t| t.kind);
        // An effect container goes back on an effect track, anything else on an ordinary one.
        let effects = matches!(item.kind, oa_doc::ItemKind::Adjustment);
        let own = s.track(*from).filter(|t| t.effects == effects && fits(Some(t), placed.get(&t.id).map_or(&[], |v| v), range)).map(|t| t.id);
        let track = own
            .or_else(|| new_tracks.iter().filter(|(_, k, e)| *k == kind && *e == effects).map(|(id, _, _)| *id).find(|id| fits(None, placed.get(id).map_or(&[], |v| v), range)))
            .unwrap_or_else(|| {
                let id = TrackId(alloc());
                let count = s.tracks.iter().filter(|t| t.kind == kind && t.effects == effects).count() + new_tracks.iter().filter(|(_, k, e)| *k == kind && *e == effects).count();
                let letter = if kind == oa_doc::TrackKind::Video { "V" } else { "A" };
                let name = if effects { format!("{letter}FX{}", count + 1) } else { format!("{letter}{}", count + 1) };
                let index = match kind {
                    oa_doc::TrackKind::Video => s.tracks.iter().rposition(|t| t.kind == kind).map_or(0, |i| i + 1) + new_tracks.len(),
                    oa_doc::TrackKind::Audio => s.tracks.len() + new_tracks.len(),
                };
                let track = if effects { Track::effects(id, &name, kind) } else { Track::new(id, &name, kind) };
                ops.push(Op::InsertTrack { seq, index, track: std::sync::Arc::new(track) });
                new_tracks.push((id, kind, effects));
                id
            });
        placed.entry(track).or_default().push(range);
        let mut copy = item.clone();
        copy.id = ItemId(alloc());
        copy.range = range;
        for fx in &mut copy.effects {
            fx.id = oa_doc::EffectId(alloc());
        }
        copy.group = item.group.map(|g| *groups.entry(g).or_insert_with(&mut *alloc));
        ids.push(copy.id);
        ops.push(Op::InsertItem { seq, track, item: copy });
    }
    Ok((ops, ids))
}

/// Edit points a dragged edge or clip can snap to.
#[derive(Clone, Debug, Default)]
pub struct Snapper {
    points: Vec<Time>,
}

impl Snapper {
    /// Every clip edge in the sequence (except `ignore`'s), zero, and `extra` points
    /// such as the playhead and markers.
    pub fn new(s: &Sequence, ignore: Option<ItemId>, extra: &[Time]) -> Self {
        Self::ignoring(s, ignore.as_slice(), extra)
    }

    /// Like [`Snapper::new`], ignoring every clip in `ignore` (a multi-clip move).
    pub fn ignoring(s: &Sequence, ignore: &[ItemId], extra: &[Time]) -> Self {
        let mut points: Vec<Time> = s
            .tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|i| !ignore.contains(&i.id))
            .flat_map(|i| [i.range.start, i.range.end()])
            .chain([Time::ZERO])
            .chain(extra.iter().copied())
            .collect();
        points.sort();
        points.dedup();
        Snapper { points }
    }

    /// The closest snap point within `tolerance` of `t`.
    pub fn snap(&self, t: Time, tolerance: Time) -> Option<Time> {
        let i = self.points.partition_point(|p| *p < t);
        [i.checked_sub(1), Some(i)]
            .into_iter()
            .flatten()
            .filter_map(|i| self.points.get(i).copied())
            .filter(|p| (*p - t).0.abs() <= tolerance.0)
            .min_by_key(|p| (*p - t).0.abs())
    }

    /// Snaps a clip being moved: whichever of its two edges is closer to a point wins.
    /// Returns the adjusted start and the point it snapped to.
    pub fn snap_range(&self, start: Time, duration: Time, tolerance: Time) -> (Time, Option<Time>) {
        let head = self.snap(start, tolerance).map(|p| (p - start, p));
        let tail = self.snap(start + duration, tolerance).map(|p| (p - (start + duration), p));
        match [head, tail].into_iter().flatten().min_by_key(|(d, _)| d.0.abs()) {
            Some((d, p)) => (start + d, Some(p)),
            None => (start, None),
        }
    }
}
