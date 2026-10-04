//! OpenTimelineIO: a timeline written as a `.otio` file, so the edit can move to another
//! editor (DaVinci Resolve, Kdenlive, and others through OTIO's adapters) for finishing.
//!
//! What carries over is the edit itself: every clip's file, where it sits on which track,
//! which part of its file it plays, gaps, speed changes (`LinearTimeWarp`, `FreezeFrame`),
//! dissolves at cuts, compound clips (as nested stacks), muted clips and tracks. Titles
//! and solids become generator clips of the same length (other editors draw their own);
//! effects, keyframes, masks and color are OpenAtelier's alone, so they're listed in each
//! clip's `metadata.openatelier` for reference rather than recreated. A video clip's own
//! sound gets a matching clip on an audio track, as other editors keep sound on audio
//! tracks. Effect tracks are left out.
//!
//! Times are `RationalTime`s at the timeline's frame rate; a clip's `source_range` is in
//! its file's own time (its timestamps), which is what importers match against the
//! file's `available_range`.

use oa_doc::{Item, ItemKind, MediaId, Project, SeqId, Sequence, TrackKind};
use oa_time::{FrameRate, Time};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Compound clips inside compound clips go this deep at most.
const MAX_DEPTH: usize = 16;

/// `seq` of `project` as an OpenTimelineIO `Timeline`. `media_path` gives each file's
/// location (absolute), and whether it has sound.
pub fn timeline(project: &Project, seq: SeqId, media_path: &dyn Fn(MediaId) -> Option<(PathBuf, bool)>) -> Result<Value, String> {
    let s = project.sequences.get(&seq).ok_or("no such timeline")?;
    let rate = rate(s.rate);
    Ok(json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": s.name,
        "global_start_time": rational(Time::ZERO, rate),
        "metadata": { "openatelier": { "sequence": seq.0 } },
        "tracks": stack(project, s, rate, None, media_path, 0),
    }))
}

/// Writes `seq` to `path` as a `.otio` file.
pub fn write(project: &Project, seq: SeqId, media_path: &dyn Fn(MediaId) -> Option<(PathBuf, bool)>, path: &Path) -> Result<(), String> {
    let value = timeline(project, seq, media_path)?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn rate(r: FrameRate) -> f64 {
    r.num as f64 / r.den.max(1) as f64
}

fn rational(t: Time, rate: f64) -> Value {
    // Whole frames stay whole numbers (29.97 fps times are frames of 1/29.97 s).
    let value = t.as_seconds_f64() * rate;
    let value = if (value - value.round()).abs() < 1e-6 { value.round() } else { value };
    json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": value })
}

fn range(start: Time, duration: Time, rate: f64) -> Value {
    json!({ "OTIO_SCHEMA": "TimeRange.1", "start_time": rational(start, rate), "duration": rational(duration, rate) })
}

/// A sequence's tracks as a `Stack` (bottom track first, as in OTIO), optionally trimmed
/// to `shown` (a compound clip's part of it).
fn stack(project: &Project, s: &Sequence, rate: f64, shown: Option<(Time, Time, &Item)>, media_path: &dyn Fn(MediaId) -> Option<(PathBuf, bool)>, depth: usize) -> Value {
    let mut children = Vec::new();
    // Video tracks bottom to top, then the sound: audio tracks, and a track for the
    // sound of each video track that has some.
    for t in s.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.effects) {
        children.push(track(project, &t.name, "Video", t.enabled, &t.items, rate, media_path, depth, false));
    }
    for t in s.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.effects) {
        let sounding = |i: &Item| match i.kind {
            ItemKind::Media { media } => i.audio_enabled() && media_path(media).is_some_and(|(_, audio)| audio),
            ItemKind::Nested { .. } => i.audio_enabled(),
            _ => false,
        };
        if t.items.iter().any(sounding) {
            let items: Vec<Item> = t.items.iter().filter(|i| sounding(i)).cloned().collect();
            children.push(track(project, &format!("{} sound", t.name), "Audio", t.enabled, &items, rate, media_path, depth, true));
        }
    }
    for t in s.tracks.iter().filter(|t| t.kind == TrackKind::Audio && !t.effects) {
        children.push(track(project, &t.name, "Audio", t.enabled, &t.items, rate, media_path, depth, false));
    }
    let (source_range, name, metadata, effects, enabled) = match shown {
        Some((start, duration, item)) => (range(start, duration, rate), item.name.clone(), item_metadata(item), time_effects(item), item.enabled),
        None => (Value::Null, "tracks".to_string(), json!({}), json!([]), true),
    };
    json!({
        "OTIO_SCHEMA": "Stack.1",
        "name": name,
        "source_range": source_range,
        "effects": effects,
        "markers": [],
        "enabled": enabled,
        "metadata": metadata,
        "children": children,
    })
}

#[allow(clippy::too_many_arguments)]
fn track(
    project: &Project,
    name: &str,
    kind: &str,
    enabled: bool,
    items: &[Item],
    rate: f64,
    media_path: &dyn Fn(MediaId) -> Option<(PathBuf, bool)>,
    depth: usize,
    sound_of_video: bool,
) -> Value {
    let mut children = Vec::new();
    let mut at = Time::ZERO;
    let mut previous_end: Option<Time> = None;
    for item in items {
        if item.range.start > at {
            children.push(gap(item.range.start - at, rate));
        }
        // A dissolve from the clip that ends where this one starts, centered on the cut.
        if let (Some(t), Some(end)) = (&item.transition_in, previous_end)
            && end == item.range.start
            && t.duration > Time::ZERO
        {
            let half = Time(t.duration.0 / 2);
            children.push(json!({
                "OTIO_SCHEMA": "Transition.1",
                "name": t.type_id,
                "transition_type": if t.type_id == "oa.transition.crossfade" { "SMPTE_Dissolve" } else { "Custom_Transition" },
                "in_offset": rational(half, rate),
                "out_offset": rational(t.duration - half, rate),
                "metadata": { "openatelier": { "type": t.type_id } },
            }));
        }
        children.push(clip(project, item, rate, media_path, depth, sound_of_video));
        at = item.range.end();
        previous_end = Some(at);
    }
    json!({
        "OTIO_SCHEMA": "Track.1",
        "name": name,
        "kind": kind,
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": enabled,
        "metadata": {},
        "children": children,
    })
}

fn gap(duration: Time, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "",
        "source_range": range(Time::ZERO, duration, rate),
        "effects": [],
        "markers": [],
        "enabled": true,
        "metadata": {},
    })
}

/// A clip's speed as OTIO effects: a freeze, or a linear time warp (negative: reversed).
fn time_effects(item: &Item) -> Value {
    let speed = item.time_map.speed;
    let scalar = speed.num() as f64 / speed.den().max(1) as f64;
    if speed.num() == 0 {
        json!([{ "OTIO_SCHEMA": "FreezeFrame.1", "name": "", "effect_name": "FreezeFrame", "time_scalar": 0.0, "metadata": {} }])
    } else if (scalar - 1.0).abs() > 1e-9 {
        json!([{ "OTIO_SCHEMA": "LinearTimeWarp.1", "name": "", "effect_name": "LinearTimeWarp", "time_scalar": scalar, "metadata": {} }])
    } else {
        json!([])
    }
}

/// What OpenAtelier knows of a clip that other editors don't: kept for reference.
fn item_metadata(item: &Item) -> Value {
    let effects: Vec<&str> = item.effects.iter().map(|e| e.type_id.as_str()).collect();
    let mut o = json!({ "item": item.id.0 });
    if !effects.is_empty() {
        o["effects"] = json!(effects);
    }
    if !item.masks.is_empty() {
        o["masks"] = json!(item.masks.len());
    }
    if let Some(t) = &item.transition_out {
        o["transition_out"] = json!({ "type": t.type_id, "seconds": t.duration.as_seconds_f64() });
    }
    json!({ "openatelier": o })
}

fn clip(project: &Project, item: &Item, rate: f64, media_path: &dyn Fn(MediaId) -> Option<(PathBuf, bool)>, depth: usize, sound_of_video: bool) -> Value {
    let duration = item.range.duration;
    let start = item.time_map.source_in;
    let reference = match &item.kind {
        ItemKind::Media { media } => {
            let info = project.media(*media).and_then(|m| m.info.clone());
            match media_path(*media) {
                Some((path, _)) => json!({
                    "OTIO_SCHEMA": "ExternalReference.1",
                    "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    "target_url": file_url(&path),
                    "available_range": info.and_then(|i| i.playable_duration()).map_or(Value::Null, |d| range(Time::ZERO, d, rate)),
                    "metadata": {},
                }),
                None => json!({ "OTIO_SCHEMA": "MissingReference.1", "name": item.name, "available_range": null, "metadata": {} }),
            }
        }
        ItemKind::Nested { sequence } => {
            let inner = project.sequences.get(sequence).filter(|_| depth < MAX_DEPTH && !sound_of_video);
            if let Some(inner) = inner {
                return stack(project, inner, rate, Some((start, duration, item)), media_path, depth + 1);
            }
            json!({ "OTIO_SCHEMA": "MissingReference.1", "name": item.name, "available_range": null, "metadata": {} })
        }
        ItemKind::Solid | ItemKind::Text | ItemKind::Adjustment | ItemKind::Plugin { .. } => {
            let kind = match &item.kind {
                ItemKind::Solid => "SolidColor",
                ItemKind::Text => "Text",
                ItemKind::Adjustment => "Adjustment",
                ItemKind::Plugin { type_id, .. } => type_id.as_str(),
                _ => "",
            };
            json!({
                "OTIO_SCHEMA": "GeneratorReference.1",
                "name": item.name,
                "generator_kind": kind,
                "parameters": {},
                "available_range": null,
                "metadata": {},
            })
        }
    };
    // Generators have no file time: their clip starts at 0.
    let start = if matches!(item.kind, ItemKind::Media { .. }) { start } else { Time::ZERO };
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": item.name,
        "source_range": range(start, duration, rate),
        "media_references": { "DEFAULT_MEDIA": reference },
        "active_media_reference_key": "DEFAULT_MEDIA",
        "effects": time_effects(item),
        "markers": [],
        "enabled": item.enabled,
        "metadata": item_metadata(item),
    })
}

/// `path` as a `file://` URL: forward slashes, a slash before a drive letter, and every
/// byte outside the unreserved set (spaces, accents…) percent-encoded.
pub fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = if s.starts_with('/') { s } else { format!("/{s}") };
    let mut out = String::from("file://");
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::*;
    use oa_time::{Rational, TimeRange};
    use std::sync::Arc;

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    /// A timeline with the usual things: two clips of one file with a dissolve at the cut,
    /// a gap, a clip at 2× speed, a title above, a compound clip, and a muted clip.
    fn project() -> (Project, SeqId) {
        let mut p = Project::new("otio");
        let v = FormatVariant { id: VariantId(90), name: "16:9".into(), size: CanvasSize { width: 1920, height: 1080 }, overrides: Default::default() };
        let media = MediaId(7);
        p.media.insert(
            media,
            Arc::new(MediaRef {
                id: media,
                path: "C:/footage/a shot é.mov".into(),
                fingerprint: None,
                info: Some(MediaInfo { width: 1920, height: 1080, duration: secs(20.0), rate: Some(FrameRate::FPS_25), has_video: true, has_audio: true, still: false, color: Default::default(), alpha: false }),
                scaling: Default::default(),
                folder: String::new(),
                color: Default::default(),
            }),
        );
        let clip = |id: u64, start: f64, len: f64, source: f64| {
            let mut i = Item::new(ItemId(id), &format!("clip {id}"), ItemKind::Media { media }, TimeRange::new(secs(start), secs(len)));
            i.time_map = TimeMap::new(secs(source), Rational::new(1, 1));
            i
        };
        // Inside the compound clip: one clip.
        let mut inner = Sequence::new(SeqId(2), "Inner", FrameRate::FPS_25, v.clone());
        let mut it = Track::new(TrackId(20), "V1", TrackKind::Video);
        it.items.push(clip(21, 0.0, 4.0, 10.0));
        inner.tracks.push(Arc::new(it));
        p.sequences.insert(SeqId(2), Arc::new(inner));

        let mut seq = Sequence::new(SeqId(1), "Main", FrameRate::FPS_25, v);
        let mut v1 = Track::new(TrackId(10), "V1", TrackKind::Video);
        v1.items.push(clip(11, 0.0, 2.0, 1.0));
        let mut second = clip(12, 2.0, 2.0, 5.0);
        second.transition_in = Some(Transition::new("oa.transition.crossfade", secs(0.4)));
        v1.items.push(second);
        let mut fast = clip(13, 5.0, 1.0, 8.0);
        fast.time_map = TimeMap::new(secs(8.0), Rational::new(2, 1));
        v1.items.push(fast);
        let mut nested = Item::new(ItemId(14), "Compound", ItemKind::Nested { sequence: SeqId(2) }, TimeRange::new(secs(6.0), secs(3.0)));
        nested.time_map = TimeMap::new(secs(0.5), Rational::new(1, 1));
        v1.items.push(nested);
        let mut v2 = Track::new(TrackId(30), "V2", TrackKind::Video);
        v2.items.push(Item::new(ItemId(31), "Title", ItemKind::Text, TimeRange::new(secs(1.0), secs(2.0))));
        let mut a1 = Track::new(TrackId(40), "A1", TrackKind::Audio);
        let mut muted = clip(41, 0.0, 3.0, 0.0);
        muted.enabled = false;
        a1.items.push(muted);
        seq.tracks = vec![Arc::new(v1), Arc::new(v2), Arc::new(a1)];
        p.sequences.insert(SeqId(1), Arc::new(seq));
        (p, SeqId(1))
    }

    fn path_of(p: &Project) -> impl Fn(MediaId) -> Option<(PathBuf, bool)> + '_ {
        |m| p.media(m).map(|r| (PathBuf::from(&r.path), true))
    }

    #[test]
    fn a_timeline_becomes_otio() {
        let (p, seq) = project();
        let t = timeline(&p, seq, &path_of(&p)).unwrap();
        assert_eq!(t["OTIO_SCHEMA"], "Timeline.1");
        let tracks = t["tracks"]["children"].as_array().unwrap();
        let names: Vec<(&str, &str)> = tracks.iter().map(|t| (t["name"].as_str().unwrap(), t["kind"].as_str().unwrap())).collect();
        assert_eq!(names, vec![("V1", "Video"), ("V2", "Video"), ("V1 sound", "Audio"), ("A1", "Audio")]);

        let v1 = tracks[0]["children"].as_array().unwrap();
        let schemas: Vec<&str> = v1.iter().map(|c| c["OTIO_SCHEMA"].as_str().unwrap()).collect();
        assert_eq!(schemas, vec!["Clip.2", "Transition.1", "Clip.2", "Gap.1", "Clip.2", "Stack.1"]);
        let frames = |v: &Value| v["value"].as_f64().unwrap();
        // The first clip: 2 s (50 frames) from 1 s (frame 25) into the file.
        assert_eq!(frames(&v1[0]["source_range"]["start_time"]), 25.0);
        assert_eq!(frames(&v1[0]["source_range"]["duration"]), 50.0);
        assert_eq!(v1[0]["source_range"]["duration"]["rate"], 25.0);
        let url = v1[0]["media_references"]["DEFAULT_MEDIA"]["target_url"].as_str().unwrap();
        assert_eq!(url, "file:///C:/footage/a%20shot%20%C3%A9.mov");
        // The dissolve, centered on the cut.
        assert_eq!(v1[1]["transition_type"], "SMPTE_Dissolve");
        assert_eq!((frames(&v1[1]["in_offset"]), frames(&v1[1]["out_offset"])), (5.0, 5.0));
        // A 1 s gap, then the fast clip with its time warp.
        assert_eq!(frames(&v1[3]["source_range"]["duration"]), 25.0);
        assert_eq!(v1[4]["effects"][0]["OTIO_SCHEMA"], "LinearTimeWarp.1");
        assert_eq!(v1[4]["effects"][0]["time_scalar"], 2.0);
        // The compound clip: a stack showing 3 s from 0.5 s in, its own track inside.
        assert_eq!(frames(&v1[5]["source_range"]["start_time"]), 12.5);
        assert_eq!(v1[5]["children"][0]["children"][0]["OTIO_SCHEMA"], "Clip.2");
        // The title: a generator after a 1 s gap.
        let v2 = tracks[1]["children"].as_array().unwrap();
        assert_eq!(v2[0]["OTIO_SCHEMA"], "Gap.1");
        assert_eq!(v2[1]["media_references"]["DEFAULT_MEDIA"]["generator_kind"], "Text");
        // Muted stays muted.
        assert_eq!(tracks[3]["children"][0]["enabled"], false);
        // Every track's children add up to where its last clip ends (transitions take
        // no time of their own).
        let length = |t: &Value| -> f64 {
            t["children"].as_array().unwrap().iter().filter(|c| c["OTIO_SCHEMA"] != "Transition.1").map(|c| frames(&c["source_range"]["duration"])).sum()
        };
        assert_eq!(length(&tracks[0]), 225.0);
    }

    #[test]
    fn it_writes_a_file_that_reads_back_as_json() {
        let (p, seq) = project();
        let path = std::env::temp_dir().join(format!("oa-otio-{}.otio", std::process::id()));
        write(&p, seq, &path_of(&p), &path).unwrap();
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back, timeline(&p, seq, &path_of(&p)).unwrap());
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url(Path::new("/home/me/My Clip.mp4")), "file:///home/me/My%20Clip.mp4");
        assert_eq!(file_url(Path::new("C:\\a\\b#1.mov")), "file:///C:/a/b%231.mov");
    }
}
