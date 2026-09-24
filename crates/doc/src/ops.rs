use crate::format::{CanvasSize, FormatVariant};
use crate::model::*;
use oa_params::{ParamId, ParamSet, ParamSource};
use oa_time::TimeRange;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Which parameter set a [`Op::SetParam`] edits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParamTarget {
    Item,
    Effect(EffectId),
    /// The item's override in one format variant.
    VariantOverride(VariantId),
    /// The transition on one end of the item.
    Transition(ClipEnd),
}

/// Every change to a project. Ops reference stable ids only, never positions of
/// other objects, so they stay meaningful in logs and (later) collaborative merges.
// `InsertItem` carries a whole item; ops are short-lived and applied one at a time, so
// the size difference between variants costs nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    AddSequence(Arc<Sequence>),
    RemoveSequence(SeqId),
    AddMedia(Arc<MediaRef>),
    RemoveMedia(MediaId),
    /// How a file's pixels are scaled (pixel art / smooth / automatic).
    SetMediaScaling { media: MediaId, scaling: MediaScaling },
    /// Which media-bin folder a file sits in ("" is the top).
    SetMediaFolder { media: MediaId, folder: String },
    /// A file's input color transform (curve, gamut, range, matrix, exposure).
    SetMediaColor { media: MediaId, color: crate::color::InputColor },
    /// The media bin's folders (an empty folder is still a folder).
    SetBinFolders(Vec<String>),
    /// Adds, replaces or (with `None`) removes a point track in the project's library.
    SetPointTrack { id: u64, track: Option<Arc<oa_params::PointTrack>> },
    InsertTrack { seq: SeqId, index: usize, track: Arc<Track> },
    RemoveTrack { seq: SeqId, track: TrackId },
    /// Mute/hide a whole track (disabled tracks don't render or play).
    SetTrackEnabled { seq: SeqId, track: TrackId, enabled: bool },
    SetTrackName { seq: SeqId, track: TrackId, name: String },
    /// Replaces the timeline's dividers (kept sorted by time).
    SetDividers { seq: SeqId, dividers: Vec<Divider> },
    /// Enable or disable one clip.
    SetItemEnabled { seq: SeqId, item: ItemId, enabled: bool },
    /// The clip's outro plays its intro backwards (instead of its own outro effects).
    SetReverseIntro { seq: SeqId, item: ItemId, on: bool },
    /// Puts a clip in a group (clips sharing an id move and copy together) or, with
    /// `None`, takes it out.
    SetItemGroup { seq: SeqId, item: ItemId, group: Option<u64> },
    InsertItem { seq: SeqId, track: TrackId, item: Item },
    RemoveItem { seq: SeqId, item: ItemId },
    SetItemTiming { seq: SeqId, item: ItemId, range: TimeRange, time_map: TimeMap },
    SetParam { seq: SeqId, item: ItemId, target: ParamTarget, param: ParamId, source: Option<ParamSource> },
    InsertEffect { seq: SeqId, item: ItemId, index: usize, effect: EffectInstance },
    RemoveEffect { seq: SeqId, item: ItemId, effect: EffectId },
    SetEffectEnabled { seq: SeqId, item: ItemId, effect: EffectId, enabled: bool },
    SetEffectRole { seq: SeqId, item: ItemId, effect: EffectId, role: EffectRole },
    /// Adds, replaces or (with `None`) removes the transition on one end of a clip.
    SetTransition { seq: SeqId, item: ItemId, end: ClipEnd, transition: Option<Transition> },
    InsertVariant { seq: SeqId, index: usize, variant: FormatVariant, make_active: bool },
    RemoveVariant { seq: SeqId, variant: VariantId },
    SetVariantSize { seq: SeqId, variant: VariantId, size: CanvasSize },
    SetVariantName { seq: SeqId, variant: VariantId, name: String },
    SetActiveVariant { seq: SeqId, variant: VariantId },
    /// Sets (or with `None` clears) a sequence-wide setting, e.g. its background.
    SetSequenceParam { seq: SeqId, param: ParamId, source: Option<ParamSource> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    NotFound(&'static str, u64),
    AlreadyExists(&'static str, u64),
    Overlap,
    WrongTrackKind,
    InUse(&'static str, u64),
    NestingCycle,
    LastVariant,
    /// The edit (or undo) would leave the project with no timeline at all.
    LastSequence,
    /// A number that isn't finite (NaN, infinity), a curve with no keys, or a zero size.
    InvalidValue,
    InvalidRange,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NotFound(what, id) => write!(f, "{what} {id} not found"),
            EditError::AlreadyExists(what, id) => write!(f, "{what} {id} already exists"),
            EditError::Overlap => write!(f, "item would overlap another item on the track"),
            EditError::WrongTrackKind => write!(f, "item kind does not belong on this track"),
            EditError::InUse(what, id) => write!(f, "{what} {id} is still in use"),
            EditError::NestingCycle => write!(f, "a sequence cannot contain itself"),
            EditError::LastVariant => write!(f, "a sequence needs at least one format variant"),
            EditError::LastSequence => write!(f, "a project needs at least one timeline"),
            EditError::InvalidValue => write!(f, "that value isn't valid"),
            EditError::InvalidRange => write!(f, "item duration must be positive"),
        }
    }
}

impl std::error::Error for EditError {}

type R<T> = Result<T, EditError>;

fn seq_mut(p: &mut Project, id: SeqId) -> R<&mut Sequence> {
    p.sequences.get_mut(&id).map(Arc::make_mut).ok_or(EditError::NotFound("sequence", id.0))
}

fn item_mut(p: &mut Project, seq: SeqId, id: ItemId) -> R<&mut Item> {
    let s = seq_mut(p, seq)?;
    if id == crate::model::BACKGROUND {
        return Ok(&mut s.background);
    }
    let (t, i) = s.find_item(id).ok_or(EditError::NotFound("item", id.0))?;
    Ok(&mut Arc::make_mut(&mut s.tracks[t]).items[i])
}

fn variant_mut(s: &mut Sequence, id: VariantId) -> R<&mut FormatVariant> {
    s.variants.iter_mut().find(|v| v.id == id).ok_or(EditError::NotFound("variant", id.0))
}

fn check_nesting(p: &Project, seq: SeqId, items: &[Item]) -> R<()> {
    for it in items {
        if let ItemKind::Nested { sequence } = it.kind {
            if p.sequence(sequence).is_none() {
                return Err(EditError::NotFound("sequence", sequence.0));
            }
            if p.sequence_reaches(sequence, seq) {
                return Err(EditError::NestingCycle);
            }
        }
    }
    Ok(())
}

fn kind_fits(track: &Track, item: &ItemKind) -> bool {
    // Effect tracks hold effect containers, and only they do.
    if track.effects || matches!(item, ItemKind::Adjustment) {
        return track.effects && matches!(item, ItemKind::Adjustment);
    }
    match track.kind {
        TrackKind::Video => true,
        TrackKind::Audio => matches!(item, ItemKind::Media { .. } | ItemKind::Nested { .. } | ItemKind::Plugin { .. }),
    }
}

fn params_valid(set: &ParamSet) -> bool {
    set.0.values().all(ParamSource::is_valid)
}

fn item_valid(it: &Item) -> bool {
    params_valid(&it.params) && it.effects.iter().all(|e| params_valid(&e.params))
}

impl Op {
    /// Values no edit may store, whatever the UI let through: non-finite numbers, curves
    /// without keys, empty canvases. Refused before anything changes.
    fn validate(&self) -> R<()> {
        let ok = match self {
            Op::SetParam { source: Some(src), .. } | Op::SetSequenceParam { source: Some(src), .. } => src.is_valid(),
            Op::SetMediaColor { color, .. } => color.exposure.is_finite(),
            Op::SetVariantSize { size, .. } => size.width > 0 && size.height > 0,
            Op::InsertVariant { variant, .. } => variant.size.width > 0 && variant.size.height > 0,
            Op::InsertItem { item, .. } => item_valid(item),
            Op::InsertEffect { effect, .. } => params_valid(&effect.params),
            _ => true,
        };
        if ok { Ok(()) } else { Err(EditError::InvalidValue) }
    }

    /// Applies the op and returns the ops that undo it (to be applied in order).
    /// On error the project is left unchanged.
    pub fn apply(self, p: &mut Project) -> R<Vec<Op>> {
        self.validate()?;
        Ok(match self {
            Op::AddSequence(s) => {
                if p.sequences.contains_key(&s.id) {
                    return Err(EditError::AlreadyExists("sequence", s.id.0));
                }
                let id = s.id;
                p.sequences.insert(id, s);
                vec![Op::RemoveSequence(id)]
            }
            Op::RemoveSequence(id) => {
                if p.sequences.keys().any(|&other| other != id && p.sequence_reaches(other, id)) {
                    return Err(EditError::InUse("sequence", id.0));
                }
                let s = p.sequences.remove(&id).ok_or(EditError::NotFound("sequence", id.0))?;
                vec![Op::AddSequence(s)]
            }
            Op::AddMedia(m) => {
                if p.media.contains_key(&m.id) {
                    return Err(EditError::AlreadyExists("media", m.id.0));
                }
                let id = m.id;
                p.media.insert(id, m);
                vec![Op::RemoveMedia(id)]
            }
            Op::RemoveMedia(id) => {
                let used = p.sequences.values().flat_map(|s| s.tracks.iter()).flat_map(|t| t.items.iter());
                if used.into_iter().any(|i| i.kind == ItemKind::Media { media: id }) {
                    return Err(EditError::InUse("media", id.0));
                }
                let m = p.media.remove(&id).ok_or(EditError::NotFound("media", id.0))?;
                vec![Op::AddMedia(m)]
            }
            Op::InsertTrack { seq, index, track } => {
                check_nesting(p, seq, &track.items)?;
                let s = seq_mut(p, seq)?;
                if s.track(track.id).is_some() {
                    return Err(EditError::AlreadyExists("track", track.id.0));
                }
                let id = track.id;
                s.tracks.insert(index.min(s.tracks.len()), track);
                vec![Op::RemoveTrack { seq, track: id }]
            }
            Op::RemoveTrack { seq, track } => {
                let s = seq_mut(p, seq)?;
                let index = s.tracks.iter().position(|t| t.id == track).ok_or(EditError::NotFound("track", track.0))?;
                let track = s.tracks.remove(index);
                vec![Op::InsertTrack { seq, index, track }]
            }
            Op::SetTrackEnabled { seq, track, enabled } => {
                let s = seq_mut(p, seq)?;
                let t = s.tracks.iter_mut().find(|t| t.id == track).ok_or(EditError::NotFound("track", track.0))?;
                let old = std::mem::replace(&mut Arc::make_mut(t).enabled, enabled);
                vec![Op::SetTrackEnabled { seq, track, enabled: old }]
            }
            Op::SetTrackName { seq, track, name } => {
                let s = seq_mut(p, seq)?;
                let t = s.tracks.iter_mut().find(|t| t.id == track).ok_or(EditError::NotFound("track", track.0))?;
                let old = std::mem::replace(&mut Arc::make_mut(t).name, name);
                vec![Op::SetTrackName { seq, track, name: old }]
            }
            Op::SetDividers { seq, mut dividers } => {
                if dividers.iter().any(|d| d.at < oa_time::Time::ZERO) {
                    return Err(EditError::InvalidRange);
                }
                dividers.sort_by_key(|d| (d.at, d.id));
                let old = std::mem::replace(&mut seq_mut(p, seq)?.dividers, dividers);
                vec![Op::SetDividers { seq, dividers: old }]
            }
            Op::SetItemEnabled { seq, item, enabled } => {
                let it = item_mut(p, seq, item)?;
                let old = std::mem::replace(&mut it.enabled, enabled);
                vec![Op::SetItemEnabled { seq, item, enabled: old }]
            }
            Op::SetReverseIntro { seq, item, on } => {
                let it = item_mut(p, seq, item)?;
                let old = std::mem::replace(&mut it.outro_reverses_intro, on);
                vec![Op::SetReverseIntro { seq, item, on: old }]
            }
            Op::SetItemGroup { seq, item, group } => {
                let it = item_mut(p, seq, item)?;
                let old = std::mem::replace(&mut it.group, group);
                vec![Op::SetItemGroup { seq, item, group: old }]
            }
            Op::InsertItem { seq, track, item } => {
                if item.range.duration <= oa_time::Time::ZERO {
                    return Err(EditError::InvalidRange);
                }
                check_nesting(p, seq, std::slice::from_ref(&item))?;
                let s = seq_mut(p, seq)?;
                if s.find_item(item.id).is_some() {
                    return Err(EditError::AlreadyExists("item", item.id.0));
                }
                let t = s.tracks.iter_mut().find(|t| t.id == track).ok_or(EditError::NotFound("track", track.0))?;
                if !kind_fits(t, &item.kind) {
                    return Err(EditError::WrongTrackKind);
                }
                let index = t.insertion_index(item.range, None).ok_or(EditError::Overlap)?;
                let id = item.id;
                Arc::make_mut(t).items.insert(index, item);
                vec![Op::RemoveItem { seq, item: id }]
            }
            Op::RemoveItem { seq, item } => {
                let s = seq_mut(p, seq)?;
                let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
                let track = Arc::make_mut(&mut s.tracks[ti]);
                let removed = track.items.remove(ii);
                vec![Op::InsertItem { seq, track: track.id, item: removed }]
            }
            Op::SetItemTiming { seq, item, range, time_map } => {
                if range.duration <= oa_time::Time::ZERO {
                    return Err(EditError::InvalidRange);
                }
                let s = seq_mut(p, seq)?;
                let (ti, ii) = s.find_item(item).ok_or(EditError::NotFound("item", item.0))?;
                let track = Arc::make_mut(&mut s.tracks[ti]);
                let new_index = track.insertion_index(range, Some(item)).ok_or(EditError::Overlap)?;
                let mut it = track.items.remove(ii);
                let old = Op::SetItemTiming { seq, item, range: it.range, time_map: it.time_map };
                it.range = range;
                it.time_map = time_map;
                let at = if new_index > ii { new_index - 1 } else { new_index };
                track.items.insert(at, it);
                vec![old]
            }
            Op::SetParam { seq, item, target, param, source } => {
                fn swap(set: &mut ParamSet, param: &ParamId, source: Option<ParamSource>) -> Option<ParamSource> {
                    match source {
                        Some(src) => set.0.insert(param.clone(), src),
                        None => set.0.remove(param),
                    }
                }
                let previous = match &target {
                    ParamTarget::Item => swap(&mut item_mut(p, seq, item)?.params, &param, source),
                    ParamTarget::Effect(e) => {
                        let it = item_mut(p, seq, item)?;
                        let fx = it.effects.iter_mut().find(|x| x.id == *e).ok_or(EditError::NotFound("effect", e.0))?;
                        swap(&mut fx.params, &param, source)
                    }
                    ParamTarget::Transition(end) => {
                        let it = item_mut(p, seq, item)?;
                        let tr = it.transition_mut(*end).as_mut().ok_or(EditError::NotFound("transition", item.0))?;
                        swap(&mut tr.params, &param, source)
                    }
                    ParamTarget::VariantOverride(v) => {
                        let s = seq_mut(p, seq)?;
                        if s.find_item(item).is_none() {
                            return Err(EditError::NotFound("item", item.0));
                        }
                        let overrides = &mut variant_mut(s, *v)?.overrides;
                        let previous = swap(overrides.entry(item).or_default(), &param, source);
                        if overrides.get(&item).is_some_and(|o| o.0.is_empty()) {
                            overrides.remove(&item);
                        }
                        previous
                    }
                };
                vec![Op::SetParam { seq, item, target, param, source: previous }]
            }
            Op::InsertEffect { seq, item, index, effect } => {
                let it = item_mut(p, seq, item)?;
                if it.effects.iter().any(|e| e.id == effect.id) {
                    return Err(EditError::AlreadyExists("effect", effect.id.0));
                }
                let id = effect.id;
                it.effects.insert(index.min(it.effects.len()), effect);
                vec![Op::RemoveEffect { seq, item, effect: id }]
            }
            Op::SetEffectEnabled { seq, item, effect, enabled } => {
                let it = item_mut(p, seq, item)?;
                let fx = it.effects.iter_mut().find(|x| x.id == effect).ok_or(EditError::NotFound("effect", effect.0))?;
                let old = std::mem::replace(&mut fx.enabled, enabled);
                vec![Op::SetEffectEnabled { seq, item, effect, enabled: old }]
            }
            Op::SetEffectRole { seq, item, effect, role } => {
                if let EffectRole::In { duration } | EffectRole::Out { duration } = role
                    && duration <= oa_time::Time::ZERO
                {
                    return Err(EditError::InvalidRange);
                }
                let it = item_mut(p, seq, item)?;
                let fx = it.effects.iter_mut().find(|x| x.id == effect).ok_or(EditError::NotFound("effect", effect.0))?;
                let old = std::mem::replace(&mut fx.role, role);
                vec![Op::SetEffectRole { seq, item, effect, role: old }]
            }
            Op::SetTransition { seq, item, end, transition } => {
                if transition.as_ref().is_some_and(|t| t.duration <= oa_time::Time::ZERO) {
                    return Err(EditError::InvalidRange);
                }
                let it = item_mut(p, seq, item)?;
                let old = std::mem::replace(it.transition_mut(end), transition);
                vec![Op::SetTransition { seq, item, end, transition: old }]
            }
            Op::RemoveEffect { seq, item, effect } => {
                let it = item_mut(p, seq, item)?;
                let index = it.effects.iter().position(|e| e.id == effect).ok_or(EditError::NotFound("effect", effect.0))?;
                let effect = it.effects.remove(index);
                vec![Op::InsertEffect { seq, item, index, effect }]
            }
            Op::InsertVariant { seq, index, variant, make_active } => {
                let s = seq_mut(p, seq)?;
                if s.variant(variant.id).is_some() {
                    return Err(EditError::AlreadyExists("variant", variant.id.0));
                }
                let (id, prev_active) = (variant.id, s.active_variant);
                s.variants.insert(index.min(s.variants.len()), variant);
                let mut undo = vec![Op::RemoveVariant { seq, variant: id }];
                if make_active {
                    s.active_variant = id;
                    undo.push(Op::SetActiveVariant { seq, variant: prev_active });
                }
                undo
            }
            Op::RemoveVariant { seq, variant } => {
                let s = seq_mut(p, seq)?;
                if s.variants.len() <= 1 {
                    return Err(EditError::LastVariant);
                }
                let index = s.variants.iter().position(|v| v.id == variant).ok_or(EditError::NotFound("variant", variant.0))?;
                let removed = s.variants.remove(index);
                let was_active = s.active_variant == variant;
                if was_active {
                    s.active_variant = s.variants[0].id;
                }
                vec![Op::InsertVariant { seq, index, variant: removed, make_active: was_active }]
            }
            Op::SetVariantSize { seq, variant, size } => {
                let v = variant_mut(seq_mut(p, seq)?, variant)?;
                let old = std::mem::replace(&mut v.size, size);
                vec![Op::SetVariantSize { seq, variant, size: old }]
            }
            Op::SetVariantName { seq, variant, name } => {
                let v = variant_mut(seq_mut(p, seq)?, variant)?;
                let old = std::mem::replace(&mut v.name, name);
                vec![Op::SetVariantName { seq, variant, name: old }]
            }
            Op::SetActiveVariant { seq, variant } => {
                let s = seq_mut(p, seq)?;
                if s.variant(variant).is_none() {
                    return Err(EditError::NotFound("variant", variant.0));
                }
                let old = std::mem::replace(&mut s.active_variant, variant);
                vec![Op::SetActiveVariant { seq, variant: old }]
            }
            Op::SetMediaScaling { media, scaling } => {
                let m = p.media.get_mut(&media).ok_or(EditError::NotFound("media", media.0))?;
                let old = std::mem::replace(&mut Arc::make_mut(m).scaling, scaling);
                vec![Op::SetMediaScaling { media, scaling: old }]
            }
            Op::SetMediaFolder { media, folder } => {
                let m = p.media.get_mut(&media).ok_or(EditError::NotFound("media", media.0))?;
                let old = std::mem::replace(&mut Arc::make_mut(m).folder, folder);
                vec![Op::SetMediaFolder { media, folder: old }]
            }
            Op::SetMediaColor { media, color } => {
                let m = p.media.get_mut(&media).ok_or(EditError::NotFound("media", media.0))?;
                let old = std::mem::replace(&mut Arc::make_mut(m).color, color);
                vec![Op::SetMediaColor { media, color: old }]
            }
            Op::SetBinFolders(folders) => {
                let old = std::mem::replace(&mut p.bin_folders, folders);
                vec![Op::SetBinFolders(old)]
            }
            Op::SetPointTrack { id, track } => {
                let old = match track {
                    Some(t) => p.tracks.insert(id, t),
                    None => p.tracks.remove(&id),
                };
                vec![Op::SetPointTrack { id, track: old }]
            }
            Op::SetSequenceParam { seq, param, source } => {
                let s = seq_mut(p, seq)?;
                let old = match source {
                    Some(src) => s.params.0.insert(param.clone(), src),
                    None => s.params.0.remove(&param),
                };
                vec![Op::SetSequenceParam { seq, param, source: old }]
            }
        })
    }
}
