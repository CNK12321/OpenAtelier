//! Audio output and the playback clock.
//!
//! * A producer thread pulls from the [`AudioSource`] and fills the ring ahead of time.
//! * The device callback only pops from the ring, applies gain and counts samples — no
//!   locks, no allocation.
//! * The **clock** is what the samples that have actually reached the speakers say, minus
//!   the device's output latency. Video presentation follows it (DESIGN.md §12).

use crate::ring::Ring;
use crate::{AudioError, AudioFormat, AudioSource};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use oa_time::Time;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Shared {
    ring: Ring,
    format: AudioFormat,
    /// Bumped on every seek. The producer reacts first, then the consumer flushes.
    epoch: AtomicU64,
    /// Epoch the producer has already sought to; the consumer flushes when it differs.
    produced_epoch: AtomicU64,
    consumed_epoch: AtomicU64,
    /// Timeline position (flicks) the current epoch started at.
    base: AtomicI64,
    /// Samples per channel handed to the device since the last flush.
    played: AtomicU64,
    /// Device buffering between the callback and the speakers, in samples per channel.
    latency: AtomicU64,
    gain: AtomicU32,
    playing: AtomicBool,
    /// Samples (per channel) that must be buffered before playback starts, so the first
    /// callback after play or seek never runs dry.
    prefill: usize,
    /// Cleared on every seek: the ring has to fill again before the clock runs.
    primed: AtomicBool,
    stop: AtomicBool,
    underruns: AtomicU64,
    /// Set when the source runs out.
    ended: AtomicBool,
}

impl Shared {
    fn position(&self) -> Time {
        let played = self.played.load(Ordering::Relaxed).saturating_sub(self.latency.load(Ordering::Relaxed));
        let base = Time(self.base.load(Ordering::Relaxed));
        base + Time::from_rational_floor(oa_time::Rational::new(played as i64, self.format.sample_rate as i64))
    }
}

pub struct AudioEngine {
    shared: Arc<Shared>,
    stream: Option<cpal::Stream>,
    producer: Option<std::thread::JoinHandle<()>>,
    device_name: String,
}

impl AudioEngine {
    /// Opens the default output device and starts feeding it from the source `make`
    /// builds for the device's own format (so nothing plays at the wrong pitch on a
    /// 44.1 kHz or surround device).
    pub fn new(make: impl FnOnce(AudioFormat) -> Box<dyn AudioSource>, start_at: Time) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        let device_name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "audio device".into());
        let supported = device.default_output_config().map_err(|e| AudioError::Device(e.to_string()))?;
        let format = AudioFormat { sample_rate: supported.sample_rate(), channels: supported.channels() };
        let mut source = make(format);
        if source.format() != format {
            return Err(AudioError::Device(format!("source format {:?} doesn't match the device's {format:?}", source.format())));
        }
        source.seek(start_at)?;

        // Roughly a quarter second of lookahead.
        let capacity = (format.sample_rate as usize * format.channels as usize / 4).max(4096);
        let shared = Arc::new(Shared {
            ring: Ring::new(capacity),
            format,
            epoch: AtomicU64::new(1),
            produced_epoch: AtomicU64::new(1),
            consumed_epoch: AtomicU64::new(1),
            base: AtomicI64::new(start_at.0),
            played: AtomicU64::new(0),
            latency: AtomicU64::new(0),
            gain: AtomicU32::new(1.0f32.to_bits()),
            playing: AtomicBool::new(false),
            prefill: format.sample_rate as usize / 20, // 50 ms
            primed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            ended: AtomicBool::new(false),
        });

        let config = cpal::StreamConfig {
            channels: format.channels,
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let callback_shared = shared.clone();
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let s = &callback_shared;
                    // A seek happened: drop what the producer had already queued.
                    let produced = s.produced_epoch.load(Ordering::Acquire);
                    if s.consumed_epoch.load(Ordering::Relaxed) != produced {
                        s.ring.clear();
                        s.consumed_epoch.store(produced, Ordering::Relaxed);
                        s.played.store(0, Ordering::Relaxed);
                        s.primed.store(false, Ordering::Relaxed);
                    }
                    if !s.playing.load(Ordering::Relaxed) {
                        out.fill(0.0);
                        return;
                    }
                    // Wait for enough audio before starting the clock, so playback never
                    // begins with a glitch. The playhead simply waits with it.
                    if !s.primed.load(Ordering::Relaxed) {
                        let buffered = s.ring.available() / s.format.channels as usize;
                        if buffered < s.prefill && !s.ended.load(Ordering::Relaxed) {
                            out.fill(0.0);
                            return;
                        }
                        s.primed.store(true, Ordering::Relaxed);
                    }
                    let filled = s.ring.pop(out);
                    if filled < out.len() && !s.ended.load(Ordering::Relaxed) {
                        s.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    let gain = f32::from_bits(s.gain.load(Ordering::Relaxed));
                    if gain != 1.0 {
                        for sample in out[..filled].iter_mut() {
                            *sample *= gain;
                        }
                    }
                    // Count the whole buffer: the device played it, silence included, so
                    // the clock keeps moving even when a source has nothing to give.
                    s.played.fetch_add((out.len() / s.format.channels as usize) as u64, Ordering::Relaxed);
                },
                move |err| eprintln!("audio output error: {err}"),
                None,
            )
            .map_err(|e| AudioError::Device(e.to_string()))?;
        stream.play().map_err(|e| AudioError::Device(e.to_string()))?;

        let producer_shared = shared.clone();
        let producer = std::thread::Builder::new()
            .name("oa-audio-producer".into())
            .spawn(move || feed(producer_shared, source))
            .map_err(|e| AudioError::Device(e.to_string()))?;

        Ok(AudioEngine { shared, stream: Some(stream), producer: Some(producer), device_name })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn format(&self) -> AudioFormat {
        self.shared.format
    }

    /// Where playback actually is, from the samples the device has consumed.
    pub fn position(&self) -> Time {
        self.shared.position()
    }

    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Relaxed) && self.shared.ring.available() == 0
    }

    /// Restarts decoding at `t`; buffered audio is dropped.
    pub fn seek(&self, t: Time) {
        self.shared.primed.store(false, Ordering::Relaxed);
        self.shared.base.store(t.0, Ordering::Relaxed);
        self.shared.ended.store(false, Ordering::Relaxed);
        self.shared.epoch.fetch_add(1, Ordering::Release);
    }

    /// 0 = silent, 1 = unity.
    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.store(gain.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Buffered audio, in seconds.
    pub fn buffered(&self) -> f64 {
        self.shared.ring.available() as f64 / (self.shared.format.sample_rate as f64 * self.shared.format.channels as f64)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.stream.take();
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

/// Producer thread: keep the ring full, and honor seeks.
fn feed(shared: Arc<Shared>, mut source: Box<dyn AudioSource>) {
    let mut block = vec![0.0f32; 4096];
    let mut epoch = shared.epoch.load(Ordering::Acquire);
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let current = shared.epoch.load(Ordering::Acquire);
        if current != epoch {
            epoch = current;
            let target = Time(shared.base.load(Ordering::Relaxed));
            if let Err(e) = source.seek(target) {
                eprintln!("audio seek failed: {e}");
            }
            // Tell the consumer to drop everything queued before this point.
            shared.produced_epoch.store(epoch, Ordering::Release);
            continue;
        }
        if shared.ring.free() < block.len() {
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }
        let n = source.read(&mut block);
        if n == 0 {
            if source.finished() {
                shared.ended.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }
        let mut written = 0;
        while written < n {
            if shared.stop.load(Ordering::Relaxed) || shared.epoch.load(Ordering::Acquire) != epoch {
                break;
            }
            written += shared.ring.push(&block[written..n]);
            if written < n {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
