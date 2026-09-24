//! Breaking a compound clip apart: the clips inside it come back out onto the timeline,
//! each where it plays now, and the compound clip goes.
//!
//! It's nesting run backwards. Each clip inside is shifted to where the compound clip
//! puts it and trimmed to the stretch the compound clip shows (the rest of the inside
//! was never seen). Its picture tracks keep their stacking: the lowest goes on the
//! compound clip's own track (free once it's gone), each one above on the next track up
//! that has room, or a new one there; sound tracks on the first sound track with room,
//! or a new one. The compound itself stays in the bin (other clips may play it).
//!
//! What was done to the compound clip as a whole — its transform, effects and
//! transitions — worked on the finished picture of everything inside, so it can't be
//! handed on to the pieces; [`Broken::dropped_own_changes`] says when some was lost.

use oa_doc::{EditError, Item, ItemId, ItemKind, Op, ParamTarget, Project, SeqId, Track, TrackId, TrackKind};
use oa_time::{Time, TimeRange};
use std::sync::Arc;

type R<T> = Result<T, EditError>;

/// What breaking a compound clip apart does.
#[derive(Debug)]
pub struct Broken {
    pub ops: Vec<Op>,
    /// The clips it put on the timeline (to select them).
    pub clips: Vec<ItemId>,
    /// The compound clip had its own transform, effects or transitions, which the pieces
    /// don't get.
    pub dropped_own_changes: bool,
}

/// Ops breaking compound clip `item` apart. `alloc` hands out fresh ids.
pub fn break_apart(p: &Project, seq: SeqId, item: ItemId, mut alloc: impl FnMut() -> u64) -> R<Broken> {
    let s = p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))?;
    let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
    let outer = &s.tracks[ti].items[ii];
    let ItemKind::Nested { sequence } = outer.kind else { return Err(EditError::WrongTrackKind) };
    // Played at another speed (or frozen), its insides would need retiming too.
    if outer.time_map.speed.num() != outer.time_map.speed.den() {
        return Err(EditError::InvalidRange);
    }
    let inner = p.sequence(sequence).ok_or(EditError::NotFound("sequence", sequence.0))?;
    let shown = outer.range;
    let shift = shown.start - outer.time_map.source_in;
    let own_track = s.tracks[ti].id;

    // Every inner clip, where it lands and trimmed to what's shown, track by track.
    let mut placed: Vec<(TrackKind, String, Vec<Item>)> = Vec::new();
    let mut clips = Vec::new();
    let active = inner.active();
    let mut overrides = Vec::new();
    for track in inner.tracks.iter().filter(|t| t.enabled) {
        let mut out = Vec::new();
        for it in &track.items {
            let (start, end) = (it.range.start + shift, it.range.end() + shift);
            if end <= shown.start || start >= shown.end() {
                continue; // outside what the compound clip shows
            }
            let mut piece = it.clone();
            piece.range = TimeRange::new(start, end - start);
            if start < shown.start {
                let Some((range, map)) = piece.trimmed_head(shown.start) else { continue };
                piece.range = range;
                piece.time_map = map;
            }
            if piece.range.end() > shown.end() {
                piece.range.duration = shown.end() - piece.range.start;
            }
            if piece.range.duration <= Time::ZERO {
                continue;
            }
            piece.id = ItemId(alloc());
            piece.group = None;
            if let Some(o) = active.overrides.get(&it.id) {
                overrides.push((piece.id, o.clone()));
            }
            clips.push(piece.id);
            out.push(piece);
        }
        if !out.is_empty() {
            placed.push((track.kind, track.name.clone(), out));
        }
    }

    let mut ops = vec![Op::RemoveItem { seq, item }];
    // Room on a track over the stretch shown, the compound clip itself not counting.
    let free = |t: &Track| !t.items.iter().any(|i| i.id != item && i.range.overlaps(shown));
    let mut used: Vec<TrackId> = Vec::new();
    // Where the last picture track went (tracks later in the list draw on top).
    let mut above = ti;
    let mut inserted = 0usize;
    let mut new_tracks = 0usize;
    for (kind, name, items) in placed {
        let track = match kind {
            TrackKind::Video if used.is_empty() && s.tracks[ti].kind == TrackKind::Video => {
                above = ti;
                own_track
            }
            TrackKind::Video => {
                // Once a new track has gone in, the ones after it follow it (the timeline's own
                // tracks have moved up one).
                let found = if inserted > 0 { None } else { s.tracks.iter().enumerate().skip(above + 1).find(|(_, t)| t.kind == TrackKind::Video && !t.effects && !used.contains(&t.id) && free(t)).map(|(i, t)| (i, t.id)) };
                match found {
                    Some((i, id)) => {
                        above = i;
                        id
                    }
                    None => {
                        let id = TrackId(alloc());
                        new_tracks += 1;
                        let index = above + 1 + inserted;
                        inserted += 1;
                        ops.push(Op::InsertTrack { seq, index, track: Arc::new(Track::new(id, &name, TrackKind::Video)) });
                        id
                    }
                }
            }
            TrackKind::Audio => match s.tracks.iter().find(|t| t.kind == TrackKind::Audio && !t.effects && !used.contains(&t.id) && free(t)) {
                Some(t) => t.id,
                None => {
                    let id = TrackId(alloc());
                    new_tracks += 1;
                    ops.push(Op::InsertTrack { seq, index: s.tracks.len() + new_tracks - 1, track: Arc::new(Track::new(id, &name, TrackKind::Audio)) });
                    id
                }
            },
        };
        used.push(track);
        ops.extend(items.into_iter().map(|item| Op::InsertItem { seq, track, item }));
    }
    // Each piece's settings for the format it was made in, on the timeline's own format.
    let format = s.active().id;
    for (piece, set) in overrides {
        for (param, source) in set.0 {
            ops.push(Op::SetParam { seq, item: piece, target: ParamTarget::VariantOverride(format), param, source: Some(source) });
        }
    }
    let dropped_own_changes = !outer.effects.is_empty() || !outer.params.0.is_empty() || outer.transition_in.is_some() || outer.transition_out.is_some();
    Ok(Broken { ops, clips, dropped_own_changes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::*;
    use oa_time::FrameRate;

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    /// A timeline with a compound clip on V1 at 10 s showing 2–8 s of its inside, which
    /// is a picture clip 0–5 s and one 4–10 s on two tracks, and sound 0–10 s.
    fn project() -> (Document, SeqId, ItemId, TrackId) {
        let mut doc = Document::new(Project::new("t"));
        let size = CanvasSize { width: 1920, height: 1080 };
        let variant = |doc: &mut Document| FormatVariant { id: VariantId(doc.alloc_id()), name: "Wide".into(), size, overrides: Default::default() };
        let (outer, inner) = (SeqId(doc.alloc_id()), SeqId(doc.alloc_id()));
        let (v1, iv1, iv2, ia1) = (TrackId(doc.alloc_id()), TrackId(doc.alloc_id()), TrackId(doc.alloc_id()), TrackId(doc.alloc_id()));
        let (outer_variant, inner_variant) = (variant(&mut doc), variant(&mut doc));
        let clip = |doc: &mut Document, name: &str, media: u64, from: i64, len: i64| Item::new(ItemId(doc.alloc_id()), name, ItemKind::Media { media: MediaId(media) }, TimeRange::new(secs(from), secs(len)));
        let (a, b, sound) = (clip(&mut doc, "a", 1, 0, 5), clip(&mut doc, "b", 2, 4, 6), clip(&mut doc, "sound", 3, 0, 10));
        let mut compound = Item::new(ItemId(doc.alloc_id()), "Compound", ItemKind::Nested { sequence: inner }, TimeRange::new(secs(10), secs(6)));
        compound.time_map.source_in = secs(2);
        let id = compound.id;
        doc.edit(
            "setup",
            vec![
                Op::AddSequence(Arc::new(Sequence::new(outer, "Main", FrameRate::FPS_30, outer_variant))),
                Op::AddSequence(Arc::new(Sequence::new(inner, "Inner", FrameRate::FPS_30, inner_variant))),
                Op::InsertTrack { seq: outer, index: 0, track: Arc::new(Track::new(v1, "V1", TrackKind::Video)) },
                Op::InsertTrack { seq: inner, index: 0, track: Arc::new(Track::new(iv1, "V1", TrackKind::Video)) },
                Op::InsertTrack { seq: inner, index: 1, track: Arc::new(Track::new(iv2, "V2", TrackKind::Video)) },
                Op::InsertTrack { seq: inner, index: 2, track: Arc::new(Track::new(ia1, "A1", TrackKind::Audio)) },
                Op::InsertItem { seq: inner, track: iv1, item: a },
                Op::InsertItem { seq: inner, track: iv2, item: b },
                Op::InsertItem { seq: inner, track: ia1, item: sound },
                Op::InsertItem { seq: outer, track: v1, item: compound },
            ],
        )
        .unwrap();
        (doc, outer, id, v1)
    }

    #[test]
    fn the_pieces_land_where_they_played_trimmed_to_what_was_shown() {
        let (mut doc, seq, item, v1) = project();
        let mut next = 10_000;
        let broken = break_apart(doc.project(), seq, item, || {
            next += 1;
            next
        })
        .unwrap();
        assert!(!broken.dropped_own_changes);
        doc.edit("break", broken.ops).unwrap();
        let s = doc.project().sequence(seq).unwrap();
        assert!(s.item(item).is_none(), "the compound clip is gone");
        let find = |name: &str| s.tracks.iter().find_map(|t| t.items.iter().find(|i| i.name == name).map(|i| (t.id, t.kind, i.clone()))).unwrap();
        // Inside, 2–8 s was shown at 10 s: a (0–5) shows 2–5 → 10–13, from 2 s into it.
        let (track, _, a) = find("a");
        assert_eq!((track, a.range, a.time_map.source_in), (v1, TimeRange::new(secs(10), secs(3)), secs(2)), "on the compound's own track");
        // b (4–10) shows 4–8 → 12–16.
        let (track, kind, b) = find("b");
        assert_eq!((kind, b.range), (TrackKind::Video, TimeRange::new(secs(12), secs(4))));
        let (index_a, index_b) = (s.tracks.iter().position(|t| t.id == v1).unwrap(), s.tracks.iter().position(|t| t.id == track).unwrap());
        assert!(index_b > index_a, "the upper picture track stays on top");
        let (_, kind, sound) = find("sound");
        assert_eq!((kind, sound.range, sound.time_map.source_in), (TrackKind::Audio, TimeRange::new(secs(10), secs(6)), secs(2)));
        assert_eq!(broken.clips.len(), 3);
        // The compound is still in the bin.
        assert_eq!(doc.project().sequences.len(), 2);
    }

    #[test]
    fn only_compound_clips_at_normal_speed_break_apart() {
        let (doc, seq, item, _) = project();
        let s = doc.project().sequence(seq).unwrap();
        let mut slow = (*doc.project()).clone();
        let seq_mut = Arc::make_mut(slow.sequences.get_mut(&seq).unwrap());
        let (t, i) = seq_mut.find_item(item).unwrap();
        Arc::make_mut(&mut seq_mut.tracks[t]).items[i].time_map.speed = oa_time::Rational::new(1, 2);
        assert!(matches!(break_apart(&slow, seq, item, || 1), Err(EditError::InvalidRange)));
        let not_compound = s.tracks.iter().flat_map(|t| t.items.iter()).find(|i| i.id != item);
        assert!(not_compound.is_none() || matches!(break_apart(doc.project(), seq, not_compound.unwrap().id, || 1), Err(EditError::WrongTrackKind)));
    }
}
