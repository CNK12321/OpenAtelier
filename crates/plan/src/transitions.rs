//! When transitions are active: the timing rules shared by rendering, the timeline UI
//! and (later) audio crossfades.
//!
//! * A clip's **head** transition sits on the cut from the clip that ends exactly where
//!   it starts — centered on the cut, half before and half after — so both clips are
//!   shown past their edit points (using the media beyond them). With nothing directly
//!   before, it runs from transparent over the clip's first `duration`.
//! * A clip's **tail** transition runs to transparent over its last `duration`, only
//!   when nothing follows directly (that cut belongs to the next clip's head).
//! * Durations are clamped so a transition never reaches past either clip's other end.

use oa_doc::{ClipEnd, Item, Track, Transition};
use oa_time::{Time, TimeRange};

/// A transition in effect at some instant.
#[derive(Clone, Debug)]
pub struct Active<'a> {
    /// Outgoing clip, or `None` when transitioning in from nothing.
    pub from: Option<&'a Item>,
    /// Incoming clip, or `None` when transitioning out to nothing.
    pub to: Option<&'a Item>,
    /// The clip that owns the transition, and which end it's on.
    pub owner: &'a Item,
    pub end: ClipEnd,
    pub transition: &'a Transition,
    /// Where it runs on the timeline.
    pub window: TimeRange,
    /// 0 at the start of the window, 1 at the end.
    pub progress: f64,
}

fn usable(i: &Item) -> bool {
    i.enabled
}

/// The clip ending exactly where `items[i]` starts.
fn adjacent_before(items: &[Item], i: usize) -> Option<&Item> {
    let prev = &items[i.checked_sub(1)?];
    (prev.range.end() == items[i].range.start && usable(prev)).then_some(prev)
}

fn adjacent_after(items: &[Item], i: usize) -> Option<&Item> {
    let next = items.get(i + 1)?;
    (next.range.start == items[i].range.end() && usable(next)).then_some(next)
}

/// The window of the transition on `end` of `track.items[i]`, if it has one that applies.
pub fn window(track: &Track, i: usize, end: ClipEnd) -> Option<TimeRange> {
    let items = &track.items;
    let item = &items[i];
    let tr = item.transition(end)?;
    match end {
        ClipEnd::Head => match adjacent_before(items, i) {
            Some(prev) => {
                let d = tr.duration.min(Time(prev.range.duration.0 * 2)).min(Time(item.range.duration.0 * 2));
                let before = Time(d.0 / 2);
                let after = d - before;
                let before = before.min(prev.range.duration);
                let after = after.min(item.range.duration);
                Some(TimeRange::new(item.range.start - before, before + after))
            }
            None => Some(TimeRange::new(item.range.start, tr.duration.min(item.range.duration))),
        },
        ClipEnd::Tail => {
            if adjacent_after(items, i).is_some() {
                return None;
            }
            let d = tr.duration.min(item.range.duration);
            Some(TimeRange::new(item.range.end() - d, d))
        }
    }
}

/// The transition running on `track` at `t`, if any.
pub fn active(track: &Track, t: Time) -> Option<Active<'_>> {
    let items = &track.items;
    // Candidates: the clip under `t` (its head or tail) and the one right after (a cut
    // transition starts before its clip does).
    let after = items.partition_point(|it| it.range.start <= t);
    let candidates = [after.checked_sub(1).map(|i| (i, ClipEnd::Head)), after.checked_sub(1).map(|i| (i, ClipEnd::Tail)), Some((after, ClipEnd::Head))];
    for (i, end) in candidates.into_iter().flatten() {
        let Some(owner) = items.get(i).filter(|it| usable(it)) else { continue };
        let Some(w) = window(track, i, end) else { continue };
        if !w.contains(t) {
            continue;
        }
        let transition = owner.transition(end).expect("window implies a transition");
        let (from, to) = match end {
            ClipEnd::Head => (adjacent_before(items, i), Some(owner)),
            ClipEnd::Tail => (Some(owner), None),
        };
        let progress = ((t - w.start).0 as f64 / w.duration.0.max(1) as f64).clamp(0.0, 1.0);
        return Some(Active { from, to, owner, end, transition, window: w, progress });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::{ItemId, ItemKind, TrackId, TrackKind};

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    fn track(items: &[(u64, f64, f64)]) -> Track {
        let mut t = Track::new(TrackId(1), "V1", TrackKind::Video);
        for &(id, start, dur) in items {
            t.items.push(Item::new(ItemId(id), "c", ItemKind::Solid, TimeRange::new(secs(start), secs(dur))));
        }
        t
    }

    #[test]
    fn a_cut_transition_is_centered_on_the_cut() {
        let mut t = track(&[(1, 0.0, 4.0), (2, 4.0, 4.0)]);
        t.items[1].transition_in = Some(Transition::new("x", secs(1.0)));
        assert!(active(&t, secs(3.4)).is_none());
        let a = active(&t, secs(3.5)).expect("starts half a second before the cut");
        assert_eq!((a.from.map(|i| i.id), a.to.map(|i| i.id)), (Some(ItemId(1)), Some(ItemId(2))));
        assert_eq!(a.progress, 0.0);
        let mid = active(&t, secs(4.0)).unwrap();
        assert!((mid.progress - 0.5).abs() < 1e-9);
        assert!(active(&t, secs(4.5)).is_none(), "ends half a second after");
    }

    #[test]
    fn fades_in_and_out_from_nothing() {
        let mut t = track(&[(1, 2.0, 4.0)]);
        t.items[0].transition_in = Some(Transition::new("x", secs(1.0)));
        t.items[0].transition_out = Some(Transition::new("x", secs(2.0)));
        let a = active(&t, secs(2.25)).unwrap();
        assert!(a.from.is_none() && a.to.is_some() && (a.progress - 0.25).abs() < 1e-9);
        let b = active(&t, secs(5.0)).unwrap();
        assert!(b.to.is_none() && (b.progress - 0.5).abs() < 1e-9);
        assert!(active(&t, secs(3.5)).is_none());
    }

    #[test]
    fn a_tail_transition_yields_to_the_next_clips_head() {
        let mut t = track(&[(1, 0.0, 4.0), (2, 4.0, 4.0)]);
        t.items[0].transition_out = Some(Transition::new("out", secs(1.0)));
        assert!(active(&t, secs(3.8)).is_none(), "a clip follows directly: no fade out");
        // Move the second clip away: now it fades.
        t.items[1].range.start = secs(5.0);
        assert_eq!(active(&t, secs(3.8)).unwrap().transition.type_id, "out");
    }

    #[test]
    fn durations_clamp_to_the_clips() {
        let mut t = track(&[(1, 0.0, 0.2), (2, 0.2, 4.0)]);
        t.items[1].transition_in = Some(Transition::new("x", secs(2.0)));
        let w = window(&t, 1, ClipEnd::Head).unwrap();
        assert!(w.start >= Time::ZERO, "never reaches before the previous clip's start: {w:?}");
    }
}
