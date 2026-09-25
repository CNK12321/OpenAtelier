//! What the sound cards show — the live meters every effect card has, the equalizer's
//! curve — and the biquad filters sound shaders are built from.

use oa_params::Evaluated;
use oa_script::dsp::Shape;
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
        for &[l, r] in after.as_chunks::<2>().0 {
            lr += l * r;
            ll += l * l;
            rr += r * r;
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

fn coefficients(b: &Band, rate: f32) -> [f32; 5] {
    let shape = match b.shape {
        BandShape::LowShelf => Shape::LowShelf,
        BandShape::Peak => Shape::Peak,
        BandShape::HighShelf => Shape::HighShelf,
    };
    oa_script::dsp::biquad(shape, b.freq, b.q, b.gain_db, rate)
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

#[cfg(test)]
mod tests {
    use crate::fx::ClockSpan;
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
        let mut p = crate::fx::processor("oa.audio.compressor", 1, RATE).unwrap();
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
        let lat = crate::fx::processor("oa.audio.limiter", 1, RATE).unwrap().latency();
        assert_eq!(lat, 144, "3 ms at 48 kHz");
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
        use crate::fx::tail_seconds;
        let echo = tail_seconds("oa.audio.echo", &values("oa.audio.echo", &[("delay", Value::Float(0.5)), ("feedback", Value::Float(0.5))]));
        assert!((echo - 0.5 * 9.97).abs() < 0.1, "{echo}");
        let hall = tail_seconds("oa.audio.reverb", &values("oa.audio.reverb", &[("space", Value::Enum("cathedral".into()))]));
        let room = tail_seconds("oa.audio.reverb", &values("oa.audio.reverb", &[("space", Value::Enum("small room".into()))]));
        assert!(hall > 3.0 * room && room > 0.1, "{hall} vs {room}");
        assert_eq!(tail_seconds("oa.audio.bass", &values("oa.audio.bass", &[])), 0.0);
    }

    /// Pitch Shift's formant stage changes the timbre, not the pitch: a buzz keeps its
    /// frequency while its brightest region moves up.
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
        let up = run("oa.audio.pitch", &[("semitones", Value::Float(0.0)), ("formant", Value::Float(7.0))], &buzz, 1);
        let out = &up[1024 + 4800..];
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
