//! Encoding through an `ffmpeg` process.

use crate::{ExportError, VideoSink};
use oa_time::FrameRate;
use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
    /// Intra-only, fast to decode — good for round-tripping into another editor.
    ProRes,
    /// ProRes 4444 in a MOV: keeps transparency (an alpha channel), for overlays.
    ProRes4444,
    /// VP9 in WebM with an alpha channel: transparent video for the web.
    WebmAlpha,
    /// An animated GIF at `fps` frames per second (256 colors from a palette made for the
    /// whole clip, ordered dithering, looping, transparent where the picture is). No sound.
    Gif { fps: u32 },
}

impl VideoCodec {
    /// Keeps transparency: fed straight-alpha RGBA instead of NV12.
    pub fn has_alpha(self) -> bool {
        matches!(self, VideoCodec::ProRes4444 | VideoCodec::WebmAlpha | VideoCodec::Gif { .. })
    }

    fn args(self, crf: u32) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            VideoCodec::H264 => v(&["-c:v", "libx264", "-preset", "medium", "-crf", &crf.to_string()]),
            VideoCodec::Hevc => v(&["-c:v", "libx265", "-preset", "medium", "-crf", &crf.to_string()]),
            VideoCodec::ProRes => v(&["-c:v", "prores_ks", "-profile:v", "3"]),
            VideoCodec::ProRes4444 => v(&["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuva444p10le", "-alpha_bits", "16"]),
            VideoCodec::WebmAlpha => v(&["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", &(crf + 13).min(63).to_string(), "-row-mt", "1"]),
            VideoCodec::Gif { fps } => vec![
                "-vf".into(),
                format!(
                    "fps={},split[a][b];[a]palettegen=stats_mode=diff:reserve_transparent=1[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:alpha_threshold=128",
                    fps.clamp(1, 50)
                ),
                "-loop".into(),
                "0".into(),
            ],
        }
    }
}

pub struct FfmpegSink {
    child: Child,
    stdin: Option<ChildStdin>,
    output: std::path::PathBuf,
    rgba: bool,
}

impl FfmpegSink {
    pub fn start(
        out: &Path,
        size: [u32; 2],
        rate: FrameRate,
        codec: VideoCodec,
        crf: u32,
        audio: Option<&Path>,
    ) -> Result<Self, ExportError> {
        let rgba = codec.has_alpha();
        let gif = matches!(codec, VideoCodec::Gif { .. });
        let mut command = Command::new("ffmpeg");
        // No console window flashing up (and taking the keyboard) while it encodes.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        command
            .args(["-v", "error", "-nostdin", "-y"])
            // Raw frames arrive on stdin in exactly the format the GPU produced: NV12, or
            // straight-alpha sRGB RGBA for formats that keep transparency.
            .args(["-f", "rawvideo", "-pix_fmt", if rgba { "rgba" } else { "nv12" }])
            .args(["-s", &format!("{}x{}", size[0], size[1])])
            .args(["-r", &format!("{}/{}", rate.num, rate.den)])
            .args(["-i", "-"]);
        if let Some(audio) = audio.filter(|_| !gif) {
            command.arg("-i").arg(audio);
            let sound = if codec == VideoCodec::WebmAlpha { ["-c:a", "libopus", "-b:a", "160k"] } else { ["-c:a", "aac", "-b:a", "192k"] };
            command.args(sound).arg("-shortest");
        }
        command.args(codec.args(crf));
        match codec {
            VideoCodec::Gif { .. } => {}
            VideoCodec::ProRes4444 | VideoCodec::WebmAlpha => {
                command.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
            }
            _ => {
                // We convert with BT.709 limited range, so say so in the file: without
                // these tags players (and our own probe) guess by frame height.
                command
                    .args(["-pix_fmt", "yuv420p", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"])
                    .args(["-movflags", "+faststart"]);
            }
        }
        command.arg(out).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| ExportError::Encode(format!("could not run ffmpeg: {e}")))?;
        let stdin = child.stdin.take();
        Ok(FfmpegSink { child, stdin, output: out.to_path_buf(), rgba })
    }

    fn write(&mut self, parts: &[&[u8]]) -> Result<(), ExportError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| ExportError::Encode("encoder already closed".into()))?;
        for p in parts {
            stdin.write_all(p).map_err(|e| ExportError::Encode(format!("the encoder stopped reading ({e}); output: {}", self.output.display())))?;
        }
        Ok(())
    }
}

impl VideoSink for FfmpegSink {
    fn push_nv12(&mut self, luma: &[u8], chroma: &[u8]) -> Result<(), ExportError> {
        self.write(&[luma, chroma])
    }

    fn wants_rgba(&self) -> bool {
        self.rgba
    }

    fn push_rgba(&mut self, rgba: &[u8]) -> Result<(), ExportError> {
        self.write(&[rgba])
    }

    fn describe(&self) -> String {
        "ffmpeg (software)".into()
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        drop(self.stdin.take()); // EOF tells ffmpeg to flush and mux
        let output = self.child.wait_with_output().map_err(|e| ExportError::Encode(e.to_string()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(ExportError::Encode(String::from_utf8_lossy(&output.stderr).trim().to_string()))
        }
    }
}
