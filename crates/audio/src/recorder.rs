//! Recording sound from an input device (a microphone), and the take it produces.
//!
//! The device callback only appends samples to a shared buffer and tracks the peak
//! level (for a meter); the UI reads snapshots of it to draw the waveform while it
//! records. A finished [`Take`] can be split into parts — by hand or at its pauses — and
//! each part written as a WAV file.

use crate::AudioError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Recorded sound: interleaved `f32` samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Take {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Take {
    /// Length in sample frames (one per channel group).
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    /// The loudest absolute sample in each of `buckets` equal slices of `[from, to)`
    /// (sample frames): a waveform overview.
    pub fn peaks(&self, from: usize, to: usize, buckets: usize) -> Vec<f32> {
        let ch = self.channels.max(1) as usize;
        let (from, to) = (from.min(self.frames()), to.min(self.frames()));
        if to <= from || buckets == 0 {
            return vec![0.0; buckets];
        }
        (0..buckets)
            .map(|b| {
                let a = from + (to - from) * b / buckets;
                let z = (from + (to - from) * (b + 1) / buckets).max(a + 1).min(to);
                self.samples[a * ch..z * ch].iter().fold(0.0f32, |m, s| m.max(s.abs()))
            })
            .collect()
    }

    /// Where the sound pauses: the middle of every stretch at least `min_gap` seconds
    /// long whose level stays below `threshold` (linear, 0..1). Cut points in seconds,
    /// not counting silence at the very start or end.
    pub fn pauses(&self, threshold: f32, min_gap: f64) -> Vec<f64> {
        let ch = self.channels.max(1) as usize;
        let window = (self.sample_rate as usize / 100).max(1); // 10 ms
        let loud: Vec<bool> = self
            .samples
            .chunks(window * ch)
            .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len().max(1) as f32).sqrt() >= threshold)
            .collect();
        let step = window as f64 / self.sample_rate.max(1) as f64;
        let (Some(first), Some(last)) = (loud.iter().position(|l| *l), loud.iter().rposition(|l| *l)) else { return Vec::new() };
        let mut cuts = Vec::new();
        let mut quiet_from: Option<usize> = None;
        for (i, &l) in loud.iter().enumerate().take(last + 1).skip(first) {
            match (l, quiet_from) {
                (false, None) => quiet_from = Some(i),
                (true, Some(q)) => {
                    if (i - q) as f64 * step >= min_gap {
                        cuts.push((q + i) as f64 / 2.0 * step);
                    }
                    quiet_from = None;
                }
                _ => {}
            }
        }
        cuts
    }

    /// Writes seconds `[from, to)` as a 16-bit PCM WAV file.
    pub fn write_wav(&self, path: &Path, from: f64, to: f64) -> std::io::Result<()> {
        let ch = self.channels.max(1) as usize;
        let at = |s: f64| ((s.max(0.0) * self.sample_rate as f64) as usize).min(self.frames());
        let (a, z) = (at(from), at(to).max(at(from)));
        let data = &self.samples[a * ch..z * ch];
        let bytes = (data.len() * 2) as u32;
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        let rate = self.sample_rate;
        let block = self.channels * 2;
        f.write_all(b"RIFF")?;
        f.write_all(&(36 + bytes).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?; // PCM
        f.write_all(&self.channels.to_le_bytes())?;
        f.write_all(&rate.to_le_bytes())?;
        f.write_all(&(rate * block as u32).to_le_bytes())?;
        f.write_all(&block.to_le_bytes())?;
        f.write_all(&16u16.to_le_bytes())?;
        f.write_all(b"data")?;
        f.write_all(&bytes.to_le_bytes())?;
        for s in data {
            f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())?;
        }
        f.flush()
    }
}

struct Shared {
    /// Samples not yet collected by `Recorder::drain`.
    pending: Mutex<Vec<f32>>,
    /// Loudest sample since the meter last looked (f32 bits).
    peak: AtomicU32,
    /// Not keeping what arrives (paused).
    paused: AtomicBool,
}

/// A recording in progress from one input device.
pub struct Recorder {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
    rate: u32,
    channels: u16,
    device: String,
}

impl Recorder {
    /// The input devices there are (their names), the default first.
    pub fn devices() -> Vec<String> {
        let host = cpal::default_host();
        let name = |d: &cpal::Device| d.description().map(|d| d.name().to_string()).ok();
        let default = host.default_input_device().as_ref().and_then(name);
        let mut out: Vec<String> = default.iter().cloned().collect();
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                if let Some(n) = name(&d)
                    && !out.contains(&n)
                {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Starts recording from `device` (by name), or the default input.
    pub fn start(device: Option<&str>) -> Result<Recorder, AudioError> {
        let host = cpal::default_host();
        let name = |d: &cpal::Device| d.description().map(|d| d.name().to_string()).unwrap_or_default();
        let dev = match device {
            Some(want) => host.input_devices().ok().and_then(|mut ds| ds.find(|d| name(d) == want)),
            None => None,
        }
        .or_else(|| host.default_input_device())
        .ok_or(AudioError::NoDevice)?;
        let supported = dev.default_input_config().map_err(|e| AudioError::Device(e.to_string()))?;
        let (rate, channels) = (supported.sample_rate(), supported.channels());
        let shared = Arc::new(Shared {
            pending: Mutex::new(Vec::new()),
            peak: AtomicU32::new(0),
            paused: AtomicBool::new(false),
        });
        let config = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let s = shared.clone();
        let err = |e: cpal::Error| eprintln!("audio input error: {e}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I16 => dev.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let f: Vec<f32> = data.iter().map(|x| *x as f32 / 32768.0).collect();
                    push(&s, &f);
                },
                err,
                None,
            ),
            _ => dev.build_input_stream(config, move |data: &[f32], _: &cpal::InputCallbackInfo| push(&s, data), err, None),
        }
        .map_err(|e| AudioError::Device(e.to_string()))?;
        stream.play().map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(Recorder { shared, _stream: stream, rate, channels, device: name(&dev) })
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// The loudest sample since the last call (0..1), for a level meter.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.shared.peak.swap(0, Ordering::Relaxed))
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// An empty take in this recording's format, for [`Recorder::drain`] to fill.
    pub fn new_take(&self) -> Take {
        Take { sample_rate: self.rate, channels: self.channels, samples: Vec::new() }
    }

    /// Moves what arrived since the last call onto the end of `take`: the UI keeps its
    /// own copy growing, rather than copying the whole take every frame.
    pub fn drain(&self, take: &mut Take) {
        let fresh = std::mem::take(&mut *self.shared.pending.lock().unwrap_or_else(|e| e.into_inner()));
        take.samples.extend_from_slice(&fresh);
    }

    /// Stops, adding the last of it to `take`.
    pub fn stop(self, take: &mut Take) {
        self.drain(take);
    }
}

fn push(s: &Shared, data: &[f32]) {
    let peak = data.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let old = f32::from_bits(s.peak.load(Ordering::Relaxed));
    if peak > old {
        s.peak.store(peak.to_bits(), Ordering::Relaxed);
    }
    if !s.paused.load(Ordering::Relaxed) {
        // A short lock: the UI only swaps the buffer out.
        s.pending.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 s tone, 0.5 s silence, 1 s tone, at 1 kHz sampling, mono.
    fn speech() -> Take {
        let rate = 1000;
        let mut samples = Vec::new();
        for part in [(1.0, 0.5f32), (0.5, 0.0), (1.0, 0.5)] {
            for i in 0..(part.0 * rate as f64) as usize {
                samples.push(part.1 * (i as f32 * 0.3).sin());
            }
        }
        Take { sample_rate: rate, channels: 1, samples }
    }

    #[test]
    fn pauses_are_found_between_the_words() {
        let t = speech();
        assert!((t.seconds() - 2.5).abs() < 1e-9);
        let cuts = t.pauses(0.05, 0.25);
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!((cuts[0] - 1.25).abs() < 0.03, "{cuts:?}");
        assert!(t.pauses(0.05, 0.8).is_empty(), "a pause shorter than asked for isn't one");
        let peaks = t.peaks(0, t.frames(), 5);
        assert!(peaks[0] > 0.4 && peaks[2] < 0.5 && peaks[4] > 0.4, "{peaks:?}");
    }

    #[test]
    fn parts_write_as_wav() {
        let t = speech();
        let path = std::env::temp_dir().join("oa-recorder-test.wav");
        t.write_wav(&path, 1.5, 2.5).expect("write");
        let bytes = std::fs::read(&path).expect("read");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 1000 * 2, "one second of 16-bit mono at 1 kHz");
        let _ = std::fs::remove_file(path);
    }
}
