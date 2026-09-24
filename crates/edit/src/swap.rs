//! Swapping a clip's media: a file (or compound clip) dropped onto a clip on the timeline
//! takes that clip's place and keeps everything done to it — its spot and length on the
//! timeline, its speed, transform, crop, effects, transitions and keyframes. Only what
//! it shows (and its name) changes.

use oa_doc::{EditError, ItemId, ItemKind, Op, Project, SeqId};
use oa_time::{Time, TimeRange};

type R<T> = Result<T, EditError>;

/// Whether the clip `item` shows media that can be swapped (a file or a compound clip,
/// not a title or a solid).
pub fn can_swap(p: &Project, seq: SeqId, item: ItemId) -> bool {
    p.sequence(seq).and_then(|s| s.item(item)).is_some_and(|it| matches!(it.kind, ItemKind::Media { .. } | ItemKind::Nested { .. }))
}

/// Ops putting `kind` (named `name`) in place of what clip `item` shows. `length` is how
/// much the new media has to play (`None`: as much as asked, a still picture).
///
/// The clip keeps its start on the timeline and, when the new media is long enough, its
/// length and the point in the media it starts from. When that point is past what the
/// new media has, it starts earlier so the clip still ends inside it; and when the new
/// media is shorter than the whole clip, the clip is cut down to it. One undo step.
pub fn swap_media(p: &Project, seq: SeqId, item: ItemId, kind: ItemKind, name: &str, length: Option<Time>) -> R<Vec<Op>> {
    let s = p.sequence(seq).ok_or(EditError::NotFound("sequence", seq.0))?;
    let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
    let (track, old) = (&s.tracks[ti], &s.tracks[ti].items[ii]);
    if !matches!(old.kind, ItemKind::Media { .. } | ItemKind::Nested { .. }) {
        return Err(EditError::WrongTrackKind);
    }
    if old.kind == kind {
        return Ok(Vec::new());
    }
    let mut new = old.clone();
    new.kind = kind;
    new.name = name.to_string();
    if let Some(length) = length {
        // How much of the media the clip plays, at its speed.
        let speed = old.time_map.speed.num() as f64 / old.time_map.speed.den().max(1) as f64;
        let span = old.time_map.source_time(old.range.duration) - old.time_map.source_in;
        if speed > 0.0 {
            if span <= length {
                // It fits: start where it did, or early enough to end inside the media.
                new.time_map.source_in = old.time_map.source_in.min(length - span);
            } else {
                // Longer than the whole media: play all of it, and end there.
                new.time_map.source_in = Time::ZERO;
                let duration = Time::from_seconds_f64(length.as_seconds_f64() / speed).max(crate::timeline::min_duration(s));
                new.range = TimeRange::new(old.range.start, duration.min(old.range.duration));
            }
        } else {
            // A freeze frame: hold a frame that exists.
            new.time_map.source_in = old.time_map.source_in.min(length);
        }
    }
    Ok(vec![Op::RemoveItem { seq, item }, Op::InsertItem { seq, track: track.id, item: new }])
}


#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::*;
    use oa_params::{ParamSource, Value};
    use oa_time::FrameRate;
    use std::sync::Arc;

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    /// A 10 s clip of media 1 at 2 s on the timeline, from 4 s into the file, moved and
    /// blurred.
    fn project() -> (Project, SeqId, ItemId) {
        let mut doc = Document::new(Project::new("t"));
        let (seq, track, item) = (SeqId(doc.alloc_id()), TrackId(doc.alloc_id()), ItemId(doc.alloc_id()));
        let size = CanvasSize { width: 1920, height: 1080 };
        let variant = FormatVariant { id: VariantId(doc.alloc_id()), name: "Wide".into(), size, overrides: Default::default() };
        let mut clip = Item::new(item, "Old", ItemKind::Media { media: MediaId(1) }, TimeRange::new(secs(2), secs(10)));
        clip.time_map.source_in = secs(4);
        clip.params.set(schema::POSITION, ParamSource::Static(Value::Vec2([0.2, -0.1])));
        clip.effects.push(EffectInstance::new(EffectId(doc.alloc_id()), "oa.blur.gaussian"));
        doc.edit(
            "setup",
            vec![
                Op::AddSequence(Arc::new(Sequence::new(seq, "Main", FrameRate::FPS_30, variant))),
                Op::InsertTrack { seq, index: 0, track: Arc::new(Track::new(track, "V1", TrackKind::Video)) },
                Op::InsertItem { seq, track, item: clip },
            ],
        )
        .unwrap();
        ((*doc.snapshot()).clone(), seq, item)
    }

    fn swapped(length: Option<Time>) -> Item {
        let (p, seq, item) = project();
        let ops = swap_media(&p, seq, item, ItemKind::Media { media: MediaId(2) }, "New", length).unwrap();
        let mut doc = Document::new(p);
        doc.edit("swap", ops).unwrap();
        doc.project().sequence(seq).unwrap().item(item).unwrap().clone()
    }

    #[test]
    fn the_media_changes_and_everything_done_to_the_clip_stays() {
        let it = swapped(Some(secs(60)));
        assert_eq!(it.kind, ItemKind::Media { media: MediaId(2) });
        assert_eq!(it.name, "New");
        assert_eq!(it.range, TimeRange::new(secs(2), secs(10)), "same place and length");
        assert_eq!(it.time_map.source_in, secs(4), "from the same point in the media");
        assert!(it.params.get(schema::POSITION).is_some(), "its transform");
        assert_eq!(it.effects.len(), 1, "its effects");
    }

    #[test]
    fn shorter_media_starts_earlier_or_cuts_the_clip_down() {
        // 12 s of media: the clip's 10 s start at 2 s in, so it ends at the media's end.
        let it = swapped(Some(secs(12)));
        assert_eq!((it.time_map.source_in, it.range.duration), (secs(2), secs(10)));
        // 6 s of media: all of it plays, and the clip is 6 s long.
        let it = swapped(Some(secs(6)));
        assert_eq!((it.time_map.source_in, it.range), (Time::ZERO, TimeRange::new(secs(2), secs(6))));
        // A still: as long as the clip likes.
        let it = swapped(None);
        assert_eq!((it.time_map.source_in, it.range.duration), (secs(4), secs(10)));
    }

    #[test]
    fn swapping_in_the_same_media_does_nothing() {
        let (p, seq, item) = project();
        assert!(can_swap(&p, seq, item));
        assert_eq!(swap_media(&p, seq, item, ItemKind::Media { media: MediaId(1) }, "Old", None).unwrap(), Vec::<Op>::new(), "the same media: nothing to do");
    }
}
