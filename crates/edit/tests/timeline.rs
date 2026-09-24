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
    // At 1 s V1 is taken (and V2 is free).
    let (ops, ids) = timeline::paste(doc.project(), SEQ, &copied, secs(1.0), &mut alloc).unwrap();
    apply(&mut doc, ops);
    let s = doc.project().sequence(SEQ).unwrap();
    let track_of = |id: ItemId| s.find_item(id).map(|(t, _)| s.tracks[t].id);
    assert_ne!(track_of(ids[0]), Some(V1));
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
