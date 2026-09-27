//! Rendering the timeline's sound to a WAV file for the muxer.
//!
//! Offline: no device, no real-time constraint, just pull until the sequence ends.

use oa_audio::AudioSource;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// How many sample frames the sound of `frames` video frames at `rate` lasts: exact
/// (integer arithmetic on the frame boundaries), so the sound and the picture are the
/// same length to the sample — the muxer never cuts a last frame (`-shortest`) or leaves
/// sound running past it.
pub fn samples_for_frames(rate: oa_time::FrameRate, first: i64, frames: u64, sample_rate: u32) -> u64 {
    let span = rate.frame_start(first + frames as i64) - rate.frame_start(first);
    let samples = span.0 as i128 * sample_rate as i128;
    let flicks = oa_time::FLICKS_PER_SECOND as i128;
    ((samples + flicks / 2) / flicks) as u64
}

/// Writes 16-bit stereo PCM, `total_frames` sample frames of it, and returns how many
/// seconds were written.
pub fn write(path: &Path, source: &mut dyn AudioSource, total_frames: u64) -> std::io::Result<f64> {
    let mut file = BufWriter::new(File::create(path)?);
    let format = source.format();
    header(&mut file, format.sample_rate, format.channels, (total_frames * format.channels as u64 * 2) as u32)?;
    let seconds = pcm(&mut file, source, total_frames, &mut |_| true)?;
    file.flush()?;
    Ok(seconds)
}

/// The size of the header [`write`] puts before the samples.
#[cfg_attr(not(windows), allow(dead_code))] // read by the Media Foundation sink
pub const HEADER_BYTES: u64 = 44;

/// A WAV header for `total_frames` frames of 16-bit PCM.
pub fn write_header(out: &mut impl Write, sample_rate: u32, channels: u16, total_frames: u64) -> std::io::Result<()> {
    header(out, sample_rate, channels, (total_frames * channels as u64 * 2) as u32)
}

/// Pulls `total_frames` frames from `source` into `out` as 16-bit little-endian PCM, in
/// blocks, telling `progress` how many frames are out so far after each (it returns
/// false to stop: a canceled export). Returns the seconds written.
pub fn pcm(out: &mut impl Write, source: &mut dyn AudioSource, total_frames: u64, progress: &mut dyn FnMut(u64) -> bool) -> std::io::Result<f64> {
    let format = source.format();
    let mut block = vec![0.0f32; 4096 * format.channels as usize];
    let mut bytes = Vec::with_capacity(block.len() * 2);
    let mut written_frames = 0u64;
    while written_frames < total_frames {
        let wanted = ((total_frames - written_frames) as usize * format.channels as usize).min(block.len());
        let n = source.read(&mut block[..wanted]);
        let samples = if n == 0 {
            // Nothing left to decode: pad with silence so audio and video stay the same
            // length (the clock does the same thing during playback).
            block[..wanted].fill(0.0);
            wanted
        } else {
            n
        };
        bytes.clear();
        for sample in &block[..samples] {
            // Rounded to the nearest step (truncating biased every sample toward zero).
            let clamped = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            bytes.extend_from_slice(&clamped.to_le_bytes());
        }
        out.write_all(&bytes)?;
        written_frames += (samples / format.channels as usize) as u64;
        if !progress(written_frames) {
            break;
        }
        if n == 0 && source.finished() && written_frames >= total_frames {
            break;
        }
    }
    Ok(written_frames as f64 / format.sample_rate as f64)
}

fn header(file: &mut impl Write, sample_rate: u32, channels: u16, data_bytes: u32) -> std::io::Result<()> {
    let block_align = channels * 2;
    let byte_rate = sample_rate * block_align as u32;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // PCM chunk size
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::samples_for_frames;
    use oa_time::FrameRate;

    /// The sound of N frames is exactly their length in samples — at 29.97 a frame is
    /// 1601.6 samples, so the count comes from the frame boundaries, not N × a rounded
    /// frame — and a range starting later gets the same span.
    #[test]
    fn sound_lasts_exactly_as_long_as_the_frames() {
        assert_eq!(samples_for_frames(FrameRate::FPS_30, 0, 90, 48_000), 144_000);
        assert_eq!(samples_for_frames(FrameRate::FPS_29_97, 0, 90, 48_000), 144_144);
        assert_eq!(samples_for_frames(FrameRate::FPS_29_97, 0, 1, 48_000), 1602);
        assert_eq!(samples_for_frames(FrameRate::FPS_24, 100, 24, 44_100), 44_100);
    }
}
