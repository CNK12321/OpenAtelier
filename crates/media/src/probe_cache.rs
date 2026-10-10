//! Probes kept on disk, so a project's files aren't probed again every time it opens.
//!
//! A probe of a long video is a scan of every packet (its frame index): seconds for a
//! big file, and a project opens hundreds of them. Each finished probe is kept under
//! `<app dir>/cache/probes/<key>.json`, keyed by the file's path, size and when it was
//! last written — a changed file is a new key, so it's probed again. Unreadable or
//! outdated cache files are ignored (and overwritten).

use crate::{AudioTrack, FrameIndex, MediaProbe, VideoTrack};
use oa_gpu::{TransferFn, VideoColor, YuvMatrix};
use oa_time::{FrameRate, Rational, Time};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bump when a probe's contents change meaning (every cached probe is then made again).
const VERSION: u32 = 1;

fn dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("oa-test-probes-{}", std::process::id()));
    }
    crate::app_dir().join("cache").join("probes")
}

/// The cache's name for `path` as it is now: `None` if it can't be read.
pub(crate) fn key(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update(&meta.len().to_le_bytes());
    h.update(&modified.to_le_bytes());
    h.update(&VERSION.to_le_bytes());
    Some(format!("{:032x}", h.digest128()))
}

pub(crate) fn read(key: &str) -> Option<MediaProbe> {
    read_in(&dir(), key)
}

fn read_in(dir: &Path, key: &str) -> Option<MediaProbe> {
    let text = std::fs::read(dir.join(format!("{key}.json"))).ok()?;
    serde_json::from_slice::<Kept>(&text).ok().filter(|k| k.version == VERSION).map(Kept::into_probe)
}

pub(crate) fn write(key: &str, probe: &MediaProbe) {
    write_in(&dir(), key, probe);
    prune();
}

/// Keeps the cache under 256 MB: once a session (on its first write), the oldest probes
/// go until it's under 192 MB.
fn prune() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| {
            let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = std::fs::read_dir(dir())
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some((m.modified().ok()?, m.len(), e.path()))
                })
                .collect();
            let mut total: u64 = files.iter().map(|f| f.1).sum();
            if total < 256 << 20 {
                return;
            }
            files.sort();
            for (_, len, path) in files {
                if total < 192 << 20 {
                    break;
                }
                if std::fs::remove_file(&path).is_ok() {
                    total -= len;
                }
            }
        });
    });
}

/// Written aside, then moved into place, so a reader never sees half a file.
fn write_in(dir: &Path, key: &str, probe: &MediaProbe) {
    let _ = std::fs::create_dir_all(dir);
    let Ok(json) = serde_json::to_vec(&Kept::from_probe(probe)) else { return };
    let part = dir.join(format!("{key}.{}.part", std::process::id()));
    if std::fs::write(&part, json).is_ok() && std::fs::rename(&part, dir.join(format!("{key}.json"))).is_err() {
        let _ = std::fs::remove_file(&part);
    }
}

/// A probe as it's kept.
#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    container: String,
    /// Flicks.
    duration: i64,
    video: Option<KeptVideo>,
    audio: Option<(String, u32, u16)>,
}

#[derive(Serialize, Deserialize)]
struct KeptVideo {
    codec: String,
    pixel_format: String,
    size: [u32; 2],
    coded: [u32; 2],
    rotation: u32,
    time_base: Rational,
    avg_rate: Option<FrameRate>,
    /// Matrix (0 = BT.601, 1 = BT.709, 2 = BT.2020), full range, transfer (0 = SDR,
    /// 1 = linear).
    color: (u8, bool, u8),
    hdr: bool,
    transfer_tag: Option<String>,
    primaries_tag: Option<String>,
    has_alpha: bool,
    still: bool,
    pts: Vec<i64>,
    keyframes: Vec<usize>,
}

impl Kept {
    fn from_probe(p: &MediaProbe) -> Self {
        Kept {
            version: VERSION,
            container: p.container.clone(),
            duration: p.duration.0,
            video: p.video.as_ref().map(|v| KeptVideo {
                codec: v.codec.clone(),
                pixel_format: v.pixel_format.clone(),
                size: [v.width, v.height],
                coded: [v.coded_width, v.coded_height],
                rotation: v.rotation_quarter_turns,
                time_base: v.time_base,
                avg_rate: v.avg_rate,
                color: (
                    match v.color.matrix {
                        YuvMatrix::Bt601 => 0,
                        YuvMatrix::Bt709 => 1,
                        YuvMatrix::Bt2020 => 2,
                    },
                    v.color.full_range,
                    match v.color.transfer {
                        TransferFn::Sdr => 0,
                        TransferFn::Linear => 1,
                    },
                ),
                hdr: v.hdr,
                transfer_tag: v.transfer_tag.clone(),
                primaries_tag: v.primaries_tag.clone(),
                has_alpha: v.has_alpha,
                still: v.still,
                pts: v.index.pts.clone(),
                keyframes: v.index.keyframes.clone(),
            }),
            audio: p.audio.as_ref().map(|a| (a.codec.clone(), a.sample_rate, a.channels)),
        }
    }

    fn into_probe(self) -> MediaProbe {
        MediaProbe {
            container: self.container,
            duration: Time(self.duration),
            video: self.video.map(|v| VideoTrack {
                codec: v.codec,
                pixel_format: v.pixel_format,
                width: v.size[0],
                height: v.size[1],
                coded_width: v.coded[0],
                coded_height: v.coded[1],
                rotation_quarter_turns: v.rotation,
                time_base: v.time_base,
                avg_rate: v.avg_rate,
                color: VideoColor {
                    matrix: match v.color.0 {
                        0 => YuvMatrix::Bt601,
                        2 => YuvMatrix::Bt2020,
                        _ => YuvMatrix::Bt709,
                    },
                    full_range: v.color.1,
                    transfer: if v.color.2 == 1 { TransferFn::Linear } else { TransferFn::Sdr },
                },
                hdr: v.hdr,
                transfer_tag: v.transfer_tag,
                primaries_tag: v.primaries_tag,
                has_alpha: v.has_alpha,
                still: v.still,
                index: FrameIndex { time_base: v.time_base, pts: v.pts, keyframes: v.keyframes },
            }),
            audio: self.audio.map(|(codec, sample_rate, channels)| AudioTrack { codec, sample_rate, channels }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe comes back from the cache as it went in; a changed file is a new key.
    #[test]
    fn probes_come_back_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("oa-probe-cache-{}", std::process::id()));
        let probe = MediaProbe {
            container: "mov,mp4".into(),
            duration: Time::from_seconds(3),
            video: Some(VideoTrack {
                codec: "h264".into(),
                pixel_format: "yuv420p10le".into(),
                width: 1080,
                height: 1920,
                coded_width: 1920,
                coded_height: 1080,
                rotation_quarter_turns: 1,
                time_base: Rational::new(1, 30_000),
                avg_rate: Some(FrameRate::FPS_30),
                color: VideoColor { matrix: YuvMatrix::Bt2020, full_range: true, transfer: TransferFn::Sdr },
                hdr: true,
                transfer_tag: Some("arib-std-b67".into()),
                primaries_tag: Some("bt2020".into()),
                has_alpha: false,
                still: false,
                index: FrameIndex { time_base: Rational::new(1, 30_000), pts: vec![0, 1000, 2000, 3000], keyframes: vec![0, 2] },
            }),
            audio: Some(AudioTrack { codec: "aac".into(), sample_rate: 48_000, channels: 2 }),
        };
        write_in(&dir, "k", &probe);
        assert_eq!(read_in(&dir, "k"), Some(probe));
        assert_eq!(read_in(&dir, "missing"), None);

        let file = dir.join("clip.bin");
        std::fs::write(&file, b"one").unwrap();
        let before = key(&file).unwrap();
        std::fs::write(&file, b"longer").unwrap();
        assert_ne!(key(&file).unwrap(), before, "a changed file is probed again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
