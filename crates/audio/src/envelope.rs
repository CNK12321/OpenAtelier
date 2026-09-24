//! Loudness envelopes: how loud a file is, overall and in three bands, every 10 ms —
//! what properties connected to the sound ([`oa_params::Modulator::Follow`]) read.
//!
//! A file is analyzed once (mono, 16 kHz, split by one-pole filters at 200 Hz and
//! 4 kHz); a [`Follower`] then answers "how loud is this item / the mix at `t`" from the
//! timeline's clips — their placement, speed, gain and fades — without decoding.

use crate::{AudioClip, AudioFormat, AudioSource, FfmpegAudioSource};
use oa_params::{SoundBand, SoundSource};
use oa_time::Time;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Envelope values per second.
pub const ENVELOPE_RATE: f64 = 100.0;
const ANALYSIS_RATE: u32 = 16_000;

/// One file's levels: linear RMS per [`SoundBand`], smoothed (quick rise, ~120 ms fall).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Envelope {
    pub levels: Vec<[f32; 4]>,
}

impl Envelope {
    /// Decodes and analyzes `path` (blocking; seconds for a long file).
    pub fn analyze(path: &Path) -> Option<Envelope> {
        let mut source = FfmpegAudioSource::open(path, AudioFormat { sample_rate: ANALYSIS_RATE, channels: 1 }).ok()?;
        let mut samples = Vec::new();
        let mut block = vec![0f32; 16_384];
        loop {
            let n = source.read(&mut block);
            samples.extend_from_slice(&block[..n]);
            if n == 0 && source.finished() {
                break;
            }
        }
        (!samples.is_empty()).then(|| Envelope::from_samples(&samples, ANALYSIS_RATE))
    }

    /// Analyzes mono `samples` at `rate`.
    pub fn from_samples(samples: &[f32], rate: u32) -> Envelope {
        let rate = rate.max(1) as f32;
        let pole = |hz: f32| 1.0 - (-2.0 * std::f32::consts::PI * hz / rate).exp();
        let (a_low, a_high) = (pole(200.0), pole(4000.0));
        let window = ((rate as f64 / ENVELOPE_RATE).round() as usize).max(1);
        let (mut low, mut mid_low) = (0f32, 0f32);
        let mut levels = Vec::with_capacity(samples.len() / window + 1);
        let mut held = [0f32; 4];
        for chunk in samples.chunks(window) {
            let mut sum = [0f32; 4];
            for &x in chunk {
                low += a_low * (x - low);
                mid_low += a_high * (x - mid_low);
                let bands = [x, low, mid_low - low, x - mid_low];
                for (s, b) in sum.iter_mut().zip(bands) {
                    *s += b * b;
                }
            }
            for (h, s) in held.iter_mut().zip(sum) {
                let rms = (s / chunk.len() as f32).sqrt();
                // Quick to rise, slower to fall, so things pulse instead of flicker.
                *h = if rms > *h { *h + (rms - *h) * 0.7 } else { *h * 0.92 + rms * 0.08 };
            }
            levels.push(held);
        }
        Envelope { levels }
    }

    /// The level of `band` at `t` in the file (linear, interpolated; silence outside).
    pub fn at(&self, t: Time, band: SoundBand) -> f32 {
        let x = t.as_seconds_f64() * ENVELOPE_RATE;
        if x < 0.0 || self.levels.is_empty() {
            return 0.0;
        }
        let i = x.floor() as usize;
        let f = (x - i as f64) as f32;
        let get = |i: usize| self.levels.get(i).map_or(0.0, |l| l[band.index()]);
        get(i) * (1.0 - f) + get(i + 1) * f
    }
}

/// Answers sound levels on the timeline from the clips that play and their files'
/// envelopes. Cheap to query; build one per edit.
#[derive(Clone, Default)]
pub struct Follower {
    clips: Vec<(AudioClip, Arc<Envelope>)>,
}

impl Follower {
    /// `envelopes` by media id; clips whose file isn't analyzed (yet) are silent.
    pub fn new(clips: &[AudioClip], envelopes: &HashMap<u64, Arc<Envelope>>) -> Follower {
        Follower { clips: clips.iter().filter_map(|c| Some((c.clone(), envelopes.get(&c.media)?.clone()))).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// The linear RMS level of `source`'s `band` at timeline time `t`: one clip's, or
    /// every clip playing then summed by power (the mix). Sound effects on the clips
    /// aren't heard here; gain and fades are.
    pub fn level(&self, t: Time, source: SoundSource, band: SoundBand) -> f64 {
        let power: f64 = self
            .clips
            .iter()
            .filter(|(c, _)| c.range.contains(t) && (source == SoundSource::Mix || source == SoundSource::Item(c.id)))
            .map(|(c, env)| {
                let v = (env.at(c.source_at(t), band) * c.amplitude(t)) as f64;
                v * v
            })
            .sum();
        power.sqrt()
    }

    /// A provider for [`oa_params::signal::with`] at timeline time `t`.
    pub fn at(self: &Arc<Self>, t: Time) -> std::rc::Rc<dyn Fn(SoundSource, SoundBand) -> f64> {
        let me = self.clone();
        std::rc::Rc::new(move |s, b| me.level(t, s, b))
    }
}

/// The media ids `clips` play, each with its file, once.
pub fn media_of(clips: &[AudioClip]) -> Vec<(u64, std::path::PathBuf)> {
    let mut seen = HashMap::new();
    for c in clips {
        seen.entry(c.media).or_insert_with(|| c.path.clone());
    }
    seen.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_time::TimeRange;
    use std::path::PathBuf;

    fn tone(hz: f32, seconds: f32, amp: f32) -> Vec<f32> {
        (0..(seconds * ANALYSIS_RATE as f32) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / ANALYSIS_RATE as f32).sin()).collect()
    }

    #[test]
    fn bands_hear_what_they_should() {
        let low = Envelope::from_samples(&tone(60.0, 1.0, 0.5), ANALYSIS_RATE);
        let high = Envelope::from_samples(&tone(8000.0 * 0.9, 1.0, 0.5), ANALYSIS_RATE);
        let t = Time::from_seconds_f64(0.5);
        assert!(low.at(t, SoundBand::Bass) > 4.0 * low.at(t, SoundBand::Treble), "a 60 Hz tone is bass");
        assert!(high.at(t, SoundBand::Treble) > 4.0 * high.at(t, SoundBand::Bass), "a 7.2 kHz tone is treble");
        // A sine's RMS is amplitude / √2.
        assert!((low.at(t, SoundBand::Loudness) - 0.5 / 2f32.sqrt()).abs() < 0.03);
    }

    #[test]
    fn the_follower_places_clips_on_the_timeline() {
        let mut samples = vec![0.0; ANALYSIS_RATE as usize];
        samples.extend(tone(440.0, 1.0, 0.8));
        let env = Arc::new(Envelope::from_samples(&samples, ANALYSIS_RATE));
        let secs = Time::from_seconds_f64;
        // The file is silent for 1 s then loud; the clip starts 1 s into it at 10 s.
        let clip = AudioClip::new(5, 9, PathBuf::from("x"), TimeRange::new(secs(10.0), secs(1.0)), secs(1.0));
        let follower = Arc::new(Follower::new(&[clip], &HashMap::from([(9, env)])));
        let at = |t: f64, s| follower.level(secs(t), s, SoundBand::Loudness);
        assert!(at(10.5, SoundSource::Mix) > 0.4);
        assert!(at(10.5, SoundSource::Item(5)) > 0.4);
        assert_eq!(at(10.5, SoundSource::Item(6)), 0.0);
        assert_eq!(at(9.5, SoundSource::Mix), 0.0, "before the clip");
        let provider = follower.at(secs(10.5));
        assert!(oa_params::signal::with(provider, || oa_params::signal::level(SoundSource::Mix, SoundBand::Loudness)) > 0.4);
    }
}
