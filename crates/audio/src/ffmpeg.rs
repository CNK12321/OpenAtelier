//! Audio decoding through an `ffmpeg` process.
//!
//! Audio is small and lives on the CPU anyway, so this trades a process boundary for not
//! writing another platform decoder. It sits behind [`AudioSource`], so a Media
//! Foundation / CoreAudio / libav decoder can replace it without touching the engine.

use crate::{AudioError, AudioFormat, AudioSource};
use oa_time::Time;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

pub struct FfmpegAudioSource {
    path: PathBuf,
    format: AudioFormat,
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    /// Leftover bytes of a sample that spanned two reads.
    partial: Vec<u8>,
    finished: bool,
}

impl FfmpegAudioSource {
    /// Decodes `path` to interleaved `f32` at exactly `format`'s rate and channel count.
    pub fn open(path: &Path, format: AudioFormat) -> Result<Self, AudioError> {
        let mut source = FfmpegAudioSource {
            path: path.to_path_buf(),
            format,
            child: None,
            stdout: None,
            partial: Vec::new(),
            finished: false,
        };
        source.spawn(Time::ZERO)?;
        Ok(source)
    }

    fn spawn(&mut self, from: Time) -> Result<(), AudioError> {
        self.stop();
        let mut command = Command::new("ffmpeg");
        // No console window flashing up (and taking the keyboard) on every seek.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        command.args(["-v", "error", "-nostdin"]);
        if from > Time::ZERO {
            command.args(["-ss", &format!("{:.6}", from.as_seconds_f64())]);
        }
        command
            .arg("-i")
            .arg(&self.path)
            .args(["-map", "0:a:0?", "-vn", "-f", "f32le"])
            .args(["-ar", &self.format.sample_rate.to_string()])
            .args(["-ac", &self.format.channels.to_string()])
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let mut child = command.spawn().map_err(|e| AudioError::Decode(format!("could not run ffmpeg: {e}")))?;
        self.stdout = child.stdout.take();
        self.child = Some(child);
        self.partial.clear();
        self.finished = false;
        Ok(())
    }

    fn stop(&mut self) {
        self.stdout = None; // closing the pipe makes ffmpeg exit
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for FfmpegAudioSource {
    fn drop(&mut self) {
        self.stop();
    }
}

impl AudioSource for FfmpegAudioSource {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn read(&mut self, out: &mut [f32]) -> usize {
        let Some(stdout) = self.stdout.as_mut() else { return 0 };
        let mut bytes = std::mem::take(&mut self.partial);
        let wanted = out.len() * 4;
        let mut buffer = vec![0u8; wanted.saturating_sub(bytes.len())];
        while bytes.len() < wanted {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    self.finished = true;
                    break;
                }
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(_) => {
                    self.finished = true;
                    break;
                }
            }
        }
        let samples = (bytes.len() / 4).min(out.len());
        for (i, slot) in out[..samples].iter_mut().enumerate() {
            let b = &bytes[i * 4..i * 4 + 4];
            *slot = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        self.partial = bytes.split_off(samples * 4);
        samples
    }

    fn seek(&mut self, t: Time) -> Result<(), AudioError> {
        self.spawn(t.max(Time::ZERO))
    }

    fn finished(&self) -> bool {
        self.finished && self.partial.len() < 4
    }
}
