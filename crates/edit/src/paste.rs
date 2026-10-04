//! Placing clips into a track's free space (pasting or duplicating into a picked track).
//!
//! Like every command here, these read a snapshot and return ops.

use oa_doc::{EditError, Item, ItemId, Op, Project, SeqId, Track, TrackId};
use oa_time::{Time, TimeRange};

type R<T> = Result<T, EditError>;

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
    let s = p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))?;
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
    use oa_doc::{Document, FormatVariant, ItemKind, Sequence, TrackKind, VariantId};
    use std::sync::Arc;

    const SEQ: SeqId = SeqId(1);
    const V1: TrackId = TrackId(2);

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    /// V1: solids at 0–2, 3–4 and 6–8 s.
    fn doc() -> Document {
        let mut p = Project::new("t");
        p.next_id = 100;
        let v = FormatVariant { id: VariantId(3), name: "w".into(), size: oa_doc::CanvasSize::new(16, 9), overrides: Default::default() };
        let mut s = Sequence::new(SEQ, "Main", oa_time::FrameRate::FPS_30, v);
        let mut t = Track::new(V1, "V1", TrackKind::Video);
        for (id, a, b) in [(10, 0, 2), (11, 3, 4), (12, 6, 8)] {
            t.items.push(Item::new(ItemId(id), "clip", ItemKind::Solid, TimeRange::new(secs(a), secs(b - a))));
        }
        s.tracks.push(Arc::new(t));
        p.sequences.insert(SEQ, Arc::new(s));
        Document::new(p)
    }

    /// (id, start in tenths of a second) on one track.
    fn starts(doc: &Document, track: TrackId) -> Vec<(u64, i64)> {
        let t = doc.project().sequence(SEQ).unwrap().track(track).unwrap();
        t.items.iter().map(|i| (i.id.0, (i.range.start.as_seconds_f64() * 10.0).round() as i64)).collect()
    }

    #[test]
    fn pastes_into_the_closest_gap_after_the_playhead() {
        let mut d = doc();
        let t = d.project().sequence(SEQ).unwrap().track(V1).unwrap().clone();
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
