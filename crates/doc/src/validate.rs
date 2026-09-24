//! Checking — and where it's safe, repairing — a project loaded from disk.
//!
//! Ops maintain every invariant the engine relies on (sorted, non-overlapping items;
//! unique ids; a variant to render; ids below `next_id`), but a file can come from an
//! older build, a crash mid-write, a merge tool or a hand edit. Loading runs [`repair`]
//! so a damaged file opens with a list of what was fixed instead of panicking later in
//! the renderer. Repairs never delete footage decisions silently: overlapping clips move
//! to a new track rather than being trimmed, and everything done is reported.

use crate::format::{AspectPreset, FormatVariant};
use crate::model::*;
use oa_time::{FrameRate, Time};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// What [`repair`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Problems that were fixed.
    pub repaired: Vec<String>,
    /// Problems left in place (the project still opens; the UI should say so).
    pub warnings: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.repaired.is_empty() && self.warnings.is_empty()
    }
}

fn all_ids(p: &Project) -> impl Iterator<Item = u64> + '_ {
    let seqs = p.sequences.values().flat_map(|s| {
        let variants = s.variants.iter().map(|v| v.id.0);
        let tracks = s.tracks.iter().flat_map(|t| {
            std::iter::once(t.id.0).chain(t.items.iter().flat_map(|i| std::iter::once(i.id.0).chain(i.effects.iter().map(|e| e.id.0))))
        });
        std::iter::once(s.id.0).chain(variants).chain(tracks)
    });
    p.media.keys().map(|m| m.0).chain(seqs)
}

/// Checks `p` against the invariants ops maintain and fixes what can be fixed.
pub fn repair(p: &mut Project) -> Report {
    let mut r = Report::default();
    // Derived, not damage: not reported.
    for s in p.sequences.values_mut() {
        if !s.background_in_sync() {
            std::sync::Arc::make_mut(s).sync_background();
        }
    }

    // Ids must stay below `next_id`, or new objects would collide with old ones.
    let max = all_ids(p).max().unwrap_or(0);
    if p.next_id <= max {
        r.repaired.push(format!("next id {} was not above the largest id in use ({max})", p.next_id));
        p.next_id = max + 1;
    }
    let mut fresh = {
        let mut next = p.next_id;
        move || {
            next += 1;
            next - 1
        }
    };

    let seq_ids: Vec<SeqId> = p.sequences.keys().copied().collect();
    let media: BTreeSet<MediaId> = p.media.keys().copied().collect();
    let mut seen_items = BTreeSet::new();
    for id in seq_ids.clone() {
        let s = Arc::make_mut(p.sequences.get_mut(&id).expect("listed"));
        let name = s.name.clone();

        if s.rate.num == 0 || s.rate.den == 0 {
            r.repaired.push(format!("sequence {name:?}: invalid frame rate {}/{}; set to 30", s.rate.num, s.rate.den));
            s.rate = FrameRate::FPS_30;
        }
        if s.variants.is_empty() {
            let preset = AspectPreset::by_id("landscape-16x9").expect("built-in preset");
            let v = FormatVariant { id: VariantId(fresh()), name: preset.name.into(), size: preset.size(1080), overrides: BTreeMap::new() };
            r.repaired.push(format!("sequence {name:?} had no format; added {}", v.name));
            s.variants.push(v);
        }
        let mut variant_ids = BTreeSet::new();
        for v in &mut s.variants {
            if !variant_ids.insert(v.id) {
                let new = VariantId(fresh());
                r.repaired.push(format!("sequence {name:?}: duplicate format id {}; renumbered {:?} to {}", v.id.0, v.name, new.0));
                v.id = new;
                variant_ids.insert(new);
            }
            if v.size.width == 0 || v.size.height == 0 {
                r.repaired.push(format!("format {:?} had a zero size; set to 1920x1080", v.name));
                v.size = crate::format::CanvasSize { width: 1920, height: 1080 };
            }
        }
        if s.variant(s.active_variant).is_none() {
            r.repaired.push(format!("sequence {name:?}: the active format didn't exist; using {:?}", s.variants[0].name));
            s.active_variant = s.variants[0].id;
        }

        let mut track_ids = BTreeSet::new();
        let mut extra_tracks = Vec::new();
        for ti in 0..s.tracks.len() {
            let track = Arc::make_mut(&mut s.tracks[ti]);
            if !track_ids.insert(track.id) {
                let new = TrackId(fresh());
                r.repaired.push(format!("sequence {name:?}: duplicate track id {}; renumbered {:?}", track.id.0, track.name));
                track.id = new;
                track_ids.insert(new);
            }
            let before = track.items.len();
            track.items.retain(|i| i.range.duration > Time::ZERO);
            if track.items.len() < before {
                r.repaired.push(format!("track {:?}: removed {} empty clip(s)", track.name, before - track.items.len()));
            }
            if !track.items.is_sorted_by_key(|i| i.range.start) {
                track.items.sort_by_key(|i| i.range.start);
                r.repaired.push(format!("track {:?}: clips were out of order", track.name));
            }
            for item in &mut track.items {
                if !seen_items.insert(item.id) {
                    let new = ItemId(fresh());
                    r.repaired.push(format!("clip {:?}: duplicate id {}; renumbered to {}", item.name, item.id.0, new.0));
                    item.id = new;
                    seen_items.insert(new);
                }
                repair_item(item, &mut fresh, &mut r);
                match &item.kind {
                    ItemKind::Media { media: m } if !media.contains(m) => {
                        r.warnings.push(format!("clip {:?} refers to media {} which isn't in the project", item.name, m.0));
                    }
                    ItemKind::Nested { sequence } if !seq_ids.contains(sequence) => {
                        r.warnings.push(format!("clip {:?} nests sequence {} which doesn't exist", item.name, sequence.0));
                    }
                    _ => {}
                }
            }
            // Overlaps: keep the first clip where it is, move the others to a new track.
            let mut kept: Vec<Item> = Vec::new();
            let mut moved: Vec<Item> = Vec::new();
            for item in track.items.drain(..) {
                if kept.last().is_some_and(|last| last.range.overlaps(item.range)) {
                    moved.push(item);
                } else {
                    kept.push(item);
                }
            }
            track.items = kept;
            if !moved.is_empty() {
                r.repaired.push(format!("track {:?}: {} overlapping clip(s) moved to a new track", track.name, moved.len()));
                let mut t = Track::new(TrackId(fresh()), &format!("{} (overlaps)", track.name), track.kind);
                t.items = moved;
                extra_tracks.push((ti, t));
            }
        }
        // Insert above their source tracks, from the top down so indexes stay valid.
        for (ti, t) in extra_tracks.into_iter().rev() {
            s.tracks.insert(ti + 1, Arc::new(t));
        }

        // Overrides for clips that no longer exist are dead weight.
        let items: BTreeSet<ItemId> = s.tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
        for v in &mut s.variants {
            let before = v.overrides.len();
            v.overrides.retain(|id, _| items.contains(id));
            if v.overrides.len() < before {
                r.repaired.push(format!("format {:?}: dropped overrides for {} missing clip(s)", v.name, before - v.overrides.len()));
            }
            for (id, set) in &mut v.overrides {
                sanitize_params(set, &format!("override of clip {} in {:?}", id.0, v.name), &mut r);
            }
        }
    }

    p.next_id = p.next_id.max(fresh());
    // Clips moved to a new track can overlap each other; another pass moves them again.
    // Each pass leaves at least one clip per track in place, so this ends.
    if r.repaired.iter().any(|m| m.contains("overlapping")) {
        let again = repair(p);
        r.repaired.extend(again.repaired);
    }

    // Nesting cycles would recurse until the planner's depth limit.
    for id in &seq_ids {
        let nested: Vec<SeqId> = p
            .sequence(*id)
            .into_iter()
            .flat_map(|s| s.tracks.iter())
            .flat_map(|t| t.items.iter())
            .filter_map(|i| if let ItemKind::Nested { sequence } = i.kind { Some(sequence) } else { None })
            .collect();
        if nested.iter().any(|n| p.sequence_reaches(*n, *id)) {
            r.warnings.push(format!("sequence {} contains itself through nesting", id.0));
        }
    }
    r
}

fn repair_item(item: &mut Item, fresh: &mut impl FnMut() -> u64, r: &mut Report) {
    let what = format!("clip {:?}", item.name);
    sanitize_params(&mut item.params, &what, r);
    for end in [ClipEnd::Head, ClipEnd::Tail] {
        let slot = item.transition_mut(end);
        if slot.as_ref().is_some_and(|t| t.duration <= Time::ZERO) {
            *slot = None;
            r.repaired.push(format!("{what}: removed a zero-length {end:?} transition"));
        }
        if let Some(t) = slot {
            sanitize_params(&mut t.params, &format!("{what} {end:?} transition"), r);
        }
    }
    let mut effect_ids = BTreeSet::new();
    for fx in &mut item.effects {
        if !effect_ids.insert(fx.id) {
            let new = EffectId(fresh());
            r.repaired.push(format!("{what}: duplicate effect id {}; renumbered {}", fx.id.0, fx.type_id));
            fx.id = new;
            effect_ids.insert(new);
        }
        sanitize_params(&mut fx.params, &format!("{what} effect {}", fx.type_id), r);
    }
}

fn sanitize_params(set: &mut oa_params::ParamSet, what: &str, r: &mut Report) {
    let mut dropped = Vec::new();
    set.0.retain(|id, src| {
        let ok = src.sanitize();
        if !ok {
            dropped.push(id.as_str().to_string());
        }
        ok
    });
    for id in dropped {
        r.repaired.push(format!("{what}: {id} had no usable value; reset to its default"));
    }
}
