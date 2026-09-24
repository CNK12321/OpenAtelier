//! Damaged project files open, with what was fixed reported.

use oa_doc::*;
use oa_params::{Curve, KeyframeAnchor, ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

fn secs(s: i64) -> Time {
    Time::from_seconds(s)
}

/// A healthy project: one sequence, one track, two clips, one media file.
fn healthy() -> Project {
    let mut p = Project::new("t");
    p.next_id = 50;
    let wide = AspectPreset::by_id("landscape-16x9").unwrap();
    let variant = FormatVariant { id: VariantId(2), name: wide.name.into(), size: wide.size(1080), overrides: BTreeMap::new() };
    let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_30, variant);
    let mut track = Track::new(TrackId(3), "V1", TrackKind::Video);
    for (id, start) in [(10, 0), (11, 5)] {
        track.items.push(Item::new(ItemId(id), &format!("c{id}"), ItemKind::Media { media: MediaId(4) }, TimeRange::new(secs(start), secs(5))));
    }
    seq.tracks.push(Arc::new(track));
    p.sequences.insert(SeqId(1), Arc::new(seq));
    p.media.insert(MediaId(4), Arc::new(MediaRef { id: MediaId(4), path: "a.mp4".into(), fingerprint: None, info: None, scaling: Default::default(), folder: String::new(), color: Default::default() }));
    p
}

/// Serializes, lets `damage` edit the raw JSON, and loads it back.
fn load_damaged(damage: impl FnOnce(&mut serde_json::Value)) -> (Project, Report) {
    let mut file: serde_json::Value = serde_json::from_str(&ProjectFile::to_json(&healthy()).unwrap()).unwrap();
    damage(&mut file["project"]);
    ProjectFile::load(&file.to_string()).expect("a damaged project still opens")
}

fn items(p: &Project) -> Vec<(u64, i64, u64)> {
    let s = p.sequence(SeqId(1)).unwrap();
    s.tracks
        .iter()
        .flat_map(|t| t.items.iter().map(move |i| (t.id.0, i.range.start.0 / oa_time::FLICKS_PER_SECOND, i.id.0)))
        .collect()
}

/// Every invariant ops rely on, checked directly.
fn assert_sound(p: &Project) {
    let s = p.sequence(SeqId(1)).unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for t in &s.tracks {
        assert!(ids.insert(t.id.0), "duplicate track id");
        for w in t.items.windows(2) {
            assert!(w[0].range.end() <= w[1].range.start, "overlap or disorder on {}", t.name);
        }
        for i in &t.items {
            assert!(ids.insert(i.id.0), "duplicate item id {}", i.id.0);
            assert!(i.id.0 < p.next_id);
        }
    }
    assert!(s.variant(s.active_variant).is_some());
}

#[test]
fn a_healthy_file_needs_nothing() {
    let (p, report) = load_damaged(|_| {});
    assert!(report.is_clean(), "{report:?}");
    // Only derived data changes: the background takes the timeline's length.
    let mut expected = healthy();
    for s in expected.sequences.values_mut() {
        Arc::make_mut(s).sync_background();
    }
    assert_eq!(p, expected);
}

#[test]
fn overlaps_move_to_a_new_track_instead_of_being_cut() {
    let (p, report) = load_damaged(|v| {
        // Clip 11 now starts at 3 s, inside clip 10 (0..5 s); a third overlaps both.
        let items = &mut v["sequences"]["1"]["tracks"][0]["items"];
        items[1]["range"]["start"] = json!(3 * oa_time::FLICKS_PER_SECOND);
        let mut third = items[0].clone();
        third["id"] = json!(12);
        third["range"]["start"] = json!(4 * oa_time::FLICKS_PER_SECOND);
        items.as_array_mut().unwrap().push(third);
    });
    assert_sound(&p);
    let s = p.sequence(SeqId(1)).unwrap();
    assert_eq!(s.tracks.len(), 3, "both overlapping clips kept, each on its own new track: {:?}", items(&p));
    assert_eq!(s.tracks[1].name, "V1 (overlaps)");
    assert!(report.repaired.iter().any(|m| m.contains("overlapping")), "{report:?}");
    // Nothing was lost.
    let mut all: Vec<u64> = items(&p).into_iter().map(|(_, _, id)| id).collect();
    all.sort();
    assert_eq!(all, vec![10, 11, 12]);
}

#[test]
fn duplicate_ids_and_a_stale_next_id_are_renumbered() {
    let (p, report) = load_damaged(|v| {
        v["sequences"]["1"]["tracks"][0]["items"][1]["id"] = json!(10);
        v["next_id"] = json!(5);
    });
    assert_sound(&p);
    assert!(p.next_id > 11, "next id moved past everything in use");
    assert_eq!(report.repaired.len(), 2, "{report:?}");
    // And the repaired project is editable: a new id doesn't collide.
    let mut doc = Document::new(p);
    let id = ItemId(doc.alloc_id());
    let item = Item::new(id, "new", ItemKind::Solid, TimeRange::new(secs(20), secs(1)));
    doc.edit("add", vec![Op::InsertItem { seq: SeqId(1), track: TrackId(3), item }]).unwrap();
}

#[test]
fn out_of_order_clips_sort_and_empty_ones_go() {
    let (p, report) = load_damaged(|v| {
        let items = v["sequences"]["1"]["tracks"][0]["items"].as_array_mut().unwrap();
        items.swap(0, 1);
        let mut empty = items[0].clone();
        empty["id"] = json!(13);
        empty["range"]["start"] = json!(30 * oa_time::FLICKS_PER_SECOND);
        empty["range"]["duration"] = json!(0);
        items.push(empty);
    });
    assert_sound(&p);
    assert_eq!(items(&p), vec![(3, 0, 10), (3, 5, 11)]);
    assert_eq!(report.repaired.len(), 2, "{report:?}");
}

#[test]
fn a_sequence_without_formats_gets_one() {
    let (p, report) = load_damaged(|v| {
        v["sequences"]["1"]["variants"] = json!([]);
    });
    assert_sound(&p);
    let s = p.sequence(SeqId(1)).unwrap();
    assert_eq!(s.variants.len(), 1);
    assert_eq!((s.canvas().width, s.canvas().height), (1920, 1080));
    assert!(!report.repaired.is_empty());
}

#[test]
fn empty_keyframe_curves_fall_back_to_defaults_instead_of_crashing() {
    let mut p = healthy();
    let seq = Arc::make_mut(p.sequences.get_mut(&SeqId(1)).unwrap());
    let track = Arc::make_mut(&mut seq.tracks[0]);
    track.items[0].params.set(schema::OPACITY, ParamSource::Animated(Curve { anchor: KeyframeAnchor::ClipStart, keys: vec![] }));
    track.items[0].params.set(schema::ROTATION, ParamSource::Static(Value::Float(5.0)));
    let text = ProjectFile::to_json(&p).unwrap();
    let (loaded, report) = ProjectFile::load(&text).unwrap();
    let item = loaded.sequence(SeqId(1)).unwrap().item(ItemId(10)).unwrap();
    assert!(item.params.get(schema::OPACITY).is_none(), "the empty curve is gone");
    assert!(item.params.get(schema::ROTATION).is_some(), "good values stay");
    assert_eq!(report.repaired.len(), 1, "{report:?}");
    // Evaluating every visual param at any time is safe now.
    let _ = item.params.eval(schema::visual(), None, &item.eval_context(secs(1)));
}

#[test]
fn dangling_references_are_reported_not_fatal() {
    let (p, report) = load_damaged(|v| {
        v["media"] = json!({});
    });
    assert_sound(&p);
    assert_eq!(report.warnings.len(), 2, "both clips name missing media: {report:?}");
}
