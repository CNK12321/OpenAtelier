//! Sound effects on clips: bass boost, pitch shift, echo, reverb, the mixing desk's
//! dynamics, denoise — Atelier Core's, and any a plugin adds. Every one is a sound
//! shader ([`crate::shader`]) declared in a plugin manifest: Atelier Core's come from its
//! folder (`plugins/atelier-core`), built into the program, through the same path a
//! plugin's take ([`install`]).
//!
//! They're stored on a clip exactly like picture effects (`EffectInstance`s, in the
//! same list, with the same roles and keyframable params evaluated on the clip's
//! clocks), and run by the mixer on each clip's decoded samples before its gain — so
//! playback and export sound the same. Every processor is streaming and stateful; the
//! mixer resets it on a seek and, for effects that look ahead (a spectrum, a limiter),
//! primes it by its [`Processor::latency`] so the output stays in sync with the picture.

pub use crate::dynamics::{eq_bands, eq_response_db, meter, Band, BandShape, Meter};
pub use crate::shader::ClockSpan;
use crate::shader::{Program, ShaderProcessor};
use oa_doc::EffectRole;
use oa_graph::registry::{EffectDescriptor, EffectKind, EffectUsage};
use oa_params::{Evaluated, ParamSchema, ParamSet, Value};
use oa_script::{Env, Frame, NoHost, Section};
use std::f32::consts::PI;
use std::sync::{Arc, OnceLock, RwLock};

/// Where a sound effect is offered, like a picture effect's usage: over the whole clip,
/// or as the clip's intro/outro (a fade).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FxUsage {
    Passive,
    InOut,
}

/// How long an effect rings on after its clip: a script reading the params.
#[derive(Debug)]
struct Tail {
    section: Section,
    frame: Frame,
}

/// A sound effect's type: what the UI shows and what runs it.
#[derive(Debug)]
pub struct FxInfo {
    pub type_id: String,
    pub name: String,
    pub description: String,
    pub params: Vec<ParamSchema>,
    pub usage: FxUsage,
    /// The sound shader that runs it.
    pub shader: Arc<Program>,
    /// The live meter its card shows (`oa_graph::registry::METER_*`).
    pub meter: Option<String>,
    tail: Option<Tail>,
}

impl PartialEq for FxInfo {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id && self.shader == other.shader
    }
}

impl FxInfo {
    /// A sound effect written as a sound shader.
    pub fn shader(type_id: &str, name: &str, description: &str, params: Vec<ParamSchema>, usage: FxUsage, source: &str) -> Result<FxInfo, String> {
        let program = Program::compile(source, &params)?;
        Ok(FxInfo { type_id: type_id.into(), name: name.into(), description: description.into(), params, usage, shader: Arc::new(program), meter: None, tail: None })
    }

    /// A plugin's sound effect, as its manifest declares it.
    pub fn from_descriptor(d: &EffectDescriptor) -> Result<FxInfo, String> {
        if d.kind != EffectKind::Sound {
            return Err("not a sound effect".into());
        }
        let source = d.shader.as_ref().map(|s| s.source.clone()).ok_or("no sound shader")?;
        let mut program = Program::compile(&source, &d.params)?;
        program.latency = d.latency;
        let tail = match &d.tail {
            Some(src) => {
                let env = Env { writes: &["out"], params: &d.params, ..Env::default() };
                let (section, frame) = oa_script::compile(src, env).map_err(|e| format!("tail: {e}"))?;
                Some(Tail { section, frame })
            }
            None => None,
        };
        let usage = if d.usage == EffectUsage::InOut { FxUsage::InOut } else { FxUsage::Passive };
        Ok(FxInfo {
            type_id: d.type_id.to_string(),
            name: d.name.clone(),
            description: d.description.clone(),
            params: d.params.clone(),
            usage,
            shader: Arc::new(program),
            meter: d.meter.clone(),
            tail,
        })
    }

    /// How long it keeps sounding after its input stops, in seconds (at most 12).
    pub fn tail_seconds(&self, v: &Evaluated) -> f64 {
        let Some(t) = &self.tail else { return 0.0 };
        let mut regs = vec![0.0; t.frame.registers];
        t.frame.load_params(&mut regs, &self.params, v);
        let mut stack = Vec::new();
        oa_script::run(&t.section.block, &mut regs, &mut stack, &mut NoHost, true);
        oa_script::run(&t.section.main, &mut regs, &mut stack, &mut NoHost, true);
        let seconds = regs[0] as f64;
        if seconds.is_finite() { seconds.clamp(0.0, 12.0) } else { 0.0 }
    }
}

/// How long an effect keeps sounding after its input stops (an echo's repeats, a room's
/// reverberation), so the mixer runs it on past the end of its clip.
pub fn tail_seconds(type_id: &str, v: &Evaluated) -> f64 {
    info(type_id).map_or(0.0, |i| i.tail_seconds(v))
}

/// Atelier Core's sound effects, in its manifest's order.
fn builtins() -> &'static [Arc<FxInfo>] {
    static C: OnceLock<Vec<Arc<FxInfo>>> = OnceLock::new();
    C.get_or_init(|| {
        oa_graph::plugin::core()
            .sounds()
            .map(|d| Arc::new(FxInfo::from_descriptor(d).unwrap_or_else(|e| panic!("Atelier Core's sound effect {}: {e}", d.type_id))))
            .collect()
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

/// A processor for `type_id`: its sound shader, running.
pub(crate) fn processor(type_id: &str, channels: usize, rate: f32) -> Option<Box<dyn Processor>> {
    let program = info(type_id)?.shader.clone();
    Some(Box::new(ShaderProcessor::new(program, channels, rate)))
}

// ---- keeping a sped-up clip's pitch ----

/// Two crossfaded taps sweeping through a delay line: reading slower or faster than
/// it's written changes the pitch. (Pitch Shift does the same in its sound shader; this
/// one is the mixer's own, for a clip whose speed changed but whose pitch shouldn't.)
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
            // The taps jump back a window when they run out, each faded out as it does.
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

/// The plain pitch shifter: for keeping a sped-up clip's pitch, which runs outside any
/// effect chain.
pub(crate) fn plain_pitch(ch: usize, rate: f32) -> Box<dyn Processor> {
    Box::new(PitchShift::new(ch.max(1), rate))
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

    /// How fast each of Atelier Core's sound effects runs, as a share of real time
    /// (stereo, 48 kHz): `cargo test --release -p oa-audio -- --ignored --nocapture cost`.
    #[test]
    #[ignore]
    fn cost_of_each_core_effect() {
        let seconds = 10.0;
        let input: Vec<f32> = sine(220.0, seconds, 0.3).into_iter().flat_map(|x| [x, x * 0.8]).collect();
        for fx in core_catalog() {
            let v = ParamSet::default().eval(&fx.params, None, &EvalContext::at(Time::ZERO, Time::ZERO));
            let mut p = processor(&fx.type_id, 2, RATE).unwrap();
            let mut buf = input.clone();
            let start = std::time::Instant::now();
            for block in buf.chunks_mut(2 * 512) {
                p.process(block, &v, &ClockSpan::default());
            }
            let share = start.elapsed().as_secs_f32() / seconds;
            println!("{:<22} {:>6.2}% of real time", fx.type_id, share * 100.0);
        }
    }

    #[test]
    fn voice_cleanup_runs_and_cuts_hum() {
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
        let lat = 1024;
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
