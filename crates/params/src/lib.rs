//! The parameter system: every animatable property of every clip, effect, transition
//! and plugin goes through here, so keyframing and overrides work everywhere.
//!
//! Design rules:
//! * Parameters are addressed by stable string ids, never by position.
//! * Unknown parameters (e.g. from a newer plugin) are preserved, not dropped.
//! * Keyframes record which clock they follow ([`KeyframeAnchor`]): the clip start
//!   (moves when the clip head is trimmed) or the source media (stays glued to the
//!   footage — required for masks and tracking data).

mod curve;
pub mod signal;
mod value;

pub use curve::{Curve, Ease, EaseDir, Interp, Keyframe, KeyframeAnchor};
pub use signal::{SoundBand, SoundSource};
pub use value::{Gradient, GradientStop, ParamType, Value};

use oa_time::Time;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How new keyframes ease when nothing else says (the first key of a curve, or a key
/// added before all others). Linear unless the app sets the user's preference; it only
/// affects keys being created, never how stored curves evaluate.
static DEFAULT_INTERP: std::sync::RwLock<Interp> = std::sync::RwLock::new(Interp::Linear);

pub fn set_default_interp(interp: Interp) {
    *DEFAULT_INTERP.write().unwrap_or_else(|e| e.into_inner()) = interp;
}

pub fn default_interp() -> Interp {
    *DEFAULT_INTERP.read().unwrap_or_else(|e| e.into_inner())
}

/// Stable parameter id, e.g. `"transform.position"` or `"radius"`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(pub Arc<str>);

impl ParamId {
    pub fn new(s: &str) -> Self {
        ParamId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ParamId {
    fn from(s: &str) -> Self {
        ParamId::new(s)
    }
}

/// How a numeric value relates to space and resolution. The planner uses this to
/// keep previews at reduced resolution identical (up to sampling) to full exports.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Unit {
    None,
    /// Pixels in the layer's own space. Scaled by the layer's raster scale.
    LayerPixels,
    /// Fraction of the canvas along each axis (0 = center for positions).
    CanvasFraction,
    /// Fraction of the layer's source size (0..1), e.g. anchor or reframe focus.
    SourceFraction,
    Degrees,
    /// A direction on screen in degrees: 0 = towards the right, 90 = down (clockwise).
    /// Edited with a dial.
    Direction,
    Seconds,
    Decibels,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSchema {
    pub id: ParamId,
    pub ty: ParamType,
    pub default: Value,
    pub unit: Unit,
    pub animatable: bool,
    /// Soft UI range (values outside are allowed).
    pub range: Option<(f64, f64)>,
    /// Choices for `Enum` params, in order. The index is what shaders receive.
    #[serde(default)]
    pub options: Vec<String>,
    /// An `Enum` whose value is any number of its options (none to all), kept as their
    /// keys joined by commas in option order ("red,green,blue"); shaders receive a
    /// bitmask (bit `i` for option `i`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
    /// Default clock for new keyframes on this parameter.
    pub default_anchor: KeyframeAnchor,
}

impl ParamSchema {
    pub fn new(id: &str, default: Value, unit: Unit) -> Self {
        ParamSchema {
            id: ParamId::new(id),
            ty: default.ty(),
            default,
            unit,
            animatable: true,
            range: None,
            options: Vec::new(),
            multiple: false,
            default_anchor: KeyframeAnchor::ClipStart,
        }
    }

    /// Declares the choices of an `Enum` parameter.
    pub fn options(mut self, options: &[&str]) -> Self {
        self.options = options.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Index of an enum value in this schema's options.
    pub fn option_index(&self, value: &Value) -> Option<usize> {
        let key = value.as_enum()?;
        self.options.iter().position(|o| o == key)
    }

    /// Makes an `Enum` parameter take any number of its options (see [`Self::multiple`]).
    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    /// The options a many-option enum value has chosen, in option order (unknown keys
    /// left out).
    pub fn chosen<'a>(&'a self, value: &Value) -> Vec<&'a str> {
        let keys: Vec<&str> = value.as_enum().map(|s| s.split(',').map(str::trim).collect()).unwrap_or_default();
        self.options.iter().filter(|o| keys.contains(&o.as_str())).map(String::as_str).collect()
    }

    /// A many-option enum value with `chosen` (in option order, whatever order given).
    pub fn choose(&self, chosen: &[&str]) -> Value {
        Value::Enum(self.options.iter().filter(|o| chosen.contains(&o.as_str())).cloned().collect::<Vec<_>>().join(","))
    }

    /// What a shader receives for an enum value: its option's index, or for a
    /// many-option one the bitmask of what's chosen.
    pub fn enum_uniform(&self, value: &Value) -> f32 {
        if self.multiple {
            // (24 options at most: a float holds whole numbers exactly up to 2^24.)
            let chosen = self.chosen(value);
            let bits = self.options.iter().take(24).enumerate().filter(|(_, o)| chosen.contains(&o.as_str())).fold(0u32, |m, (i, _)| m | (1 << i));
            bits as f32
        } else {
            self.option_index(value).unwrap_or(0) as f32
        }
    }

    pub fn range(mut self, lo: f64, hi: f64) -> Self {
        self.range = Some((lo, hi));
        self
    }

    pub fn static_only(mut self) -> Self {
        self.animatable = false;
        self
    }

    pub fn anchored_to_source(mut self) -> Self {
        self.default_anchor = KeyframeAnchor::SourceMedia;
        self
    }
}

/// Where a parameter's value comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ParamSource {
    Static(Value),
    Animated(Curve),
    /// A base value (static or keyframed) plus deterministic procedural motion, e.g.
    /// shaking text: `Modulated { base: position keys, modulator: Wiggle { .. } }`.
    Modulated { base: Box<ParamSource>, modulator: Modulator },
}

impl ParamSource {
    /// Safe to store: finite numbers only, and no curve without keys (which can't be
    /// evaluated).
    pub fn is_valid(&self) -> bool {
        match self {
            ParamSource::Static(v) => v.is_finite(),
            ParamSource::Animated(c) => !c.keys.is_empty() && c.keys.iter().all(|k| k.value.is_finite()),
            ParamSource::Modulated { base, modulator } => {
                let lfo_ok = !matches!(modulator, Modulator::Lfo { phase, decay, .. } if !phase.is_finite() || !decay.is_finite());
                base.is_valid() && lfo_ok && modulator.rates().iter().all(|s| s.is_valid())
            }
        }
    }

    pub fn eval(&self, ctx: &EvalContext) -> Value {
        match self {
            ParamSource::Static(v) => v.clone(),
            ParamSource::Animated(c) => c.eval(ctx),
            ParamSource::Modulated { base, modulator } => modulator.apply(base.eval(ctx), ctx),
        }
    }

    pub fn is_animated(&self) -> bool {
        match self {
            ParamSource::Static(_) => false,
            ParamSource::Animated(c) => c.keys.len() > 1,
            ParamSource::Modulated { .. } => true,
        }
    }

    /// Wraps this source in a wiggle ("shake") with static amplitude and frequency.
    pub fn wiggle(self, amplitude: f64, frequency_hz: f64, seed: u64) -> ParamSource {
        ParamSource::Modulated {
            base: Box::new(self),
            modulator: Modulator::Wiggle {
                amplitude: Box::new(ParamSource::Static(Value::Float(amplitude))),
                frequency: Box::new(ParamSource::Static(Value::Float(frequency_hz))),
                octaves: 2,
                seed,
                anchor: KeyframeAnchor::ClipStart,
                offset: Time::ZERO,
            },
        }
    }

    /// Adds a regular oscillation with static amplitude and frequency (Hz).
    pub fn lfo(self, wave: LfoWave, amplitude: f64, frequency_hz: f64) -> ParamSource {
        ParamSource::Modulated {
            base: Box::new(self),
            modulator: Modulator::Lfo {
                wave,
                amplitude: Box::new(ParamSource::Static(Value::Float(amplitude))),
                frequency: Box::new(ParamSource::Static(Value::Float(frequency_hz))),
                phase: 0.0,
                decay: 0.0,
                anchor: KeyframeAnchor::ClipStart,
                offset: Time::ZERO,
            },
        }
    }

    /// The wave (LFO) on this source, and what it oscillates around — the outermost one
    /// if there are several layers of motion.
    pub fn find_lfo(&self) -> Option<(&ParamSource, &Modulator)> {
        match self {
            ParamSource::Modulated { base, modulator: m @ Modulator::Lfo { .. } } => Some((base, m)),
            ParamSource::Modulated { base, .. } => base.find_lfo(),
            _ => None,
        }
    }

    /// Mutable [`ParamSource::find_lfo`]: (base, the LFO).
    pub fn find_lfo_mut(&mut self) -> Option<(&mut ParamSource, &mut Modulator)> {
        let here = matches!(self, ParamSource::Modulated { modulator: Modulator::Lfo { .. }, .. });
        let ParamSource::Modulated { base, modulator } = self else { return None };
        if here { Some((&mut **base, modulator)) } else { base.find_lfo_mut() }
    }

    /// Adds an offset that follows the sound: `amount` at full level, nothing below
    /// `floor_db`.
    pub fn follow(self, source: SoundSource, band: SoundBand, amount: f64, floor_db: f64) -> ParamSource {
        self.follow_on(None, source, band, amount, floor_db)
    }

    /// [`ParamSource::follow`] for one component of a vector (`axis`: 0 = x, 1 = y), or
    /// all of it (`None`).
    pub fn follow_on(self, axis: Option<u8>, source: SoundSource, band: SoundBand, amount: f64, floor_db: f64) -> ParamSource {
        ParamSource::Modulated {
            base: Box::new(self),
            modulator: Modulator::Follow {
                source,
                band,
                amount: Box::new(ParamSource::Static(Value::Float(amount))),
                floor_db: Box::new(ParamSource::Static(Value::Float(floor_db))),
                axis,
            },
        }
    }

    /// The sound connection offsetting component `axis` (`None`: the whole value), and
    /// what it rides on.
    pub fn find_follow_on(&self, axis: Option<u8>) -> Option<(&ParamSource, &Modulator)> {
        match self {
            ParamSource::Modulated { base, modulator: m @ Modulator::Follow { axis: a, .. } } if *a == axis => Some((base, m)),
            ParamSource::Modulated { base, .. } => base.find_follow_on(axis),
            _ => None,
        }
    }

    /// Mutable [`ParamSource::find_follow_on`].
    pub fn find_follow_on_mut(&mut self, axis: Option<u8>) -> Option<(&mut ParamSource, &mut Modulator)> {
        let here = matches!(self, ParamSource::Modulated { modulator: Modulator::Follow { axis: a, .. }, .. } if *a == axis);
        let ParamSource::Modulated { base, modulator } = self else { return None };
        if here { Some((&mut **base, modulator)) } else { base.find_follow_on_mut(axis) }
    }

    /// This source without the sound connection on component `axis`.
    pub fn remove_follow_on(self, axis: Option<u8>) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Follow { axis: a, .. } } if a == axis => *base,
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.remove_follow_on(axis)), modulator },
            other => other,
        }
    }

    /// The sound connection on this source and what it offsets (the outermost one).
    pub fn find_follow(&self) -> Option<(&ParamSource, &Modulator)> {
        match self {
            ParamSource::Modulated { base, modulator: m @ Modulator::Follow { .. } } => Some((base, m)),
            ParamSource::Modulated { base, .. } => base.find_follow(),
            _ => None,
        }
    }

    /// Mutable [`ParamSource::find_follow`].
    pub fn find_follow_mut(&mut self) -> Option<(&mut ParamSource, &mut Modulator)> {
        let here = matches!(self, ParamSource::Modulated { modulator: Modulator::Follow { .. }, .. });
        let ParamSource::Modulated { base, modulator } = self else { return None };
        if here { Some((&mut **base, modulator)) } else { base.find_follow_mut() }
    }

    /// This source without its (outermost) sound connection.
    pub fn remove_follow(self) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Follow { .. } } => *base,
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.remove_follow()), modulator },
            other => other,
        }
    }

    /// The track this follows (at any depth), and its clock.
    pub fn find_track(&self) -> Option<(&Arc<PointTrack>, Time)> {
        match self {
            ParamSource::Modulated { modulator: Modulator::Track { track, clock, .. }, .. } => Some((track, *clock)),
            ParamSource::Modulated { base, .. } => base.find_track(),
            _ => None,
        }
    }

    /// How it uses its track (following it, or stabilizing against it).
    pub fn track_use(&self) -> Option<&TrackUse> {
        match self {
            ParamSource::Modulated { modulator: Modulator::Track { mode, .. }, .. } => Some(mode),
            ParamSource::Modulated { base, .. } => base.track_use(),
            _ => None,
        }
    }

    /// What the track adds at `ctx` (the track's position when following it, the
    /// correction when stabilizing): the value minus its offset, before any motion on top.
    pub fn track_contribution(&self, ctx: &EvalContext) -> Option<[f64; 2]> {
        match self {
            ParamSource::Modulated { modulator: Modulator::Track { track, clock, mode }, .. } => mode.contribution(track, ctx.clip_time + *clock),
            ParamSource::Modulated { base, .. } => base.track_contribution(ctx),
            _ => None,
        }
    }

    /// Uses its track another way (`mode`) with the offset as it is: between two
    /// stabilizations the offset is the clip's resting place and only the correction
    /// changes (so a point to hold the tracked point at is really reached).
    pub fn replace_track_use(self, mode: TrackUse) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { track, clock, .. } } => ParamSource::Modulated { base, modulator: Modulator::Track { track, clock, mode } },
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.replace_track_use(mode)), modulator },
            other => other,
        }
    }

    /// Uses its track another way (`mode`), with the offset moved so the value at `ctx`
    /// stays where it is — switching to stabilizing doesn't make the clip jump.
    pub fn with_track_use(self, mode: TrackUse, ctx: &EvalContext) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { track, clock, mode: old } } => {
                let t = ctx.clip_time + clock;
                let (was, now) = (old.contribution(&track, t).unwrap_or([0.0; 2]), mode.contribution(&track, t).unwrap_or([0.0; 2]));
                ParamSource::Modulated { base: Box::new(base.shifted([was[0] - now[0], was[1] - now[1]])), modulator: Modulator::Track { track, clock, mode } }
            }
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.with_track_use(mode, ctx)), modulator },
            other => other,
        }
    }

    /// The value it adds on top of the track (its own, innermost value: the offset).
    pub fn track_offset(&self) -> Option<&ParamSource> {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { .. } } => Some(base),
            ParamSource::Modulated { base, .. } => base.track_offset(),
            _ => None,
        }
    }

    /// Follows `track` from here on: the value becomes the plain `offset` from it (any
    /// keyframes go; motion layered on top — wiggle, waves, sound — stays on top).
    /// Replaces a track it already follows.
    pub fn with_track(self, track: Arc<PointTrack>, clock: Time, offset: [f64; 2]) -> ParamSource {
        self.with_track_as(track, clock, offset, TrackUse::Follow)
    }

    /// [`ParamSource::with_track`], using the track as `mode` says.
    pub fn with_track_as(self, track: Arc<PointTrack>, clock: Time, offset: [f64; 2], mode: TrackUse) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { .. } } => base.with_track_as(track, clock, offset, mode),
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.with_track_as(track, clock, offset, mode)), modulator },
            _ => ParamSource::Modulated { base: Box::new(ParamSource::Static(Value::Vec2(offset))), modulator: Modulator::Track { track, clock, mode } },
        }
    }

    /// Scales or turns with two tracks (`pair`, a [`Modulator::TrackPair`]) — replacing
    /// a pair it already follows; its own value (keyframes too) stays underneath.
    pub fn with_track_pair(self, pair: Modulator) -> ParamSource {
        ParamSource::Modulated { base: Box::new(self.without_track_pair()), modulator: pair }
    }

    /// Stops scaling or turning with two tracks (its own value is left).
    pub fn without_track_pair(self) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::TrackPair { .. } } => base.without_track_pair(),
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.without_track_pair()), modulator },
            other => other,
        }
    }

    /// The two tracks it scales or turns with, if any.
    pub fn track_pair(&self) -> Option<(&Arc<PointTrack>, &Arc<PointTrack>)> {
        match self {
            ParamSource::Modulated { modulator: Modulator::TrackPair { a, b, .. }, .. } => Some((a, b)),
            ParamSource::Modulated { base, .. } => base.track_pair(),
            _ => None,
        }
    }

    /// The same, following `track`'s new points (when that track has been edited); a
    /// stabilization is worked out again for them.
    pub fn retracked(self, track: &Arc<PointTrack>) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { track: old, clock, mode } } if old.id == track.id => {
                ParamSource::Modulated { base, modulator: Modulator::Track { track: track.clone(), clock, mode: mode.rebuilt(track) } }
            }
            ParamSource::Modulated { base, modulator: Modulator::TrackPair { a, b, clock, measure, aspect, reference } } if a.id == track.id || b.id == track.id => {
                let pick = |old: Arc<PointTrack>| if old.id == track.id { track.clone() } else { old };
                let modulator = Modulator::TrackPair { a: pick(a), b: pick(b), clock, measure, aspect, reference };
                ParamSource::Modulated { base: Box::new(base.retracked(track)), modulator }
            }
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.retracked(track)), modulator },
            other => other,
        }
    }

    /// Stops following the track; `shift` (the track's position somewhere) is added to
    /// the offset, so the value there stays where it was.
    pub fn without_track(self, shift: [f64; 2]) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Track { .. } } => base.shifted(shift),
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.without_track(shift)), modulator },
            other => other,
        }
    }

    /// Every value (each key's, a static one) moved by `d` (vectors only).
    fn shifted(self, d: [f64; 2]) -> ParamSource {
        let add = |v: Value| match v {
            Value::Vec2([a, b]) => Value::Vec2([a + d[0], b + d[1]]),
            other => other,
        };
        match self {
            ParamSource::Static(v) => ParamSource::Static(add(v)),
            ParamSource::Animated(mut c) => {
                for k in &mut c.keys {
                    k.value = add(std::mem::replace(&mut k.value, Value::Float(0.0)));
                }
                ParamSource::Animated(c)
            }
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.shifted(d)), modulator },
        }
    }

    /// Whether any part of this source follows the sound (so whoever plans frames
    /// needs loudness envelopes).
    pub fn follows_sound(&self) -> bool {
        match self {
            ParamSource::Modulated { base, modulator } => matches!(modulator, Modulator::Follow { .. }) || base.follows_sound() || modulator.rates().iter().any(|s| s.follows_sound()),
            _ => false,
        }
    }

    /// This source without its (outermost) wave; unchanged if it has none.
    pub fn remove_lfo(self) -> ParamSource {
        match self {
            ParamSource::Modulated { base, modulator: Modulator::Lfo { .. } } => *base,
            ParamSource::Modulated { base, modulator } => ParamSource::Modulated { base: Box::new(base.remove_lfo()), modulator },
            other => other,
        }
    }

    /// Writes `value` at the instant `ctx`, the way an interactive edit should:
    /// * a static value is replaced;
    /// * a keyframed value gets a key at that instant on the curve's own clock (an
    ///   existing key there keeps its easing; a new one borrows its neighbor's);
    /// * a modulated value has its base edited so that, with the procedural motion on
    ///   top, it shows `value` at this instant.
    pub fn set_at(&mut self, ctx: &EvalContext, value: Value) {
        let full = self.eval(ctx);
        match self {
            ParamSource::Static(v) => *v = value,
            ParamSource::Animated(c) => c.set_value_at(c.clock(ctx), value),
            ParamSource::Modulated { base, .. } => {
                let motion = full.plus(&base.eval(ctx), -1.0);
                base.set_at(ctx, value.plus(&motion, -1.0));
            }
        }
    }

    /// Turns keyframing on: the current value becomes the first key at `ctx`.
    /// Already-keyframed sources are returned unchanged (modulated ones keyframe their
    /// base).
    pub fn keyframed(self, ctx: &EvalContext, anchor: KeyframeAnchor) -> ParamSource {
        match self {
            ParamSource::Static(v) => {
                let t = match anchor {
                    KeyframeAnchor::ClipStart => ctx.clip_time,
                    KeyframeAnchor::SourceMedia => ctx.source_time,
                };
                ParamSource::Animated(Curve::new(anchor, vec![Keyframe { t, value: v, interp: default_interp() }]))
            }
            ParamSource::Modulated { base, modulator } => {
                ParamSource::Modulated { base: Box::new(base.keyframed(ctx, anchor)), modulator }
            }
            animated => animated,
        }
    }

    /// The keyframe curve this source is driven by, if any (looking through modulators).
    pub fn curve(&self) -> Option<&Curve> {
        match self {
            ParamSource::Static(_) => None,
            ParamSource::Animated(c) => Some(c),
            ParamSource::Modulated { base, .. } => base.curve(),
        }
    }

    pub fn curve_mut(&mut self) -> Option<&mut Curve> {
        match self {
            ParamSource::Static(_) => None,
            ParamSource::Animated(c) => Some(c),
            ParamSource::Modulated { base, .. } => base.curve_mut(),
        }
    }

    /// Restores the invariants a loaded file might break: keyframes sorted with no
    /// duplicate times, no NaN values, no empty curves. Returns false if nothing usable
    /// is left (the caller should drop the parameter so it falls back to its default).
    pub fn sanitize(&mut self) -> bool {
        match self {
            ParamSource::Static(v) => v.is_finite(),
            ParamSource::Animated(c) => {
                c.keys.retain(|k| k.value.is_finite());
                c.keys.sort_by_key(|k| k.t);
                c.keys.dedup_by_key(|k| k.t);
                !c.keys.is_empty()
            }
            ParamSource::Modulated { base, modulator } => {
                if !base.sanitize() {
                    return false;
                }
                match modulator {
                    Modulator::Wiggle { octaves, .. } => *octaves = (*octaves).clamp(1, 8),
                    Modulator::Lfo { phase, decay, .. } => {
                        if !phase.is_finite() {
                            *phase = 0.0;
                        }
                        if !decay.is_finite() {
                            *decay = 0.0;
                        }
                    }
                    Modulator::Follow { .. } => {}
                    Modulator::Track { track, .. } => {
                        if !track.is_clean() {
                            let mut t = (**track).clone();
                            t.sanitize();
                            *track = Arc::new(t);
                        }
                    }
                    Modulator::TrackPair { a, b, aspect, reference, .. } => {
                        for track in [a, b] {
                            if !track.is_clean() {
                                let mut t = (**track).clone();
                                t.sanitize();
                                *track = Arc::new(t);
                            }
                        }
                        if !(aspect.is_finite() && *aspect > 0.0) {
                            *aspect = 1.0;
                        }
                        if !reference.is_finite() {
                            *reference = 1.0;
                        }
                    }
                }
                for s in modulator.rates_mut() {
                    if !s.sanitize() {
                        **s = ParamSource::Static(Value::Float(0.0));
                    }
                }
                true
            }
        }
    }

    /// Its keys kept within a clip `length` long (see [`Curve::confine`]): what an edit
    /// does first, so keys out of sight don't pull on the one being set.
    pub fn confine(&mut self, length: Time) {
        if let Some(c) = self.curve_mut() {
            c.confine(length);
        }
    }

    /// Re-expresses this source for a clip whose start moved `delta` later on the
    /// timeline, so every instant keeps its value: clip-anchored keyframes shift back by
    /// `delta`, clip-anchored modulators carry the offset. Source-anchored timing is
    /// untouched (it never depended on the clip start). Used by split and head trims
    /// that must not change the picture.
    pub fn shift_clip_clock(&mut self, delta: Time) {
        match self {
            ParamSource::Static(_) => {}
            ParamSource::Animated(c) => c.shift_clip_clock(delta),
            ParamSource::Modulated { base, modulator } => {
                base.shift_clip_clock(delta);
                modulator.shift_clip_clock(delta);
            }
        }
    }
}

/// Procedural motion layered on a parameter. Modulators are pure functions of
/// (settings, seed, time) so frames that use them stay cacheable and render identically
/// in preview and export.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Modulator {
    /// Smooth random offset per component in `[-amplitude, amplitude]`. `amplitude` and
    /// `frequency` (Hz) are float sources, so shake intensity can itself be keyframed.
    /// Applies to float, vector and color values (color alpha is left alone).
    Wiggle {
        amplitude: Box<ParamSource>,
        frequency: Box<ParamSource>,
        octaves: u32,
        seed: u64,
        anchor: KeyframeAnchor,
        /// Added to the anchor clock before sampling the noise, so a split clip's second
        /// half keeps shaking exactly as before.
        #[serde(default)]
        offset: Time,
    },
    /// A regular oscillation (low-frequency oscillator) added to every component:
    /// `amplitude × wave(frequency × t + phase)`, the wave in `[-1, 1]`. Pulses, bobbing,
    /// breathing. Amplitude and frequency are float sources, so they can be keyframed.
    Lfo {
        wave: LfoWave,
        amplitude: Box<ParamSource>,
        frequency: Box<ParamSource>,
        /// Where in the cycle it starts, 0..1.
        #[serde(default)]
        phase: f64,
        /// How fast the swing dies down, per second (0: it never does).
        #[serde(default)]
        decay: f64,
        anchor: KeyframeAnchor,
        /// See `Wiggle::offset`.
        #[serde(default)]
        offset: Time,
    },
    /// An offset that follows the sound (see [`signal`]): `amount × level`, the level
    /// running from 0 at `floor_db` to 1 at [`signal::CEILING_DB`]. Pulse a scale with
    /// the kick, brighten with the treble, shake with the voice.
    Follow {
        source: SoundSource,
        band: SoundBand,
        /// The offset at full level (float source, so it can be keyframed).
        amount: Box<ParamSource>,
        /// dBFS below which the sound counts as silence (float source).
        floor_db: Box<ParamSource>,
        /// Only one component of a vector (0 = x, 1 = y, 2 = z) — a position that sways
        /// sideways with the beat; `None` offsets every component (and plain numbers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis: Option<u8>,
    },
    /// Follows a [`PointTrack`]: its position is added, so the property's own value is
    /// an **offset** from the track. The track is carried along (shared, not copied) so
    /// evaluating stays self-contained; the project's copy (`Project::tracks`) is what
    /// the editor lists and updates.
    Track {
        track: Arc<PointTrack>,
        /// Timeline time = clip time + `clock` (the clip's start when it was attached;
        /// moved along by splits and head trims, like the other clocks).
        clock: Time,
        /// Following the track, or stabilizing against it.
        #[serde(default)]
        mode: TrackUse,
    },
    /// Follows two [`PointTrack`]s at once: how far apart they are (a scale, multiplied
    /// in) or which way the line between them points (a rotation in degrees, added),
    /// each compared with when it was linked — something tracked at two points grows,
    /// shrinks and turns, and the property does too.
    TrackPair {
        a: Arc<PointTrack>,
        b: Arc<PointTrack>,
        /// Timeline time = clip time + `clock`, as for [`Modulator::Track`].
        clock: Time,
        measure: PairMeasure,
        /// Width ÷ height of what the tracks' positions are fractions of, so distances
        /// and angles are measured true on a picture that isn't square.
        aspect: f64,
        /// The distance (scale) or the angle in degrees (rotation) when it was linked.
        reference: f64,
    },
}

/// What a [`Modulator::TrackPair`] reads from its two tracks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairMeasure {
    Scale,
    Rotation,
}

impl PairMeasure {
    /// The distance between `a` and `b` (Scale) or the angle of the line from `a` to `b`
    /// in degrees (Rotation), on a picture `aspect` wide per unit of height.
    pub fn of(self, a: [f64; 2], b: [f64; 2], aspect: f64) -> f64 {
        let d = [(b[0] - a[0]) * aspect, b[1] - a[1]];
        match self {
            PairMeasure::Scale => (d[0] * d[0] + d[1] * d[1]).sqrt(),
            PairMeasure::Rotation => d[1].atan2(d[0]).to_degrees(),
        }
    }
}

impl Modulator {
    /// A [`Modulator::TrackPair`] linked at timeline time `now` (its reference is the
    /// tracks' distance or angle there); `None` if a track has no points there or the
    /// two points coincide.
    pub fn track_pair(a: Arc<PointTrack>, b: Arc<PointTrack>, clock: Time, measure: PairMeasure, aspect: f64, now: Time) -> Option<Modulator> {
        let (pa, pb) = (a.at(now)?, b.at(now)?);
        let reference = measure.of(pa, pb, aspect);
        if measure == PairMeasure::Scale && reference < 1e-9 {
            return None;
        }
        Some(Modulator::TrackPair { a, b, clock, measure, aspect, reference })
    }
}

/// What a position does with its track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum TrackUse {
    /// Goes where the track goes (a title on a moving car): value = offset + track.
    #[default]
    Follow,
    /// Moves against it, so what was tracked holds still (steadying shaky footage — the
    /// clip's own position, stabilized by a track of its own picture).
    Stabilize(Stabilize),
}

/// How firmly a stabilization holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stabilize {
    /// How much of the movement is taken out: 1 all of it, 0.5 half.
    pub strength: f64,
    /// Hold the tracked point exactly where it was at `anchor` (a locked-off shot).
    pub lock: bool,
    /// Otherwise: only movement quicker than this many seconds is taken out — the shake
    /// goes, a slow pan stays. Longer is steadier.
    pub smoothing: f64,
    /// Where the point is held when locked (track units).
    pub anchor: [f64; 2],
    /// The correction at each of the track's points (derived from the above; worked out
    /// again whenever the track or the settings change).
    pub correction: Arc<PointTrack>,
}

impl Stabilize {
    /// A stabilization against `track` (the correction worked out now).
    pub fn new(track: &PointTrack, strength: f64, lock: bool, smoothing: f64, anchor: [f64; 2]) -> Stabilize {
        let strength = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { 1.0 };
        let smoothing = if smoothing.is_finite() { smoothing.clamp(0.01, 60.0) } else { 1.0 };
        let target = if lock { Vec::new() } else { track.smoothed(smoothing) };
        let points = track
            .points
            .iter()
            .enumerate()
            .map(|(i, (t, p))| {
                let goal = if lock { anchor } else { target[i].1 };
                (*t, [strength * (goal[0] - p[0]), strength * (goal[1] - p[1])])
            })
            .collect();
        let correction = Arc::new(PointTrack { id: track.id, name: track.name.clone(), points });
        Stabilize { strength, lock, smoothing, anchor, correction }
    }
}

impl TrackUse {
    /// What it adds at timeline time `t`.
    pub fn contribution(&self, track: &PointTrack, t: Time) -> Option<[f64; 2]> {
        match self {
            TrackUse::Follow => track.at(t),
            TrackUse::Stabilize(s) => s.correction.at(t),
        }
    }

    /// The same settings, for `track`'s (new) points.
    pub fn rebuilt(&self, track: &PointTrack) -> TrackUse {
        match self {
            TrackUse::Follow => TrackUse::Follow,
            TrackUse::Stabilize(s) => TrackUse::Stabilize(Stabilize::new(track, s.strength, s.lock, s.smoothing, s.anchor)),
        }
    }
}

/// A point's path over the timeline — made by the AI tracker, clicked in, or recorded
/// with the mouse. Positions are in position units: fractions of the canvas, from its
/// center (what `transform.position` is, so a centered clip following it sits on it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointTrack {
    pub id: u64,
    pub name: String,
    /// Timeline time and position, sorted by time with no duplicate times.
    pub points: Vec<(Time, [f64; 2])>,
}

impl PointTrack {
    pub fn new(id: u64, name: &str) -> Self {
        PointTrack { id, name: name.into(), points: Vec::new() }
    }

    /// Where it is at `t`: linear between points, held before the first and after the
    /// last; `None` while it has no points.
    pub fn at(&self, t: Time) -> Option<[f64; 2]> {
        let first = self.points.first()?;
        if t <= first.0 {
            return Some(first.1);
        }
        let i = self.points.partition_point(|p| p.0 <= t);
        let Some(b) = self.points.get(i) else { return self.points.last().map(|p| p.1) };
        let a = self.points[i - 1];
        let f = (t - a.0).as_seconds_f64() / (b.0 - a.0).as_seconds_f64().max(1e-12);
        Some([a.1[0] + (b.1[0] - a.1[0]) * f, a.1[1] + (b.1[1] - a.1[1]) * f])
    }

    /// The path with movement quicker than `seconds` taken out: each point a Gaussian-
    /// weighted average of its neighbors in time (works on unevenly spaced points).
    pub fn smoothed(&self, seconds: f64) -> Vec<(Time, [f64; 2])> {
        let sigma = seconds.max(1e-6);
        let reach = 3.0 * sigma;
        let times: Vec<f64> = self.points.iter().map(|p| p.0.as_seconds_f64()).collect();
        let mut lo = 0;
        self.points
            .iter()
            .enumerate()
            .map(|(i, (t, _))| {
                while times[lo] < times[i] - reach {
                    lo += 1;
                }
                let (mut sum, mut total) = ([0.0; 2], 0.0);
                for (j, (_, q)) in self.points.iter().enumerate().skip(lo).take_while(|(j, _)| times[*j] <= times[i] + reach) {
                    let d = (times[j] - times[i]) / sigma;
                    let w = (-0.5 * d * d).exp();
                    sum[0] += w * q[0];
                    sum[1] += w * q[1];
                    total += w;
                }
                (*t, [sum[0] / total, sum[1] / total])
            })
            .collect()
    }

    /// Adds (or moves) the point at `t`.
    pub fn set(&mut self, t: Time, p: [f64; 2]) {
        match self.points.binary_search_by_key(&t, |q| q.0) {
            Ok(i) => self.points[i].1 = p,
            Err(i) => self.points.insert(i, (t, p)),
        }
    }

    /// Replaces the points from the first of `points` to the last of them; the rest stay.
    pub fn replace(&mut self, points: &[(Time, [f64; 2])]) {
        let (Some(lo), Some(hi)) = (points.first().map(|p| p.0), points.last().map(|p| p.0)) else { return };
        self.points.retain(|p| p.0 < lo || p.0 > hi);
        self.points.extend_from_slice(points);
        self.sanitize();
    }

    fn is_clean(&self) -> bool {
        self.points.windows(2).all(|w| w[0].0 < w[1].0) && self.points.iter().all(|p| p.1.iter().all(|x| x.is_finite()))
    }

    /// Sorted, no duplicate times, finite.
    pub fn sanitize(&mut self) {
        self.points.retain(|p| p.1.iter().all(|x| x.is_finite()));
        self.points.sort_by_key(|p| p.0);
        self.points.dedup_by_key(|p| p.0);
    }
}

/// The shape of an [`Modulator::Lfo`] cycle.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LfoWave {
    Sine,
    Triangle,
    Square,
    /// Rises steadily, then drops back.
    Saw,
}

impl LfoWave {
    /// The wave at cycle position `x` (any real; one cycle per 1.0), in `[-1, 1]`.
    pub fn at(self, x: f64) -> f64 {
        let f = x.rem_euclid(1.0);
        match self {
            LfoWave::Sine => (f * std::f64::consts::TAU).sin(),
            LfoWave::Triangle => 1.0 - 4.0 * (f - 0.25).rem_euclid(1.0).min(1.0 - (f - 0.25).rem_euclid(1.0)),
            LfoWave::Square => if f < 0.5 { 1.0 } else { -1.0 },
            LfoWave::Saw => 2.0 * f - 1.0,
        }
    }
}

impl Modulator {
    /// The float sources driving it (amplitude and frequency, …).
    fn rates_mut(&mut self) -> Vec<&mut Box<ParamSource>> {
        match self {
            Modulator::Wiggle { amplitude, frequency, .. } | Modulator::Lfo { amplitude, frequency, .. } => vec![amplitude, frequency],
            Modulator::Follow { amount, floor_db, .. } => vec![amount, floor_db],
            Modulator::Track { .. } | Modulator::TrackPair { .. } => Vec::new(),
        }
    }

    fn rates(&self) -> Vec<&ParamSource> {
        match self {
            Modulator::Wiggle { amplitude, frequency, .. } | Modulator::Lfo { amplitude, frequency, .. } => vec![amplitude, frequency],
            Modulator::Follow { amount, floor_db, .. } => vec![amount, floor_db],
            Modulator::Track { .. } | Modulator::TrackPair { .. } => Vec::new(),
        }
    }

    fn shift_clip_clock(&mut self, delta: Time) {
        for s in self.rates_mut() {
            s.shift_clip_clock(delta);
        }
        match self {
            Modulator::Wiggle { anchor, offset, .. } | Modulator::Lfo { anchor, offset, .. } => {
                if *anchor == KeyframeAnchor::ClipStart {
                    *offset += delta;
                }
            }
            // It listens at the timeline instant, which a clip-clock shift doesn't move.
            Modulator::Follow { .. } => {}
            // Same instants, same place on the track.
            Modulator::Track { clock, .. } | Modulator::TrackPair { clock, .. } => *clock += delta,
        }
    }

    fn apply(&self, value: Value, ctx: &EvalContext) -> Value {
        match self {
            Modulator::Track { track, clock, mode } => match (value, mode.contribution(track, ctx.clip_time + *clock)) {
                (Value::Vec2([a, b]), Some([x, y])) => Value::Vec2([a + x, b + y]),
                (v, _) => v,
            },
            Modulator::TrackPair { a, b, clock, measure, aspect, reference } => {
                let t = ctx.clip_time + *clock;
                let (Some(pa), Some(pb), Value::Float(v)) = (a.at(t), b.at(t), &value) else { return value };
                let now = measure.of(pa, pb, *aspect);
                match measure {
                    PairMeasure::Scale if *reference > 1e-9 => Value::Float(v * now / reference),
                    // The turn since linking, the short way round.
                    PairMeasure::Rotation => Value::Float(v + (now - reference + 180.0).rem_euclid(360.0) - 180.0),
                    _ => value,
                }
            }
            Modulator::Follow { source, band, amount, floor_db, axis } => {
                let amount = amount.eval(ctx).as_float().unwrap_or(0.0);
                if amount == 0.0 {
                    return value;
                }
                let floor = floor_db.eval(ctx).as_float().unwrap_or(-48.0);
                let d = amount * signal::normalized(signal::level(*source, *band), floor);
                match value {
                    Value::Float(v) => Value::Float(v + d),
                    Value::Int(v) => Value::Int(v + d.round() as i64),
                    // One component only, when it says so.
                    Value::Vec2(mut v) => {
                        for (k, c) in v.iter_mut().enumerate() {
                            if axis.is_none_or(|a| a as usize == k) {
                                *c += d;
                            }
                        }
                        Value::Vec2(v)
                    }
                    Value::Vec3(mut v) => {
                        for (k, c) in v.iter_mut().enumerate() {
                            if axis.is_none_or(|a| a as usize == k) {
                                *c += d;
                            }
                        }
                        Value::Vec3(v)
                    }
                    Value::Color([r, g, b, a]) => Value::Color([r + d, g + d, b + d, a]),
                    other => other,
                }
            }
            Modulator::Lfo { wave, amplitude, frequency, phase, decay, anchor, offset } => {
                let amp = amplitude.eval(ctx).as_float().unwrap_or(0.0);
                if amp == 0.0 {
                    return value;
                }
                let freq = frequency.eval(ctx).as_float().unwrap_or(0.0);
                let t = match anchor {
                    KeyframeAnchor::ClipStart => ctx.clip_time,
                    KeyframeAnchor::SourceMedia => ctx.source_time,
                } + *offset;
                let s = t.as_seconds_f64();
                // Decay: the swing shrinks by e every `1 / decay` seconds (0 keeps it steady).
                let fade = if *decay > 0.0 { (-decay * s.max(0.0)).exp() } else { 1.0 };
                let d = amp * fade * wave.at(s * freq + phase);
                match value {
                    Value::Float(v) => Value::Float(v + d),
                    Value::Vec2([a, b]) => Value::Vec2([a + d, b + d]),
                    Value::Vec3([a, b, c]) => Value::Vec3([a + d, b + d, c + d]),
                    Value::Color([r, g, b, a]) => Value::Color([r + d, g + d, b + d, a]),
                    other => other,
                }
            }
            Modulator::Wiggle { amplitude, frequency, octaves, seed, anchor, offset } => {
                let amp = amplitude.eval(ctx).as_float().unwrap_or(0.0);
                let freq = frequency.eval(ctx).as_float().unwrap_or(0.0);
                if amp == 0.0 {
                    return value;
                }
                let t = match anchor {
                    KeyframeAnchor::ClipStart => ctx.clip_time,
                    KeyframeAnchor::SourceMedia => ctx.source_time,
                } + *offset;
                let x = t.as_seconds_f64() * freq;
                let n = |component: u64| amp * noise::fractal(*seed ^ component.wrapping_mul(0x9E37_79B9), x, *octaves);
                match value {
                    Value::Float(v) => Value::Float(v + n(0)),
                    Value::Vec2([a, b]) => Value::Vec2([a + n(0), b + n(1)]),
                    Value::Vec3([a, b, c]) => Value::Vec3([a + n(0), b + n(1), c + n(2)]),
                    Value::Color([r, g, b, a]) => Value::Color([r + n(0), g + n(1), b + n(2), a]),
                    other => other,
                }
            }
        }
    }
}

mod noise {
    fn hash(seed: u64, i: i64) -> f64 {
        // splitmix64 → [-1, 1]
        let mut z = seed.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// Smooth 1D value noise in [-1, 1].
    fn value(seed: u64, x: f64) -> f64 {
        let i = x.floor();
        let f = x - i;
        let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0); // quintic smoothstep
        let (a, b) = (hash(seed, i as i64), hash(seed, i as i64 + 1));
        a + (b - a) * s
    }

    /// Octaves of value noise, normalized back to [-1, 1].
    pub fn fractal(seed: u64, x: f64, octaves: u32) -> f64 {
        let (mut sum, mut norm, mut amp, mut freq) = (0.0, 0.0, 1.0, 1.0);
        for o in 0..octaves.clamp(1, 8) {
            sum += amp * value(seed.wrapping_add(o as u64 * 7919), x * freq);
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm
    }
}

/// The two clocks a keyframe can follow, both already resolved for one instant.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct EvalContext {
    /// Time since the clip's start on the timeline.
    pub clip_time: Time,
    /// Time in the source media (after the clip's time map).
    pub source_time: Time,
}

impl EvalContext {
    pub fn at(clip_time: Time, source_time: Time) -> Self {
        EvalContext { clip_time, source_time }
    }
}

/// Stored parameter values for one clip/effect. Missing entries mean "default".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamSet(pub BTreeMap<ParamId, ParamSource>);

impl ParamSet {
    pub fn get(&self, id: &str) -> Option<&ParamSource> {
        self.0.get(id)
    }

    pub fn set(&mut self, id: &str, source: ParamSource) -> Option<ParamSource> {
        self.0.insert(ParamId::new(id), source)
    }

    pub fn remove(&mut self, id: &str) -> Option<ParamSource> {
        self.0.remove(id)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Evaluates every schema parameter, applying `overrides` first (e.g. a format
    /// variant's values), then stored values, then schema defaults. Values whose type
    /// doesn't match the schema fall back to the default rather than failing a render.
    pub fn eval(
        &self,
        schema: &[ParamSchema],
        overrides: Option<&ParamSet>,
        ctx: &EvalContext,
    ) -> Evaluated {
        let values = schema
            .iter()
            .map(|s| {
                let src = overrides
                    .and_then(|o| o.0.get(&s.id))
                    .or_else(|| self.0.get(&s.id));
                let v = src.and_then(|src| src.eval(ctx).coerce(s.ty));
                (s.id.clone(), v.unwrap_or_else(|| s.default.clone()))
            })
            .collect();
        Evaluated(values)
    }
}

impl std::borrow::Borrow<str> for ParamId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Plain values for one instant, in schema order.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluated(pub Vec<(ParamId, Value)>);

impl Evaluated {
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.as_str() == id).map(|(_, v)| v)
    }

    pub fn float(&self, id: &str) -> f64 {
        self.get(id).and_then(Value::as_float).unwrap_or(0.0)
    }

    pub fn vec2(&self, id: &str) -> [f64; 2] {
        self.get(id).and_then(Value::as_vec2).unwrap_or([0.0; 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A many-option enum keeps what's chosen in option order, ignores what it doesn't
    /// know, and hands shaders a bitmask; a plain enum still hands over its index.
    #[test]
    fn many_option_enums() {
        let s = ParamSchema::new("channels", Value::Enum("red,green,blue".into()), Unit::None).options(&["red", "green", "blue", "alpha"]).multiple();
        assert_eq!(s.chosen(&s.default), vec!["red", "green", "blue"]);
        assert_eq!(s.enum_uniform(&s.default), 7.0);
        let v = s.choose(&["alpha", "red", "purple"]);
        assert_eq!(v, Value::Enum("red,alpha".into()), "option order, unknown left out");
        assert_eq!(s.enum_uniform(&v), 9.0);
        assert_eq!(s.enum_uniform(&s.choose(&[])), 0.0);
        assert_eq!(s.chosen(&Value::Enum(" blue , purple".into())), vec!["blue"]);
        let plain = ParamSchema::new("mode", Value::Enum("over".into()), Unit::None).options(&["behind", "over"]);
        assert_eq!(plain.enum_uniform(&plain.default), 1.0);
    }

    fn secs(s: f64) -> Time {
        Time::from_seconds_f64(s)
    }

    #[test]
    fn overrides_then_stored_then_default() {
        let schema = [
            ParamSchema::new("a", Value::Float(1.0), Unit::None),
            ParamSchema::new("b", Value::Float(2.0), Unit::None),
            ParamSchema::new("c", Value::Float(3.0), Unit::None),
        ];
        let mut stored = ParamSet::default();
        stored.set("a", ParamSource::Static(Value::Float(10.0)));
        stored.set("b", ParamSource::Static(Value::Float(20.0)));
        stored.set("unknown.from.newer.plugin", ParamSource::Static(Value::Bool(true)));
        let mut ov = ParamSet::default();
        ov.set("b", ParamSource::Static(Value::Float(200.0)));
        let e = stored.eval(&schema, Some(&ov), &EvalContext::at(Time::ZERO, Time::ZERO));
        assert_eq!((e.float("a"), e.float("b"), e.float("c")), (10.0, 200.0, 3.0));
        assert!(stored.get("unknown.from.newer.plugin").is_some());
    }

    #[test]
    fn type_mismatch_falls_back_to_default() {
        let schema = [ParamSchema::new("a", Value::Float(1.0), Unit::None)];
        let mut stored = ParamSet::default();
        stored.set("a", ParamSource::Static(Value::Bool(true)));
        let e = stored.eval(&schema, None, &EvalContext::at(Time::ZERO, Time::ZERO));
        assert_eq!(e.float("a"), 1.0);
    }

    #[test]
    fn wiggle_is_deterministic_bounded_and_moves() {
        let src = ParamSource::Static(Value::Vec2([100.0, 50.0])).wiggle(4.0, 8.0, 42);
        let at = |s: f64| src.eval(&EvalContext::at(secs(s), secs(s))).as_vec2().unwrap();
        let mut distinct = std::collections::BTreeSet::new();
        for i in 0..200 {
            let v = at(i as f64 / 60.0);
            assert!((v[0] - 100.0).abs() <= 4.0 && (v[1] - 50.0).abs() <= 4.0, "{v:?}");
            assert_eq!(v, at(i as f64 / 60.0));
            distinct.insert(v[0].to_bits());
        }
        assert!(distinct.len() > 150);
        // Different seed, different shake; components move independently.
        let other = ParamSource::Static(Value::Vec2([100.0, 50.0])).wiggle(4.0, 8.0, 43);
        let t = EvalContext::at(secs(0.37), secs(0.37));
        assert_ne!(other.eval(&t), src.eval(&t));
        let v = at(0.37);
        assert_ne!(v[0] - 100.0, v[1] - 50.0);
    }

    #[test]
    fn shake_intensity_can_be_keyframed() {
        // Shake ramps from nothing to 10px over one second.
        let amplitude = ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::linear(secs(0.0), Value::Float(0.0)), Keyframe::linear(secs(1.0), Value::Float(10.0))],
        ));
        let src = ParamSource::Modulated {
            base: Box::new(ParamSource::Static(Value::Float(5.0))),
            modulator: Modulator::Wiggle {
                amplitude: Box::new(amplitude),
                frequency: Box::new(ParamSource::Static(Value::Float(12.0))),
                octaves: 3,
                seed: 7,
                anchor: KeyframeAnchor::ClipStart,
                offset: Time::ZERO,
            },
        };
        let at = |s: f64| src.eval(&EvalContext::at(secs(s), secs(s))).as_float().unwrap();
        assert_eq!(at(0.0), 5.0);
        let max_early = (0..50).map(|i| (at(i as f64 * 0.001) - 5.0).abs()).fold(0.0, f64::max);
        let max_late = (0..50).map(|i| (at(1.0 + i as f64 * 0.013) - 5.0).abs()).fold(0.0, f64::max);
        assert!(max_early < 0.5 && max_late > 2.0, "{max_early} {max_late}");
        assert!(src.is_animated());
    }

    #[test]
    fn shifting_the_clip_clock_keeps_every_value() {
        let keys = ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::linear(secs(0.0), Value::Float(0.0)), Keyframe::ease(secs(2.0), Value::Float(8.0))],
        ));
        let glued = ParamSource::Animated(Curve::new(
            KeyframeAnchor::SourceMedia,
            vec![Keyframe::linear(secs(0.0), Value::Float(0.0)), Keyframe::linear(secs(4.0), Value::Float(4.0))],
        ));
        for original in [keys.clone().wiggle(3.0, 5.0, 9), glued.wiggle(1.0, 2.0, 1), keys] {
            // The clip now starts 1.5 s later on the timeline (e.g. the back half of a split).
            let delta = secs(1.5);
            let mut shifted = original.clone();
            shifted.shift_clip_clock(delta);
            for i in 0..40 {
                let (clip, source) = (secs(1.5 + i as f64 * 0.05), secs(10.0 + i as f64 * 0.05));
                let before = original.eval(&EvalContext::at(clip, source));
                let after = shifted.eval(&EvalContext::at(clip - delta, source));
                assert_eq!(before, after, "at {i}");
            }
        }
    }

    #[test]
    fn set_at_keys_animated_values_and_respects_wiggle() {
        let ctx = |s: f64| EvalContext::at(secs(s), secs(s));
        let mut keyed = ParamSource::Animated(Curve::new(
            KeyframeAnchor::ClipStart,
            vec![Keyframe::ease(secs(0.0), Value::Float(0.0)), Keyframe::ease(secs(4.0), Value::Float(4.0))],
        ));
        keyed.set_at(&ctx(2.0), Value::Float(10.0));
        let curve = keyed.curve().unwrap();
        assert_eq!(curve.keys.len(), 3);
        assert_eq!(curve.keys[1].interp, curve.keys[0].interp, "a new key eases like its neighbor");
        assert_eq!(keyed.eval(&ctx(2.0)), Value::Float(10.0));
        assert_eq!(keyed.eval(&ctx(4.0)), Value::Float(4.0), "other keys are untouched");

        let mut shaky = ParamSource::Static(Value::Vec2([1.0, 1.0])).wiggle(0.5, 3.0, 11);
        shaky.set_at(&ctx(0.7), Value::Vec2([5.0, -2.0]));
        let v = shaky.eval(&ctx(0.7)).as_vec2().unwrap();
        assert!((v[0] - 5.0).abs() < 1e-12 && (v[1] + 2.0).abs() < 1e-12, "{v:?}");
        assert!(shaky.is_animated(), "the shake is still there");

        let mut still = ParamSource::Static(Value::Float(1.0));
        still.set_at(&ctx(3.0), Value::Float(2.0));
        assert_eq!(still, ParamSource::Static(Value::Float(2.0)));
    }

    #[test]
    fn anchors_pick_the_right_clock() {
        let curve = |anchor| {
            Curve::new(
                anchor,
                vec![
                    Keyframe::linear(secs(0.0), Value::Float(0.0)),
                    Keyframe::linear(secs(10.0), Value::Float(10.0)),
                ],
            )
        };
        // Clip head trimmed by 2s: clip_time 1s shows source_time 3s.
        let ctx = EvalContext::at(secs(1.0), secs(3.0));
        let clip = curve(KeyframeAnchor::ClipStart).eval(&ctx).as_float().unwrap();
        let src = curve(KeyframeAnchor::SourceMedia).eval(&ctx).as_float().unwrap();
        assert!((clip - 1.0).abs() < 1e-9);
        assert!((src - 3.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod lfo_tests {
    use super::*;

    fn at(s: f64) -> EvalContext {
        let t = Time::from_seconds_f64(s);
        EvalContext::at(t, t)
    }

    #[test]
    fn waves_hit_their_peaks() {
        for wave in [LfoWave::Sine, LfoWave::Triangle] {
            assert!((wave.at(0.25) - 1.0).abs() < 1e-9 && (wave.at(0.75) + 1.0).abs() < 1e-9, "{wave:?}");
            assert!(wave.at(0.0).abs() < 1e-9, "{wave:?} starts at the middle");
        }
        assert_eq!((LfoWave::Square.at(0.1), LfoWave::Square.at(0.6)), (1.0, -1.0));
        assert!((LfoWave::Saw.at(0.0) + 1.0).abs() < 1e-9 && LfoWave::Saw.at(0.5).abs() < 1e-9);
    }

    #[test]
    fn lfo_oscillates_around_the_base_and_survives_a_split() {
        let src = ParamSource::Static(Value::Float(1.0)).lfo(LfoWave::Sine, 0.5, 2.0);
        assert!(src.is_valid());
        let v = |s: f64| src.eval(&at(s)).as_float().unwrap();
        assert!((v(0.0) - 1.0).abs() < 1e-9);
        assert!((v(0.125) - 1.5).abs() < 1e-9, "a quarter cycle at 2 Hz: the peak");
        assert!((v(0.375) - 0.5).abs() < 1e-9);
        // A split 0.3 s in: the second half carries on exactly where it was.
        let mut later = src.clone();
        later.shift_clip_clock(Time::from_seconds_f64(0.3));
        assert!((later.eval(&at(0.1)).as_float().unwrap() - v(0.4)).abs() < 1e-6);
        // Serializes and reads back.
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(serde_json::from_str::<ParamSource>(&json).unwrap(), src);
    }
}

#[cfg(test)]
mod lfo_decay_tests {
    use super::*;

    #[test]
    fn decay_shrinks_the_swing_and_the_wave_can_be_found_and_removed() {
        let mut src = ParamSource::Static(Value::Float(0.0)).lfo(LfoWave::Square, 1.0, 1.0).wiggle(0.0, 1.0, 7);
        let (_, m) = src.find_lfo_mut().expect("under the wiggle");
        if let Modulator::Lfo { decay, .. } = m {
            *decay = 1.0;
        }
        let at = |s: f64| {
            let t = Time::from_seconds_f64(s);
            src.eval(&EvalContext::at(t, t)).as_float().unwrap()
        };
        assert!((at(0.1) - 1.0f64.mul_add((-0.1f64).exp(), 0.0)).abs() < 1e-9);
        assert!((at(2.1) - (-2.1f64).exp()).abs() < 1e-9, "one e-fold per second");
        assert!(src.is_valid());
        let plain = src.clone().remove_lfo();
        assert!(plain.find_lfo().is_none());
        assert_eq!(plain.eval(&EvalContext::at(Time::ZERO, Time::ZERO)).as_float(), Some(0.0));
    }
}

#[cfg(test)]
mod track_pair_tests {
    use super::*;

    fn track(id: u64, points: &[(i64, [f64; 2])]) -> Arc<PointTrack> {
        Arc::new(PointTrack { id, name: format!("t{id}"), points: points.iter().map(|(s, p)| (Time::from_seconds(*s), *p)).collect() })
    }

    /// Two tracked points drifting apart and turning: a linked scale grows with their
    /// distance, a linked rotation turns with the line between them — both from the
    /// value they had when linked, which stays keyframable underneath.
    #[test]
    fn two_tracks_scale_and_turn() {
        let a = track(1, &[(0, [0.25, 0.5]), (2, [0.25, 0.5])]);
        let b = track(2, &[(0, [0.75, 0.5]), (2, [0.25, 0.0])]);
        let at = |s: i64| EvalContext::at(Time::from_seconds(s), Time::from_seconds(s));
        let pair = |m| Modulator::track_pair(a.clone(), b.clone(), Time::ZERO, m, 2.0, Time::ZERO).unwrap();
        // On a 2:1 picture: 1.0 across at the start, 0.5 straight up at the end.
        let scale = ParamSource::Static(Value::Float(1.5)).with_track_pair(pair(PairMeasure::Scale));
        assert_eq!(scale.eval(&at(0)).as_float(), Some(1.5));
        assert!((scale.eval(&at(2)).as_float().unwrap() - 0.75).abs() < 1e-9);
        let turn = ParamSource::Static(Value::Float(10.0)).with_track_pair(pair(PairMeasure::Rotation));
        assert!((turn.eval(&at(2)).as_float().unwrap() - (10.0 - 90.0)).abs() < 1e-9);
        assert!(turn.track_pair().is_some());
        // Unlinked: its own value again; a moved track is picked up.
        assert_eq!(turn.clone().without_track_pair().eval(&at(2)).as_float(), Some(10.0));
        let moved = track(2, &[(0, [0.75, 0.5]), (2, [0.5, 1.0])]);
        assert!((turn.retracked(&moved).eval(&at(2)).as_float().unwrap() - (10.0 + 45.0)).abs() < 1e-9);
        // Saved and loaded.
        let json = serde_json::to_string(&scale).unwrap();
        assert_eq!(serde_json::from_str::<ParamSource>(&json).unwrap(), scale);
    }
}
