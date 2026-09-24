//! Sound effects on clips: bass boost, pitch shift, echo, reverb, threshold (a noise
//! gate), denoise — and any a plugin adds, written as sound shaders.
//!
//! They're stored on a clip exactly like picture effects (`EffectInstance`s, in the
//! same list, with the same roles and keyframable params evaluated on the clip's
//! clocks), and run by the mixer on each clip's decoded samples before its gain — so
//! playback and export sound the same. Like picture effects, each one is either native
//! (a processor built into the host) or a shader ([`crate::shader`]): Atelier Core ships
//! some of each, and plugins add shaders through the same [`install`] path.
//!
//! Every processor is streaming and stateful (filters, delay lines, an FFT frame); the
//! mixer resets it on a seek and, for effects that look ahead (denoise), primes it by
//! its [`Processor::latency`] so the output stays in sync with the picture.

pub use crate::dynamics::{eq_bands, eq_response_db, meter, tail_seconds, Band, BandShape, Meter, SPACES};
pub use crate::shader::ClockSpan;
use crate::shader::{Program, ShaderProcessor};
use oa_doc::EffectRole;
use oa_params::{Evaluated, ParamSchema, ParamSet, Unit, Value};
use std::f32::consts::PI;
use std::sync::{Arc, OnceLock, RwLock};

/// Where a sound effect is offered, like a picture effect's usage: over the whole clip,
/// or as the clip's intro/outro (a fade).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FxUsage {
    Passive,
    InOut,
}

/// A sound effect's type: what the UI shows and what runs it.
#[derive(Debug, PartialEq)]
pub struct FxInfo {
    pub type_id: String,
    pub name: String,
    pub description: String,
    pub params: Vec<ParamSchema>,
    pub usage: FxUsage,
    /// The sound shader that runs it; `None` for native effects.
    pub shader: Option<Arc<Program>>,
}

impl FxInfo {
    /// A sound effect written as a sound shader (a plugin's, or a built-in one).
    pub fn shader(type_id: &str, name: &str, description: &str, params: Vec<ParamSchema>, usage: FxUsage, source: &str) -> Result<FxInfo, String> {
        let program = Program::compile(source, &params)?;
        Ok(FxInfo { type_id: type_id.into(), name: name.into(), description: description.into(), params, usage, shader: Some(Arc::new(program)) })
    }

    fn native(type_id: &str, name: &str, description: &str, params: Vec<ParamSchema>) -> FxInfo {
        FxInfo { type_id: type_id.into(), name: name.into(), description: description.into(), params, usage: FxUsage::Passive, shader: None }
    }
}

/// Tone: an oscillator added on top of the sound.
/// * `pitch` in Hz (keyframe it for a glide), wobbled by `vibrato` (Hz) ± `vibrato_depth`
///   semitones; `pulse` is the square wave's duty cycle.
/// * Envelope: rises over `attack`, then (with `decay` > 0) dies away exponentially;
///   `repeat` > 0 restarts it every that many seconds (beeps, a metronome).
/// * `follow` makes it sound only while the clip does (an envelope follower), `original`
///   is how much of the clip's own sound stays; as an intro/outro it fades with the clip.
const TONE: &str = "\
// Where this note is: restarted every `repeat` seconds when that's on.
let t = select(repeat > 0, time % max(repeat, 0.001), time);

// The oscillator.
let hz = pitch * pow(2, vibrato_depth / 12 * sin(TAU * vibrato * time));
state phase = 0;
phase = fract(phase + hz / sample_rate);
let sine = sin(TAU * phase);
let square = select(phase < pulse, 1, -1);
let triangle = 1 - 4 * abs(phase - 0.5);
let saw = 2 * phase - 1;
let osc = select(wave < 0.5, sine, select(wave < 1.5, square, select(wave < 2.5, triangle, select(wave < 3.5, saw, noise()))));

// Its envelope.
let rise = select(attack > 0, clamp(t / max(attack, 0.0001), 0, 1), 1);
let fall = select(decay > 0, exp(-max(t - attack, 0) / max(decay, 0.0001)), 1);

// How loud the clip itself is right now (for `follow`).
state level = 0;
level = max(abs(in), level * exp(-1 / (0.08 * sample_rate)));
let duck = mix(1, clamp(level * 4, 0, 1), follow);

out = in * original + osc * db(gain) * rise * fall * duck * visibility;
";

/// Atelier Core's sound effects, in menu order.
fn builtins() -> &'static [Arc<FxInfo>] {
    static C: OnceLock<Vec<Arc<FxInfo>>> = OnceLock::new();
    C.get_or_init(|| {
        let p = |id: &str, v: f64, unit: Unit, lo: f64, hi: f64| ParamSchema::new(id, Value::Float(v), unit).range(lo, hi);
        let shader = |id: &str, name: &str, description: &str, params: Vec<ParamSchema>, usage: FxUsage, source: &str| {
            FxInfo::shader(id, name, description, params, usage, source).unwrap_or_else(|e| panic!("built-in sound shader {id}: {e}"))
        };
        let list = vec![
            FxInfo::native("oa.audio.bass", "Bass Boost", "Lifts the low end.", vec![p("boost", 8.0, Unit::Decibels, 0.0, 18.0), p("frequency", 110.0, Unit::None, 40.0, 300.0)]),
            FxInfo::native(
                "oa.audio.pitch",
                "Pitch Shift",
                "Higher or lower, same speed. Formant moves the voice's character (child to giant) on its own; \"keep voice\" holds it where it was while the pitch moves.",
                vec![
                    p("semitones", 5.0, Unit::None, -12.0, 12.0),
                    p("mix", 1.0, Unit::None, 0.0, 1.0),
                    p("formant", 0.0, Unit::None, -12.0, 12.0),
                    ParamSchema::new("keep_voice", Value::Bool(false), Unit::None),
                ],
            ),
            FxInfo::native(
                "oa.audio.echo",
                "Echo",
                "Repeats that fade away.",
                vec![p("delay", 0.35, Unit::Seconds, 0.02, 1.5), p("feedback", 0.4, Unit::None, 0.0, 0.9), p("mix", 0.35, Unit::None, 0.0, 1.0)],
            ),
            FxInfo::native(
                "oa.audio.reverb",
                "Reverb",
                "The sound of a room: pick a space, or set your own. It keeps ringing after the clip ends.",
                vec![
                    ParamSchema::new("space", Value::Enum("custom".into()), Unit::None).options(&crate::dynamics::SPACES),
                    p("room", 0.6, Unit::None, 0.0, 1.0),
                    p("damping", 0.4, Unit::None, 0.0, 1.0),
                    p("predelay", 0.0, Unit::Seconds, 0.0, 0.2),
                    p("mix", 0.3, Unit::None, 0.0, 1.0),
                ],
            ),
            FxInfo::native(
                "oa.audio.eq",
                "Equalizer",
                "Shape the tone: a low shelf, three bands and a high shelf. Drag the points on its curve.",
                vec![
                    p("low_freq", 100.0, Unit::None, 20.0, 600.0),
                    p("low_gain", 0.0, Unit::Decibels, -18.0, 18.0),
                    p("p1_freq", 300.0, Unit::None, 40.0, 18000.0),
                    p("p1_gain", 0.0, Unit::Decibels, -18.0, 18.0),
                    p("p1_q", 1.0, Unit::None, 0.2, 10.0),
                    p("p2_freq", 1500.0, Unit::None, 40.0, 18000.0),
                    p("p2_gain", 0.0, Unit::Decibels, -18.0, 18.0),
                    p("p2_q", 1.0, Unit::None, 0.2, 10.0),
                    p("p3_freq", 5000.0, Unit::None, 40.0, 18000.0),
                    p("p3_gain", 0.0, Unit::Decibels, -18.0, 18.0),
                    p("p3_q", 1.0, Unit::None, 0.2, 10.0),
                    p("high_freq", 8000.0, Unit::None, 1500.0, 18000.0),
                    p("high_gain", 0.0, Unit::Decibels, -18.0, 18.0),
                ],
            ),
            FxInfo::native(
                "oa.audio.compressor",
                "Compressor",
                "Evens out the level: whatever goes over the threshold is turned down by the ratio.",
                vec![
                    p("threshold", -18.0, Unit::Decibels, -60.0, 0.0),
                    p("ratio", 4.0, Unit::None, 1.0, 20.0),
                    p("attack", 0.01, Unit::Seconds, 0.0001, 0.2),
                    p("release", 0.15, Unit::Seconds, 0.01, 1.5),
                    p("knee", 6.0, Unit::Decibels, 0.0, 18.0),
                    p("makeup", 0.0, Unit::Decibels, 0.0, 24.0),
                ],
            ),
            FxInfo::native(
                "oa.audio.limiter",
                "Limiter",
                "Nothing gets past the ceiling — put it last, on the whole mix.",
                vec![p("ceiling", -1.0, Unit::Decibels, -24.0, 0.0), p("release", 0.08, Unit::Seconds, 0.005, 1.0)],
            ),
            FxInfo::native(
                "oa.audio.deess",
                "De-esser",
                "Tames harsh s and sh sounds in a voice, leaving the rest alone.",
                vec![p("frequency", 6500.0, Unit::None, 2000.0, 12000.0), p("threshold", -30.0, Unit::Decibels, -60.0, 0.0), p("amount", 10.0, Unit::Decibels, 0.0, 24.0)],
            ),
            FxInfo::native(
                "oa.audio.gate",
                "Threshold",
                "Mutes everything quieter than the threshold (a noise gate).",
                vec![p("threshold", -40.0, Unit::Decibels, -80.0, 0.0), p("release", 0.15, Unit::Seconds, 0.01, 1.0)],
            ),
            FxInfo::native(
                "oa.audio.crush",
                "Bit Crush",
                "Coarse and grainy: fewer bits, a lower sample rate, like old hardware.",
                vec![p("bits", 6.0, Unit::None, 1.0, 16.0), p("rate", 8000.0, Unit::None, 200.0, 48000.0), p("mix", 1.0, Unit::None, 0.0, 1.0)],
            ),
            FxInfo::native(
                "oa.audio.denoise",
                "Denoise",
                "Removes steady background noise (hiss, hum, fans).",
                vec![p("reduction", 18.0, Unit::Decibels, 0.0, 40.0), p("sensitivity", 2.0, Unit::None, 0.5, 4.0)],
            ),
            // Written as sound shaders, the way a plugin writes them.
            shader(
                "oa.audio.fade",
                "Fade",
                "Fades the sound in as an intro, out as an outro.",
                vec![p("curve", 1.0, Unit::None, 0.25, 4.0)],
                FxUsage::InOut,
                "out = in * pow(visibility, curve);",
            ),
            shader(
                "oa.audio.muffle",
                "Muffle",
                "Dull and far away, as if through a wall. As an intro it opens up; as an outro it closes in.",
                vec![p("cutoff", 600.0, Unit::None, 80.0, 8000.0), p("amount", 1.0, Unit::None, 0.0, 1.0)],
                FxUsage::Passive,
                "// A one-pole low-pass. Over an intro or outro it opens up as the clip arrives.\n\
                 let open = select(visibility < 1, visibility ^ 2, 0);\n\
                 let hz = mix(cutoff, 18000, open);\n\
                 let k = 1 - exp(-TAU * hz / sample_rate);\n\
                 state low = 0;\n\
                 low += (in - low) * k;\n\
                 out = mix(in, low, amount);",
            ),
            shader(
                "oa.audio.tone",
                "Tone",
                "Adds a tone on top of the sound: a sine, square, triangle or saw wave (or noise) at a pitch, with its own attack, decay and repeats.",
                vec![
                    ParamSchema::new("wave", Value::Enum("sine".into()), Unit::None).options(&["sine", "square", "triangle", "saw", "noise"]),
                    p("pitch", 440.0, Unit::None, 20.0, 8000.0),
                    p("gain", -12.0, Unit::Decibels, -60.0, 0.0),
                    p("attack", 0.01, Unit::Seconds, 0.0, 2.0),
                    p("decay", 0.0, Unit::Seconds, 0.0, 10.0),
                    p("repeat", 0.0, Unit::Seconds, 0.0, 10.0),
                    p("vibrato", 0.0, Unit::None, 0.0, 12.0),
                    p("vibrato_depth", 0.5, Unit::None, 0.0, 2.0),
                    p("pulse", 0.5, Unit::None, 0.05, 0.95),
                    p("follow", 0.0, Unit::None, 0.0, 1.0),
                    p("original", 1.0, Unit::None, 0.0, 1.0),
                ],
                FxUsage::Passive,
                TONE,
            ),
            shader(
                "oa.audio.tremolo",
                "Tremolo",
                "The level swings up and down.",
                vec![p("speed", 5.0, Unit::None, 0.1, 20.0), p("depth", 0.5, Unit::None, 0.0, 1.0)],
                FxUsage::Passive,
                "let swing = 0.5 + 0.5 * sin(TAU * speed * time);\nout = in * (1 - depth * swing);",
            ),
            shader(
                "oa.audio.drive",
                "Drive",
                "Warm saturation, up to a fuzzy distortion.",
                vec![p("drive", 12.0, Unit::Decibels, 0.0, 36.0), p("mix", 1.0, Unit::None, 0.0, 1.0)],
                FxUsage::Passive,
                "let d = db(drive);\nout = mix(in, tanh(in * d) / tanh(d), mix);",
            ),
            shader(
                "oa.audio.width",
                "Stereo Width",
                "Narrower (0 is mono) or wider than it was recorded.",
                vec![p("width", 1.5, Unit::None, 0.0, 3.0)],
                FxUsage::Passive,
                "let mid = (left + right) * 0.5;\nlet side = (left - right) * 0.5 * width;\nout = select(channels < 2, in, select(channel == 0, mid + side, mid - side));",
            ),
        ];
        list.into_iter().map(Arc::new).collect()
    })
}

/// Sound effects from plugins (sound shaders), replaced as a whole by [`install`].
fn installed() -> &'static RwLock<Vec<Arc<FxInfo>>> {
    static I: OnceLock<RwLock<Vec<Arc<FxInfo>>>> = OnceLock::new();
    I.get_or_init(|| RwLock::new(Vec::new()))
}

/// Makes plugins' sound effects available, replacing the ones installed before. Ids
/// already taken (by Atelier Core or an earlier plugin) are refused and reported.
pub fn install(effects: Vec<FxInfo>) -> Vec<String> {
    let mut issues = Vec::new();
    let mut keep: Vec<Arc<FxInfo>> = Vec::new();
    for fx in effects {
        if builtins().iter().chain(&keep).any(|b| b.type_id == fx.type_id) {
            issues.push(format!("{}: another sound effect already uses this id", fx.type_id));
        } else {
            keep.push(Arc::new(fx));
        }
    }
    *installed().write().unwrap_or_else(|e| e.into_inner()) = keep;
    issues
}

/// Every sound effect: Atelier Core's, then plugins'.
pub fn catalog() -> Vec<Arc<FxInfo>> {
    let mut all = builtins().to_vec();
    all.extend(installed().read().unwrap_or_else(|e| e.into_inner()).iter().cloned());
    all
}

/// Atelier Core's sound effects only.
pub fn core_catalog() -> &'static [Arc<FxInfo>] {
    builtins()
}

pub fn info(type_id: &str) -> Option<Arc<FxInfo>> {
    if let Some(b) = builtins().iter().find(|f| f.type_id == type_id) {
        return Some(b.clone());
    }
    installed().read().unwrap_or_else(|e| e.into_inner()).iter().find(|f| f.type_id == type_id).cloned()
}

/// A sound effect on one clip.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioEffect {
    /// The clip's effect instance id (processors follow it across edits).
    pub id: u64,
    pub type_id: String,
    pub params: ParamSet,
    /// When it runs, as for picture effects: the whole clip, or its intro/outro window.
    pub role: EffectRole,
}

impl AudioEffect {
    pub fn new(id: u64, type_id: &str, params: ParamSet) -> Self {
        AudioEffect { id, type_id: type_id.into(), params, role: EffectRole::Passive }
    }
}

/// A chain that makes speech easier to understand — for a transcriber more than for
/// ears: rumble cut, steady noise removed, the voice band lifted, levels evened out and
/// kept off the ceiling. Appended to each clip's own effects before the mix.
pub fn voice_cleanup() -> Vec<AudioEffect> {
    let set = |values: &[(&str, f64)]| {
        let mut p = ParamSet::default();
        for (id, v) in values {
            p.set(id, oa_params::ParamSource::Static(Value::Float(*v)));
        }
        p
    };
    // Ids from the top of the range, clear of any clip's own effects.
    let id = |n: u64| u64::MAX - n;
    vec![
        AudioEffect::new(id(0), "oa.audio.eq", set(&[("low_freq", 90.0), ("low_gain", -18.0), ("p2_freq", 3000.0), ("p2_gain", 4.0), ("p2_q", 0.8), ("high_freq", 9000.0), ("high_gain", -4.0)])),
        AudioEffect::new(id(1), "oa.audio.denoise", set(&[("reduction", 20.0), ("sensitivity", 2.0)])),
        AudioEffect::new(id(2), "oa.audio.compressor", set(&[("threshold", -30.0), ("ratio", 3.0), ("attack", 0.01), ("release", 0.15), ("makeup", 8.0)])),
        AudioEffect::new(id(3), "oa.audio.limiter", set(&[("ceiling", -1.0)])),
    ]
}

/// A running effect: processes interleaved samples in place.
pub(crate) trait Processor: Send {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, clock: &ClockSpan);
    fn reset(&mut self);
    /// Frames of delay this adds (the mixer feeds this many ahead after a seek).
    fn latency(&self) -> usize {
        0
    }
    /// How far it's turning the sound down right now (dB), for its meter.
    fn reduction_db(&self) -> f32 {
        0.0
    }
}

/// A processor for `type_id`: its sound shader, or the native one.
pub(crate) fn processor(type_id: &str, channels: usize, rate: f32) -> Option<Box<dyn Processor>> {
    if let Some(program) = info(type_id).and_then(|i| i.shader.clone()) {
        return Some(Box::new(ShaderProcessor::new(program, channels, rate)));
    }
    native(type_id, channels, rate)
}

pub(crate) fn native(type_id: &str, channels: usize, rate: f32) -> Option<Box<dyn Processor>> {
    let ch = channels.max(1);
    Some(match type_id {
        "oa.audio.bass" => Box::new(BassBoost { ch, rate, state: vec![[0.0; 4]; ch], coeffs: None }),
        "oa.audio.pitch" => Box::new(PitchFormant { pitch: PitchShift::new(ch, rate), formant: crate::dynamics::Formant::new(ch) }),
        "oa.audio.echo" => Box::new(Echo { ch, rate, line: vec![0.0; (rate * 1.6) as usize * ch], pos: 0 }),
        "oa.audio.reverb" => Box::new(Reverb::new(ch, rate)),
        "oa.audio.gate" => Box::new(Gate { ch, rate, env: 0.0, gain: 0.0 }),
        "oa.audio.crush" => Box::new(BitCrush { ch, rate, held: vec![0.0; ch], phase: 0.0 }),
        "oa.audio.denoise" => Box::new(Denoise::new(ch)),
        "oa.audio.eq" => Box::new(crate::dynamics::Equalizer::new(ch, rate)),
        "oa.audio.compressor" => Box::new(crate::dynamics::Compressor::new(ch, rate)),
        "oa.audio.limiter" => Box::new(crate::dynamics::Limiter::new(ch, rate)),
        "oa.audio.deess" => Box::new(crate::dynamics::DeEsser::new(ch, rate)),
        _ => return None,
    })
}

// ---- bit crush: fewer bits, and a coarser sample clock ----

/// Two kinds of coarseness at once, which is what makes it sound like old hardware:
/// **bits** quantizes the level, **rate** holds each sample for a while (a sample-and-
/// hold at a lower rate). `mix` blends back towards the clean signal.
struct BitCrush {
    ch: usize,
    rate: f32,
    /// The sample being held, per channel.
    held: Vec<f32>,
    /// How far through the current hold we are, in output samples.
    phase: f32,
}

impl Processor for BitCrush {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let bits = v.float("bits").clamp(1.0, 16.0) as f32;
        let target = v.float("rate").clamp(50.0, self.rate as f64 * 2.0) as f32;
        let mix = v.float("mix").clamp(0.0, 1.0) as f32;
        // How many output samples each held value lasts.
        let hold = (self.rate / target.max(1.0)).max(1.0);
        // 2^bits levels between -1 and 1.
        let levels = (2.0f32).powf(bits) * 0.5;
        for frame in buf.chunks_mut(self.ch) {
            self.phase += 1.0;
            let fresh = self.phase >= hold;
            if fresh {
                self.phase -= hold;
            }
            for (c, sample) in frame.iter_mut().enumerate() {
                if fresh {
                    // Quantize on the way in, so held samples stay on the grid.
                    self.held[c] = (*sample * levels).round() / levels;
                }
                *sample = *sample * (1.0 - mix) + self.held[c] * mix;
            }
        }
    }

    fn reset(&mut self) {
        self.held.iter_mut().for_each(|h| *h = 0.0);
        self.phase = 0.0;
    }
}

// ---- bass boost: an RBJ low-shelf biquad ----

struct BassBoost {
    ch: usize,
    rate: f32,
    /// Per channel: x1, x2, y1, y2.
    state: Vec<[f32; 4]>,
    /// ((boost, frequency), [b0, b1, b2, a1, a2]) for the params they were made for.
    coeffs: Option<((f32, f32), [f32; 5])>,
}

fn low_shelf(rate: f32, freq: f32, gain_db: f32) -> [f32; 5] {
    let a = 10f32.powf(gain_db / 40.0);
    let w = 2.0 * PI * freq / rate;
    let (sin, cos) = w.sin_cos();
    let alpha = sin / 2.0 * (2f32).sqrt(); // shelf slope 1
    let sq = 2.0 * a.sqrt() * alpha;
    let b0 = a * ((a + 1.0) - (a - 1.0) * cos + sq);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos);
    let b2 = a * ((a + 1.0) - (a - 1.0) * cos - sq);
    let a0 = (a + 1.0) + (a - 1.0) * cos + sq;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos);
    let a2 = (a + 1.0) + (a - 1.0) * cos - sq;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

impl Processor for BassBoost {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let key = (v.float("boost") as f32, v.float("frequency").clamp(20.0, 1000.0) as f32);
        let c = match self.coeffs {
            Some((k, c)) if k == key => c,
            _ => {
                let c = low_shelf(self.rate, key.1, key.0);
                self.coeffs = Some((key, c));
                c
            }
        };
        for frame in buf.chunks_exact_mut(self.ch) {
            for (x, s) in frame.iter_mut().zip(&mut self.state) {
                let y = c[0] * *x + c[1] * s[0] + c[2] * s[1] - c[3] * s[2] - c[4] * s[3];
                s[1] = s[0];
                s[0] = *x;
                s[3] = s[2];
                s[2] = y;
                *x = y;
            }
        }
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [0.0; 4]);
    }
}

// ---- pitch shift: two crossfaded taps sweeping through a delay line ----

struct PitchShift {
    ch: usize,
    line: Vec<f32>,
    len: usize,
    write: usize,
    /// Delay of the first tap, in frames (the second is half a window behind).
    delay: f32,
    window: f32,
}

impl PitchShift {
    fn new(ch: usize, rate: f32) -> Self {
        let window = (rate * 0.06).round();
        let len = (window as usize) * 2 + 4;
        PitchShift { ch, line: vec![0.0; len * ch], len, write: 0, delay: 0.0, window }
    }

    fn tap(&self, c: usize, delay: f32) -> f32 {
        let pos = self.write as f32 - 1.0 - delay;
        let pos = pos.rem_euclid(self.len as f32);
        let i = pos.floor() as usize % self.len;
        let j = (i + 1) % self.len;
        let f = pos - pos.floor();
        self.line[i * self.ch + c] * (1.0 - f) + self.line[j * self.ch + c] * f
    }
}

impl Processor for PitchShift {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let ratio = 2f32.powf(v.float("semitones") as f32 / 12.0);
        let mix = v.float("mix").clamp(0.0, 1.0) as f32;
        let w = self.window;
        for frame in buf.chunks_exact_mut(self.ch) {
            for (c, x) in frame.iter().enumerate() {
                self.line[self.write * self.ch + c] = *x;
            }
            self.write = (self.write + 1) % self.len;
            // Reading slower or faster than writing changes the pitch; the taps jump
            // back a window when they run out, each faded out as it does.
            self.delay = (self.delay + 1.0 - ratio).rem_euclid(w);
            let d2 = (self.delay + w / 2.0).rem_euclid(w);
            let g1 = (PI * self.delay / w).sin().powi(2);
            let g2 = (PI * d2 / w).sin().powi(2);
            for (c, x) in frame.iter_mut().enumerate() {
                let wet = self.tap(c, self.delay) * g1 + self.tap(c, d2) * g2;
                *x = *x * (1.0 - mix) + wet * mix;
            }
        }
    }

    fn reset(&mut self) {
        self.line.fill(0.0);
        self.delay = 0.0;
    }
}


/// Pitch Shift as the catalog offers it: the pitch moved, then the formants — the voice's
/// character — moved on their own (or held where they were, with "keep voice"). The
/// formant stage always runs, so the delay it adds never changes mid-clip.
struct PitchFormant {
    pitch: PitchShift,
    formant: crate::dynamics::Formant,
}

impl Processor for PitchFormant {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, clock: &ClockSpan) {
        self.pitch.process(buf, v, clock);
        let keep = matches!(v.get("keep_voice"), Some(Value::Bool(true)));
        let shift = v.float("formant") as f32 - if keep { v.float("semitones") as f32 } else { 0.0 };
        self.formant.run(buf, shift);
    }

    fn reset(&mut self) {
        self.pitch.reset();
        self.formant.reset();
    }

    fn latency(&self) -> usize {
        crate::dynamics::Formant::LATENCY
    }
}

/// The plain pitch shifter, without the formant stage (and its delay): for keeping a
/// sped-up clip's pitch, which runs outside any effect chain.
pub(crate) fn plain_pitch(ch: usize, rate: f32) -> Box<dyn Processor> {
    Box::new(PitchShift::new(ch.max(1), rate))
}
// ---- echo: a feedback delay line ----

struct Echo {
    ch: usize,
    rate: f32,
    line: Vec<f32>,
    pos: usize,
}

impl Processor for Echo {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let frames = self.line.len() / self.ch;
        let delay = ((v.float("delay") as f32 * self.rate) as usize).clamp(1, frames - 1);
        let feedback = v.float("feedback").clamp(0.0, 0.95) as f32;
        let mix = v.float("mix").clamp(0.0, 1.0) as f32;
        for frame in buf.chunks_exact_mut(self.ch) {
            let read = (self.pos + frames - delay) % frames;
            for (c, x) in frame.iter_mut().enumerate() {
                let echoed = self.line[read * self.ch + c];
                self.line[self.pos * self.ch + c] = *x + echoed * feedback;
                *x += echoed * mix;
            }
            self.pos = (self.pos + 1) % frames;
        }
    }

    fn reset(&mut self) {
        self.line.fill(0.0);
    }
}

// ---- reverb: Freeverb (8 damped combs, 4 allpasses per channel) ----

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

struct Reverb {
    ch: usize,
    rate: f32,
    combs: Vec<Vec<Comb>>,
    allpasses: Vec<Vec<Allpass>>,
    /// Pre-delay: the gap before the room answers (mono, up to 0.25 s).
    pre: Vec<f32>,
    pre_pos: usize,
}

impl Reverb {
    fn new(ch: usize, rate: f32) -> Self {
        const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
        const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
        let k = rate / 44_100.0;
        // Channels are detuned slightly, for width.
        let size = |n: usize, c: usize| ((n + 23 * c) as f32 * k) as usize;
        Reverb {
            ch,
            combs: (0..ch).map(|c| COMBS.iter().map(|n| Comb { buf: vec![0.0; size(*n, c)], pos: 0, store: 0.0 }).collect()).collect(),
            allpasses: (0..ch).map(|c| ALLPASSES.iter().map(|n| Allpass { buf: vec![0.0; size(*n, c)], pos: 0 }).collect()).collect(),
            rate,
            pre: vec![0.0; (rate * 0.25) as usize + 1],
            pre_pos: 0,
        }
    }
}

impl Processor for Reverb {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        // A named space sets the room; "custom" uses the sliders.
        let (room, damping, space_predelay) = crate::dynamics::reverb_space(v);
        let feedback = 0.7 + 0.28 * room as f32;
        let damp = 0.4 * damping as f32;
        let mix = v.float("mix").clamp(0.0, 1.0) as f32;
        let pre = ((v.float("predelay").max(space_predelay).clamp(0.0, 0.25) as f32 * self.rate) as usize).min(self.pre.len() - 1);
        for frame in buf.chunks_exact_mut(self.ch) {
            let dry = frame.iter().sum::<f32>() / self.ch as f32 * 0.015;
            // Through the pre-delay first.
            let n = self.pre.len();
            self.pre[self.pre_pos] = dry;
            let input = self.pre[(self.pre_pos + n - pre) % n];
            self.pre_pos = (self.pre_pos + 1) % n;
            for (c, x) in frame.iter_mut().enumerate() {
                let mut out = 0.0;
                for comb in &mut self.combs[c] {
                    let y = comb.buf[comb.pos];
                    comb.store = y * (1.0 - damp) + comb.store * damp;
                    comb.buf[comb.pos] = input + comb.store * feedback;
                    comb.pos = (comb.pos + 1) % comb.buf.len();
                    out += y;
                }
                for ap in &mut self.allpasses[c] {
                    let b = ap.buf[ap.pos];
                    ap.buf[ap.pos] = out + b * 0.5;
                    ap.pos = (ap.pos + 1) % ap.buf.len();
                    out = b - out;
                }
                *x = *x * (1.0 - mix * 0.5) + out * mix * 3.0;
            }
        }
    }

    fn reset(&mut self) {
        for c in self.combs.iter_mut().flatten() {
            c.buf.fill(0.0);
            c.store = 0.0;
        }
        for a in self.allpasses.iter_mut().flatten() {
            a.buf.fill(0.0);
        }
        self.pre.fill(0.0);
    }
}

// ---- threshold: a noise gate ----

struct Gate {
    ch: usize,
    rate: f32,
    env: f32,
    gain: f32,
}

impl Processor for Gate {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let threshold = 10f32.powf(v.float("threshold") as f32 / 20.0);
        let attack = 1.0 - (-1.0 / (0.002 * self.rate)).exp();
        let release = 1.0 - (-1.0 / (v.float("release").max(0.005) as f32 * self.rate)).exp();
        let follow = 1.0 - (-1.0 / (0.01 * self.rate)).exp();
        for frame in buf.chunks_exact_mut(self.ch) {
            let peak = frame.iter().fold(0f32, |m, x| m.max(x.abs()));
            // Envelope: jumps up with peaks, eases down.
            self.env = if peak > self.env { peak } else { self.env + (peak - self.env) * follow };
            let target = if self.env >= threshold { 1.0 } else { 0.0 };
            self.gain += (target - self.gain) * if target > self.gain { attack } else { release };
            for x in frame.iter_mut() {
                *x *= self.gain;
            }
        }
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 0.0;
    }
}

// ---- denoise: spectral gating against a tracked noise floor ----

const FFT: usize = 1024;
const HOP: usize = FFT / 4;

struct DenoiseChannel {
    input: Vec<f32>,
    output: Vec<f32>,
    smooth: Vec<f32>,
    fast: Vec<f32>,
    noise: Vec<f32>,
    gains: Vec<f32>,
    frames: u32,
}

struct Denoise {
    ch: usize,
    chans: Vec<DenoiseChannel>,
    window: Vec<f32>,
    /// Input frames since the last hop.
    fill: usize,
    /// Processed samples ready to hand out, per channel (interleaved later).
    ready: Vec<Vec<f32>>,
}

impl Denoise {
    fn new(ch: usize) -> Self {
        // sqrt-Hann for analysis and synthesis: their product, a Hann window at 75%
        // overlap, sums to a constant 2.
        let window = (0..FFT).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / FFT as f32).cos()).sqrt()).collect();
        let chan = || DenoiseChannel { input: vec![0.0; FFT], output: vec![0.0; FFT], smooth: vec![0.0; FFT / 2 + 1], fast: vec![0.0; FFT / 2 + 1], noise: vec![0.0; FFT / 2 + 1], gains: vec![1.0; FFT / 2 + 1], frames: 0 };
        // The output queue starts a hop of silence ahead, so the delay is always exactly
        // FFT samples whatever the block sizes (see `latency`).
        Denoise { ch, chans: (0..ch).map(|_| chan()).collect(), window, fill: 0, ready: vec![vec![0.0; HOP]; ch] }
    }

    fn frame(&mut self, c: usize, reduction: f32, sensitivity: f32) {
        let floor = 10f32.powf(-reduction / 20.0);
        let ch = &mut self.chans[c];
        let mut re: Vec<f32> = ch.input.iter().zip(&self.window).map(|(x, w)| x * w).collect();
        let mut im = vec![0.0f32; FFT];
        fft(&mut re, &mut im, false);
        // The first frames (the buffer still filling) set the floor outright.
        let first = ch.frames < 8;
        ch.frames += 1;
        let mut raw = [0f32; FFT / 2 + 1];
        for (k, raw) in raw.iter_mut().enumerate() {
            let power = re[k] * re[k] + im[k] * im[k];
            // Each bin's smoothed power; the noise floor follows it down at once and
            // creeps up slowly, so steady noise is tracked while speech and music (which
            // come and go) aren't.
            let s = &mut ch.smooth[k];
            *s = if first { power } else { *s * 0.85 + power * 0.15 };
            let n = &mut ch.noise[k];
            *n = if first || *s < *n { *s } else { *n * 1.004 + 1e-12 };
            // A minimum sits below the average: ~2.2× brings it back to the noise level.
            let noise = *n * 2.2;
            // Decide on lightly smoothed power (raw noise power swings too much), and
            // subtract the noise, over-subtracting by `sensitivity`, never below the floor.
            let f = &mut ch.fast[k];
            *f = if first { power } else { *f * 0.5 + power * 0.5 };
            *raw = (1.0 - sensitivity * noise / f.max(1e-12)).max(0.0).sqrt();
        }
        for k in 0..=FFT / 2 {
            // Smooth across neighboring bins and over time: lone bins poking through
            // the noise are what sounds like "musical noise".
            let near = (raw[k.saturating_sub(1)] + 2.0 * raw[k] + raw[(k + 1).min(FFT / 2)]) / 4.0;
            ch.gains[k] = ch.gains[k] * 0.5 + near.max(floor) * 0.5;
            let g = ch.gains[k];
            re[k] *= g;
            im[k] *= g;
            if k > 0 && k < FFT / 2 {
                re[FFT - k] = re[k];
                im[FFT - k] = -im[k];
            }
        }
        fft(&mut re, &mut im, true);
        // Overlap-add; the first HOP samples are complete.
        for ((out, x), w) in ch.output.iter_mut().zip(&re).zip(&self.window) {
            *out += x * w * 0.5;
        }
        self.ready[c].extend_from_slice(&ch.output[..HOP]);
        ch.output.copy_within(HOP.., 0);
        ch.output[FFT - HOP..].fill(0.0);
    }
}

impl Processor for Denoise {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let reduction = v.float("reduction").clamp(0.0, 60.0) as f32;
        let sensitivity = v.float("sensitivity").clamp(0.1, 10.0) as f32;
        let frames = buf.len() / self.ch;
        for f in 0..frames {
            for c in 0..self.ch {
                let ch = &mut self.chans[c];
                ch.input.copy_within(1.., 0);
                ch.input[FFT - 1] = buf[f * self.ch + c];
            }
            self.fill += 1;
            if self.fill == HOP {
                self.fill = 0;
                for c in 0..self.ch {
                    self.frame(c, reduction, sensitivity);
                }
            }
        }
        // Hand out what's processed, oldest first (the queue always holds enough).
        let have = self.ready[0].len().min(frames);
        for f in 0..frames {
            for c in 0..self.ch {
                buf[f * self.ch + c] = if f < have { self.ready[c][f] } else { 0.0 };
            }
        }
        for r in &mut self.ready {
            r.drain(..have);
        }
    }

    fn reset(&mut self) {
        let ch = self.ch;
        *self = Denoise::new(ch);
    }

    fn latency(&self) -> usize {
        FFT
    }
}

/// In-place radix-2 complex FFT (`inverse` scales by 1/n).
pub(crate) fn fft(re: &mut [f32], im: &mut [f32], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * PI / len as f32 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
    if inverse {
        let k = 1.0 / n as f32;
        re.iter_mut().for_each(|x| *x *= k);
        im.iter_mut().for_each(|x| *x *= k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_params::EvalContext;
    use oa_time::Time;

    const RATE: f32 = 48_000.0;

    fn run(type_id: &str, set: &[(&str, f64)], input: &[f32], ch: usize) -> Vec<f32> {
        let info = info(type_id).unwrap();
        let mut params = ParamSet::default();
        for (k, v) in set {
            params.set(k, oa_params::ParamSource::Static(Value::Float(*v)));
        }
        let v = params.eval(&info.params, None, &EvalContext::at(Time::ZERO, Time::ZERO));
        let mut p = processor(type_id, ch, RATE).unwrap();
        let mut out = input.to_vec();
        // In blocks, as the mixer does.
        for block in out.chunks_mut(480 * ch) {
            p.process(block, &v, &ClockSpan::default());
        }
        out
    }

    fn sine(freq: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(RATE * secs) as usize).map(|i| amp * (2.0 * PI * freq * i as f32 / RATE).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn db(a: f32, b: f32) -> f32 {
        20.0 * (a / b).log10()
    }

    #[test]
    fn voice_cleanup_runs_natively_and_cuts_hum() {
        let chain = voice_cleanup();
        let mut procs: Vec<_> = chain.iter().map(|e| (processor(&e.type_id, 1, RATE).expect("a native processor"), e.params.eval(&info(&e.type_id).unwrap().params, None, &EvalContext::at(Time::ZERO, Time::ZERO)))).collect();
        let hum = sine(50.0, 3.0, 0.2);
        let mut out = hum.clone();
        for block in out.chunks_mut(480) {
            for (p, v) in &mut procs {
                p.process(block, v, &ClockSpan::default());
            }
        }
        // The last second, once the denoiser has learned the noise.
        let tail = RATE as usize;
        let drop = db(rms(&out[out.len() - tail..]), rms(&hum[hum.len() - tail..]));
        assert!(drop < -12.0, "mains hum should be mostly gone: {drop:.1} dB");
    }

    #[test]
    fn bass_boost_lifts_lows_only() {
        let low = sine(60.0, 1.0, 0.1);
        let high = sine(5000.0, 1.0, 0.1);
        let lo = run("oa.audio.bass", &[("boost", 8.0)], &low, 1);
        let hi = run("oa.audio.bass", &[("boost", 8.0)], &high, 1);
        let gain_lo = db(rms(&lo[24000..]), rms(&low[24000..]));
        let gain_hi = db(rms(&hi[24000..]), rms(&high[24000..]));
        assert!((gain_lo - 8.0).abs() < 1.5, "{gain_lo}");
        assert!(gain_hi.abs() < 0.5, "{gain_hi}");
    }

    /// Frequency from zero crossings.
    fn pitch(x: &[f32]) -> f32 {
        let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 / (x.len() as f32 / RATE)
    }

    #[test]
    fn pitch_shift_moves_by_semitones() {
        let a = sine(220.0, 2.0, 0.5);
        let up = run("oa.audio.pitch", &[("semitones", 12.0)], &a, 1);
        let down = run("oa.audio.pitch", &[("semitones", -12.0)], &a, 1);
        let (pu, pd) = (pitch(&up[9600..]), pitch(&down[9600..]));
        assert!((pu / 440.0 - 1.0).abs() < 0.1, "{pu}");
        assert!((pd / 110.0 - 1.0).abs() < 0.1, "{pd}");
        // Same length, similar loudness.
        assert_eq!(up.len(), a.len());
        assert!(db(rms(&up[9600..]), rms(&a[9600..])).abs() < 3.0);
    }

    #[test]
    fn echo_repeats_after_the_delay() {
        let mut x = vec![0.0; 48_000];
        x[100] = 1.0;
        let y = run("oa.audio.echo", &[("delay", 0.25), ("feedback", 0.5), ("mix", 0.5)], &x, 1);
        assert_eq!(y[100], 1.0);
        assert!((y[100 + 12_000] - 0.5).abs() < 1e-6, "first echo");
        assert!((y[100 + 24_000] - 0.25).abs() < 1e-6, "second, fed back");
        assert!(y[100 + 6_000].abs() < 1e-9);
    }

    #[test]
    fn reverb_leaves_a_decaying_tail() {
        let mut x = vec![0.0; 96_000 * 2];
        x[0] = 1.0;
        x[1] = 1.0;
        let y = run("oa.audio.reverb", &[("mix", 0.5)], &x, 2);
        let early = rms(&y[2 * 4_800..2 * 24_000]);
        let late = rms(&y[2 * 72_000..]);
        assert!(early > 1e-3, "{early}");
        assert!(late < early * 0.3, "decays: {early} → {late}");
    }

    #[test]
    fn threshold_mutes_quiet_passages() {
        let mut x = sine(440.0, 1.0, 0.005); // -46 dBFS
        x.extend(sine(440.0, 1.0, 0.5));
        let y = run("oa.audio.gate", &[("threshold", -30.0)], &x, 1);
        assert!(rms(&y[12_000..48_000]) < 1e-4, "quiet part gated");
        assert!(db(rms(&y[60_000..]), rms(&x[60_000..])).abs() < 0.5, "loud part passes");
    }

    #[test]
    fn denoise_removes_steady_noise_and_keeps_the_tone() {
        // Two seconds of hiss, then hiss plus a tone.
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let tone = sine(440.0, 2.0, 0.3);
        let x: Vec<f32> = (0..192_000).map(|i| noise() * 0.02 + if i >= 96_000 { tone[i - 96_000] } else { 0.0 }).collect();
        let y = run("oa.audio.denoise", &[("reduction", 30.0)], &x, 1);
        let lat = FFT;
        let hiss_before = rms(&x[48_000..90_000]);
        let hiss_after = rms(&y[48_000 + lat..90_000 + lat]);
        assert!(db(hiss_after, hiss_before) < -12.0, "hiss down {} dB", db(hiss_after, hiss_before));
        let tone_after = rms(&y[120_000 + lat..180_000]);
        assert!(db(tone_after, rms(&tone[24_000..84_000])).abs() < 2.0, "tone kept");
    }
}


#[cfg(test)]
mod crush_tests {
    use super::*;

    fn eval(params: &ParamSet) -> Evaluated {
        let schema = &info("oa.audio.crush").expect("in the catalog").params;
        params.eval(schema, None, &oa_params::EvalContext::at(oa_time::Time::ZERO, oa_time::Time::ZERO))
    }

    fn set(params: &mut ParamSet, id: &str, v: f64) {
        params.set(id, oa_params::ParamSource::Static(Value::Float(v)));
    }

    /// Bit crush is coarse in two ways at once: the level lands on a grid, and samples
    /// are held rather than following the input. Each is checked on its own, since
    /// either one alone can flatten a ramp.
    #[test]
    fn bit_crush_is_coarse_in_level_and_in_time() {
        let mut fx = processor("oa.audio.crush", 1, 48_000.0).expect("bit crush");
        let ramp = || (0..200).map(|i| i as f32 / 200.0).collect::<Vec<f32>>();

        // Fine levels, a tenth of the sample rate: about one change every ten samples.
        let mut params = ParamSet::default();
        set(&mut params, "bits", 16.0);
        set(&mut params, "rate", 4_800.0);
        set(&mut params, "mix", 1.0);
        let mut buf = ramp();
        fx.process(&mut buf, &eval(&params), &ClockSpan::default());
        let changes = buf.windows(2).filter(|w| (w[0] - w[1]).abs() > 1e-6).count();
        assert!((15..=25).contains(&changes), "{changes} changes in 200 samples");

        // Two bits, no holding: every value sits on a grid of 0.5.
        fx.reset();
        set(&mut params, "bits", 2.0);
        set(&mut params, "rate", 48_000.0);
        let mut buf = ramp();
        fx.process(&mut buf, &eval(&params), &ClockSpan::default());
        for s in &buf {
            assert!((s - (s / 0.5).round() * 0.5).abs() < 1e-5, "{s} is off the level grid");
        }

        // Nothing at all with the mix down.
        fx.reset();
        set(&mut params, "mix", 0.0);
        let mut buf = ramp();
        let before = buf.clone();
        fx.process(&mut buf, &eval(&params), &ClockSpan::default());
        assert_eq!(buf, before);
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;
    use oa_params::{EvalContext, ParamSource};
    use oa_time::Time;

    fn tone(set: &[(&str, Value)], input: &[f32]) -> Vec<f32> {
        let info = info("oa.audio.tone").expect("in the catalog");
        let mut params = ParamSet::default();
        for (k, v) in set {
            params.set(k, ParamSource::Static(v.clone()));
        }
        let v = params.eval(&info.params, None, &EvalContext::at(Time::ZERO, Time::ZERO));
        let mut p = processor("oa.audio.tone", 1, 48_000.0).expect("tone");
        let mut out = input.to_vec();
        let mut at = 0.0;
        for block in out.chunks_mut(480) {
            let clock = oa_doc::EffectClock { visibility: 1.0, progress: 0.0, seconds: at };
            p.process(block, &v, &ClockSpan::still(clock));
            at += block.len() as f64 / 48_000.0;
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// The tone is at its pitch and gain, on top of the sound; each wave has its shape.
    #[test]
    fn a_tone_at_its_pitch_and_level() {
        let silence = vec![0.0f32; 48_000];
        let y = tone(&[("pitch", Value::Float(1000.0)), ("gain", Value::Float(0.0)), ("attack", Value::Float(0.0))], &silence);
        let crossings = y.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((crossings as i32 - 1000).abs() <= 2, "{crossings} Hz");
        assert!((rms(&y) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01, "a full-scale sine: {}", rms(&y));
        let square = tone(&[("wave", Value::Enum("square".into())), ("gain", Value::Float(0.0)), ("attack", Value::Float(0.0))], &silence);
        assert!((rms(&square) - 1.0).abs() < 0.01, "a square is all ±1: {}", rms(&square));
        // The sound underneath stays.
        let dc = vec![0.25f32; 4800];
        let y = tone(&[("gain", Value::Float(-60.0))], &dc);
        assert!((y[4000] - 0.25).abs() < 0.01);
    }

    /// Decay dies away, repeat restarts it, and `follow` keeps it quiet over silence.
    #[test]
    fn decay_repeat_and_follow() {
        let silence = vec![0.0f32; 96_000];
        let set = [("gain", Value::Float(0.0)), ("attack", Value::Float(0.0)), ("decay", Value::Float(0.1))];
        let y = tone(&set, &silence);
        assert!(rms(&y[..4800]) > 10.0 * rms(&y[40_000..44_800]), "dies away");
        let mut again = set.to_vec();
        again.push(("repeat", Value::Float(0.5)));
        let y = tone(&again, &silence);
        assert!(rms(&y[24_000..28_800]) > 10.0 * rms(&y[20_000..24_000]), "starts again at 0.5 s");
        let y = tone(&[("gain", Value::Float(0.0)), ("follow", Value::Float(1.0))], &silence);
        assert!(rms(&y) < 1e-4, "follows the silence");
    }
}
