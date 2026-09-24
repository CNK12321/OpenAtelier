//! Media probing and frame indexing via `ffprobe` (metadata only — no pixels).

use crate::MediaError;
use oa_gpu::{TransferFn, VideoColor, YuvMatrix};
use oa_time::{select_frame, FrameRate, Rational, Time};
use serde::Deserialize;
use std::path::Path;

/// What a file contains. A file may have video, audio, or both; stills and animated
/// images are video tracks with their own quirks.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaProbe {
    pub container: String,
    pub duration: Time,
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoTrack {
    pub codec: String,
    pub pixel_format: String,
    /// Size as displayed, after rotation metadata.
    pub width: u32,
    pub height: u32,
    /// Picture size as stored (before rotation).
    pub coded_width: u32,
    pub coded_height: u32,
    /// Clockwise display rotation in quarter turns.
    pub rotation_quarter_turns: u32,
    pub time_base: Rational,
    pub avg_rate: Option<FrameRate>,
    pub color: VideoColor,
    /// HDR transfer (PQ/HLG). Its curve is converted (the input transform), but the
    /// decoder still hands over 8 bits, so smooth gradients can band.
    pub hdr: bool,
    /// ffprobe's names for the tagged transfer and primaries ("smpte2084", "bt2020"…).
    pub transfer_tag: Option<String>,
    pub primaries_tag: Option<String>,
    /// The pixel format carries alpha (PNG, ProRes 4444, …).
    pub has_alpha: bool,
    /// A single-frame image: it has no inherent duration, so a clip using it can be any
    /// length.
    pub still: bool,
    pub index: FrameIndex,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl MediaProbe {
    pub fn has_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// Size of the picture, or `None` for audio-only files.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.video.as_ref().map(|v| [v.width, v.height])
    }

    pub fn is_still(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.still)
    }

    /// One line for lists and tooltips.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = &self.video {
            let kind = if v.still { "still" } else { "video" };
            parts.push(format!("{kind} {}x{} {}", v.width, v.height, v.codec));
            if let Some(rate) = v.avg_rate.filter(|_| !v.still) {
                parts.push(format!("{:.4} fps", rate.as_f64()));
            }
        }
        if let Some(a) = &self.audio {
            parts.push(format!("audio {} {} ch {} Hz", a.codec, a.channels, a.sample_rate));
        }
        if self.duration > Time::ZERO {
            parts.push(format!("{:.2}s", self.duration.as_seconds_f64()));
        }
        parts.join(", ")
    }
}

/// Presentation timestamps of every video frame, in presentation order.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameIndex {
    pub time_base: Rational,
    pub pts: Vec<i64>,
    /// Indices (into `pts`) of keyframes, ascending.
    pub keyframes: Vec<usize>,
}

impl FrameIndex {
    pub fn len(&self) -> usize {
        self.pts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }

    /// Frame shown at source time `t` — via the project-wide frame-selection rule.
    pub fn frame_at(&self, t: Time) -> Option<usize> {
        select_frame(&self.pts, self.time_base, t.as_rational())
    }

    /// Exact presentation time of frame `i`, rounded toward −∞ to flicks.
    pub fn time_of(&self, i: usize) -> Time {
        Time::from_rational_floor(Rational::new(self.pts[i], 1) * self.time_base)
    }

    /// The last keyframe at or before frame `i`.
    pub fn keyframe_before(&self, i: usize) -> usize {
        let k = self.keyframes.partition_point(|&k| k <= i);
        k.checked_sub(1).map_or(0, |k| self.keyframes[k])
    }

    /// Largest distance between keyframes (GOP length).
    pub fn max_gop(&self) -> usize {
        self.keyframes.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(self.pts.len())
    }

    /// True when frame durations differ by more than 1%.
    pub fn is_variable_rate(&self) -> bool {
        let d: Vec<i64> = self.pts.windows(2).map(|w| w[1] - w[0]).collect();
        match (d.iter().min(), d.iter().max()) {
            (Some(&lo), Some(&hi)) if lo > 0 => (hi - lo) as f64 / lo as f64 > 0.01,
            _ => false,
        }
    }
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    packets: Vec<Packet>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    pix_fmt: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    time_base: Option<String>,
    avg_frame_rate: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

#[derive(Deserialize)]
struct SideData {
    rotation: Option<f64>,
}

#[derive(Deserialize)]
struct Packet {
    pts: Option<i64>,
    flags: Option<String>,
}

#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
    format_name: Option<String>,
}

/// Codecs that are single images rather than moving pictures.
const STILL_CODECS: &[&str] = &["mjpeg", "png", "bmp", "webp", "tiff", "targa", "ppm", "jpegls"];

fn ratio(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once('/')?;
    let (a, b) = (a.parse().ok()?, b.parse().ok()?);
    (b != 0 && a != 0).then_some((a, b))
}

fn run(args: &[&str], path: &Path) -> Result<Probe, MediaError> {
    let out = crate::tool("ffprobe")
        .args(["-v", "error", "-of", "json"])
        .args(args)
        .arg(path)
        .output()
        .map_err(|e| MediaError::Probe(format!("could not run ffprobe: {e}")))?;
    if !out.status.success() {
        return Err(MediaError::Probe(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| MediaError::Probe(format!("unexpected ffprobe output: {e}")))
}

pub fn probe(path: &Path) -> Result<MediaProbe, MediaError> {
    let all = run(
        &[
            "-show_entries",
            "stream=codec_type,codec_name,pix_fmt,width,height,time_base,avg_frame_rate,color_range,color_space,color_transfer,color_primaries,sample_rate,channels:stream_side_data=rotation:format=duration,format_name",
        ],
        path,
    )?;
    let format = all.format.unwrap_or(Format { duration: None, format_name: None });
    let container = format.format_name.unwrap_or_default();
    let duration = format
        .duration
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(Time::from_seconds_f64)
        .unwrap_or(Time::ZERO);

    let audio = all.streams.iter().find(|s| s.codec_type.as_deref() == Some("audio")).map(|s| AudioTrack {
        codec: s.codec_name.clone().unwrap_or_default(),
        sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()).unwrap_or(48_000),
        channels: s.channels.unwrap_or(2),
    });

    let video = match all.streams.into_iter().find(|s| s.codec_type.as_deref() == Some("video")) {
        Some(s) => Some(video_track(path, s)?),
        None => None,
    };
    if video.is_none() && audio.is_none() {
        return Err(MediaError::Probe("file has no audio or video".into()));
    }
    Ok(MediaProbe { container, duration, video, audio })
}

fn video_track(path: &Path, s: Stream) -> Result<VideoTrack, MediaError> {
    let (tb_num, tb_den) = s.time_base.as_deref().and_then(ratio).unwrap_or((1, 1000));
    let time_base = Rational::new(tb_num, tb_den);
    let codec = s.codec_name.clone().unwrap_or_default();
    let pixel_format = s.pix_fmt.clone().unwrap_or_default();
    let still = STILL_CODECS.contains(&codec.as_str());

    // Stills need no packet scan; everything else gets a full frame index.
    let index = if still {
        FrameIndex { time_base, pts: vec![0], keyframes: vec![0] }
    } else {
        let scanned = run(&["-select_streams", "v:0", "-show_entries", "packet=pts,flags"], path)?;
        let mut frames: Vec<(i64, bool)> = scanned
            .packets
            .iter()
            .filter_map(|p| Some((p.pts?, p.flags.as_deref().is_some_and(|f| f.starts_with('K')))))
            .collect();
        frames.sort_by_key(|f| f.0);
        frames.dedup_by_key(|f| f.0);
        if frames.is_empty() {
            return Err(MediaError::Probe("video stream has no timestamped frames".into()));
        }
        let keyframes: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| f.1).map(|(i, _)| i).collect();
        FrameIndex {
            time_base,
            pts: frames.iter().map(|f| f.0).collect(),
            keyframes: if keyframes.is_empty() { vec![0] } else { keyframes },
        }
    };

    let (coded_width, coded_height) = (s.width.unwrap_or(0), s.height.unwrap_or(0));
    // FFmpeg reports the display matrix angle counter-clockwise; we store clockwise turns.
    let degrees = s.side_data_list.iter().find_map(|d| d.rotation).unwrap_or(0.0);
    let rotation_quarter_turns = (((-degrees / 90.0).round() as i64).rem_euclid(4)) as u32;
    let (width, height) = match rotation_quarter_turns {
        1 | 3 => (coded_height, coded_width),
        _ => (coded_width, coded_height),
    };
    let mut color = VideoColor::guess(height);
    match s.color_space.as_deref() {
        Some("bt709") => color.matrix = YuvMatrix::Bt709,
        Some("smpte170m" | "bt470bg") => color.matrix = YuvMatrix::Bt601,
        Some("bt2020nc" | "bt2020c") => color.matrix = YuvMatrix::Bt2020,
        _ => {}
    }
    // `yuvj*` formats and RGB images are full range.
    color.full_range = s.color_range.as_deref() == Some("pc") || pixel_format.starts_with("yuvj") || pixel_format.contains("rgb");
    color.transfer = TransferFn::Sdr;

    Ok(VideoTrack {
        codec,
        has_alpha: pixel_format.contains('a') && !pixel_format.starts_with("ya") || pixel_format.contains("rgba") || pixel_format.contains("bgra"),
        pixel_format,
        width,
        height,
        coded_width,
        coded_height,
        rotation_quarter_turns,
        time_base,
        avg_rate: s.avg_frame_rate.as_deref().and_then(ratio).map(|(n, d)| FrameRate::new(n as u32, d as u32)),
        color,
        hdr: matches!(s.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67")),
        transfer_tag: s.color_transfer.clone().filter(|t| t != "unknown"),
        primaries_tag: s.color_primaries.clone().filter(|p| p != "unknown"),
        still,
        index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(pts: &[i64], keys: &[usize]) -> FrameIndex {
        FrameIndex { time_base: Rational::new(1, 15360), pts: pts.to_vec(), keyframes: keys.to_vec() }
    }

    #[test]
    fn keyframes_and_gop() {
        let idx = index(&(0..100).map(|i| i * 512).collect::<Vec<_>>(), &[0, 30, 60, 90]);
        assert_eq!(idx.keyframe_before(0), 0);
        assert_eq!(idx.keyframe_before(29), 0);
        assert_eq!(idx.keyframe_before(30), 30);
        assert_eq!(idx.keyframe_before(99), 90);
        assert_eq!(idx.max_gop(), 30);
        assert!(!idx.is_variable_rate());
        assert_eq!(idx.frame_at(idx.time_of(42)), Some(42));
        assert_eq!(idx.frame_at(idx.time_of(42) - Time(1)), Some(41));
    }

    #[test]
    fn variable_rate_detection() {
        assert!(index(&[0, 512, 1100, 1536], &[0]).is_variable_rate());
    }
}
