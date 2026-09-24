mod common;

use common::*;
use oa_doc::*;
use oa_edit::transform::{self, Gesture, Guide, Handle, Handles, Modifiers, Scope};
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamSource, Value};
use oa_plan::scene;
use oa_time::Time;

fn close(a: [f64; 2], b: [f64; 2], eps: f64) -> bool {
    (a[0] - b[0]).abs() < eps && (a[1] - b[1]).abs() < eps
}

fn placement(doc: &Document, id: u64, v: VariantId, t: Time) -> scene::Placement {
    transform::placement_of(doc.project(), SEQ, v, ItemId(id), t).unwrap()
}

/// Runs a whole drag: begin at `from`, move to `to`, commit as one undo step.
#[allow(clippy::too_many_arguments)]
fn drag(doc: &mut Document, id: u64, v: VariantId, t: Time, handle: Handle, from: [f64; 2], to: [f64; 2], mods: Modifiers, scope: Scope) -> Vec<Guide> {
    let g = Gesture::begin(doc.project(), SEQ, v, ItemId(id), t, handle, from, scope).unwrap();
    let mut guides = Vec::new();
    // A few intermediate moves, like a real drag; each recomputes from the start.
    for i in 1..=4 {
        let f = i as f64 / 4.0;
        let p = [from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f];
        let u = g.update(doc.project(), p, mods, 8.0).unwrap();
        guides = u.guides;
        doc.edit_coalesced("drag", Some("drag"), u.ops).unwrap();
    }
    doc.seal();
    guides
}

#[test]
fn clicks_select_the_topmost_layer_under_the_pointer() {
    let mut doc = doc_with(&[(V1, 10, MEDIA, 0.0, 5.0, 0.0), (V2, 11, STILL, 0.0, 5.0, 0.0)]);
    let v = doc.project().sequence(SEQ).unwrap().variant(WIDE).unwrap().clone();
    // The still fills the canvas too (default fit is fill) — it's on top, so it wins.
    assert_eq!(scene::hit_test(doc.project(), SEQ, &v, secs(1.0), [100.0, 100.0]), Some(ItemId(11)));
    // Shrink it to a quarter around the center: the corner now hits the video beneath.
    doc.edit(
        "shrink",
        vec![Op::SetParam {
            seq: SEQ,
            item: ItemId(11),
            target: ParamTarget::Item,
            param: schema::SCALE.into(),
            source: Some(ParamSource::Static(Value::Vec2([0.25, 0.25]))),
        }],
    )
    .unwrap();
    assert_eq!(scene::hit_test(doc.project(), SEQ, &v, secs(1.0), [960.0, 540.0]), Some(ItemId(11)));
    assert_eq!(scene::hit_test(doc.project(), SEQ, &v, secs(1.0), [100.0, 100.0]), Some(ItemId(10)));
    assert_eq!(scene::hit_test(doc.project(), SEQ, &v, secs(9.0), [100.0, 100.0]), None, "nothing after the clips end");

    // The 4K video fills 1920x1080 exactly: its corners are the canvas corners.
    let p = placement(&doc, 10, WIDE, secs(1.0));
    assert!(close(p.corners()[2], [1920.0, 1080.0], 1e-9), "{:?}", p.corners());
}

#[test]
fn moving_follows_the_pointer_and_snaps_to_the_center() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    doc.edit(
        "small",
        vec![Op::SetParam {
            seq: SEQ,
            item: ItemId(10),
            target: ParamTarget::Item,
            param: schema::SCALE.into(),
            source: Some(ParamSource::Static(Value::Vec2([0.2, 0.2]))),
        }],
    )
    .unwrap();
    let before = placement(&doc, 10, WIDE, secs(1.0));
    // No snapping here: a 300,-100 px move.
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Body, [960.0, 540.0], [1260.0, 440.0], Modifiers { step: true, ..Default::default() }, Scope::AllFormats);
    let after = placement(&doc, 10, WIDE, secs(1.0));
    assert!(close(after.pivot, [before.pivot[0] + 300.0, before.pivot[1] - 100.0], 1e-6));
    assert_eq!(doc.undo_label(), Some("drag"));

    // Back toward the middle, landing 5 px off: snaps onto the center, with guides.
    let guides = drag(&mut doc, 10, WIDE, secs(1.0), Handle::Body, [1260.0, 440.0], [965.0, 543.0], Modifiers::default(), Scope::AllFormats);
    assert!(close(placement(&doc, 10, WIDE, secs(1.0)).pivot, [960.0, 540.0], 1e-6));
    assert!(guides.contains(&Guide::Vertical(960.0)) && guides.contains(&Guide::Horizontal(540.0)), "{guides:?}");

    // One undo per drag.
    doc.undo().unwrap();
    doc.undo().unwrap();
    assert!(close(placement(&doc, 10, WIDE, secs(1.0)).pivot, before.pivot, 1e-9));
}

#[test]
fn a_dragged_corner_stays_under_the_pointer() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    // Rotated and off-center, so the math can't hide behind axis alignment.
    doc.edit(
        "pose",
        vec![
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::ROTATION.into(), source: Some(ParamSource::Static(Value::Float(30.0))) },
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::SCALE.into(), source: Some(ParamSource::Static(Value::Vec2([0.5, 0.5]))) },
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::POSITION.into(), source: Some(ParamSource::Static(Value::Vec2([0.1, -0.05]))) },
        ],
    )
    .unwrap();
    let start = placement(&doc, 10, WIDE, secs(1.0));
    let corner = start.corners()[2];
    let target = [corner[0] + 120.0, corner[1] - 40.0];
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Corner(2), corner, target, Modifiers { free: true, ..Default::default() }, Scope::AllFormats);
    let after = placement(&doc, 10, WIDE, secs(1.0));
    assert!(close(after.corners()[2], target, 1e-6), "{:?} vs {target:?}", after.corners()[2]);
    assert!(close(after.pivot, start.pivot, 1e-9), "scaling pivots on the anchor");

    // Uniform (default) keeps the aspect ratio.
    let corner = after.corners()[0];
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Corner(0), corner, [corner[0] - 50.0, corner[1] + 7.0], Modifiers::default(), Scope::AllFormats);
    let s0 = after.values.vec2(schema::SCALE);
    let s1 = placement(&doc, 10, WIDE, secs(1.0)).values.vec2(schema::SCALE);
    assert!((s1[0] / s1[1] - s0[0] / s0[1]).abs() < 1e-9, "{s0:?} → {s1:?}");

    // An edge handle changes one axis only.
    let edge = Handles::new(&placement(&doc, 10, WIDE, secs(1.0)), 30.0).edges[1];
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Edge(1), edge, [edge[0] + 30.0, edge[1] + 30.0], Modifiers::default(), Scope::AllFormats);
    let s2 = placement(&doc, 10, WIDE, secs(1.0)).values.vec2(schema::SCALE);
    assert!(s2[1] == s1[1] && s2[0] != s1[0]);
}

#[test]
fn rotation_turns_around_the_anchor_and_steps_with_a_modifier() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    let p = placement(&doc, 10, WIDE, secs(1.0));
    let (cx, cy) = (p.pivot[0], p.pivot[1]);
    // From right of the pivot to below it: a quarter turn clockwise (y is down).
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Rotate, [cx + 100.0, cy], [cx, cy + 100.0], Modifiers::default(), Scope::AllFormats);
    let r = placement(&doc, 10, WIDE, secs(1.0)).values.float(schema::ROTATION);
    assert!((r - 90.0).abs() < 1e-9, "{r}");
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Rotate, [cx + 100.0, cy], [cx + 100.0, cy + 20.0], Modifiers { step: true, ..Default::default() }, Scope::AllFormats);
    let r = placement(&doc, 10, WIDE, secs(1.0)).values.float(schema::ROTATION);
    assert_eq!(r, 105.0, "90° + 11.3° rounds to the 15° step");
}

#[test]
fn moving_the_anchor_leaves_the_picture_still() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    doc.edit(
        "pose",
        vec![
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::ROTATION.into(), source: Some(ParamSource::Static(Value::Float(-20.0))) },
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::SCALE.into(), source: Some(ParamSource::Static(Value::Vec2([0.4, 0.7]))) },
            Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::SQUASH.into(), source: Some(ParamSource::Static(Value::Float(0.3))) },
        ],
    )
    .unwrap();
    let before = placement(&doc, 10, WIDE, secs(1.0));
    let grab = before.pivot;
    let to = [grab[0] + 90.0, grab[1] + 33.0];
    drag(&mut doc, 10, WIDE, secs(1.0), Handle::Anchor, grab, to, Modifiers { step: true, ..Default::default() }, Scope::AllFormats);
    let after = placement(&doc, 10, WIDE, secs(1.0));
    for (a, b) in before.corners().iter().zip(after.corners()) {
        assert!(close(*a, b, 1e-6), "{a:?} moved to {b:?}");
    }
    assert!(close(after.pivot, to, 1e-6), "the pivot is where it was dropped");
}

#[test]
fn animated_properties_get_a_key_at_the_playhead() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    let keys = ParamSource::Animated(Curve::new(
        KeyframeAnchor::ClipStart,
        vec![Keyframe::linear(secs(0.0), Value::Vec2([0.0, 0.0])), Keyframe::linear(secs(4.0), Value::Vec2([0.2, 0.0]))],
    ));
    doc.edit("anim", vec![Op::SetParam { seq: SEQ, item: ItemId(10), target: ParamTarget::Item, param: schema::POSITION.into(), source: Some(keys) }]).unwrap();
    let at_2 = placement(&doc, 10, WIDE, secs(2.0)).pivot;
    drag(&mut doc, 10, WIDE, secs(2.0), Handle::Body, at_2, [at_2[0], at_2[1] + 108.0], Modifiers { step: true, ..Default::default() }, Scope::AllFormats);
    let it = item(&doc, 10);
    let curve = it.params.get(schema::POSITION).and_then(|s| s.curve()).unwrap();
    assert_eq!(curve.keys.len(), 3, "a key was added, none replaced");
    assert_eq!(curve.keys[1].t, secs(2.0));
    assert_eq!(curve.keys[1].value, Value::Vec2([0.1, 0.1]));
    // The ends of the move are untouched.
    assert!(close(placement(&doc, 10, WIDE, secs(4.0)).values.vec2(schema::POSITION), [0.2, 0.0], 1e-12));
}

#[test]
fn variant_edits_stay_in_their_variant() {
    let mut doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    let wide_before = placement(&doc, 10, WIDE, secs(1.0));
    let p = placement(&doc, 10, TALL, secs(1.0)).pivot;
    drag(&mut doc, 10, TALL, secs(1.0), Handle::Body, p, [p[0] + 50.0, p[1]], Modifiers { step: true, ..Default::default() }, Scope::Variant(TALL));
    assert!(close(placement(&doc, 10, TALL, secs(1.0)).pivot, [p[0] + 50.0, p[1]], 1e-6));
    assert!(close(placement(&doc, 10, WIDE, secs(1.0)).pivot, wide_before.pivot, 1e-12), "16:9 is untouched");
    assert!(item(&doc, 10).params.get(schema::POSITION).is_none());

    // Editing "all formats" while viewing 9:16 still edits the 9:16 override there,
    // because that's where the value on screen comes from.
    let p = placement(&doc, 10, TALL, secs(1.0)).pivot;
    drag(&mut doc, 10, TALL, secs(1.0), Handle::Body, p, [p[0], p[1] + 10.0], Modifiers { step: true, ..Default::default() }, Scope::AllFormats);
    assert!(close(placement(&doc, 10, WIDE, secs(1.0)).pivot, wide_before.pivot, 1e-12));

    // Reset clears the override.
    let ops = transform::reset_transform(doc.project(), SEQ, ItemId(10), Scope::Variant(TALL)).unwrap();
    doc.edit("reset", ops).unwrap();
    assert!(close(placement(&doc, 10, TALL, secs(1.0)).pivot, [540.0, 960.0], 1e-9));
}

#[test]
fn handles_are_picked_before_the_body() {
    let doc = doc_with(&[(V1, 10, STILL, 0.0, 5.0, 0.0)]);
    let p = placement(&doc, 10, WIDE, secs(1.0));
    let h = Handles::new(&p, 40.0);
    assert_eq!(h.pick(&p, [h.corners[1][0] + 3.0, h.corners[1][1]], 6.0), Some(Handle::Corner(1)));
    assert_eq!(h.pick(&p, p.pivot, 6.0), Some(Handle::Anchor));
    assert_eq!(h.pick(&p, [p.pivot[0] + 200.0, p.pivot[1]], 6.0), Some(Handle::Body));
    assert_eq!(h.pick(&p, h.rotate, 6.0), Some(Handle::Rotate));
    assert_eq!(h.pick(&p, [-500.0, -500.0], 6.0), None);

    let ops = transform::nudge(doc.project(), SEQ, WIDE, ItemId(10), secs(1.0), [10.0, 0.0], Scope::AllFormats).unwrap();
    let mut doc = doc;
    doc.edit("nudge", ops).unwrap();
    assert!(close(placement(&doc, 10, WIDE, secs(1.0)).pivot, [p.pivot[0] + 10.0, p.pivot[1]], 1e-9));
}
