#![allow(dead_code)]

use oa_doc::*;
use oa_graph::registry::Registry;
use oa_plan::{plan_frame, PlanOptions};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const SEQ: SeqId = SeqId(1);
pub const WIDE: VariantId = VariantId(2);
pub const TALL: VariantId = VariantId(3);
pub const V1: TrackId = TrackId(4);
pub const V2: TrackId = TrackId(5);
pub const MEDIA: MediaId = MediaId(6);
pub const STILL: MediaId = MediaId(7);

pub fn secs(s: f64) -> Time {
    Time::from_seconds_f64(s)
}

fn variant(id: VariantId, preset: &str) -> FormatVariant {
    let p = AspectPreset::by_id(preset).unwrap();
    FormatVariant { id, name: p.name.into(), size: p.size(1080), overrides: BTreeMap::new() }
}

/// 1920x1080 + 1080x1920 variants, two video tracks, a 20 s 4K video and a still.
pub fn project() -> Project {
    let mut p = Project::new("t");
    p.next_id = 1000;
    let mut seq = Sequence::new(SEQ, "Main", FrameRate::FPS_30, variant(WIDE, "landscape-16x9"));
    seq.variants.push(variant(TALL, "vertical-9x16"));
    seq.tracks.push(Arc::new(Track::new(V1, "V1", TrackKind::Video)));
    seq.tracks.push(Arc::new(Track::new(V2, "V2", TrackKind::Video)));
    p.sequences.insert(SEQ, Arc::new(seq));
    let video = MediaInfo {
        width: 3840,
        height: 2160,
        duration: secs(20.0),
        rate: Some(FrameRate::FPS_30),
        has_video: true,
        has_audio: true,
        still: false,
        ..Default::default()
    };
    let still = MediaInfo { width: 800, height: 800, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() };
    for (id, info) in [(MEDIA, video), (STILL, still)] {
        p.media.insert(id, Arc::new(MediaRef { id, path: format!("{}.bin", id.0), fingerprint: Some(format!("fp{}", id.0)), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }));
    }
    p
}

/// A `Document` around [`project`] with clips placed by `(track, id, media, start, duration, source_in)`.
pub fn doc_with(clips: &[(TrackId, u64, MediaId, f64, f64, f64)]) -> Document {
    let mut doc = Document::new(project());
    let ops = clips
        .iter()
        .map(|&(track, id, media, start, dur, source_in)| {
            let mut item = Item::new(ItemId(id), &format!("clip{id}"), ItemKind::Media { media }, TimeRange::new(secs(start), secs(dur)));
            item.time_map.source_in = secs(source_in);
            Op::InsertItem { seq: SEQ, track, item }
        })
        .collect();
    doc.edit("setup", ops).unwrap();
    doc
}

pub fn item(doc: &Document, id: u64) -> Item {
    doc.project().sequence(SEQ).unwrap().item(ItemId(id)).cloned().expect("item exists")
}

/// The render graph's output cache key at `t` — equal keys mean identical frames.
pub fn frame_key(p: &Project, t: Time, variant: VariantId) -> String {
    let opts = PlanOptions { variant: Some(variant), ..Default::default() };
    let plan = plan_frame(p, SEQ, t, &opts, &Registry::with_builtins()).unwrap();
    format!("{:?}", plan.graph.node(plan.graph.output).key)
}
