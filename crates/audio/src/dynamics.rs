//! The mixing-desk effects: a parametric equalizer, a compressor, a limiter and a
//! de-esser — plus the formant stage of Pitch Shift, the live meters every effect card
//! shows, and how long an effect keeps sounding after its clip ends (its tail).

use crate::fx::{ClockSpan, Processor};
use oa_params::Evaluated;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---- live meters ----

/// What an effect is doing right now, for its card: levels in and out (dBFS peaks), how
/// much it's turning the sound down (dynamics effects), and how alike the two channels
/// are (1 = mono, 0 = unrelated, −1 = opposite).
#[derive(Clone, Copy, Debug)]
pub struct Meter {
    pub input_db: f32,
    pub output_db: f32,
    pub reduction_db: f32,
    pub correlation: f32,
    at: Instant,
}

fn meters() -> &'static Mutex<HashMap<u64, Meter>> {
    static M: OnceLock<Mutex<HashMap<u64, Meter>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// The effect instance's meter, if it has run in the last half second.
pub fn meter(effect: u64) -> Option<Meter> {
    let m = *meters().lock().unwrap_or_else(|e| e.into_inner()).get(&effect)?;
    (m.at.elapsed() < Duration::from_millis(500)).then_some(m)
}

fn peak_db(buf: &[f32]) -> f32 {
    let p = buf.iter().fold(0f32, |m, x| m.max(x.abs()));
    if p <= 1e-6 { -120.0 } else { 20.0 * p.log10() }
}

/// Measures one effect's block: `before` is its input, `after` its output.
pub(crate) fn publish(effect: u64, before: &[f32], after: &[f32], channels: usize, reduction_db: f32) {
    let correlation = if channels == 2 {
        let (mut lr, mut ll, mut rr) = (0f32, 0f32, 0f32);
        for f in after.chunks_exact(2) {
            lr += f[0] * f[1];
            ll += f[0] * f[0];
            rr += f[1] * f[1];
        }
        if ll * rr > 1e-12 { lr / (ll * rr).sqrt() } else { 1.0 }
    } else {
        1.0
    };
    let m = Meter { input_db: peak_db(before), output_db: peak_db(after), reduction_db, correlation, at: Instant::now() };
    let mut all = meters().lock().unwrap_or_else(|e| e.into_inner());
    // Smooth the reduction a little (blocks are short), and let peaks fall back gently.
    let m = match all.get(&effect) {
        Some(old) if old.at.elapsed() < Duration::from_millis(200) => Meter {
            input_db: m.input_db.max(old.input_db - 1.5),
            output_db: m.output_db.max(old.output_db - 1.5),
            reduction_db: old.reduction_db * 0.6 + m.reduction_db * 0.4,
            correlation: old.correlation * 0.8 + m.correlation * 0.2,
            ..m
        },
        _ => m,
    };
    all.insert(effect, m);
}

// ---- tails ----

/// How long an effect keeps sounding after its input stops (an echo's repeats, a room's
/// reverberation), so the mixer runs it on past the end of its clip.
pub fn tail_seconds(type_id: &str, v: &Evaluated) -> f64 {
    // Repeats fading by `feedback` each pass, until 60 dB down.
    let passes = |feedback: f64| if feedback > 0.01 { (0.001f64).ln() / feedback.min(0.97).ln() } else { 1.0 };
    let t = match type_id {
        "oa.audio.echo" => v.float("delay").max(0.02) * passes(v.float("feedback").clamp(0.0, 0.95)),
        "oa.audio.reverb" => {
            let (room, _, predelay) = reverb_space(v);
            v.float("predelay").max(0.0).max(predelay) + 0.034 * passes(0.7 + 0.28 * room)
        }
        "oa.audio.pitch" => 0.15,
        "oa.audio.compressor" | "oa.audio.limiter" | "oa.audio.deess" => 0.05,
        _ => 0.0,
    };
    t.clamp(0.0, 12.0)
}

/// Reverb rooms: size, damping and pre-delay for each named space (custom uses the
/// sliders).
pub const SPACES: [&str; 7] = ["custom", "small room", "studio", "hall", "cathedral", "plate", "cave"];

pub(crate) fn reverb_space(v: &Evaluated) -> (f64, f64, f64) {
    match v.get("space").and_then(|s| s.as_enum()).unwrap_or("custom") {
        "small room" => (0.3, 0.6, 0.005),
        "studio" => (0.5, 0.5, 0.01),
        "hall" => (0.82, 0.35, 0.025),
        "cathedral" => (0.96, 0.25, 0.04),
        "plate" => (0.72, 0.1, 0.0),
        "cave" => (0.92, 0.55, 0.06),
        _ => (v.float("room").clamp(0.0, 1.0), v.float("damping").clamp(0.0, 1.0), 0.0),
    }
}

// ---- biquads ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BandShape {
    LowShelf,
    Peak,
    HighShelf,
}

/// One equalizer band as the card draws and drags it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub shape: BandShape,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    /// Param ids of its frequency, gain and (peaks) width.
    pub ids: [&'static str; 3],
}

/// The equalizer's bands from its params: a low shelf, three peaks, a high shelf.
pub fn eq_bands(v: &Evaluated) -> [Band; 5] {
    let f = |id: &str| v.float(id) as f32;
    let peak = |ids: [&'static str; 3]| Band { shape: BandShape::Peak, freq: f(ids[0]), gain_db: f(ids[1]), q: f(ids[2]).max(0.1), ids };
    [
        Band { shape: BandShape::LowShelf, freq: f("low_freq"), gain_db: f("low_gain"), q: 0.707, ids: ["low_freq", "low_gain", ""] },
        peak(["p1_freq", "p1_gain", "p1_q"]),
        peak(["p2_freq", "p2_gain", "p2_q"]),
        peak(["p3_freq", "p3_gain", "p3_q"]),
        Band { shape: BandShape::HighShelf, freq: f("high_freq"), gain_db: f("high_gain"), q: 0.707, ids: ["high_freq", "high_gain", ""] },
    ]
}

/// RBJ cookbook biquad coefficients: (b0, b1, b2, a1, a2), normalized.
fn coefficients(b: &Band, rate: f32) -> [f32; 5] {
    let w = 2.0 * PI * b.freq.clamp(10.0, rate * 0.45) / rate;
    let (sw, cw) = w.sin_cos();
    let a = 10f32.powf(b.gain_db / 40.0);
    let alpha = sw / (2.0 * b.q);
    let (b0, b1, b2, a0, a1, a2) = match b.shape {
        BandShape::Peak => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
        BandShape::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) - (a - 1.0) * cw + s), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - s), (a + 1.0) + (a - 1.0) * cw + s, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - s)
        }
        BandShape::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) + (a - 1.0) * cw + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - s), (a + 1.0) - (a - 1.0) * cw + s, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - s)
        }
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// How much the equalizer lifts or cuts `freq` (dB): what the card's curve shows.
pub fn eq_response_db(v: &Evaluated, freq: f32, rate: f32) -> f32 {
    let w = 2.0 * PI * freq / rate;
    eq_bands(v)
        .iter()
        .filter(|b| b.gain_db.abs() > 0.01)
        .map(|b| {
            let [b0, b1, b2, a1, a2] = coefficients(b, rate);
            // |H(e^jw)|², with z⁻¹ = e^(−jw).
            let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
            let (nr, ni) = (b0 + b1 * c1 + b2 * c2, -(b1 * s1 + b2 * s2));
            let (dr, di) = (1.0 + a1 * c1 + a2 * c2, -(a1 * s1 + a2 * s2));
            10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-12)).log10()
        })
        .sum()
}

/// A biquad's running state for one channel (transposed direct form II).
#[derive(Clone, Copy, Default)]
struct Biquad {
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn run(&mut self, x: f32, c: &[f32; 5]) -> f32 {
        let y = c[0] * x + self.z1;
        self.z1 = c[1] * x - c[3] * y + self.z2;
        self.z2 = c[2] * x - c[4] * y;
        y
    }
}

// ---- equalizer ----

pub(crate) struct Equalizer {
    ch: usize,
    rate: f32,
    state: Vec<[Biquad; 5]>,
}

impl Equalizer {
    pub(crate) fn new(ch: usize, rate: f32) -> Self {
        Equalizer { ch, rate, state: vec![[Biquad::default(); 5]; ch] }
    }
}

impl Processor for Equalizer {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let bands = eq_bands(v);
        let coeffs: Vec<(usize, [f32; 5])> = bands.iter().enumerate().filter(|(_, b)| b.gain_db.abs() > 0.01).map(|(i, b)| (i, coefficients(b, self.rate))).collect();
        if coeffs.is_empty() {
            return;
        }
        for frame in buf.chunks_exact_mut(self.ch) {
            for (c, x) in frame.iter_mut().enumerate() {
                for (i, k) in &coeffs {
                    *x = self.state[c][*i].run(*x, k);
                }
            }
        }
    }

    fn reset(&mut self) {
        self.state.iter_mut().for_each(|s| *s = [Biquad::default(); 5]);
    }
}

// ---- compressor ----

fn coefficient(seconds: f64, rate: f32) -> f32 {
    1.0 - (-1.0 / (seconds.max(0.0001) as f32 * rate)).exp()
}

/// How much to turn a level down (dB, ≥ 0), with a soft knee around the threshold.
fn reduction(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = level_db - threshold;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee > 0.0 && over.abs() <= knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

pub(crate) struct Compressor {
    ch: usize,
    rate: f32,
    env_db: f32,
    gr: f32,
}

impl Compressor {
    pub(crate) fn new(ch: usize, rate: f32) -> Self {
        Compressor { ch, rate, env_db: -120.0, gr: 0.0 }
    }
}

impl Processor for Compressor {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let (threshold, ratio, knee) = (v.float("threshold") as f32, v.float("ratio") as f32, v.float("knee").max(0.0) as f32);
        let makeup = 10f32.powf(v.float("makeup") as f32 / 20.0);
        let attack = coefficient(v.float("attack"), self.rate);
        let release = coefficient(v.float("release"), self.rate);
        for frame in buf.chunks_exact_mut(self.ch) {
            // Linked: the loudest channel decides, so the stereo image doesn't wander.
            let peak = frame.iter().fold(0f32, |m, x| m.max(x.abs()));
            let level = if peak > 1e-6 { 20.0 * peak.log10() } else { -120.0 };
            let target = reduction(level, threshold, ratio, knee);
            self.gr += (target - self.gr) * if target > self.gr { attack } else { release };
            self.env_db = level;
            let g = 10f32.powf(-self.gr / 20.0) * makeup;
            frame.iter_mut().for_each(|x| *x *= g);
        }
    }

    fn reset(&mut self) {
        self.gr = 0.0;
        self.env_db = -120.0;
    }

    fn reduction_db(&self) -> f32 {
        self.gr
    }
}

// ---- limiter: nothing past the ceiling, looking a little ahead ----

/// Lookahead: the gain is already down when a peak arrives.
const LOOKAHEAD_SECONDS: f32 = 0.003;

pub(crate) struct Limiter {
    ch: usize,
    rate: f32,
    delay: Vec<f32>,
    /// The gain each of the delayed frames needs.
    needs: Vec<f32>,
    pos: usize,
    len: usize,
    gain: f32,
}

impl Limiter {
    pub(crate) fn new(ch: usize, rate: f32) -> Self {
        let len = ((rate * LOOKAHEAD_SECONDS) as usize).max(1) + 1;
        Limiter { ch, rate, delay: vec![0.0; len * ch], needs: vec![1.0; len], pos: 0, len, gain: 1.0 }
    }
}

impl Processor for Limiter {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let ceiling = 10f32.powf(v.float("ceiling").min(0.0) as f32 / 20.0);
        let release = coefficient(v.float("release"), self.rate);
        for frame in buf.chunks_exact_mut(self.ch) {
            let peak = frame.iter().fold(0f32, |m, x| m.max(x.abs()));
            self.needs[self.pos] = if peak > ceiling { ceiling / peak } else { 1.0 };
            // The quietest gain anything in the window needs: reached before its peak
            // leaves the delay line, so nothing gets through above the ceiling.
            let target = self.needs.iter().fold(1f32, |m, g| m.min(*g));
            self.gain = if target < self.gain { target } else { self.gain + (target - self.gain) * release };
            let out = (self.pos + 1) % self.len;
            for (c, x) in frame.iter_mut().enumerate() {
                self.delay[self.pos * self.ch + c] = *x;
                *x = self.delay[out * self.ch + c] * self.gain;
            }
            self.pos = out;
        }
    }

    fn reset(&mut self) {
        self.delay.fill(0.0);
        self.needs.fill(1.0);
        self.gain = 1.0;
    }

    fn latency(&self) -> usize {
        self.len - 1
    }

    fn reduction_db(&self) -> f32 {
        -20.0 * self.gain.max(1e-6).log10()
    }
}

// ---- de-esser: turning the hiss of s and sh down, and only that ----

pub(crate) struct DeEsser {
    ch: usize,
    rate: f32,
    /// Listens for sibilance: a high-pass per channel.
    detect: Vec<Biquad>,
    /// Turns it down: a high shelf per channel, its gain following the reduction.
    shelf: Vec<Biquad>,
    env: f32,
    gr: f32,
}

impl DeEsser {
    pub(crate) fn new(ch: usize, rate: f32) -> Self {
        DeEsser { ch, rate, detect: vec![Biquad::default(); ch], shelf: vec![Biquad::default(); ch], env: 0.0, gr: 0.0 }
    }
}

/// Frames between updates of the de-esser's shelf (~0.7 ms at 48 kHz).
const SHELF_STEP: usize = 32;

impl Processor for DeEsser {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, _clock: &ClockSpan) {
        let freq = v.float("frequency").clamp(2000.0, 12000.0) as f32;
        let threshold = v.float("threshold") as f32;
        let most = v.float("amount").max(0.0) as f32;
        let w = 2.0 * PI * freq.min(self.rate * 0.45) / self.rate;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / (2.0 * 0.707);
        let a0 = 1.0 + alpha;
        let hp = [(1.0 + cw) / 2.0 / a0, -(1.0 + cw) / a0, (1.0 + cw) / 2.0 / a0, -2.0 * cw / a0, (1.0 - alpha) / a0];
        let (attack, release) = (coefficient(0.001, self.rate), coefficient(0.06, self.rate));
        // The shelf sits a little below the detector, so the whole hiss is under it.
        let shelf_band = |gr: f32| Band { shape: BandShape::HighShelf, freq: freq * 0.8, gain_db: -gr, q: 0.707, ids: ["", "", ""] };
        for chunk in buf.chunks_mut(SHELF_STEP * self.ch) {
            let shelf = coefficients(&shelf_band(self.gr), self.rate);
            for frame in chunk.chunks_exact_mut(self.ch) {
                let mut peak = 0f32;
                for (c, x) in frame.iter_mut().enumerate() {
                    peak = peak.max(self.detect[c].run(*x, &hp).abs());
                    *x = self.shelf[c].run(*x, &shelf);
                }
                self.env += (peak - self.env) * if peak > self.env { attack } else { release };
                let level = if self.env > 1e-6 { 20.0 * self.env.log10() } else { -120.0 };
                let target = reduction(level, threshold, 4.0, 4.0).min(most);
                self.gr += (target - self.gr) * if target > self.gr { attack } else { release };
            }
        }
    }

    fn reset(&mut self) {
        self.detect.iter_mut().chain(self.shelf.iter_mut()).for_each(|b| *b = Biquad::default());
        self.env = 0.0;
        self.gr = 0.0;
    }

    fn reduction_db(&self) -> f32 {
        self.gr
    }
}

// ---- formants: the voice's character, moved on its own ----

const FFT: usize = 1024;
const HOP: usize = FFT / 4;

struct FormantChannel {
    input: Vec<f32>,
    output: Vec<f32>,
}

/// Moves a sound's spectral envelope — the resonances that make a voice sound like a
/// child, an adult, a giant — by `shift` semitones without touching its pitch: each
/// frame's magnitudes are divided by their smoothed envelope and multiplied by the
/// envelope read further up or down. Short-time Fourier transform, a Hann window at 75%
/// overlap; a fixed FFT's worth of delay.
pub(crate) struct Formant {
    ch: usize,
    chans: Vec<FormantChannel>,
    window: Vec<f32>,
    fill: usize,
    ready: Vec<Vec<f32>>,
}

impl Formant {
    pub(crate) fn new(ch: usize) -> Self {
        let window = (0..FFT).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / FFT as f32).cos()).sqrt()).collect();
        let chan = || FormantChannel { input: vec![0.0; FFT], output: vec![0.0; FFT] };
        Formant { ch, chans: (0..ch).map(|_| chan()).collect(), window, fill: 0, ready: vec![vec![0.0; HOP]; ch] }
    }

    fn frame(&mut self, c: usize, ratio: f32) {
        let ch = &mut self.chans[c];
        let mut re: Vec<f32> = ch.input.iter().zip(&self.window).map(|(x, w)| x * w).collect();
        let mut im = vec![0.0f32; FFT];
        crate::fx::fft(&mut re, &mut im, false);
        if (ratio - 1.0).abs() > 1e-3 {
            const BINS: usize = FFT / 2 + 1;
            let mag: Vec<f32> = (0..BINS).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
            // The envelope: log magnitude averaged over ~±250 Hz.
            let log: Vec<f32> = mag.iter().map(|m| (m + 1e-9).ln()).collect();
            let reach = 5usize;
            let env: Vec<f32> = (0..BINS)
                .map(|k| {
                    let (lo, hi) = (k.saturating_sub(reach), (k + reach).min(BINS - 1));
                    (log[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32).exp()
                })
                .collect();
            let at = |x: f32| {
                let i = (x.floor() as usize).min(BINS - 1);
                let j = (i + 1).min(BINS - 1);
                let f = x - x.floor();
                env[i] * (1.0 - f) + env[j] * f
            };
            for k in 1..BINS {
                let from = k as f32 / ratio;
                let target = if from < (BINS - 1) as f32 { at(from) } else { env[BINS - 1] * 0.1 };
                let g = (target / env[k].max(1e-9)).clamp(0.0, 16.0);
                re[k] *= g;
                im[k] *= g;
                if k < FFT / 2 {
                    re[FFT - k] = re[k];
                    im[FFT - k] = -im[k];
                }
            }
        }
        crate::fx::fft(&mut re, &mut im, true);
        for ((out, x), w) in ch.output.iter_mut().zip(&re).zip(&self.window) {
            *out += x * w * 0.5;
        }
        self.ready[c].extend_from_slice(&ch.output[..HOP]);
        ch.output.copy_within(HOP.., 0);
        ch.output[FFT - HOP..].fill(0.0);
    }

    /// Moves the envelope by `semitones` (0: passes the sound through, delayed).
    pub(crate) fn run(&mut self, buf: &mut [f32], semitones: f32) {
        let ratio = 2f32.powf(semitones / 12.0);
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
                    self.frame(c, ratio);
                }
            }
        }
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

    pub(crate) fn reset(&mut self) {
        let ch = self.ch;
        *self = Formant::new(ch);
    }

    pub(crate) const LATENCY: usize = FFT;
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_params::{EvalContext, ParamSet, ParamSource, Value};
    use oa_time::Time;

    const RATE: f32 = 48_000.0;

    fn values(type_id: &str, set: &[(&str, Value)]) -> Evaluated {
        let info = crate::fx::info(type_id).expect("in the catalog");
        let mut params = ParamSet::default();
        for (k, v) in set {
            params.set(k, ParamSource::Static(v.clone()));
        }
        params.eval(&info.params, None, &EvalContext::at(Time::ZERO, Time::ZERO))
    }

    fn run(type_id: &str, set: &[(&str, Value)], input: &[f32], ch: usize) -> Vec<f32> {
        let v = values(type_id, set);
        let mut p = crate::fx::processor(type_id, ch, RATE).unwrap();
        let mut out = input.to_vec();
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

    fn gain_db(out: &[f32], input: &[f32]) -> f32 {
        20.0 * (rms(out) / rms(input)).log10()
    }

    /// A peak band lifts its frequency by its gain and leaves the others; the curve the
    /// card draws says the same as what comes out.
    #[test]
    fn the_equalizer_does_what_its_curve_shows() {
        let set = [("p2_freq", Value::Float(1000.0)), ("p2_gain", Value::Float(9.0)), ("p2_q", Value::Float(1.4))];
        for (freq, expect) in [(1000.0, 9.0), (8000.0, 0.0), (100.0, 0.0)] {
            let x = sine(freq, 0.5, 0.1);
            let y = run("oa.audio.eq", &set, &x, 1);
            let measured = gain_db(&y[12_000..], &x[12_000..]);
            let drawn = eq_response_db(&values("oa.audio.eq", &set), freq, RATE);
            assert!((measured - expect).abs() < 0.6, "{freq} Hz: {measured} dB");
            assert!((measured - drawn).abs() < 0.3, "{freq} Hz: measured {measured}, drawn {drawn}");
        }
        // Shelves: the low one lifts the lows only.
        let set = [("low_freq", Value::Float(150.0)), ("low_gain", Value::Float(-12.0))];
        let (lo, hi) = (sine(40.0, 0.5, 0.1), sine(6000.0, 0.5, 0.1));
        assert!(gain_db(&run("oa.audio.eq", &set, &lo, 1)[12_000..], &lo[12_000..]) < -10.0);
        assert!(gain_db(&run("oa.audio.eq", &set, &hi, 1)[12_000..], &hi[12_000..]).abs() < 0.5);
    }

    /// Above the threshold the compressor lets through 1/ratio of the excess; below it,
    /// nothing changes; and it reports what it's doing.
    #[test]
    fn the_compressor_turns_loud_passages_down_by_its_ratio() {
        let set = [("threshold", Value::Float(-20.0)), ("ratio", Value::Float(4.0)), ("knee", Value::Float(0.0)), ("attack", Value::Float(0.001)), ("release", Value::Float(0.05)), ("makeup", Value::Float(0.0))];
        let loud = sine(200.0, 1.0, 0.5); // −6 dBFS peaks: 14 dB over
        let y = run("oa.audio.compressor", &set, &loud, 1);
        let peak_out = 20.0 * y[24_000..].iter().fold(0f32, |m, x| m.max(x.abs())).log10();
        assert!((peak_out - (-20.0 + 14.0 / 4.0)).abs() < 1.0, "{peak_out} dBFS");
        let quiet = sine(200.0, 1.0, 0.05);
        assert!(gain_db(&run("oa.audio.compressor", &set, &quiet, 1)[24_000..], &quiet[24_000..]).abs() < 0.1);
        let mut p = Compressor::new(1, RATE);
        let mut block = loud[..24_000].to_vec();
        p.process(&mut block, &values("oa.audio.compressor", &set), &ClockSpan::default());
        assert!((p.reduction_db() - 10.5).abs() < 1.0, "reports {} dB", p.reduction_db());
    }

    /// Nothing gets past the limiter's ceiling, even a sudden peak.
    #[test]
    fn the_limiter_holds_the_ceiling() {
        let mut x = sine(300.0, 1.0, 0.3);
        for s in &mut x[20_000..20_050] {
            *s = 1.4; // a sudden spike
        }
        let y = run("oa.audio.limiter", &[("ceiling", Value::Float(-3.0))], &x, 1);
        let ceiling = 10f32.powf(-3.0 / 20.0);
        let over = y.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(over <= ceiling + 1e-4, "{over} > {ceiling}");
        // Quiet passages pass through (delayed by the lookahead).
        let lat = Limiter::new(1, RATE).latency();
        assert!((y[10_000 + lat] - x[10_000]).abs() < 1e-4);
    }

    /// The de-esser turns a hiss down and leaves the voice's body alone.
    #[test]
    fn the_de_esser_only_touches_the_hiss() {
        let set = [("frequency", Value::Float(5000.0)), ("threshold", Value::Float(-30.0)), ("amount", Value::Float(12.0))];
        let hiss = sine(8000.0, 0.5, 0.3);
        let body = sine(300.0, 0.5, 0.3);
        assert!(gain_db(&run("oa.audio.deess", &set, &hiss, 1)[12_000..], &hiss[12_000..]) < -6.0);
        assert!(gain_db(&run("oa.audio.deess", &set, &body, 1)[12_000..], &body[12_000..]).abs() < 0.5);
    }

    /// Echo and reverb say how long they ring; plain effects don't.
    #[test]
    fn tails_last_as_long_as_the_effect_rings() {
        let echo = tail_seconds("oa.audio.echo", &values("oa.audio.echo", &[("delay", Value::Float(0.5)), ("feedback", Value::Float(0.5))]));
        assert!((echo - 0.5 * 9.97).abs() < 0.1, "{echo}");
        let hall = tail_seconds("oa.audio.reverb", &values("oa.audio.reverb", &[("space", Value::Enum("cathedral".into()))]));
        let room = tail_seconds("oa.audio.reverb", &values("oa.audio.reverb", &[("space", Value::Enum("small room".into()))]));
        assert!(hall > 3.0 * room && room > 0.1, "{hall} vs {room}");
        assert_eq!(tail_seconds("oa.audio.bass", &values("oa.audio.bass", &[])), 0.0);
    }

    /// Moving formants changes the timbre, not the pitch: a buzz keeps its frequency
    /// while its brightest region moves up.
    #[test]
    fn formants_move_without_the_pitch() {
        // A buzz: 150 Hz with harmonics shaped by a resonance near 1 kHz.
        let buzz: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = i as f32 / RATE;
                (1..30).map(|h| {
                    let f = 150.0 * h as f32;
                    let shape = (-((f - 1000.0) / 400.0).powi(2)).exp();
                    shape * (2.0 * PI * f * t).sin()
                }).sum::<f32>() * 0.05
            })
            .collect();
        let mut f = Formant::new(1);
        let mut up = buzz.clone();
        f.run(&mut up, 7.0);
        let out = &up[Formant::LATENCY + 4800..];
        // The fundamental, from where the sound best repeats itself (50–400 Hz).
        let fundamental = |x: &[f32]| {
            let n = 8192;
            let lag = ((RATE / 400.0) as usize..(RATE / 50.0) as usize)
                .max_by(|a, b| {
                    let r = |l: usize| x[..n].iter().zip(&x[l..l + n]).map(|(p, q)| p * q).sum::<f32>();
                    r(*a).total_cmp(&r(*b))
                })
                .unwrap();
            RATE / lag as f32
        };
        let (before, after) = (fundamental(&buzz[4800..]), fundamental(out));
        assert!((before - 150.0).abs() < 3.0 && (after - before).abs() < 3.0, "pitch kept: {before} → {after}");
        // And there's more energy up high than before.
        let highs = |x: &[f32]| {
            let mut prev = 0.0;
            rms(&x.iter().map(|v| {
                let d = v - prev;
                prev = *v;
                d
            }).collect::<Vec<f32>>())
        };
        assert!(highs(out) > 1.3 * highs(&buzz[4800..]), "brighter: {} vs {}", highs(out), highs(&buzz[4800..]));
    }
}
