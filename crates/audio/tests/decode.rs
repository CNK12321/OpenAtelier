//! Audio decoding and seeking, checked against a generated tone whose amplitude ramps
//! up over time — so the loudness of a block tells you *where* in the file it came from.
//!
//! Needs `ffmpeg` on PATH; skips otherwise. No audio device is involved.

use oa_audio::{AudioFormat, AudioSource, FfmpegAudioSource};
use oa_time::Time;
use std::path::PathBuf;
use std::process::Command;

const SECONDS: f64 = 6.0;

fn tone() -> Option<PathBuf> {
    if !Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: ffmpeg not on PATH");
        return None;
    }
    let dir = std::env::temp_dir().join("oa-media-tests");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("ramp_tone.wav");
    if !path.exists() {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=0.9*(t/{SECONDS})*sin(2*PI*440*t):d={SECONDS}:s=48000"))
            .arg(&path)
            .output()
            .ok()?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    Some(path)
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

/// Reads `seconds` worth of audio, retrying while the process warms up.
fn read_block(source: &mut FfmpegAudioSource, seconds: f64) -> Vec<f32> {
    let format = source.format();
    let wanted = (seconds * format.sample_rate as f64) as usize * format.channels as usize;
    let mut out = vec![0.0; wanted];
    let mut filled = 0;
    while filled < wanted {
        let n = source.read(&mut out[filled..]);
        filled += n;
        if n == 0 {
            break;
        }
    }
    out.truncate(filled);
    out
}

#[test]
fn decodes_at_the_requested_format() {
    let Some(path) = tone() else { return };
    let format = AudioFormat::stereo_48k();
    let mut source = FfmpegAudioSource::open(&path, format).expect("open");
    assert_eq!(source.format(), format);

    let block = read_block(&mut source, 0.5);
    assert_eq!(block.len(), 48_000 / 2 * 2, "half a second of stereo samples");
    assert!(rms(&block) > 0.001, "should not be silence");
}

#[test]
fn seeking_lands_where_it_was_asked_to() {
    let Some(path) = tone() else { return };
    let mut source = FfmpegAudioSource::open(&path, AudioFormat::stereo_48k()).expect("open");

    // The tone ramps from silence to full over the file, so loudness ≈ position.
    let early = rms(&read_block(&mut source, 0.2));
    source.seek(Time::from_seconds_f64(5.0)).expect("seek");
    let late = rms(&read_block(&mut source, 0.2));
    source.seek(Time::from_seconds_f64(2.5)).expect("seek back");
    let middle = rms(&read_block(&mut source, 0.2));

    // Expected amplitudes: ~0.1/6, ~5/6, ~2.5/6 of full scale.
    assert!(late > middle && middle > early, "early {early:.3}, middle {middle:.3}, late {late:.3}");
    let ratio = late / middle;
    assert!((1.5..2.5).contains(&ratio), "5s should be about twice as loud as 2.5s, got {ratio:.2}");
}

#[test]
fn reports_the_end_of_the_file() {
    let Some(path) = tone() else { return };
    let mut source = FfmpegAudioSource::open(&path, AudioFormat::stereo_48k()).expect("open");
    source.seek(Time::from_seconds_f64(SECONDS - 0.3)).expect("seek");
    let mut total = 0;
    let mut block = vec![0.0; 4096];
    for _ in 0..500 {
        let n = source.read(&mut block);
        total += n;
        if n == 0 && source.finished() {
            break;
        }
    }
    assert!(source.finished(), "must report the end");
    let seconds = total as f64 / (48_000.0 * 2.0);
    assert!((seconds - 0.3).abs() < 0.05, "expected ~0.3s of tail, got {seconds:.3}s");
}
