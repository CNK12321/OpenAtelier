//! The export's sound, mixed on a thread of its own while the frames render.
//!
//! It used to be mixed to a WAV before the first frame — minutes of an hour-long
//! export's time (the sound mixes at ~170× real time, but everything waited on it). Now
//! the mixer runs beside the renderer and each encoder takes the sound the way it can:
//!
//! * **Media Foundation** reads raw sound as it interleaves it with the picture, so the
//!   mixer writes a WAV that grows as it goes ([`LiveAudio`]: how many frames are in so
//!   far), and the sink reads up to there — waiting only if the picture ever gets ahead
//!   of the mix.
//! * **ffmpeg** takes its inputs when it starts, so the picture is encoded on its own and
//!   the mixer feeds a second ffmpeg that encodes the sound (AAC, or Opus for WebM) at the
//!   same time; the two are put together at the end in one copying pass — the same
//!   rewrite the file's index move (`faststart`) always cost.

use crate::ExportError;
use oa_audio::{AudioFormat, MixState, TimelineAudio};
use oa_time::Time;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Where the mixer's sound goes.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioTarget {
    /// A WAV that grows as it's written (`LiveAudio` says how far).
    LiveWav,
    /// Encoded by ffmpeg into `path` with `args` (the codec's), for muxing afterwards.
    Encoded { path: PathBuf, args: Vec<String> },
}

/// A WAV being written while it's read: its path, format and length, and how many frames
/// are in it so far.
#[derive(Clone, Debug)]
pub struct LiveAudio {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: u64,
    written: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

impl LiveAudio {
    /// Creates the WAV (its header; the samples follow as they're mixed).
    pub fn create(path: PathBuf, format: AudioFormat, total_frames: u64) -> Result<Self, ExportError> {
        let mut file = std::fs::File::create(&path).map_err(|e| ExportError::Io(format!("{}: {e}", path.display())))?;
        crate::wav::write_header(&mut file, format.sample_rate, format.channels, total_frames).map_err(|e| ExportError::Io(e.to_string()))?;
        Ok(LiveAudio {
            path,
            sample_rate: format.sample_rate,
            channels: format.channels,
            total_frames,
            written: Arc::default(),
            done: Arc::default(),
            error: Arc::default(),
        })
    }

    pub fn seconds(&self) -> f64 {
        self.total_frames as f64 / self.sample_rate.max(1) as f64
    }

    /// Frames written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// The mixer has stopped (finished, stopped or failed).
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Waits until `frames` frames are in the file (or the mixer has stopped). Err if it
    /// stopped on an error.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn wait_for(&self, frames: u64) -> Result<(), ExportError> {
        while self.written() < frames.min(self.total_frames) {
            if self.done.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        match self.error.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(e) => Err(ExportError::Io(format!("mixing the sound: {e}"))),
            None => Ok(()),
        }
    }
}

/// The mixer thread.
pub struct Mixdown {
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<f64, String>>>,
    /// Seconds the mix took on its thread, once finished.
    pub seconds: f64,
}

impl Mixdown {
    /// Starts mixing `state` from `start` for `live.total_frames` frames, into `live`'s
    /// WAV or (`target`) an encoder.
    pub fn start(state: MixState, start: Time, live: &LiveAudio, target: AudioTarget) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (thread_live, thread_cancel) = (live.clone(), cancel.clone());
        let thread = std::thread::Builder::new()
            .name("oa-export-sound".into())
            .spawn(move || {
                let clock = std::time::Instant::now();
                let result = mix(state, start, &thread_live, &target, &thread_cancel).map(|()| clock.elapsed().as_secs_f64());
                if let Err(e) = &result {
                    *thread_live.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
                }
                thread_live.done.store(true, Ordering::Release);
                result
            })
            .ok();
        Mixdown { cancel, thread, seconds: 0.0 }
    }

    /// Stops the mix where it is (an export stopped early).
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Waits for the mix to be done; Err if it failed.
    pub fn finish(&mut self) -> Result<(), ExportError> {
        let result = match self.thread.take() {
            Some(t) => t.join().unwrap_or_else(|_| Err("the sound mixer stopped".into())),
            None => Err("couldn't start the sound mixer".into()),
        };
        self.seconds = result.as_ref().copied().unwrap_or(0.0);
        result.map(|_| ()).map_err(|e| ExportError::Io(format!("mixing the sound: {e}")))
    }
}

impl Drop for Mixdown {
    /// A canceled export: the mixer stops at its next block.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn mix(state: MixState, start: Time, live: &LiveAudio, target: &AudioTarget, cancel: &AtomicBool) -> Result<(), String> {
    let format = AudioFormat { sample_rate: live.sample_rate, channels: live.channels };
    let mut timeline = TimelineAudio::with_handle(oa_audio::MixHandle::new(state), format, start);
    let mut progress = |frames: u64| {
        live.written.store(frames, Ordering::Release);
        !cancel.load(Ordering::Relaxed)
    };
    match target {
        AudioTarget::LiveWav => {
            // After the header `LiveAudio::create` wrote. Unbuffered: what `written` counts
            // is in the file (the sink reads it back).
            let mut file = std::fs::OpenOptions::new().append(true).open(&live.path).map_err(|e| format!("{}: {e}", live.path.display()))?;
            crate::wav::pcm(&mut file, &mut timeline, live.total_frames, &mut progress).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())
        }
        AudioTarget::Encoded { path, args } => {
            let mut encoder = crate::ffmpeg::tool()
                .args(["-v", "error", "-nostdin", "-y", "-f", "s16le"])
                .args(["-ar", &live.sample_rate.to_string(), "-ac", &live.channels.to_string(), "-i", "-"])
                .args(args)
                .arg(path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("could not run ffmpeg: {e}"))?;
            let mut stdin = encoder.stdin.take().ok_or("ffmpeg took no input")?;
            let written = crate::wav::pcm(&mut stdin, &mut timeline, live.total_frames, &mut progress);
            drop(stdin); // the end of the sound
            let out = encoder.wait_with_output().map_err(|e| e.to_string())?;
            written.map_err(|e| e.to_string())?;
            if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
        }
    }
}

/// The sound's encoding for a file of `extension` (muxed in by copying).
pub fn audio_codec(extension: &str) -> (Vec<String>, &'static str) {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match extension {
        "webm" => (v(&["-c:a", "libopus", "-b:a", "160k"]), "mka"),
        _ => (v(&["-c:a", "aac", "-b:a", "192k"]), "m4a"),
    }
}

/// The encoding of a sound-only export by its file's extension (`None`: a WAV, written
/// as it is).
pub fn sound_only_codec(extension: &str) -> Option<Vec<String>> {
    let v = |a: &[&str]| Some(a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    match extension {
        "m4a" | "aac" => v(&["-c:a", "aac", "-b:a", "256k"]),
        "mp3" => v(&["-c:a", "libmp3lame", "-q:a", "2"]),
        "flac" => v(&["-c:a", "flac"]),
        "opus" | "ogg" => v(&["-c:a", "libopus", "-b:a", "160k"]),
        _ => None,
    }
}

/// `path` with `.export-audio.<ext>` for its sound's temporary file.
pub fn temp_beside(path: &Path, ext: &str) -> PathBuf {
    path.with_extension(format!("export-audio.{ext}"))
}
