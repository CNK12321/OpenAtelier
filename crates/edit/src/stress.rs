//! A stand-in project for measuring how long an export takes.
//!
//! Given how long the finished video should be and how hard the edit works the renderer
//! (its *intensity*), this builds a timeline like a real one: cuts of media on the
//! bottom track with transitions between them, pictures and titles over it, sound
//! underneath, effects with keyframed and wiggling settings throughout. It's built from
//! one seed, so the same recipe always gives the same project — two measurements can be
//! compared — and every effect comes from the registry it's given, so a new effect (or
//! a plugin's) is part of the measurement as soon as it exists.
//!
//! `oa stress` builds one, exports a slice of it and reports what the whole thing would
//! take.

use oa_doc::{
    schema, CanvasSize, ClipEnd, Document, EditError, EffectId, EffectInstance, EffectRole, FormatVariant, Item, ItemId, ItemKind, MediaId, Op, SeqId, Sequence, Track, TrackId,
    TrackKind, Transition, VariantId,
};
use oa_graph::registry::{EffectUsage, Registry};
use oa_params::{Curve, Keyframe, KeyframeAnchor, LfoWave, ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use std::sync::Arc;

/// What a piece of media in the pool can be used for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Video,
    Still,
    Audio,
}

/// A piece of media the generated project may use.
#[derive(Clone, Debug)]
pub struct Source {
    pub media: MediaId,
    pub kind: SourceKind,
    /// How much of it there is (stills: as long as you like).
    pub duration: Time,
    pub name: String,
}

/// What to build.
#[derive(Clone, Debug)]
pub struct Recipe {
    /// How long the finished video is.
    pub length: Time,
    /// 0 = plain cuts, nothing else; 1 = every track, effect and keyframe at once.
    pub intensity: f64,
    pub rate: FrameRate,
    pub size: CanvasSize,
    pub seed: u64,
}

impl Default for Recipe {
    fn default() -> Self {
        Recipe { length: Time::from_seconds(60), intensity: 0.5, rate: FrameRate::FPS_30, size: CanvasSize { width: 1920, height: 1080 }, seed: 1 }
    }
}

/// What was built, for the report.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    pub seq: SeqId,
    pub clips: usize,
    pub effects: usize,
    pub transitions: usize,
    pub keyframed: usize,
    pub modulated: usize,
    pub video_tracks: usize,
    pub audio_tracks: usize,
}

/// The effects a clip can be given: passive ones, intros and outros, and (on titles)
/// text effects.
struct Menu {
    passive: Vec<String>,
    in_out: Vec<String>,
    text: Vec<String>,
}

/// A small deterministic random source (xorshift): the same seed always builds the same
/// project, so two measurements of it mean something.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// 0..1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> Option<&'a T> {
        (!from.is_empty()).then(|| &from[(self.next() % from.len() as u64) as usize])
    }
}

/// Builds the project into `doc` (one edit) and says what it made.
pub fn build(doc: &mut Document, sources: &[Source], recipe: &Recipe, registry: &Registry) -> Result<Built, EditError> {
    let mut rng = Rng(recipe.seed | 1);
    let hard = recipe.intensity.clamp(0.0, 1.0);
    let length = recipe.length.max(Time::from_seconds(1));

    let video: Vec<&Source> = sources.iter().filter(|s| s.kind == SourceKind::Video).collect();
    let stills: Vec<&Source> = sources.iter().filter(|s| s.kind == SourceKind::Still).collect();
    let audio: Vec<&Source> = sources.iter().filter(|s| s.kind == SourceKind::Audio).collect();

    // The effects to draw on, from the registry itself.
    let ids = |list: Vec<&Arc<oa_graph::registry::EffectDescriptor>>| list.iter().map(|d| d.type_id.to_string()).collect::<Vec<_>>();
    let menu = Menu {
        passive: ids(registry.offered(EffectUsage::Passive)),
        in_out: ids(registry.offered(EffectUsage::InOut)),
        text: ids(registry.offered_for(EffectUsage::Passive, true).into_iter().filter(|d| d.kind.text_only()).collect()),
    };
    let cuts = ids(registry.offered(EffectUsage::Cut));
    let sounds: Vec<String> = registry.effects().filter(|d| registry.is_sound(&d.type_id)).map(|d| d.type_id.to_string()).collect();

    let seq = SeqId(doc.alloc_id());
    let variant = FormatVariant { id: VariantId(doc.alloc_id()), name: "Wide".into(), size: recipe.size, overrides: Default::default() };
    let mut ops = vec![Op::AddSequence(Arc::new(Sequence::new(seq, "Stress", recipe.rate, variant)))];
    let mut built = Built { seq, clips: 0, effects: 0, transitions: 0, keyframed: 0, modulated: 0, video_tracks: 0, audio_tracks: 0 };

    // How many tracks: one of each at 0, four picture and three sound at 1.
    let video_tracks = 1 + (hard * 3.0).round() as usize;
    let audio_tracks = 1 + (hard * 2.0).round() as usize;
    built.video_tracks = video_tracks;
    built.audio_tracks = audio_tracks;
    let mut track_ids = Vec::new();
    for i in 0..video_tracks {
        let id = TrackId(doc.alloc_id());
        track_ids.push(id);
        ops.push(Op::InsertTrack { seq, index: i, track: Arc::new(Track::new(id, &format!("V{}", i + 1), TrackKind::Video)) });
    }
    let mut audio_ids = Vec::new();
    for i in 0..audio_tracks {
        let id = TrackId(doc.alloc_id());
        audio_ids.push(id);
        ops.push(Op::InsertTrack { seq, index: video_tracks + i, track: Arc::new(Track::new(id, &format!("A{}", i + 1), TrackKind::Audio)) });
    }

    // V1: cut to cut, end to end, the whole length. Busier edits cut faster.
    let mut at = Time::ZERO;
    let mut first = true;
    while at < length {
        let seconds = rng.range(1.0, 2.5) * (6.0 - 4.5 * hard) / 3.0;
        let duration = Time::from_seconds_f64(seconds).min(length - at).max(recipe.rate.frame_start(2));
        let source = rng.pick(&video).or_else(|| rng.pick(&stills)).copied();
        let mut item = Item::new(ItemId(doc.alloc_id()), "cut", kind_of(source), TimeRange::new(at, duration));
        if let Some(s) = source.filter(|s| s.kind == SourceKind::Video && s.duration > duration) {
            item.time_map.source_in = Time::from_seconds_f64(rng.range(0.0, (s.duration - duration).as_seconds_f64()));
        }
        dress(&mut item, doc, &mut rng, hard, &menu, false, &mut built);
        // A transition on the cut before it.
        if !first && rng.chance(0.65 * hard) && let Some(t) = rng.pick(&cuts) {
            *item.transition_mut(ClipEnd::Head) = Some(Transition::new(t, Time::from_seconds_f64(rng.range(0.3, 0.9))));
            built.transitions += 1;
        }
        built.clips += 1;
        ops.push(Op::InsertItem { seq, track: track_ids[0], item });
        at += duration;
        first = false;
    }

    // The tracks above: pictures and titles, here and there, over the cuts.
    for (n, track) in track_ids.iter().enumerate().skip(1) {
        let mut at = Time::from_seconds_f64(rng.range(0.0, 3.0));
        while at < length {
            let duration = Time::from_seconds_f64(rng.range(1.5, 4.0)).min(length - at);
            if duration <= Time::ZERO {
                break;
            }
            let title = stills.is_empty() || rng.chance(0.5);
            let id = ItemId(doc.alloc_id());
            let mut item = if title {
                let mut item = Item::new(id, "title", ItemKind::Text, TimeRange::new(at, duration));
                item.params.set(schema::TEXT_CONTENT, ParamSource::Static(Value::Text(format!("Layer {} · {:.0}s", n + 1, at.as_seconds_f64()))));
                item.params.set(schema::TEXT_SIZE, ParamSource::Static(Value::Float(rng.range(60.0, 140.0))));
                item
            } else {
                let s = rng.pick(&stills).copied();
                Item::new(id, "picture", kind_of(s), TimeRange::new(at, duration))
            };
            dress(&mut item, doc, &mut rng, hard, &menu, title, &mut built);
            built.clips += 1;
            ops.push(Op::InsertItem { seq, track: *track, item });
            // A gap, shorter the busier the edit is.
            at += duration + Time::from_seconds_f64(rng.range(0.2, 4.0) * (1.2 - hard));
        }
    }

    // Sound: clips end to end, with gain moves and sound effects.
    for track in &audio_ids {
        if audio.is_empty() {
            break;
        }
        let mut at = Time::ZERO;
        while at < length {
            let source = rng.pick(&audio).copied().expect("not empty");
            let duration = Time::from_seconds_f64(rng.range(4.0, 12.0)).min(length - at).min(source.duration);
            if duration <= Time::ZERO {
                break;
            }
            let mut item = Item::new(ItemId(doc.alloc_id()), &source.name, ItemKind::Media { media: source.media }, TimeRange::new(at, duration));
            if rng.chance(0.9 * hard) {
                let mut gains = Rng(rng.next() | 1);
                item.params.set(schema::AUDIO_GAIN, keyframes(&mut rng, duration, || Value::Float(gains.range(-8.0, 4.0))));
                built.keyframed += 1;
            }
            for _ in 0..(hard * 2.0).round() as usize {
                if let Some(id) = rng.pick(&sounds) {
                    item.effects.push(EffectInstance::new(EffectId(doc.alloc_id()), id));
                    built.effects += 1;
                }
            }
            built.clips += 1;
            ops.push(Op::InsertItem { seq, track: *track, item });
            at += duration;
        }
    }

    doc.edit("Build a stress project", ops)?;
    Ok(built)
}

fn kind_of(source: Option<&Source>) -> ItemKind {
    match source {
        Some(s) => ItemKind::Media { media: s.media },
        // Nothing in the pool: a solid still gives the renderer something to do.
        None => ItemKind::Solid,
    }
}


/// A few keyframes across a clip, eased.
fn keyframes(rng: &mut Rng, duration: Time, mut value: impl FnMut() -> Value) -> ParamSource {
    let count = 2 + (rng.unit() * 3.0) as usize;
    let keys = (0..count)
        .map(|i| {
            let t = Time::from_seconds_f64(duration.as_seconds_f64() * i as f64 / (count - 1) as f64);
            if i % 2 == 0 { Keyframe::ease(t, value()) } else { Keyframe::linear(t, value()) }
        })
        .collect();
    ParamSource::Animated(Curve::new(KeyframeAnchor::ClipStart, keys))
}

/// Gives a clip its effects, keyframes and wiggles, by how hard the recipe is working.
fn dress(item: &mut Item, doc: &mut Document, rng: &mut Rng, hard: f64, menu: &Menu, text: bool, built: &mut Built) {
    let (passive, in_out) = (&menu.passive[..], &menu.in_out[..]);
    let text_fx: &[String] = if text { &menu.text } else { &[] };
    let duration = item.range.duration;
    // Effects: none at 0, a few at 1 (a title's own text effects among them).
    let count = (hard * 3.0).round() as usize;
    for _ in 0..count {
        let from = if !text_fx.is_empty() && rng.chance(0.5) { text_fx } else { passive };
        if let Some(id) = rng.pick(from) {
            item.effects.push(EffectInstance::new(EffectId(doc.alloc_id()), id));
            built.effects += 1;
        }
    }
    // An intro, an outro, or both.
    for (role, chance) in [(EffectRole::In { duration: Time::from_seconds_f64(0.6) }, 0.7 * hard), (EffectRole::Out { duration: Time::from_seconds_f64(0.6) }, 0.55 * hard)] {
        if rng.chance(chance)
            && let Some(id) = rng.pick(in_out)
        {
            let mut fx = EffectInstance::new(EffectId(doc.alloc_id()), id);
            fx.role = role;
            item.effects.push(fx);
            built.effects += 1;
        }
    }
    // Moving about: keyframes on the position or the scale…
    if rng.chance(0.85 * hard) {
        let scale = rng.chance(0.5);
        let (param, mut make): (&str, Box<dyn FnMut() -> Value>) = if scale {
            ("transform.scale", Box::new(|| Value::Vec2([1.0, 1.0])))
        } else {
            ("transform.position", Box::new(|| Value::Vec2([0.0, 0.0])))
        };
        let spread = if scale { 0.25 } else { 0.2 };
        let mut values = Vec::new();
        for _ in 0..5 {
            let base = make();
            let Value::Vec2([x, y]) = base else { continue };
            values.push(Value::Vec2([x + rng.range(-spread, spread), y + rng.range(-spread, spread)]));
        }
        let mut i = 0;
        item.params.set(param, keyframes(rng, duration, || {
            i += 1;
            values.get(i % values.len()).cloned().unwrap_or(Value::Vec2([0.0, 0.0]))
        }));
        built.keyframed += 1;
    }
    // … and wiggles and waves on top (procedural, evaluated every frame).
    if rng.chance(0.65 * hard) {
        let source = ParamSource::Static(Value::Float(0.0)).wiggle(rng.range(1.0, 8.0), rng.range(0.5, 4.0), rng.next());
        item.params.set(schema::ROTATION, source);
        built.modulated += 1;
    }
    if rng.chance(0.5 * hard) {
        let wave = [LfoWave::Sine, LfoWave::Triangle, LfoWave::Square, LfoWave::Saw][(rng.next() % 4) as usize];
        item.params.set(schema::OPACITY, ParamSource::Static(Value::Float(0.9)).lfo(wave, 0.1, rng.range(0.2, 2.0)));
        built.modulated += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::Project;

    fn sources() -> Vec<Source> {
        vec![
            Source { media: MediaId(1), kind: SourceKind::Video, duration: Time::from_seconds(30), name: "a".into() },
            Source { media: MediaId(2), kind: SourceKind::Video, duration: Time::from_seconds(20), name: "b".into() },
            Source { media: MediaId(3), kind: SourceKind::Still, duration: Time::from_seconds(5), name: "c".into() },
            Source { media: MediaId(4), kind: SourceKind::Audio, duration: Time::from_seconds(60), name: "d".into() },
        ]
    }

    fn make(intensity: f64, seed: u64) -> (Document, Built) {
        let mut doc = Document::new(Project::new("stress"));
        let recipe = Recipe { length: Time::from_seconds(60), intensity, seed, ..Default::default() };
        let built = build(&mut doc, &sources(), &recipe, &Registry::with_builtins()).expect("builds");
        (doc, built)
    }

    /// The bottom track is covered end to end, whatever the intensity.
    #[test]
    fn the_cut_covers_the_whole_length() {
        for intensity in [0.0, 0.5, 1.0] {
            let (doc, built) = make(intensity, 7);
            let s = doc.project().sequence(built.seq).expect("built");
            assert!(s.duration() >= Time::from_seconds(60), "{intensity}: {:?}", s.duration());
            let v1 = &s.tracks[0];
            assert!(v1.items.len() > 5, "{intensity}: {} cuts", v1.items.len());
            // End to end, no overlaps (the document would refuse them anyway).
            for pair in v1.items.windows(2) {
                assert_eq!(pair[0].range.end(), pair[1].range.start);
            }
        }
    }

    /// Intensity is the dial: more tracks, effects, keyframes and transitions.
    #[test]
    fn intensity_decides_how_hard_it_works_the_renderer() {
        let (_, plain) = make(0.0, 3);
        let (_, busy) = make(1.0, 3);
        assert_eq!((plain.video_tracks, plain.audio_tracks), (1, 1));
        assert_eq!((busy.video_tracks, busy.audio_tracks), (4, 3));
        assert_eq!(plain.effects, 0, "a plain edit is cuts alone");
        assert!(busy.effects > 40, "{}", busy.effects);
        assert!(busy.clips > plain.clips && busy.transitions > plain.transitions);
        assert!(busy.keyframed > plain.keyframed && busy.modulated > plain.modulated);
    }

    /// The same seed builds the same project; another seed builds a different one.
    #[test]
    fn the_seed_decides_everything() {
        let (a, _) = make(0.7, 11);
        let (b, _) = make(0.7, 11);
        let (c, _) = make(0.7, 12);
        let json = |d: &Document| oa_doc::ProjectFile::to_json(d.project()).expect("json");
        assert_eq!(json(&a), json(&b));
        assert_ne!(json(&a), json(&c));
    }
}
