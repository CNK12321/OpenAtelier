use oa_doc::*;
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamId, ParamSource, Value};
use oa_time::{FrameRate, Rational, Time, TimeRange};
use std::collections::BTreeMap;
use std::sync::Arc;

const SEQ: SeqId = SeqId(1);
const V_WIDE: VariantId = VariantId(2);
const TRACK: TrackId = TrackId(3);
const MEDIA: MediaId = MediaId(4);

fn secs(s: i64) -> Time {
    Time::from_seconds(s)
}

fn variant(id: VariantId, preset: &str) -> FormatVariant {
    let p = AspectPreset::by_id(preset).unwrap();
    FormatVariant { id, name: p.name.into(), size: p.size(1080), overrides: BTreeMap::new() }
}

fn base() -> Document {
    let mut project = Project::new("test");
    project.next_id = 100;
    let mut doc = Document::new(project);
    let seq = Sequence::new(SEQ, "Main", FrameRate::FPS_30, variant(V_WIDE, "landscape-16x9"));
    let media = MediaRef { id: MEDIA, path: "clip.mp4".into(), fingerprint: None, info: None, scaling: Default::default(), folder: String::new(), color: Default::default() };
    doc.edit(
        "setup",
        vec![
            Op::AddSequence(Arc::new(seq)),
            Op::AddMedia(Arc::new(media)),
            Op::InsertTrack { seq: SEQ, index: 0, track: Arc::new(Track::new(TRACK, "V1", TrackKind::Video)) },
        ],
    )
    .unwrap();
    doc
}

fn clip(id: u64, start: i64, dur: i64) -> Item {
    Item::new(ItemId(id), "clip", ItemKind::Media { media: MEDIA }, TimeRange::new(secs(start), secs(dur)))
}

fn insert(doc: &mut Document, item: Item) -> Result<(), EditError> {
    doc.edit("insert", vec![Op::InsertItem { seq: SEQ, track: TRACK, item }])
}

#[test]
fn undo_redo_restores_exact_state() {
    let mut doc = base();
    let before = doc.snapshot();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    doc.edit(
        "param",
        vec![Op::SetParam {
            seq: SEQ,
            item: ItemId(10),
            target: ParamTarget::Item,
            param: ParamId::new(schema::OPACITY),
            source: Some(ParamSource::Static(Value::Float(0.5))),
        }],
    )
    .unwrap();
    let after = doc.snapshot();
    assert!(doc.undo().unwrap());
    assert!(doc.undo().unwrap());
    assert_eq!(*doc.snapshot(), *before);
    assert!(doc.redo().unwrap());
    assert!(doc.redo().unwrap());
    assert_eq!(*doc.snapshot(), *after);
    assert!(!doc.redo().unwrap());
}

#[test]
fn overlap_and_failed_edits_leave_no_trace() {
    let mut doc = base();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    let snap = doc.snapshot();
    // Second op overlaps: the whole transaction must be rejected.
    let err = doc.edit(
        "two",
        vec![
            Op::InsertItem { seq: SEQ, track: TRACK, item: clip(11, 10, 2) },
            Op::InsertItem { seq: SEQ, track: TRACK, item: clip(12, 4, 2) },
        ],
    );
    assert_eq!(err, Err(EditError::Overlap));
    assert!(Arc::ptr_eq(&snap, &doc.snapshot()));
    assert_eq!(doc.undo_label(), Some("insert"));
}

#[test]
fn items_are_found_by_time() {
    let mut doc = base();
    insert(&mut doc, clip(11, 10, 2)).unwrap();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    let p = doc.project();
    let track = p.sequence(SEQ).unwrap().track(TRACK).unwrap();
    assert_eq!(track.item_at(secs(0)).map(|i| i.id), Some(ItemId(10)));
    assert_eq!(track.item_at(secs(5)), None);
    assert_eq!(track.item_at(secs(11)).map(|i| i.id), Some(ItemId(11)));
    assert_eq!(p.sequence(SEQ).unwrap().duration(), secs(12));
}

#[test]
fn slider_drag_coalesces_into_one_undo_step() {
    let mut doc = base();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    let before = doc.snapshot();
    for v in [0.9, 0.7, 0.4] {
        doc.edit_coalesced(
            "opacity",
            Some("drag-opacity"),
            vec![Op::SetParam {
                seq: SEQ,
                item: ItemId(10),
                target: ParamTarget::Item,
                param: ParamId::new(schema::OPACITY),
                source: Some(ParamSource::Static(Value::Float(v))),
            }],
        )
        .unwrap();
    }
    doc.seal();
    doc.undo().unwrap();
    assert_eq!(*doc.snapshot(), *before);
}

#[test]
fn head_trim_keeps_source_anchored_keyframes_on_the_footage() {
    let mut item = clip(10, 0, 10);
    item.time_map = TimeMap { source_in: secs(0), speed: Rational::integer(2) };
    let curve = |anchor| {
        ParamSource::Animated(Curve::new(
            anchor,
            vec![Keyframe::linear(secs(0), Value::Float(0.0)), Keyframe::linear(secs(100), Value::Float(100.0))],
        ))
    };
    item.params.set("clip", curve(KeyframeAnchor::ClipStart));
    item.params.set("src", curve(KeyframeAnchor::SourceMedia));

    let eval = |it: &Item, t| {
        let ctx = it.eval_context(t);
        let f = |id| it.params.get(id).unwrap().eval(&ctx).as_float().unwrap();
        (f("clip"), f("src"))
    };
    // At timeline 4s, 2x speed → source 8s.
    assert_eq!(eval(&item, secs(4)), (4.0, 8.0));

    let (range, map) = item.trimmed_head(secs(3)).unwrap();
    assert_eq!(range, TimeRange::new(secs(3), secs(7)));
    assert_eq!(map.source_in, secs(6));
    let mut trimmed = item.clone();
    (trimmed.range, trimmed.time_map) = (range, map);
    // Same timeline instant shows the same footage (source 8s)...
    assert_eq!(trimmed.eval_context(secs(4)).source_time, secs(8));
    // ...source-anchored animation is unchanged, clip-anchored one restarted.
    assert_eq!(eval(&trimmed, secs(4)), (1.0, 8.0));
}

#[test]
fn nesting_cycles_are_rejected() {
    let mut doc = base();
    let other = SeqId(50);
    let t2 = TrackId(51);
    doc.edit(
        "seq2",
        vec![
            Op::AddSequence(Arc::new(Sequence::new(other, "Inner", FrameRate::FPS_30, variant(VariantId(52), "square-1x1")))),
            Op::InsertTrack { seq: other, index: 0, track: Arc::new(Track::new(t2, "V1", TrackKind::Video)) },
        ],
    )
    .unwrap();
    let nest = |id, target| Item::new(ItemId(id), "nest", ItemKind::Nested { sequence: target }, TimeRange::new(secs(0), secs(1)));
    doc.edit("nest", vec![Op::InsertItem { seq: SEQ, track: TRACK, item: nest(60, other) }]).unwrap();
    let back = doc.edit("cycle", vec![Op::InsertItem { seq: other, track: t2, item: nest(61, SEQ) }]);
    assert_eq!(back, Err(EditError::NestingCycle));
    let selfnest = doc.edit("self", vec![Op::InsertItem { seq: other, track: t2, item: nest(62, other) }]);
    assert_eq!(selfnest, Err(EditError::NestingCycle));
    assert_eq!(doc.edit("rm", vec![Op::RemoveSequence(other)]), Err(EditError::InUse("sequence", 50)));
}

#[test]
fn format_variants_share_timing_and_override_visuals() {
    let mut doc = base();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    let vertical = VariantId(20);
    doc.edit(
        "add vertical",
        vec![
            Op::InsertVariant { seq: SEQ, index: 1, variant: variant(vertical, "vertical-9x16"), make_active: true },
            Op::SetParam {
                seq: SEQ,
                item: ItemId(10),
                target: ParamTarget::VariantOverride(vertical),
                param: ParamId::new(schema::FOCUS),
                source: Some(ParamSource::Static(Value::Vec2([0.7, 0.5]))),
            },
        ],
    )
    .unwrap();
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(s.canvas(), CanvasSize::new(1080, 1920));
    assert!(s.variant(vertical).unwrap().overrides.contains_key(&ItemId(10)));
    assert!(s.variant(V_WIDE).unwrap().overrides.is_empty());

    // Undo restores the previously active variant, and the last variant can't go.
    doc.undo().unwrap();
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!((s.active_variant, s.variants.len()), (V_WIDE, 1));
    assert_eq!(doc.edit("rm", vec![Op::RemoveVariant { seq: SEQ, variant: V_WIDE }]), Err(EditError::LastVariant));

    // Horizontal ↔ vertical toggle is just a size change.
    let size = doc.project().sequence(SEQ).unwrap().canvas().rotated();
    doc.edit("rotate", vec![Op::SetVariantSize { seq: SEQ, variant: V_WIDE, size }]).unwrap();
    assert_eq!(doc.project().sequence(SEQ).unwrap().canvas().matching_preset().unwrap().id, "vertical-9x16");
}

#[test]
fn project_file_round_trips() {
    let mut doc = base();
    let mut item = clip(10, 0, 5);
    item.effects.push(EffectInstance {
        id: EffectId(30),
        type_id: "com.someone.unknown".into(),
        type_version: 3,
        enabled: true,
        params: Default::default(),
        role: Default::default(),
    });
    item.params.set("future.param", ParamSource::Static(Value::Text("kept".into())));
    insert(&mut doc, item).unwrap();
    let text = ProjectFile::to_json(doc.project()).unwrap();
    let loaded = ProjectFile::from_json(&text).unwrap();
    assert_eq!(&loaded, doc.project());
    assert!(matches!(ProjectFile::from_json(&text.replace("\"version\": 1", "\"version\": 99")), Err(FileError::TooNew(99))));
}

#[test]
fn undo_history_is_capped() {
    let mut doc = base();
    for i in 0..(history_cap() + 20) {
        let op = Op::SetVariantName { seq: SEQ, variant: V_WIDE, name: format!("n{i}") };
        doc.edit("rename", vec![op]).unwrap();
    }
    assert_eq!(doc.undo_depth(), history_cap());
    while doc.undo().unwrap() {}
    // The oldest renames fell off; the earliest reachable state is after rename 19.
    assert_eq!(doc.project().sequence(SEQ).unwrap().variants[0].name, "n19");
}

fn history_cap() -> usize {
    oa_doc::MAX_UNDO
}

#[test]
fn track_and_clip_switches_undo() {
    let mut doc = base();
    insert(&mut doc, clip(10, 0, 5)).unwrap();
    let before = doc.snapshot();
    doc.edit(
        "switches",
        vec![
            Op::SetTrackEnabled { seq: SEQ, track: TRACK, enabled: false },
            Op::SetTrackName { seq: SEQ, track: TRACK, name: "Main video".into() },
            Op::SetItemEnabled { seq: SEQ, item: ItemId(10), enabled: false },
        ],
    )
    .unwrap();
    let s = doc.project().sequence(SEQ).unwrap();
    assert!(!s.track(TRACK).unwrap().enabled);
    assert_eq!(s.track(TRACK).unwrap().name, "Main video");
    assert!(!s.item(ItemId(10)).unwrap().enabled);
    doc.undo().unwrap();
    assert_eq!(doc.project(), &*before);
}


/// Media-bin folders are just a path on each file: moving one is one undoable op, and a
/// folder exists exactly as long as something is in it.
#[test]
fn media_remembers_its_bin_folder() {
    let mut doc = base();
    let media = MEDIA;
    assert_eq!(doc.project().media(media).map(|m| m.folder.as_str()), Some(""));
    doc.edit("Move", vec![Op::SetMediaFolder { media, folder: "b-roll/day 2".into() }]).expect("move");
    assert_eq!(doc.project().media(media).map(|m| m.folder.as_str()), Some("b-roll/day 2"));
    doc.undo().expect("undo");
    assert_eq!(doc.project().media(media).map(|m| m.folder.as_str()), Some(""));
    // A file that isn't there can't be moved.
    assert!(doc.edit("Move", vec![Op::SetMediaFolder { media: MediaId(999), folder: "x".into() }]).is_err());
}

/// Bin folders live in the project, so one can exist while it's still empty — and go
/// away again with an undo.
#[test]
fn the_bin_keeps_its_folders() {
    let mut doc = base();
    assert!(doc.project().bin_folders.is_empty());
    doc.edit("New folder", vec![Op::SetBinFolders(vec!["b-roll".into(), "b-roll/day 2".into()])]).expect("folders");
    assert_eq!(doc.project().bin_folders, vec!["b-roll".to_string(), "b-roll/day 2".to_string()]);
    doc.edit("Move", vec![Op::SetMediaFolder { media: MEDIA, folder: "b-roll/day 2".into() }]).expect("move");
    doc.undo().expect("undo the move");
    assert_eq!(doc.project().media(MEDIA).map(|m| m.folder.as_str()), Some(""), "the file is back at the top");
    assert_eq!(doc.project().bin_folders.len(), 2, "but the folders are still there");
    doc.undo().expect("undo the folders");
    assert!(doc.project().bin_folders.is_empty());
}

/// The background is edited like a clip (id `BACKGROUND`): effects go on it, its params
/// keyframe, undo takes them off, and a plain one isn't written to the file.
#[test]
fn background_effects_edit_like_a_clip() {
    let mut doc = base();
    let plain = ProjectFile::to_json(doc.project()).unwrap();
    assert!(!plain.contains("\"background\""), "nothing written for a plain background");
    let fx = EffectInstance::new(EffectId(60), "oa.blur.gaussian");
    doc.edit("Add", vec![Op::InsertEffect { seq: SEQ, item: BACKGROUND, index: 0, effect: fx }]).unwrap();
    let radius = ParamSource::Static(Value::Float(12.0));
    doc.edit("Set", vec![Op::SetParam { seq: SEQ, item: BACKGROUND, target: ParamTarget::Effect(EffectId(60)), param: ParamId::new("radius"), source: Some(radius.clone()) }]).unwrap();
    let s = doc.project().sequence(SEQ).unwrap();
    assert_eq!(s.item(BACKGROUND).map(|b| b.effects.len()), Some(1));
    assert_eq!(s.background.effects[0].params.get("radius"), Some(&radius));
    assert_eq!(s.duration(), Time::ZERO, "the background doesn't make the timeline longer");

    let json = ProjectFile::to_json(doc.project()).unwrap();
    let back = ProjectFile::from_json(&json).unwrap();
    assert_eq!(back.sequence(SEQ).unwrap().background.effects.len(), 1, "saved and loaded");

    doc.undo().unwrap();
    doc.undo().unwrap();
    assert!(doc.project().sequence(SEQ).unwrap().background.effects.is_empty());
}

/// The background is as long as the timeline (at least a second), through edits and undo,
/// so its keyframe and wave editors span the timeline rather than forever.
#[test]
fn the_background_follows_the_timeline() {
    let mut doc = base();
    let len = |d: &Document| d.project().sequence(SEQ).unwrap().background.range;
    assert_eq!(len(&doc), TimeRange::new(Time::ZERO, secs(1)), "an empty timeline: one second");
    insert(&mut doc, clip(20, 2, 8)).unwrap();
    assert_eq!(len(&doc), TimeRange::new(Time::ZERO, secs(10)));
    doc.undo().unwrap();
    assert_eq!(len(&doc), TimeRange::new(Time::ZERO, secs(1)));
    doc.redo().unwrap();
    assert_eq!(len(&doc), TimeRange::new(Time::ZERO, secs(10)));
}

/// The word being spoken: by the clip's word times (the last one started stays lit
/// through the pause after it), or spread evenly over the clip without them.
#[test]
fn spoken_words_follow_times_or_spread_evenly() {
    let mut title = Item::new(ItemId(1), "t", ItemKind::Text, TimeRange::new(secs(10), secs(4)));
    assert_eq!(title.spoken_word(Time::ZERO, 4), Some(0));
    assert_eq!(title.spoken_word(secs(1), 4), Some(1));
    assert_eq!(title.spoken_word(Time::from_seconds_f64(3.99), 4), Some(3));
    assert_eq!(title.spoken_word(secs(9), 4), Some(3), "past the end: the last word");
    assert_eq!(title.spoken_word(secs(1), 0), None);
    title.word_times = vec![TimeRange::new(Time::from_seconds_f64(0.5), secs(1)), TimeRange::new(secs(3), secs(1))];
    assert_eq!(title.spoken_word(Time::ZERO, 2), None, "before the first word");
    assert_eq!(title.spoken_word(secs(2), 2), Some(0), "the pause after it");
    assert_eq!(title.spoken_word(Time::from_seconds_f64(3.5), 2), Some(1));
}

/// Effect containers go on effect tracks, and only there; nothing else goes on one.
#[test]
fn effect_tracks_hold_containers_and_nothing_else() {
    let mut doc = base();
    let seq = *doc.project().sequences.keys().next().unwrap();
    let fx = TrackId(doc.alloc_id());
    doc.edit("fx", vec![Op::InsertTrack { seq, index: 0, track: Arc::new(Track::effects(fx, "VFX1", TrackKind::Video)) }]).unwrap();
    let container = |id: u64| Item::new(ItemId(id), "Effects", ItemKind::Adjustment, TimeRange::new(secs(0), secs(2)));
    assert!(doc.edit("add", vec![Op::InsertItem { seq, track: fx, item: container(900) }]).is_ok());
    let solid = Item::new(ItemId(901), "solid", ItemKind::Solid, TimeRange::new(secs(3), secs(2)));
    assert!(matches!(doc.edit("add", vec![Op::InsertItem { seq, track: fx, item: solid }]), Err(EditError::WrongTrackKind)));
    let ordinary = doc.project().sequence(seq).unwrap().tracks.iter().find(|t| !t.effects).unwrap().id;
    assert!(matches!(doc.edit("add", vec![Op::InsertItem { seq, track: ordinary, item: container(902) }]), Err(EditError::WrongTrackKind)));
}

/// Point tracks live in the project: added, changed and removed as one undoable edit
/// each, and saved with it.
#[test]
fn point_tracks_are_undoable_and_saved() {
    let mut doc = base();
    let mut track = oa_params::PointTrack::new(50, "Ball");
    track.set(secs(1), [0.1, 0.2]);
    doc.edit("track", vec![Op::SetPointTrack { id: 50, track: Some(Arc::new(track.clone())) }]).unwrap();
    assert_eq!(doc.project().tracks.get(&50).map(|t| t.name.as_str()), Some("Ball"));
    let mut moved = track.clone();
    moved.set(secs(2), [0.3, 0.2]);
    doc.edit("more", vec![Op::SetPointTrack { id: 50, track: Some(Arc::new(moved)) }]).unwrap();
    assert_eq!(doc.project().tracks[&50].points.len(), 2);
    let saved = ProjectFile::to_json(doc.project()).unwrap();
    assert_eq!(ProjectFile::from_json(&saved).unwrap().tracks[&50].points.len(), 2);
    doc.undo().unwrap();
    assert_eq!(doc.project().tracks[&50].points.len(), 1);
    doc.undo().unwrap();
    assert!(doc.project().tracks.is_empty());
}
