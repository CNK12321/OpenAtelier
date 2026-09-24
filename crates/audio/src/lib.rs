//! Audio playback: decoding, a lock-free ring, an output device and the playback clock.
//!
//! The clock is the master for playback (DESIGN.md §12): video frames are presented
//! against the audio position, because a dropped video frame is invisible next to an
//! audible glitch.

pub mod fx;
mod dynamics;
pub mod envelope;
pub mod shader;
mod engine;
mod ffmpeg;
mod recorder;
mod ring;
mod timeline;

pub use engine::AudioEngine;
pub use ffmpeg::FfmpegAudioSource;
pub use recorder::{Recorder, Take};
pub use ring::Ring;
pub use fx::AudioEffect;
pub use timeline::{db_to_amplitude, AudioBus, AudioClip, MixHandle, MixState, Opener, TimelineAudio, SILENCE_DB};

use oa_time::Time;
use std::fmt;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub fn stereo_48k() -> Self {
        AudioFormat { sample_rate: 48_000, channels: 2 }
    }
}

/// Interleaved `f32` samples at a fixed format. Implementations decode; the engine mixes
/// and plays.
pub trait AudioSource: Send {
    fn format(&self) -> AudioFormat;

    /// Fills `out` with the next samples, returning how many were written (0 means
    /// nothing is available right now — check [`finished`](AudioSource::finished)).
    fn read(&mut self, out: &mut [f32]) -> usize;

    /// Restarts decoding at `t`.
    fn seek(&mut self, t: Time) -> Result<(), AudioError>;

    /// True once the source has no more audio.
    fn finished(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioError {
    NoDevice,
    Device(String),
    Decode(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no audio output device"),
            AudioError::Device(e) => write!(f, "audio device error: {e}"),
            AudioError::Decode(e) => write!(f, "audio decode error: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}
