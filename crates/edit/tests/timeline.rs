mod common;

use common::*;
use oa_doc::*;
use oa_doc::ClipEnd;
use oa_edit::timeline::{self, Edge, Snapper};
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamSource, Value};
use oa_time::Time;

fn apply(doc: &mut Document, ops: Vec<Op>) {
    doc.edit("test", ops).expect("ops apply");
}

#[test]
fn trims_clamp_to_neighbors_and_one_frame() {
    // clip 10: 0..5 s showing source 2..7 s; clip 11: 8..10 s showing source 5..7 s.
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 5.0, 2.0), (V1, 11, MEDIA, 8.0, 2.0, 5.0)]);

    // Clip 11's head could reach back 5 s into its media (to 3 s) but clip 10 ends at 5 s.
    let ops = timeline::trim(doc.project(), SEQ, ItemId(11), Edge::Head, secs(0.0)).unwrap();
    apply(&mut doc, ops);
    let it = item(&doc, 11);
    assert_eq!((it.range.start, it.time_map.source_in), (secs(5.0), secs(2.0)));
    assert_eq!(it.range.end(), secs(10.0), "a head trim leaves the tail alone");

    // Clip 10's tail is now flush against clip 11: nothing to do.
    assert!(timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Tail, secs(30.0)).unwrap().is_empty());

    // Dragging a head past the tail leaves one frame.
    let ops = timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Head, secs(99.0)).unwrap();
    apply(&mut doc, ops);
    let one_frame = doc.project().sequence(SEQ).unwrap().rate.frame_start(1);
    assert_eq!(item(&doc, 10).range.duration, one_frame);
    assert_eq!(item(&doc, 10).range.end(), secs(5.0));
}

/// Bulk arranging: clips close up on each track, line up, move to a time as a block, or
/// spread evenly — one edit, refused whole if anything is in the way.
#[test]
fn selected_clips_arrange_together() {
    use timeline::Arrange;
    // V1: 10 at 1–3 s, 11 at 6–8 s; V2: 12 at 4–5 s; 13 (not selected) at 9–10 s on V2.
    let base = [(V1, 10, STILL, 1.0, 2.0, 0.0), (V1, 11, STILL, 6.0, 2.0, 0.0), (V2, 12, STILL, 4.0, 1.0, 0.0), (V2, 13, STILL, 9.0, 1.0, 0.0)];
    let chosen = [ItemId(10), ItemId(11), ItemId(12)];
    let starts = |doc: &Document| [10, 11, 12].map(|id| item(doc, id).range.start);

    let mut doc = doc_with(&base);
    let ops = timeline::arrange(doc.project(), SEQ, &chosen, Arrange::Together).unwrap();
    apply(&mut doc, ops);
    // 11 closes up behind 10; 12 is alone on its track (among those chosen): it stays.
    assert_eq!(starts(&doc), [secs(1.0), secs(3.0), secs(4.0)]);

    let mut doc = doc_with(&base);
    let ops = timeline::arrange(doc.project(), SEQ, &[ItemId(10), ItemId(12)], Arrange::LineUpStarts).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 12).range.start, secs(1.0));

    let mut doc = doc_with(&base);
    let ops = timeline::arrange(doc.project(), SEQ, &chosen, Arrange::StartAt(secs(0.0))).unwrap();
    apply(&mut doc, ops);
    assert_eq!(starts(&doc), [secs(0.0), secs(5.0), secs(3.0)], "moved as a block, spacing kept");

    let mut doc = doc_with(&base);
    let ops = timeline::arrange(doc.project(), SEQ, &chosen, Arrange::SpaceEvenly).unwrap();
    apply(&mut doc, ops);
    // Starts 1, 4, 6 → 1, 3.5, 6.
    assert_eq!(starts(&doc), [secs(1.0), secs(6.0), secs(3.5)]);

    // In the way: moved to 6 s, 10 would land on 11 (not chosen), and nothing moves.
    let doc = doc_with(&base);
    assert!(timeline::arrange(doc.project(), SEQ, &[ItemId(10), ItemId(12)], Arrange::StartAt(secs(6.0))).is_err());
}

/// Reversing plays the same part of the file backwards — the first frame shown is the
/// part's last, the last shown its first — and reversing back is where it started.
#[test]
fn reversing_plays_the_same_frames_backwards() {
    use oa_time::Rational;
    let doc = doc_with(&[(V1, 10, MEDIA, 1.0, 4.0, 2.0)]); // shows source 2..6 s
    let it = item(&doc, 10);
    let (range, map) = timeline::reversed(&it, true).expect("reversible");
    assert_eq!(range, it.range, "the clip keeps its place and length");
    assert!(map.speed.num() < 0);
    let rate = oa_time::FrameRate::FPS_30;
    let frame = |t: Time| rate.frame_at(t);
    // Local 0 shows the frame just before 6 s; the end shows 2 s's frame.
    assert_eq!(frame(map.source_time(Time::ZERO)), frame(secs(6.0)) - 1);
    assert_eq!(frame(map.source_time(it.range.duration - rate.frame_start(1))), frame(secs(2.0)));
    // It plays backwards all the way, one frame per frame.
    let a = frame(map.source_time(rate.frame_start(10)));
    let b = frame(map.source_time(rate.frame_start(11)));
    assert_eq!(a - b, 1);
    // Reversing back restores it exactly; already that way, nothing to do.
    let mut back = it.clone();
    back.time_map = map;
    let (_, restored) = timeline::reversed(&back, false).unwrap();
    assert_eq!((restored.source_in, restored.speed), (it.time_map.source_in, it.time_map.speed));
    assert!(timeline::reversed(&it, false).is_none());
    // A 2× clip stays 2×, backwards.
    let mut fast = it.clone();
    fast.time_map.speed = Rational::new(2, 1);
    assert_eq!(timeline::reversed(&fast, true).unwrap().1.speed, Rational::new(-2, 1));
}

#[test]
fn head_trim_is_limited_by_the_source_in_point() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 10.0, 5.0, 3.0)]);
    let ops = timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Head, secs(0.0)).unwrap();
    apply(&mut doc, ops);
    let it = item(&doc, 10);
    assert_eq!((it.range.start, it.time_map.source_in), (secs(7.0), Time::ZERO));
    // And the tail by the end of the file (20 s of media, starting at source 0).
    let ops = timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Tail, secs(100.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).range.end(), secs(27.0));
}

#[test]
fn stills_trim_to_any_length() {
    let mut doc = doc_with(&[(V1, 10, STILL, 2.0, 5.0, 0.0)]);
    let ops = timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Tail, secs(60.0)).unwrap();
    apply(&mut doc, ops);
    let ops = timeline::trim(doc.project(), SEQ, ItemId(10), Edge::Head, secs(0.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).range, oa_time::TimeRange::new(Time::ZERO, secs(60.0)));
}

#[test]
fn split_changes_no_frame() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 1.0, 6.0, 2.0)]);
    // Animate everything that could reveal a seam: keyframed scale on the clip clock,
    // focus on the source clock, a shaking position, a keyed blur, and a variant override.
    let key = |a: f64, b: f64| {
        ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0.5), Value::Vec2([a, a])), Keyframe::linear(secs(5.0), Value::Vec2([b, b]))],
        ))
    };
    let blur = EffectInstance {
        id: EffectId(50),
        type_id: "oa.blur.gaussian".into(),
        type_version: 1,
        enabled: true,
        params: {
            let mut ps = oa_params::ParamSet::default();
            ps.set(
                "radius",
                ParamSource::Animated(Curve::new(
                    KeyframeAnchor::ClipStart,
                    vec![Keyframe::linear(secs(0.0), Value::Float(0.0)), Keyframe::linear(secs(6.0), Value::Float(30.0))],
                )),
            );
            ps
        },
        role: Default::default(),
        on_duplicate: None,
    };
    let set = |param: &str, target: ParamTarget, source: ParamSource| Op::SetParam {
        seq: SEQ,
        item: ItemId(10),
        target,
        param: param.into(),
        source: Some(source),
    };
    apply(
        &mut doc,
        vec![
            Op::InsertEffect { seq: SEQ, item: ItemId(10), index: 0, effect: blur },
            set(schema::SCALE, ParamTarget::Item, key(1.0, 2.0)),
            set(schema::POSITION, ParamTarget::Item, ParamSource::Static(Value::Vec2([0.0, 0.0])).wiggle(0.05, 4.0, 3)),
            set(schema::ROTATION, ParamTarget::VariantOverride(TALL), ParamSource::Static(Value::Float(10.0)).wiggle(5.0, 2.0, 8)),
        ],
    );
    let before = doc.snapshot();
    let frames: Vec<Time> = (30..7 * 30).map(|f| doc.project().sequence(SEQ).unwrap().rate.frame_start(f)).collect();

    let mut next = 5000;
    let (ops, back) = timeline::split(doc.project(), SEQ, ItemId(10), secs(3.5), &mut || {
        next += 1;
        next
    })
    .unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).range.end(), secs(3.5));
    assert_eq!(item(&doc, back.0).range.start, secs(3.5));
    assert_ne!(item(&doc, back.0).effects[0].id, EffectId(50), "the copy gets its own effect ids");
    for &t in &frames {
        for v in [WIDE, TALL] {
            assert_eq!(frame_key(&before, t, v), frame_key(doc.project(), t, v), "frame at {t:?} in variant {v:?} changed");
        }
    }
    // Undo puts the single clip back.
    doc.undo().unwrap();
    assert!(doc.project().sequence(SEQ).unwrap().item(back).is_none());
    assert_eq!(item(&doc, 10).range.end(), secs(7.0));
}

/// At a speed whose source positions fall between flicks (7/3×), both halves of a split
/// — and a split of a split — show exactly the source times the whole clip did.
#[test]
fn split_at_an_odd_speed_is_exact() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 6.0, 1.0)]);
    let slow = oa_time::Rational::new(7, 3);
    let range = item(&doc, 10).range;
    apply(&mut doc, vec![Op::SetItemTiming { seq: SEQ, item: ItemId(10), range, time_map: TimeMap::new(secs(1.0), slow) }]);
    let whole = item(&doc, 10).clone();
    let source_at = |it: &Item, t: Time| it.time_map.source_time(t - it.range.start);
    let mut next = 7000;
    let mut alloc = || {
        next += 1;
        next
    };
    // Cut at an odd number of flicks in, then again inside the back half.
    let (ops, back) = timeline::split(doc.project(), SEQ, ItemId(10), Time(2 * oa_time::FLICKS_PER_SECOND + 1), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let (ops, last) = timeline::split(doc.project(), SEQ, back, Time(4 * oa_time::FLICKS_PER_SECOND + 2), &mut alloc).unwrap();
    apply(&mut doc, ops);
    for step in 0..6000 {
        let t = Time(step * oa_time::FLICKS_PER_SECOND / 1000 + step % 7);
        let piece = [ItemId(10), back, last].into_iter().map(|id| item(&doc, id.0).clone()).find(|it| it.range.contains(t)).unwrap();
        assert_eq!(source_at(&piece, t), source_at(&whole, t), "at {t:?}");
    }
}

/// A caption's word times stay on what's said: trimming its head, moving it, splitting
/// it and undoing all keep each word lit at the same moments on the timeline.
#[test]
fn word_times_follow_trims_moves_and_splits() {
    let mut doc = doc_with(&[]);
    let mut caption = Item::new(ItemId(20), "caption", ItemKind::Text, oa_time::TimeRange::new(secs(10.0), secs(4.0)));
    caption.word_times = [0.0, 1.0, 2.0, 3.0].iter().map(|s| oa_time::TimeRange::new(secs(*s), secs(0.5))).collect();
    apply(&mut doc, vec![Op::InsertItem { seq: SEQ, track: V1, item: caption }]);
    // Which word is lit at each timeline moment, over whichever pieces cover it.
    let lit = |doc: &Document, ids: &[u64]| -> Vec<Option<usize>> {
        (0..40)
            .map(|k| secs(10.0 + k as f64 * 0.1 + 0.05))
            .map(|t| ids.iter().map(|id| item(doc, *id)).find(|it| it.range.contains(t)).and_then(|it| it.spoken_word(t - it.range.start, 4)))
            .collect()
    };
    let before = lit(&doc, &[20]);

    let ops = timeline::trim(doc.project(), SEQ, ItemId(20), Edge::Head, secs(11.0)).unwrap();
    apply(&mut doc, ops);
    let trimmed = lit(&doc, &[20]);
    assert_eq!(&trimmed[10..], &before[10..], "after a head trim the words keep their moments");

    let mut next = 9000;
    let (ops, back) = timeline::split(doc.project(), SEQ, ItemId(20), secs(12.5), &mut || {
        next += 1;
        next
    })
    .unwrap();
    apply(&mut doc, ops);
    assert_eq!(&lit(&doc, &[20, back.0])[10..], &before[10..], "and across a split");

    doc.undo().unwrap();
    doc.undo().unwrap();
    assert_eq!(item(&doc, 20).word_times[0].start, secs(0.0), "undo puts them back");
    assert_eq!(lit(&doc, &[20]), before);

    // A move takes them along.
    let moved = oa_time::TimeRange::new(secs(20.0), secs(4.0));
    let map = item(&doc, 20).time_map;
    apply(&mut doc, vec![Op::SetItemTiming { seq: SEQ, item: ItemId(20), range: moved, time_map: map }]);
    assert_eq!(item(&doc, 20).word_times[1].start, secs(1.0));
}

#[test]
fn split_refuses_the_edges_and_split_all_cuts_every_track() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 4.0, 0.0), (V2, 11, STILL, 1.0, 4.0, 0.0)]);
    let mut n = 9000;
    let mut alloc = || {
        n += 1;
        n
    };
    assert!(timeline::split(doc.project(), SEQ, ItemId(10), secs(0.0), &mut alloc).is_err());
    assert!(timeline::split(doc.project(), SEQ, ItemId(10), secs(4.0), &mut alloc).is_err());
    let ops = timeline::split_all(doc.project(), SEQ, secs(2.0), &[], &mut alloc).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(s.track(V1).unwrap().items.len(), 2);
    assert_eq!(s.track(V2).unwrap().items.len(), 2);
}

#[test]
fn ripple_delete_closes_the_gap_on_its_track_only() {
    let mut doc = doc_with(&[
        (V1, 10, MEDIA, 0.0, 2.0, 0.0),
        (V1, 11, MEDIA, 2.0, 3.0, 0.0),
        (V1, 12, MEDIA, 6.0, 1.0, 0.0),
        (V2, 13, STILL, 5.0, 1.0, 0.0),
    ]);
    let ops = timeline::ripple_delete(doc.project(), SEQ, ItemId(11)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 12).range.start, secs(3.0));
    assert_eq!(item(&doc, 13).range.start, secs(5.0), "other tracks stay put");
    assert!(doc.project().sequence(SEQ).unwrap().item(ItemId(11)).is_none());

    // close_gap removes the hole between 10 and 12 (2 s .. 3 s).
    let ops = timeline::close_gap(doc.project(), SEQ, V1, secs(2.5)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 12).range.start, secs(2.0));
}

#[test]
fn moves_land_in_the_nearest_free_spot_and_can_change_track() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 2.0, 0.0), (V1, 11, MEDIA, 5.0, 2.0, 0.0), (V2, 12, STILL, 0.0, 1.0, 0.0)]);
    // Dropping 11 on top of 10 pushes it to 10's end.
    let ops = timeline::move_item(doc.project(), SEQ, ItemId(11), secs(0.5), None).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 11).range.start, secs(2.0));
    // Onto V2, after the still.
    let ops = timeline::move_item(doc.project(), SEQ, ItemId(11), secs(0.2), Some(V2)).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    assert!(s.track(V2).unwrap().items.iter().any(|i| i.id == ItemId(11)));
    assert_eq!(item(&doc, 11).range.start, secs(1.0));
}

#[test]
fn slip_moves_the_footage_within_the_media() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 5.0, 2.0)]);
    let ops = timeline::slip(doc.project(), SEQ, ItemId(10), secs(3.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).time_map.source_in, secs(5.0));
    // 20 s of media, 5 s shown: the in-point can't pass 15 s, nor go below 0.
    let ops = timeline::slip(doc.project(), SEQ, ItemId(10), secs(100.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).time_map.source_in, secs(15.0));
    let ops = timeline::slip(doc.project(), SEQ, ItemId(10), secs(-100.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).time_map.source_in, Time::ZERO);
    assert_eq!(item(&doc, 10).range.start, Time::ZERO, "the clip itself never moves");
}

#[test]
fn snapping_prefers_the_closest_edge() {
    let doc = doc_with(&[(V1, 10, MEDIA, 0.0, 2.0, 0.0), (V1, 11, MEDIA, 5.0, 2.0, 0.0)]);
    let s = doc.project().sequence(SEQ).unwrap();
    let snapper = Snapper::new(s, Some(ItemId(11)), &[secs(3.3)]);
    assert_eq!(snapper.snap(secs(2.1), secs(0.2)), Some(secs(2.0)));
    assert_eq!(snapper.snap(secs(2.5), secs(0.2)), None);
    // A 1 s clip dropped at 2.4 s: its tail (3.4 s) is nearer the playhead (3.3 s) than
    // its head is to the clip edge at 2 s.
    assert_eq!(snapper.snap_range(secs(2.4), secs(1.0), secs(0.5)), (secs(3.3) - secs(1.0), Some(secs(3.3))));
    assert_eq!(timeline::snap_to_frame(s, secs(0.049)), s.rate.frame_start(1));
    assert_eq!(timeline::snap_to_frame(s, secs(0.01)), Time::ZERO);
}

#[test]
fn transitions_clamp_to_the_clips_and_travel_with_splits() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 1.0, 0.0), (V1, 11, MEDIA, 1.0, 4.0, 0.0)]);
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(timeline::nearest_cut(s.track(V1).unwrap(), secs(1.1), secs(0.2)), Some(ItemId(11)));
    assert_eq!(timeline::nearest_cut(s.track(V1).unwrap(), secs(2.0), secs(0.2)), None);

    // 5 s asked for; the 1 s clip before the cut allows 2 s (1 s on each side).
    let ops = timeline::set_transition(doc.project(), SEQ, ItemId(11), ClipEnd::Head, "oa.transition.wipe", secs(5.0)).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 11).transition_in.as_ref().unwrap().duration, secs(2.0));

    // Changing only the duration keeps the transition's settings.
    let set_angle = Op::SetParam {
        seq: SEQ,
        item: ItemId(11),
        target: ParamTarget::Transition(ClipEnd::Head),
        param: "angle".into(),
        source: Some(ParamSource::Static(Value::Float(90.0))),
    };
    apply(&mut doc, vec![set_angle]);
    let ops = timeline::set_transition(doc.project(), SEQ, ItemId(11), ClipEnd::Head, "oa.transition.wipe", secs(0.5)).unwrap();
    apply(&mut doc, ops);
    let tr = item(&doc, 11).transition_in.unwrap();
    assert_eq!((tr.duration, tr.params.get("angle").is_some()), (secs(0.5), true));

    // A tail fade moves to the back half on a split; the head stays at the front.
    let ops = timeline::set_transition(doc.project(), SEQ, ItemId(11), ClipEnd::Tail, "oa.transition.crossfade", secs(1.0)).unwrap();
    apply(&mut doc, ops);
    let mut n = 7000;
    let (ops, back) = timeline::split(doc.project(), SEQ, ItemId(11), secs(3.0), &mut || {
        n += 1;
        n
    })
    .unwrap();
    apply(&mut doc, ops);
    let (front, back) = (item(&doc, 11), item(&doc, back.0));
    assert!(front.transition_in.is_some() && front.transition_out.is_none());
    assert!(back.transition_in.is_none() && back.transition_out.is_some());
}

#[test]
fn several_clips_move_together_even_through_each_others_places() {
    // 10: 0..2, 11: 2..4 on V1 (back to back), 12: 1..3 on V2.
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 2.0, 0.0), (V1, 11, MEDIA, 2.0, 2.0, 0.0), (V2, 12, STILL, 1.0, 2.0, 0.0)]);
    let all = [ItemId(10), ItemId(11), ItemId(12)];
    // 1 s later: clip 10 lands where clip 11 was — fine, 11 moves too.
    let ops = timeline::move_items(doc.project(), SEQ, &all, secs(1.0), 0).unwrap();
    apply(&mut doc, ops);
    assert_eq!(item(&doc, 10).range.start, secs(1.0));
    assert_eq!(item(&doc, 11).range.start, secs(3.0));
    assert_eq!(item(&doc, 12).range.start, secs(2.0));
    // Not before zero, and not onto a clip that isn't moving.
    assert!(timeline::move_items(doc.project(), SEQ, &all, secs(-5.0), 0).is_err());
    assert!(timeline::move_items(doc.project(), SEQ, &[ItemId(10)], secs(2.0), 0).is_err());
    // Up a track: V1 → V2 (clip 12 goes with it, off the top → the move fails).
    assert!(timeline::move_items(doc.project(), SEQ, &all, Time::ZERO, 1).is_err());
    let ops = timeline::move_items(doc.project(), SEQ, &[ItemId(11)], secs(2.0), 1).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(s.find_item(ItemId(11)).map(|(t, _)| s.tracks[t].id), Some(V2));
}

#[test]
fn paste_keeps_spacing_and_makes_room_on_a_new_track() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 2.0, 0.0), (V1, 11, MEDIA, 3.0, 1.0, 0.0)]);
    let copied: Vec<(TrackId, Item)> = [10, 11].iter().map(|&i| (V1, item(&doc, i))).collect();
    let mut next = 1000;
    let mut alloc = || {
        next += 1;
        next
    };
    // At 10 s there's room on V1.
    let (ops, ids) = timeline::paste(doc.project(), SEQ, &copied, secs(10.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert_eq!(ids.len(), 2);
    assert_eq!(item(&doc, ids[0].0).range.start, secs(10.0));
    assert_eq!(item(&doc, ids[1].0).range.start, secs(13.0), "spacing kept");
    // At 1 s V1 is taken and V2 is free: they go there, no new track.
    let tracks = doc.project().sequence(SEQ).unwrap().tracks.len();
    let (ops, ids) = timeline::paste(doc.project(), SEQ, &copied, secs(1.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    let track_of = |id: ItemId| s.find_item(id).map(|(t, _)| s.tracks[t].id);
    assert_eq!(track_of(ids[0]), Some(V2));
    assert_eq!(s.tracks.len(), tracks);
    // Grouped copies stay grouped — in a new group.
    let mut grouped = copied.clone();
    for (_, it) in &mut grouped {
        it.group = Some(7);
    }
    let (ops, ids) = timeline::paste(doc.project(), SEQ, &grouped, secs(20.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let (a, b) = (item(&doc, ids[0].0).group, item(&doc, ids[1].0).group);
    assert!(a.is_some() && a == b && a != Some(7));
}

/// With no room anywhere, a paste makes a temp layer; it goes once emptied (in the same
/// undo step), and stays when made a real track.
#[test]
fn a_paste_with_no_room_goes_on_a_temp_layer_that_clears_itself() {
    // Both tracks busy for 10 s; the copy is 1 s long.
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 10.0, 0.0), (V2, 11, STILL, 0.0, 10.0, 0.0)]);
    let mut clip = item(&doc, 10);
    clip.range.duration = secs(1.0);
    let copied = vec![(V1, clip)];
    let mut next = 1000;
    let mut alloc = || {
        next += 1;
        next
    };
    let (ops, ids) = timeline::paste(doc.project(), SEQ, &copied, secs(0.5), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let temp = |doc: &Document| doc.project().sequence(SEQ).unwrap().tracks.iter().find(|t| t.temp).map(|t| t.id);
    let layer = temp(&doc).expect("a temp layer");
    // Another paste with no room elsewhere reuses it (it has room) — no second layer.
    let tracks = doc.project().sequence(SEQ).unwrap().tracks.len();
    let (ops, more) = timeline::paste(doc.project(), SEQ, &copied, secs(3.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert_eq!(doc.project().sequence(SEQ).unwrap().tracks.len(), tracks);
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(s.find_item(more[0]).map(|(t, _)| s.tracks[t].id), Some(layer));
    apply(&mut doc, vec![Op::RemoveItem { seq: SEQ, item: more[0] }]);
    // Emptied: it goes, and undo brings back both.
    apply(&mut doc, vec![Op::RemoveItem { seq: SEQ, item: ids[0] }]);
    assert_eq!(temp(&doc), None);
    doc.undo().unwrap();
    assert_eq!(temp(&doc), Some(layer));
    assert!(doc.project().sequence(SEQ).unwrap().item(ids[0]).is_some());
    // A drag that empties it leaves it until the drag ends.
    doc.edit_coalesced("drag", Some("drag"), vec![Op::RemoveItem { seq: SEQ, item: ids[0] }]).unwrap();
    assert_eq!(temp(&doc), Some(layer));
    doc.seal();
    assert_eq!(temp(&doc), None);
    doc.undo().unwrap();
    // Kept: a real track stays when emptied.
    apply(&mut doc, vec![Op::SetTrackTemp { seq: SEQ, track: layer, temp: false }]);
    apply(&mut doc, vec![Op::RemoveItem { seq: SEQ, item: ids[0] }]);
    assert!(doc.project().sequence(SEQ).unwrap().track(layer).is_some());
}

/// A speed ramp keeps the clip playing the same part of its file: slowed, it gets longer
/// (onto a temp layer when the next clip is in the way); sped up, shorter.
#[test]
fn speed_ramps_grow_and_shrink_the_clip() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 4.0, 0.0), (V1, 11, MEDIA, 6.0, 2.0, 0.0)]);
    let mut next = 1000;
    let mut alloc = || {
        next += 1;
        next
    };
    let ramp = |a: f64, b: f64| {
        let key = |t: f64, v: f64| Keyframe { t: secs(t), value: Value::Float(v), interp: oa_params::Interp::Linear };
        ParamSource::Animated(Curve::new(KeyframeAnchor::ClipStart, vec![key(0.0, a), key(4.0, b)]))
    };
    let length = |doc: &Document| item(doc, 10).range.duration.as_seconds_f64();
    // 1× → 2× over the first 4 s: 4 s of file take less than 4 s (t + t²/8 = 4: about 2.93 s).
    let ops = timeline::set_speed_ramp(doc.project(), SEQ, ItemId(10), Some(ramp(1.0, 2.0)), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert!((length(&doc) - 2.93).abs() < 0.05, "shorter: {}", length(&doc));
    assert!((timeline::source_span(&item(&doc, 10)) - 4.0).abs() < 0.05, "the same part of the file");
    // 1× → 0.25×: longer than the 6 s before the next clip — up onto a temp layer.
    let ops = timeline::set_speed_ramp(doc.project(), SEQ, ItemId(10), Some(ramp(1.0, 0.25)), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    let (t, _) = s.find_item(ItemId(10)).unwrap();
    assert!(length(&doc) > 6.0, "longer: {}", length(&doc));
    assert!(s.tracks[t].id == V2 || s.tracks[t].temp, "moved up a track");
}

/// Opening time cuts what's under the point and pushes everything after it right;
/// removing time cuts that stretch out of every track and pulls the rest left.
#[test]
fn time_opens_and_closes_across_every_track() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 4.0, 0.0), (V2, 11, MEDIA, 3.0, 2.0, 0.0)]);
    let mut next = 1000;
    let mut alloc = || {
        next += 1;
        next
    };
    let spans = |doc: &Document| {
        let s = doc.project().sequence(SEQ).unwrap();
        let mut v: Vec<(TrackId, f64, f64)> = s.tracks.iter().flat_map(|t| t.items.iter().map(move |i| (t.id, i.range.start.as_seconds_f64(), i.range.end().as_seconds_f64()))).collect();
        v.sort_by(|a, b| (a.0 .0, a.1).partial_cmp(&(b.0 .0, b.1)).unwrap());
        v
    };
    let ops = timeline::insert_time(doc.project(), SEQ, secs(2.0), secs(1.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert_eq!(spans(&doc), [(V1, 0.0, 2.0), (V1, 3.0, 5.0), (V2, 4.0, 6.0)]);
    // Out again: back where they were (in pieces).
    let ops = timeline::remove_time(doc.project(), SEQ, secs(2.0), secs(3.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert_eq!(spans(&doc), [(V1, 0.0, 2.0), (V1, 2.0, 4.0), (V2, 3.0, 5.0)]);
    // Through the middle of both: 3.5–4.5 s goes.
    let ops = timeline::remove_time(doc.project(), SEQ, secs(3.5), secs(4.5), &mut alloc).unwrap();
    apply(&mut doc, ops);
    assert_eq!(spans(&doc), [(V1, 0.0, 2.0), (V1, 2.0, 3.5), (V2, 3.0, 3.5), (V2, 3.5, 4.0)]);
}

