use crate::color::{ColorTags, InputColor, Transfer, Gamut};
use crate::format::{CanvasSize, FormatVariant};
use oa_params::{EvalContext, ParamSet};
use oa_time::{FrameRate, Rational, Time, TimeRange};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

macro_rules! id_type {
    ($($name:ident),*) => {$(
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    )*};
}

id_type!(SeqId, TrackId, ItemId, MediaId, EffectId, VariantId);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub sequences: BTreeMap<SeqId, Arc<Sequence>>,
    pub media: BTreeMap<MediaId, Arc<MediaRef>>,
    /// Media-bin folders, as paths ("b-roll", "b-roll/day 2"). Kept here so a folder
    /// can exist before anything is in it, and survive everything being moved out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bin_folders: Vec<String>,
    /// Point tracks, by id: paths things in the picture take, which positions can
    /// follow (a following property carries the track too; see `Modulator::Track`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tracks: BTreeMap<u64, Arc<oa_params::PointTrack>>,
    /// Ids are never reused, even after deletion or undo.
    pub next_id: u64,
}

impl Project {
    pub fn new(name: &str) -> Self {
        Project { name: name.into(), next_id: 1, ..Default::default() }
    }

    /// Whether any property anywhere is connected to the sound (so planning frames needs
    /// loudness envelopes).
    pub fn follows_sound(&self) -> bool {
        let set = |p: &ParamSet| p.0.values().any(|s| s.follows_sound());
        self.sequences.values().any(|s| {
            set(&s.params)
                || std::iter::once(&s.background)
                    .chain(s.tracks.iter().flat_map(|t| t.items.iter()))
                    .any(|i| set(&i.params) || i.effects.iter().any(|e| set(&e.params)) || [&i.transition_in, &i.transition_out].into_iter().flatten().any(|t| set(&t.params)))
        })
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn sequence(&self, id: SeqId) -> Option<&Sequence> {
        self.sequences.get(&id).map(|s| &**s)
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaRef> {
        self.media.get(&id).map(|m| &**m)
    }

    /// True if `from` is `to` or contains it through nested sequences.
    pub fn sequence_reaches(&self, from: SeqId, to: SeqId) -> bool {
        let mut stack = vec![from];
        let mut seen = Vec::new();
        while let Some(s) = stack.pop() {
            if s == to {
                return true;
            }
            if seen.contains(&s) {
                continue;
            }
            seen.push(s);
            if let Some(seq) = self.sequence(s) {
                for t in &seq.tracks {
                    for i in &t.items {
                        if let ItemKind::Nested { sequence } = i.kind {
                            stack.push(sequence);
                        }
                    }
                }
            }
        }
        false
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SeqId,
    pub name: String,
    pub rate: FrameRate,
    /// At least one. Timing is shared; canvas size and visual overrides are not.
    pub variants: Vec<FormatVariant>,
    pub active_variant: VariantId,
    /// Bottom to top.
    pub tracks: Vec<Arc<Track>>,
    /// Sequence-wide settings (`schema::background()`: what shows behind the clips),
    /// keyframable on the sequence's own clock.
    #[serde(default, skip_serializing_if = "ParamSet::is_empty")]
    pub params: ParamSet,
    /// The background as a clip of its own (id [`BACKGROUND`], on no track, as long as
    /// time): it holds the background's effects, which run on it before the clips are
    /// drawn over it. Being an `Item`, its effects are edited, keyframed and reordered
    /// with the same ops and UI as any clip's.
    #[serde(default = "background_item", skip_serializing_if = "Item::is_plain_background")]
    pub background: Item,
    /// Markers across every track (sorted by time) cutting the timeline into sections
    /// that can be moved, emptied or deleted as a whole. Each starts a section and gives
    /// it a color.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dividers: Vec<Divider>,
}

/// The id of every sequence's background pseudo-clip ([`Sequence::background`]). Real
/// ids start at 1, so it never names a clip.
pub const BACKGROUND: ItemId = ItemId(0);

fn background_item() -> Item {
    // Its clock is the sequence's own (it starts at 0); its length follows the timeline
    // (`Sequence::sync_background`), so keyframe and wave editors show the timeline.
    Item::new(BACKGROUND, "Background", ItemKind::Adjustment, TimeRange::new(Time::ZERO, Time::from_seconds(1)))
}

impl Sequence {
    pub fn new(id: SeqId, name: &str, rate: FrameRate, first_variant: FormatVariant) -> Self {
        Sequence {
            id,
            name: name.into(),
            rate,
            active_variant: first_variant.id,
            variants: vec![first_variant],
            tracks: Vec::new(),
            params: ParamSet::default(),
            background: background_item(),
            dividers: Vec::new(),
        }
    }

    pub fn variant(&self, id: VariantId) -> Option<&FormatVariant> {
        self.variants.iter().find(|v| v.id == id)
    }

    pub fn active(&self) -> &FormatVariant {
        self.variant(self.active_variant).unwrap_or(&self.variants[0])
    }

    pub fn canvas(&self) -> CanvasSize {
        self.active().size
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id).map(|t| &**t)
    }

    pub fn find_item(&self, id: ItemId) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.items.iter().position(|i| i.id == id).map(|ii| (ti, ii)))
    }

    /// A clip by id — or, for [`BACKGROUND`], the background pseudo-clip.
    pub fn item(&self, id: ItemId) -> Option<&Item> {
        if id == BACKGROUND {
            return Some(&self.background);
        }
        self.find_item(id).map(|(t, i)| &self.tracks[t].items[i])
    }

    /// The background's length: the timeline's (at least a second). Its effects still
    /// run past the end — a passive effect is never outside its clip's window.
    pub fn background_length(&self) -> Time {
        self.duration().max(Time::from_seconds(1))
    }

    /// Whether the background's length still matches the timeline.
    pub fn background_in_sync(&self) -> bool {
        self.background.range == TimeRange::new(Time::ZERO, self.background_length())
    }

    /// Makes the background as long as the timeline. Every edit ends with this (and a
    /// loaded file is repaired to it), so it's derived data, never an edit of its own.
    pub fn sync_background(&mut self) {
        self.background.range = TimeRange::new(Time::ZERO, self.background_length());
    }

    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .filter_map(|t| t.items.last().map(|i| i.range.end()))
            .max()
            .unwrap_or(Time::ZERO)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub enabled: bool,
    /// Sorted by start; never overlapping.
    pub items: Vec<Item>,
    /// An **effect track**: thin, holding only effect containers (`ItemKind::Adjustment`),
    /// whose effects run over everything below it — the picture composited under it for
    /// a video one, the sound mixed below it for an audio one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub effects: bool,
}

/// A mark across the whole timeline at `at`: the start of a section, colored `color`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divider {
    pub id: u64,
    pub at: Time,
    /// sRGB.
    pub color: [u8; 3],
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl Track {
    pub fn new(id: TrackId, name: &str, kind: TrackKind) -> Self {
        Track { id, name: name.into(), kind, enabled: true, items: Vec::new(), effects: false }
    }
    /// An effect track of `kind`: its containers' effects run over everything below it.
    pub fn effects(id: TrackId, name: &str, kind: TrackKind) -> Self {
        Track { effects: true, ..Track::new(id, name, kind) }
    }


    /// Where the track's last clip ends.
    pub fn end(&self) -> Time {
        self.items.iter().map(|i| i.range.end()).max().unwrap_or(Time::ZERO)
    }

    /// The item covering `t`, in O(log n).
    pub fn item_at(&self, t: Time) -> Option<&Item> {
        let i = self.items.partition_point(|it| it.range.start <= t);
        i.checked_sub(1).map(|i| &self.items[i]).filter(|it| it.range.contains(t))
    }

    /// Index to insert `range` at, or `None` if it would overlap an existing item.
    pub(crate) fn insertion_index(&self, range: TimeRange, ignore: Option<ItemId>) -> Option<usize> {
        if self.items.iter().any(|i| Some(i.id) != ignore && i.range.overlaps(range)) {
            return None;
        }
        Some(self.items.partition_point(|it| it.range.start < range.start))
    }
}

/// Maps clip-local time to source time: `source = source_in + floor(local * speed)`.
/// Speed 0 is a freeze frame; negative speed plays in reverse.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeMap {
    pub source_in: Time,
    pub speed: Rational,
}

impl Default for TimeMap {
    fn default() -> Self {
        TimeMap { source_in: Time::ZERO, speed: Rational::ONE }
    }
}

impl TimeMap {
    pub fn source_time(&self, local: Time) -> Time {
        self.source_in + Time::from_rational_floor(local.as_rational() * self.speed)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ItemKind {
    Media { media: MediaId },
    Solid,
    Text,
    /// On an effect track, an **effect container**: no picture or sound of its own, just
    /// effects, run over everything below the track while it lasts. (Also the kind of
    /// the sequence's Background pseudo-clip.)
    Adjustment,
    Nested { sequence: SeqId },
    /// A clip type provided by a plugin. Unknown plugins keep their data intact.
    Plugin { type_id: String, version: u32, data: serde_json::Value },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub name: String,
    pub range: TimeRange,
    pub kind: ItemKind,
    #[serde(default)]
    pub time_map: TimeMap,
    /// Built-in params (see [`crate::schema`]) plus clip-type params.
    #[serde(default)]
    pub params: ParamSet,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    pub enabled: bool,
    /// Transition at the head: from the clip that ends exactly where this one starts
    /// (centered on the cut), or in from nothing when there's no such clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_in: Option<Transition>,
    /// Transition out to nothing at the tail. Ignored when a clip follows directly
    /// (that cut belongs to the next clip's `transition_in`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_out: Option<Transition>,
    /// The outro is the intro played backwards: every intro effect also runs over the
    /// clip's end with time reversed, and the clip's own outro effects are ignored.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outro_reverses_intro: bool,
    /// Clips sharing a group id are selected, moved and copied together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
    /// When each word of a title is spoken, on the clip's clock (captions have them from
    /// the transcript). Titles without them spread their words evenly over the clip.
    /// Drives "highlight when spoken" (`schema::spoken`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub word_times: Vec<TimeRange>,
}

impl Item {
    pub fn new(id: ItemId, name: &str, kind: ItemKind, range: TimeRange) -> Self {
        Item {
            id,
            name: name.into(),
            range,
            kind,
            time_map: TimeMap::default(),
            params: ParamSet::default(),
            effects: Vec::new(),
            enabled: true,
            transition_in: None,
            transition_out: None,
            outro_reverses_intro: false,
            group: None,
            word_times: Vec::new(),
        }
    }

    pub fn transition(&self, end: ClipEnd) -> Option<&Transition> {
        match end {
            ClipEnd::Head => self.transition_in.as_ref(),
            ClipEnd::Tail => self.transition_out.as_ref(),
        }
    }

    pub fn transition_mut(&mut self, end: ClipEnd) -> &mut Option<Transition> {
        match end {
            ClipEnd::Head => &mut self.transition_in,
            ClipEnd::Tail => &mut self.transition_out,
        }
    }

    /// Whether the clip's own sound plays (off once it's been extracted to an audio clip).
    pub fn audio_enabled(&self) -> bool {
        !matches!(
            self.params.get(crate::schema::AUDIO_ENABLED).map(|s| s.eval(&self.eval_context(self.range.start))),
            Some(oa_params::Value::Bool(false))
        )
    }

    /// The effects that actually run: the clip's own, except that with
    /// [`Item::outro_reverses_intro`] its outros are replaced by its intros played backwards.
    pub fn active_effects(&self) -> std::borrow::Cow<'_, [EffectInstance]> {
        if !self.outro_reverses_intro {
            return std::borrow::Cow::Borrowed(&self.effects);
        }
        let mut out: Vec<EffectInstance> = self.effects.iter().filter(|e| !matches!(e.role, EffectRole::Out { .. })).cloned().collect();
        for e in &self.effects {
            if let EffectRole::In { duration } = e.role {
                out.push(EffectInstance { role: EffectRole::Reversed { duration }, ..e.clone() });
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// A background with nothing on it (not worth writing into the file).
    pub fn is_plain_background(&self) -> bool {
        self.effects.is_empty() && self.params.is_empty()
    }

    /// The word being spoken at clip-local time `local`: by `word_times`, or — without
    /// them — `words` spread evenly over the clip. `None` before the first word.
    pub fn spoken_word(&self, local: Time, words: usize) -> Option<usize> {
        if !self.word_times.is_empty() {
            // The last word that has started (it stays lit through the pause after it).
            return self.word_times.iter().rposition(|w| w.start <= local);
        }
        if words == 0 || self.range.duration <= Time::ZERO || local < Time::ZERO {
            return None;
        }
        let k = local.0 as f64 / self.range.duration.0 as f64;
        Some(((k * words as f64) as usize).min(words - 1))
    }

    /// Both keyframe clocks for timeline time `t`.
    pub fn eval_context(&self, t: Time) -> EvalContext {
        let local = t - self.range.start;
        EvalContext::at(local, self.time_map.source_time(local))
    }

    /// Timing after moving the head to `new_start` while keeping the tail fixed and
    /// the footage in place (a head trim). Returns `None` if the item would vanish.
    pub fn trimmed_head(&self, new_start: Time) -> Option<(TimeRange, TimeMap)> {
        let end = self.range.end();
        if new_start >= end {
            return None;
        }
        let delta = new_start - self.range.start;
        let map = TimeMap {
            source_in: self.time_map.source_time(delta),
            speed: self.time_map.speed,
        };
        Some((TimeRange::new(new_start, end - new_start), map))
    }
}

/// Which end of a clip.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipEnd {
    Head,
    Tail,
}

/// A transition on one end of a clip. Its params are keyframable on the clip's clocks
/// like any other.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Registry id, e.g. `"oa.transition.crossfade"`.
    pub type_id: String,
    pub type_version: u32,
    pub duration: Time,
    #[serde(default)]
    pub params: ParamSet,
}

impl Transition {
    pub fn new(type_id: &str, duration: Time) -> Self {
        Transition { type_id: type_id.into(), type_version: 1, duration, params: ParamSet::default() }
    }
}

/// When an effect on a clip runs. Any effect shader can take any role; the shader sees
/// the difference through its clock (`visibility()`, `progress()`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectRole {
    /// For the whole clip.
    #[default]
    Passive,
    /// Over the clip's first `duration`: brings the clip in (visibility 0 → 1).
    In { duration: Time },
    /// Over the clip's last `duration`: takes the clip out (visibility 1 → 0).
    Out { duration: Time },
    /// An intro played backwards over the clip's last `duration` (the clip's "Reverse"
    /// outro). Made by [`Item::active_effects`]; not stored on clips.
    Reversed { duration: Time },
}

/// Where an effect is in its run at one instant.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EffectClock {
    /// How present the clip is: rises 0 → 1 over an in, falls 1 → 0 over an out, 1 when
    /// passive. One shader written against this works as both an in and an out.
    pub visibility: f64,
    /// 0 → 1 across the effect's window (for passive effects, across the clip).
    pub progress: f64,
    /// Seconds since the effect's window began.
    pub seconds: f64,
}

impl EffectRole {
    /// The clock for clip-local time `local` in a clip of length `length`, or `None`
    /// outside the effect's window (the effect doesn't run then).
    pub fn clock(self, local: Time, length: Time) -> Option<EffectClock> {
        let frac = |t: Time, d: Time| if d > Time::ZERO { (t.0 as f64 / d.0 as f64).clamp(0.0, 1.0) } else { 1.0 };
        match self {
            EffectRole::Passive => Some(EffectClock { visibility: 1.0, progress: frac(local, length), seconds: local.as_seconds_f64() }),
            EffectRole::In { duration } => {
                let d = duration.min(length);
                (local < d).then(|| {
                    let p = frac(local, d);
                    EffectClock { visibility: p, progress: p, seconds: local.as_seconds_f64() }
                })
            }
            EffectRole::Out { duration } => {
                let d = duration.min(length);
                let start = length - d;
                (local >= start).then(|| {
                    let p = frac(local - start, d);
                    EffectClock { visibility: 1.0 - p, progress: p, seconds: (local - start).as_seconds_f64() }
                })
            }
            EffectRole::Reversed { duration } => {
                // The intro's own clock, run backwards over the clip's last `duration`.
                let d = duration.min(length);
                let start = length - d;
                (local >= start).then(|| {
                    let back = d - (local - start);
                    let p = frac(back, d);
                    EffectClock { visibility: p, progress: p, seconds: back.as_seconds_f64() }
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectInstance {
    pub id: EffectId,
    /// Registry id, e.g. `"oa.blur.gaussian"`.
    pub type_id: String,
    pub type_version: u32,
    pub enabled: bool,
    #[serde(default)]
    pub params: ParamSet,
    #[serde(default, skip_serializing_if = "is_passive")]
    pub role: EffectRole,
}

fn is_passive(r: &EffectRole) -> bool {
    *r == EffectRole::Passive
}

impl EffectInstance {
    pub fn new(id: EffectId, type_id: &str) -> Self {
        EffectInstance { id, type_id: type_id.into(), type_version: 1, enabled: true, params: ParamSet::default(), role: EffectRole::Passive }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub id: MediaId,
    pub path: String,
    /// Content fingerprint; caches and relinking key off this, never the path.
    pub fingerprint: Option<String>,
    pub info: Option<MediaInfo>,
    /// How its pixels are scaled on screen.
    #[serde(default, skip_serializing_if = "MediaScaling::is_auto")]
    pub scaling: MediaScaling,
    /// Which folder of the media bin it sits in: "" is the top, "b-roll/interviews" is
    /// nested. Folders are just these paths — there's nothing else to keep in step.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// How its values are read as light (DESIGN.md §9); the default follows its tags.
    #[serde(default, skip_serializing_if = "InputColor::is_auto")]
    pub color: InputColor,
}

/// How a picture's pixels are scaled when drawn larger or smaller.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaScaling {
    /// Pixel for tiny pictures (pixel art, up to [`MediaScaling::PIXEL_ART_MAX`] on each
    /// side), smooth for everything else.
    #[default]
    Auto,
    /// Nearest neighbor: every source pixel stays a crisp square.
    Pixel,
    /// Filtered: smooth.
    Smooth,
}

impl MediaScaling {
    pub const PIXEL_ART_MAX: u32 = 32;

    pub fn is_auto(&self) -> bool {
        *self == MediaScaling::Auto
    }
}

impl MediaRef {
    /// The transfer curve and gamut its values are read with (its tags fill in `Auto`).
    pub fn input_color(&self) -> (Transfer, Gamut) {
        let tags = self.info.as_ref().map(|i| &i.color);
        self.color.resolve(tags.unwrap_or(&ColorTags::default()))
    }

    /// Whether it's drawn with nearest-neighbor scaling.
    pub fn pixelated(&self) -> bool {
        match self.scaling {
            MediaScaling::Pixel => true,
            MediaScaling::Smooth => false,
            MediaScaling::Auto => self.info.as_ref().is_some_and(|i| {
                i.has_video && i.width > 0 && i.width <= MediaScaling::PIXEL_ART_MAX && i.height <= MediaScaling::PIXEL_ART_MAX
            }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub duration: Time,
    pub rate: Option<FrameRate>,
    pub has_video: bool,
    pub has_audio: bool,
    /// A single image: a clip using it can be any length.
    #[serde(default)]
    pub still: bool,
    /// The file's color tags, for automatic input transforms.
    #[serde(default, skip_serializing_if = "ColorTags::is_empty")]
    pub color: ColorTags,
}

impl MediaInfo {
    /// How long the media can play, or `None` if it has no inherent length (stills).
    pub fn playable_duration(&self) -> Option<Time> {
        (!self.still && self.duration > Time::ZERO).then_some(self.duration)
    }
}
