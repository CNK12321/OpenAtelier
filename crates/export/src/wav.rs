//! Rendering the timeline's sound to a WAV file for the muxer.
//!
//! Offline: no device, no real-time constraint, just pull until the sequence ends.

use oa_audio::AudioSource;
use oa_time::Time;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Writes 16-bit stereo PCM and returns how many seconds were written.
pub fn write(path: &Path, source: &mut dyn AudioSource, duration: Time) -> std::io::Result<f64> {
    let format = source.format();
    let total_frames = (duration.as_seconds_f64() * format.sample_rate as f64).ceil() as u64;
    let mut file = BufWriter::new(File::create(path)?);
    let data_bytes = total_frames * format.channels as u64 * 2;
    header(&mut file, format.sample_rate, format.channels, data_bytes as u32)?;

    let mut block = vec![0.0f32; 4096 * format.channels as usize];
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
        for sample in &block[..samples] {
            let clamped = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            file.write_all(&clamped.to_le_bytes())?;
        }
        written_frames += (samples / format.channels as usize) as u64;
        if n == 0 && source.finished() && written_frames >= total_frames {
            break;
        }
    }
    file.flush()?;
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
