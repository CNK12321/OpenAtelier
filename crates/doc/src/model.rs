use crate::color::{ColorTags, InputColor, Transfer, Gamut};
use crate::format::{CanvasSize, FormatVariant};
use oa_params::{EvalContext, ParamSet, ParamSource};
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
    /// A **temp layer**: made for clips that had nowhere else to go (a paste with no room).
    /// It goes away by itself once it's empty, unless it's kept (made a real track).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub temp: bool,
}

impl Track {
    pub fn new(id: TrackId, name: &str, kind: TrackKind) -> Self {
        Track { id, name: name.into(), kind, enabled: true, items: Vec::new(), effects: false, temp: false }
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

/// Maps clip-local time to source time: `source = source_in + floor(phase + local ×
/// speed)`. Speed 0 is a freeze frame; negative speed plays in reverse.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeMap {
    pub source_in: Time,
    pub speed: Rational,
    /// The part of a flick (in seconds, under one flick) the source position had past
    /// `source_in` where this clip was cut from a longer one at a non-integer speed —
    /// carried so the back half of a split continues on exactly the frames the whole
    /// clip showed there, instead of one flick early.
    #[serde(default = "no_phase", skip_serializing_if = "is_no_phase")]
    pub phase: Rational,
}

/// How far a clip's word times move when its timing changes from `old` to `new`: by as
/// much clip time as the in point moved in the source (a head trim), nothing when only the
/// clip moved. Freezes and a change of speed leave them alone.
pub fn word_shift(old: &TimeMap, new: &TimeMap) -> Time {
    if old.speed != new.speed || old.speed.is_zero() {
        return Time::ZERO;
    }
    let moved = new.source_in - old.source_in;
    -Time::from_rational_floor(moved.as_rational() * old.speed.recip())
}

/// A speed ramp's speed at clip time `s` (seconds): its value there, kept to
/// [`crate::schema::MIN_SPEED`]–[`crate::schema::MAX_SPEED`].
pub fn ramp_speed(ramp: &ParamSource, s: f64) -> f64 {
    let ctx = EvalContext::at(Time::from_seconds_f64(s), Time::ZERO);
    ramp.eval(&ctx).as_float().filter(|v| v.is_finite()).unwrap_or(1.0).clamp(crate::schema::MIN_SPEED, crate::schema::MAX_SPEED)
}

/// How much source a speed ramp plays from clip time 0 to `local` (seconds; negative
/// before the clip starts): its speed added up. Piecewise between its keys — each piece
/// is smooth (whatever its easing), so Simpson's rule on 16 steps is exact for linear
/// keys and within a fraction of a frame for eased ones; modulation (a wave on the speed)
/// is sampled every 1/120 s.
pub fn ramp_integral(ramp: &ParamSource, local: f64) -> f64 {
    if local == 0.0 || !local.is_finite() {
        return 0.0;
    }
    let (a, b, sign) = if local > 0.0 { (0.0, local, 1.0) } else { (local, 0.0, -1.0) };
    // The pieces: at every key between, and more often where a modulator moves it.
    let mut cuts = vec![a, b];
    let mut curve = ramp;
    let mut modulated = false;
    while let ParamSource::Modulated { base, .. } = curve {
        modulated = true;
        curve = base;
    }
    if let ParamSource::Animated(c) = curve {
        cuts.extend(c.keys.iter().map(|k| k.t.as_seconds_f64()).filter(|t| *t > a && *t < b));
    }
    if modulated {
        let steps = (((b - a) * 120.0).ceil() as usize).min(1_000_000);
        cuts.extend((1..steps).map(|i| a + (b - a) * i as f64 / steps as f64));
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();
    const N: usize = 16;
    let mut total = 0.0;
    for w in cuts.windows(2) {
        let (x0, x1) = (w[0], w[1]);
        let h = (x1 - x0) / N as f64;
        if h <= 0.0 {
            continue;
        }
        // Just inside each end, so a hold key's jump lands in the right piece.
        let at = |i: usize| {
            let x = x0 + h * i as f64;
            let x = if i == 0 { x + h * 1e-6 } else if i == N { x - h * 1e-6 } else { x };
            ramp_speed(ramp, x)
        };
        let mut sum = at(0) + at(N);
        for i in 1..N {
            sum += at(i) * if i % 2 == 1 { 4.0 } else { 2.0 };
        }
        total += sum * h / 3.0;
    }
    sign * total
}

fn no_phase() -> Rational {
    Rational::ZERO
}

fn is_no_phase(r: &Rational) -> bool {
    r.is_zero()
}

impl Default for TimeMap {
    fn default() -> Self {
        TimeMap { source_in: Time::ZERO, speed: Rational::ONE, phase: Rational::ZERO }
    }
}

impl TimeMap {
    /// Plays from `source_in` at `speed`.
    pub fn new(source_in: Time, speed: Rational) -> Self {
        TimeMap { source_in, speed, phase: Rational::ZERO }
    }

    pub fn source_time(&self, local: Time) -> Time {
        self.source_in + Time::from_rational_floor(self.phase + local.as_rational() * self.speed)
    }

    /// The map for the part of the clip from `delta` on: the same source times, exactly.
    pub fn from(&self, delta: Time) -> TimeMap {
        let exact = self.phase + delta.as_rational() * self.speed;
        let whole = Time::from_rational_floor(exact);
        TimeMap { source_in: self.source_in + whole, speed: self.speed, phase: exact + Rational::new(-whole.0, oa_time::FLICKS_PER_SECOND) }
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
    /// Areas drawn on the clip (the Masks tab) that its properties and effects can be
    /// limited to (see [`crate::mask`]). Their animated values are in `params`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<crate::mask::Mask>,
}

impl Item {
    /// Gives every effect a fresh id (the clip is being copied), keeping each
    /// Duplicate's own effects tied to it.
    pub fn renumber_effects(&mut self, alloc: &mut dyn FnMut() -> u64) {
        let mut renamed = BTreeMap::new();
        for fx in &mut self.effects {
            let new = EffectId(alloc());
            renamed.insert(fx.id, new);
            fx.id = new;
        }
        for fx in &mut self.effects {
            fx.on_duplicate = fx.on_duplicate.and_then(|d| renamed.get(&d).copied());
        }
    }

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
            masks: Vec::new(),
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

    /// Moves every word time `by` later on the clip's clock (earlier when negative).
    pub fn shift_word_times(&mut self, by: Time) {
        if by != Time::ZERO {
            for w in &mut self.word_times {
                w.start += by;
            }
        }
    }

    /// Both keyframe clocks for timeline time `t`.
    pub fn eval_context(&self, t: Time) -> EvalContext {
        let local = t - self.range.start;
        EvalContext::at(local, self.source_time_at(local))
    }

    /// Its speed keyframed (a speed ramp), if it is: a curve (or modulation) on
    /// [`crate::schema::SPEED`]. A steady speed is the time map's.
    pub fn speed_ramp(&self) -> Option<&ParamSource> {
        self.params.get(crate::schema::SPEED).filter(|s| matches!(s, ParamSource::Animated(_) | ParamSource::Modulated { .. }))
    }

    /// Where in its source clip time `local` falls: the time map, or with a speed ramp,
    /// the in point plus the ramp's speed added up over the clip so far (the way it
    /// plays; reversed clips run it backwards, frozen ones stay put).
    pub fn source_time_at(&self, local: Time) -> Time {
        let Some(ramp) = self.speed_ramp() else { return self.time_map.source_time(local) };
        let dir = self.time_map.speed.num().signum() as f64;
        if dir == 0.0 {
            return self.time_map.source_in;
        }
        self.time_map.source_in + Time::from_seconds_f64(dir * ramp_integral(ramp, local.as_seconds_f64()))
    }

    /// How fast (and which way: negative is reversed) the source plays at clip time
    /// `local`.
    pub fn speed_at(&self, local: Time) -> f64 {
        let steady = self.time_map.speed.num() as f64 / self.time_map.speed.den().max(1) as f64;
        match self.speed_ramp() {
            Some(ramp) if steady != 0.0 => steady.signum() * ramp_speed(ramp, local.as_seconds_f64()),
            _ => steady,
        }
    }

    /// Timing after moving the head to `new_start` while keeping the tail fixed and
    /// the footage in place (a head trim). Returns `None` if the item would vanish.
    /// With a speed ramp, the in point is where the ramp had got to by then (the ramp's
    /// keys move with the head: see `oa_edit::timeline::trim`).
    pub fn trimmed_head(&self, new_start: Time) -> Option<(TimeRange, TimeMap)> {
        let end = self.range.end();
        if new_start >= end {
            return None;
        }
        let delta = new_start - self.range.start;
        let mut map = self.time_map.from(delta);
        if self.speed_ramp().is_some() {
            map.source_in = self.source_time_at(delta);
            map.phase = Rational::ZERO;
        }
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
    /// An effect on a Duplicate's copy only ([`crate::schema::DUPLICATE_EFFECT`]): the
    /// Duplicate it belongs to. It's listed under that Duplicate, not run on the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_duplicate: Option<EffectId>,
}

fn is_passive(r: &EffectRole) -> bool {
    *r == EffectRole::Passive
}

impl EffectInstance {
    pub fn new(id: EffectId, type_id: &str) -> Self {
        EffectInstance { id, type_id: type_id.into(), type_version: 1, enabled: true, params: ParamSet::default(), role: EffectRole::Passive, on_duplicate: None }
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
    /// The picture has transparency (PNG, ProRes 4444, VP9 or FFV1 with alpha…): it
    /// never counts as covering what's under it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alpha: bool,
}

impl MediaInfo {
    /// How long the media can play, or `None` if it has no inherent length (stills).
    pub fn playable_duration(&self) -> Option<Time> {
        (!self.still && self.duration > Time::ZERO).then_some(self.duration)
    }
}

#[cfg(test)]
mod speed_ramp_tests {
    use super::*;
    use oa_params::{Curve, Keyframe, KeyframeAnchor, Value};

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    fn clip(keys: Vec<Keyframe>, speed: Rational) -> Item {
        let mut it = Item::new(ItemId(1), "c", ItemKind::Solid, TimeRange::new(secs(5.0), secs(4.0)));
        it.time_map = TimeMap::new(secs(10.0), speed);
        it.params.set(crate::schema::SPEED, ParamSource::Animated(Curve::new(KeyframeAnchor::ClipStart, keys)));
        it
    }

    fn near(a: Time, b: f64) -> bool {
        (a.as_seconds_f64() - b).abs() < 1e-6
    }

    /// A ramp adds its speed up over the clip: 1× to 3× over 2 s plays 4 s of source; a
    /// held 4× from 1 s plays 1 + 4; reversed runs it backwards; the speed is kept to
    /// 0.01–100×.
    #[test]
    fn a_speed_ramp_adds_up_its_speed() {
        let linear = clip(vec![Keyframe::linear(secs(0.0), Value::Float(1.0)), Keyframe::linear(secs(2.0), Value::Float(3.0))], Rational::ONE);
        assert!(near(linear.source_time_at(secs(1.0)), 10.0 + 1.5), "{:?}", linear.source_time_at(secs(1.0)));
        assert!(near(linear.source_time_at(secs(2.0)), 10.0 + 4.0));
        assert!(near(linear.source_time_at(secs(3.0)), 10.0 + 7.0), "3× after the last key");
        assert!((linear.speed_at(secs(1.0)) - 2.0).abs() < 1e-9);
        assert!(near(linear.eval_context(secs(6.0)).source_time, 11.5), "the clip's clocks follow it");
        let held = clip(vec![Keyframe::hold(secs(0.0), Value::Float(1.0)), Keyframe::hold(secs(1.0), Value::Float(4.0))], Rational::ONE);
        assert!(near(held.source_time_at(secs(2.0)), 10.0 + 1.0 + 4.0), "{:?}", held.source_time_at(secs(2.0)));
        let reversed = clip(vec![Keyframe::linear(secs(0.0), Value::Float(1.0)), Keyframe::linear(secs(2.0), Value::Float(3.0))], Rational::new(-1, 1));
        assert!(near(reversed.source_time_at(secs(1.0)), 10.0 - 1.5));
        assert!(reversed.speed_at(secs(1.0)) < 0.0);
        let wild = clip(vec![Keyframe::linear(secs(0.0), Value::Float(1000.0)), Keyframe::linear(secs(1.0), Value::Float(0.0))], Rational::ONE);
        assert!((wild.speed_at(secs(0.0)) - 100.0).abs() < 1e-9 && (wild.speed_at(secs(1.0)) - 0.01).abs() < 1e-9);
        let steady = Item { params: ParamSet::default(), ..linear.clone() };
        assert!(near(steady.source_time_at(secs(1.0)), 11.0), "no keys: the steady speed");
    }

    /// Cut in two, the back half continues exactly where the front stops (its ramp's keys
    /// moved to its own clock, its in point where the ramp had got to).
    #[test]
    fn a_split_ramp_continues_where_it_stopped() {
        let front = clip(vec![Keyframe::linear(secs(0.0), Value::Float(0.5)), Keyframe::linear(secs(4.0), Value::Float(2.5))], Rational::ONE);
        let cut = secs(1.5);
        let (range, map) = front.trimmed_head(front.range.start + cut).unwrap();
        let mut back = front.clone();
        back.range = range;
        back.time_map = map;
        for src in back.params.0.values_mut() {
            src.shift_clip_clock(cut);
        }
        for t in [0.0, 0.7, 2.5] {
            let want = front.source_time_at(cut + secs(t));
            let got = back.source_time_at(secs(t));
            assert!((want - got).as_seconds_f64().abs() < 1e-6, "{t}: {want:?} vs {got:?}");
        }
    }
}
