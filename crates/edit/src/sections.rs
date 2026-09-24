//! Timeline sections, and placing clips into a track's free space.
//!
//! The timeline's dividers ([`Divider`], `Sequence::dividers`) run across every track and
//! cut it into **sections**: from the start to the first divider, then from each divider
//! to the next, the last running to the end of the timeline. A clip belongs to the
//! section its start falls in. Sections can be emptied, deleted (everything after closes
//! up) or reordered (their clips on every track, and their dividers, travel together),
//! and a divider placed across clips splits them, so sections hold whole clips.
//!
//! Like every command here, these read a snapshot and return ops.

use oa_doc::{Divider, EditError, Item, ItemId, Op, Project, SeqId, Sequence, Track, TrackId};
use oa_time::{Time, TimeRange};

type R<T> = Result<T, EditError>;

/// The color a divider gets unless one is picked.
pub const DEFAULT_COLOR: [u8; 3] = [236, 170, 64];

/// Colors offered for dividers.
pub const COLORS: [[u8; 3]; 8] =
    [[236, 170, 64], [230, 92, 92], [236, 120, 190], [150, 110, 230], [80, 140, 240], [70, 190, 200], [90, 190, 110], [170, 170, 170]];

/// One stretch of the timeline between dividers.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub start: Time,
    /// Where the next section starts (the last one: where the timeline ends).
    pub end: Time,
    /// The divider it starts at (`None` for the stretch before the first divider).
    pub divider: Option<Divider>,
}

impl Section {
    pub fn length(&self) -> Time {
        self.end - self.start
    }
}

fn sequence(p: &Project, seq: SeqId) -> R<&Sequence> {
    p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))
}

/// The timeline's sections, in time order. Without dividers it's one section.
pub fn sections(s: &Sequence) -> Vec<Section> {
    let mut dividers = s.dividers.clone();
    dividers.sort_by_key(|d| d.at);
    let end = s.duration().max(dividers.last().map_or(Time::ZERO, |d| d.at));
    let mut out = Vec::new();
    if dividers.first().is_none_or(|d| d.at > Time::ZERO) {
        out.push(Section { start: Time::ZERO, end: dividers.first().map_or(end, |d| d.at), divider: None });
    }
    for (i, d) in dividers.iter().enumerate() {
        let next = dividers.get(i + 1).map_or(end, |n| n.at);
        out.push(Section { start: d.at, end: next.max(d.at), divider: Some(d.clone()) });
    }
    out
}

/// The section `t` falls in (past the end: the last one).
pub fn section_at(s: &Sequence, t: Time) -> usize {
    sections(s).iter().rposition(|x| x.start <= t).unwrap_or(0)
}

/// The clips of section `index` on every track (by where they start), with their tracks.
pub fn members(s: &Sequence, index: usize) -> Vec<(TrackId, &Item)> {
    let all = sections(s);
    let Some(sec) = all.get(index) else { return Vec::new() };
    let last = index + 1 == all.len();
    s.tracks
        .iter()
        .flat_map(|t| t.items.iter().map(move |i| (t.id, i)))
        .filter(|(_, i)| i.range.start >= sec.start && (last || i.range.start < sec.end))
        .collect()
}

/// A divider at `at` across the timeline (clips it lands inside are split there, so the
/// sections part cleanly). Nothing if there's one there already.
pub fn add_divider(p: &Project, seq: SeqId, at: Time, color: [u8; 3], alloc: &mut dyn FnMut() -> u64) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    if at < Time::ZERO || s.dividers.iter().any(|d| d.at == at) {
        return Ok(Vec::new());
    }
    let mut ops = Vec::new();
    for t in &s.tracks {
        if let Some(inside) = t.items.iter().find(|i| i.range.start < at && at < i.range.end()) {
            ops.extend(crate::timeline::split(p, seq, inside.id, at, alloc)?.0);
        }
    }
    let mut dividers = s.dividers.clone();
    dividers.push(Divider { id: alloc(), at, color, name: String::new() });
    ops.push(Op::SetDividers { seq, dividers });
    Ok(ops)
}

/// The dividers with one changed (moved, recolored, renamed) or, with `None`, removed.
pub fn edit_divider(p: &Project, seq: SeqId, id: u64, change: Option<Divider>) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let mut dividers: Vec<Divider> = s.dividers.iter().filter(|d| d.id != id).cloned().collect();
    dividers.extend(change);
    Ok(vec![Op::SetDividers { seq, dividers }])
}

/// Removes the clips of section `index`, leaving the space.
pub fn clear_section(p: &Project, seq: SeqId, index: usize) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    Ok(members(s, index).into_iter().map(|(_, i)| Op::RemoveItem { seq, item: i.id }).collect())
}

/// Removes section `index` entirely: its clips go, and everything after it (clips on
/// every track, and dividers) moves back to close the gap. Its divider goes with it.
pub fn delete_section(p: &Project, seq: SeqId, index: usize) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let all = sections(s);
    let sec = all.get(index).ok_or(EditError::NotFound("section", index as u64))?;
    let gone: Vec<ItemId> = members(s, index).iter().map(|(_, i)| i.id).collect();
    let len = sec.length();
    let mut ops: Vec<Op> = gone.iter().map(|&item| Op::RemoveItem { seq, item }).collect();
    // Later clips close up, earliest first on each track (each moves into space already
    // vacated).
    if index + 1 < all.len() {
        for t in &s.tracks {
            for i in t.items.iter().filter(|i| !gone.contains(&i.id) && i.range.start >= sec.end) {
                ops.push(Op::SetItemTiming { seq, item: i.id, range: TimeRange::new(i.range.start - len, i.range.duration), time_map: i.time_map });
            }
        }
    }
    let dividers = s
        .dividers
        .iter()
        .filter(|d| Some(d.id) != sec.divider.as_ref().map(|x| x.id))
        .map(|d| if d.at >= sec.end { Divider { at: d.at - len, ..d.clone() } } else { d.clone() })
        .collect();
    ops.push(Op::SetDividers { seq, dividers });
    Ok(ops)
}

/// Moves section `from` to position `to` among the sections: the sections are laid end
/// to end in their new order from the start, each keeping its length, clips (on every
/// track) and divider (the first stretch, which has none, gets one if it moves off the
/// start).
pub fn move_section(p: &Project, seq: SeqId, from: usize, to: usize, alloc: &mut dyn FnMut() -> u64) -> R<Vec<Op>> {
    let s = sequence(p, seq)?;
    let all = sections(s);
    if from >= all.len() || to >= all.len() || from == to {
        return Ok(Vec::new());
    }
    let mut order: Vec<usize> = (0..all.len()).collect();
    let moved = order.remove(from);
    order.insert(to, moved);
    let mut start = all[0].start;
    let mut shift = vec![Time::ZERO; all.len()];
    let mut dividers = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        let sec = &all[i];
        shift[i] = start - sec.start;
        match &sec.divider {
            Some(d) => dividers.push(Divider { at: start, ..d.clone() }),
            None if k > 0 => dividers.push(Divider { id: alloc(), at: start, color: DEFAULT_COLOR, name: String::new() }),
            None => {}
        }
        start += sec.length();
    }
    // Every moved clip comes out first and goes back in its new place, so no clip is
    // ever in another's way halfway through.
    let mut moves = Vec::new();
    for (i, delta) in shift.iter().enumerate() {
        if *delta == Time::ZERO {
            continue;
        }
        for (track, item) in members(s, i) {
            let mut copy = item.clone();
            copy.range = TimeRange::new(item.range.start + *delta, item.range.duration);
            moves.push((track, copy));
        }
    }
    let mut ops: Vec<Op> = moves.iter().map(|(_, i)| Op::RemoveItem { seq, item: i.id }).collect();
    ops.extend(moves.into_iter().map(|(track, item)| Op::InsertItem { seq, track, item }));
    ops.push(Op::SetDividers { seq, dividers });
    Ok(ops)
}

/// The earliest start at or after `after` where clips spanning `spans` (offsets from
/// their first start, and lengths) all fit on `track` without touching its clips.
pub fn free_spot(track: &Track, spans: &[(Time, Time)], after: Time) -> Time {
    let fits = |at: Time| {
        spans.iter().all(|(offset, len)| {
            let r = TimeRange::new(at + *offset, *len);
            !track.items.iter().any(|i| i.range.overlaps(r))
        })
    };
    let mut candidates: Vec<Time> = std::iter::once(after).chain(track.items.iter().map(|i| i.range.end()).filter(|e| *e >= after)).collect();
    candidates.sort();
    candidates.into_iter().find(|at| fits(*at)).unwrap_or_else(|| track.end().max(after))
}

/// Copies of `clips` onto `track_id`, in the closest free space at or after `after`
/// (the playhead): they keep their spacing, or — if that would stack them on each other
/// (clips from several tracks) — go one after another. Clips of the other kind (sound
/// onto a picture track) are left out. Returns the ops and the new clips' ids.
pub fn paste_into_track(p: &Project, seq: SeqId, track_id: TrackId, clips: &[(TrackId, Item)], after: Time, alloc: &mut dyn FnMut() -> u64) -> R<(Vec<Op>, Vec<ItemId>)> {
    let s = sequence(p, seq)?;
    let t = s.track(track_id).ok_or(EditError::NotFound("track", track_id.0))?;
    let mut chosen: Vec<&Item> = clips.iter().filter(|(from, _)| s.track(*from).is_none_or(|f| f.kind == t.kind)).map(|(_, i)| i).collect();
    chosen.sort_by_key(|i| i.range.start);
    let Some(first) = chosen.first().map(|i| i.range.start) else { return Ok((Vec::new(), Vec::new())) };
    let mut spans: Vec<(Time, Time)> = chosen.iter().map(|i| (i.range.start - first, i.range.duration)).collect();
    let stacked = spans.windows(2).any(|w| w[1].0 < w[0].0 + w[0].1);
    if stacked {
        let mut at = Time::ZERO;
        for span in &mut spans {
            span.0 = at;
            at += span.1;
        }
    }
    let at = free_spot(t, &spans, after);
    let mut ops = Vec::new();
    let mut ids = Vec::new();
    let mut groups = std::collections::HashMap::new();
    for (item, (offset, len)) in chosen.into_iter().zip(spans) {
        let mut copy = item.clone();
        copy.id = ItemId(alloc());
        copy.range = TimeRange::new(at + offset, len);
        for fx in &mut copy.effects {
            fx.id = oa_doc::EffectId(alloc());
        }
        copy.group = item.group.map(|g| *groups.entry(g).or_insert_with(&mut *alloc));
        ids.push(copy.id);
        ops.push(Op::InsertItem { seq, track: track_id, item: copy });
    }
    Ok((ops, ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::{Document, FormatVariant, ItemKind, TrackKind, VariantId};
    use std::sync::Arc;

    const SEQ: SeqId = SeqId(1);
    const V1: TrackId = TrackId(2);
    const V2: TrackId = TrackId(4);

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    /// V1: solids at 0–2, 3–4 and 6–8 s. V2: one at 1–5 s.
    fn doc() -> Document {
        let mut p = Project::new("t");
        p.next_id = 100;
        let v = FormatVariant { id: VariantId(3), name: "w".into(), size: oa_doc::CanvasSize::new(16, 9), overrides: Default::default() };
        let mut s = Sequence::new(SEQ, "Main", oa_time::FrameRate::FPS_30, v);
        let mut t = Track::new(V1, "V1", TrackKind::Video);
        for (id, a, b) in [(10, 0, 2), (11, 3, 4), (12, 6, 8)] {
            t.items.push(Item::new(ItemId(id), "clip", ItemKind::Solid, TimeRange::new(secs(a), secs(b - a))));
        }
        let mut t2 = Track::new(V2, "V2", TrackKind::Video);
        t2.items.push(Item::new(ItemId(20), "over", ItemKind::Solid, TimeRange::new(secs(1), secs(4))));
        s.tracks.push(Arc::new(t));
        s.tracks.push(Arc::new(t2));
        p.sequences.insert(SEQ, Arc::new(s));
        Document::new(p)
    }

    fn edit(doc: &mut Document, f: impl FnOnce(&Project, &mut dyn FnMut() -> u64) -> R<Vec<Op>>) {
        let p = doc.snapshot();
        let ops = f(&p, &mut || doc.alloc_id()).expect("ops");
        doc.edit("x", ops).expect("applies");
    }

    /// (id, start in tenths of a second) on one track.
    fn starts(doc: &Document, track: TrackId) -> Vec<(u64, i64)> {
        let t = doc.project().sequence(SEQ).unwrap().track(track).unwrap();
        t.items.iter().map(|i| (i.id.0, (i.range.start.as_seconds_f64() * 10.0).round() as i64)).collect()
    }

    fn seq(doc: &Document) -> Sequence {
        doc.project().sequence(SEQ).unwrap().clone()
    }

    #[test]
    fn dividers_cut_every_track_into_sections() {
        let mut d = doc();
        edit(&mut d, |p, a| add_divider(p, SEQ, secs(3), DEFAULT_COLOR, a));
        edit(&mut d, |p, a| add_divider(p, SEQ, secs(7), [1, 2, 3], a));
        let s = seq(&d);
        let all = sections(&s);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].start, all[0].end, all[0].divider.is_none()), (secs(0), secs(3), true));
        assert_eq!((all[2].start, all[2].end, all[2].divider.as_ref().unwrap().color), (secs(7), secs(8), [1, 2, 3]));
        assert_eq!(s.track(V1).unwrap().items.len(), 4, "the 6–8 clip was split at 7");
        assert_eq!(s.track(V2).unwrap().items.len(), 2, "so was the 1–5 clip on V2, at 3");
        assert_eq!(members(&s, 1).len(), 3, "3–4 and 6–7 on V1, 3–5 on V2");
        assert_eq!(section_at(&s, secs(100)), 2);
    }

    #[test]
    fn deleting_a_section_closes_the_gap_on_every_track() {
        let mut d = doc();
        edit(&mut d, |p, a| add_divider(p, SEQ, secs(3), DEFAULT_COLOR, a));
        edit(&mut d, |p, a| add_divider(p, SEQ, secs(5), DEFAULT_COLOR, a));
        edit(&mut d, |p, _| delete_section(p, SEQ, 1));
        assert_eq!(starts(&d, V1), vec![(10, 0), (12, 40)], "3–5 is gone; 6–8 moved back 2 s");
        assert_eq!(starts(&d, V2), vec![(20, 10)], "V2's 3–5 part went too");
        assert_eq!(seq(&d).dividers.iter().map(|x| x.at).collect::<Vec<_>>(), vec![secs(3)], "the next divider moved back too");
        edit(&mut d, |p, _| clear_section(p, SEQ, 1));
        assert_eq!(starts(&d, V1), vec![(10, 0)]);
    }

    #[test]
    fn moving_a_section_carries_its_clips_and_divider() {
        let mut d = doc();
        edit(&mut d, |p, a| add_divider(p, SEQ, secs(3), [9, 9, 9], a));
        // [0,3): 10 and V2's 1–3 · [3,8): 11, 12 and V2's 3–5. Second to the front.
        let before = (starts(&d, V1), starts(&d, V2));
        edit(&mut d, |p, a| move_section(p, SEQ, 1, 0, a));
        assert_eq!(starts(&d, V1), vec![(11, 0), (12, 30), (10, 50)]);
        let v2 = starts(&d, V2);
        assert_eq!(v2.iter().map(|x| x.1).collect::<Vec<_>>(), vec![0, 60], "V2 moved with its sections: {v2:?}");
        let s = seq(&d);
        assert_eq!(s.dividers.len(), 2, "the old first stretch got a divider of its own");
        assert_eq!((s.dividers[0].at, s.dividers[0].color), (secs(0), [9, 9, 9]));
        assert_eq!(s.dividers[1].at, secs(5));
        d.undo().unwrap();
        assert_eq!((starts(&d, V1), starts(&d, V2)), before, "one undo step");
    }

    #[test]
    fn pastes_into_the_closest_gap_after_the_playhead() {
        let mut d = doc();
        let t = seq(&d).track(V1).unwrap().clone();
        assert_eq!(free_spot(&t, &[(Time::ZERO, secs(1))], secs(1)), secs(2));
        assert_eq!(free_spot(&t, &[(Time::ZERO, secs(2))], secs(1)), secs(4));
        assert_eq!(free_spot(&t, &[(Time::ZERO, secs(3))], secs(1)), secs(8));
        let one = (V1, Item::new(ItemId(50), "c", ItemKind::Solid, TimeRange::new(secs(9), secs(1))));
        let p = d.snapshot();
        let mut n = 500;
        let (ops, ids) = paste_into_track(&p, SEQ, V1, &[one], secs(1), &mut || {
            n += 1;
            n
        })
        .unwrap();
        d.edit("paste", ops).unwrap();
        assert!(starts(&d, V1).contains(&(ids[0].0, 20)));
    }
}
