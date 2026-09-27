//! Encoding through an `ffmpeg` process.

use crate::mixdown::AudioTarget;
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
    /// One PNG per frame (`name_00000.png`, `name_00001.png`… beside the path asked for),
    /// transparency kept: for compositing elsewhere. No sound.
    PngSequence,
    /// The sound alone, no picture: WAV, or encoded by the file's extension (M4A, MP3,
    /// FLAC, Opus). See [`audio_extensions`].
    Audio,
}

/// File types the sound alone can be exported as.
pub fn audio_extensions() -> &'static [&'static str] {
    &["wav", "m4a", "mp3", "flac", "opus"]
}

impl VideoCodec {
    /// Keeps transparency: fed straight-alpha RGBA instead of NV12.
    pub fn has_alpha(self) -> bool {
        matches!(self, VideoCodec::ProRes4444 | VideoCodec::WebmAlpha | VideoCodec::Gif { .. } | VideoCodec::PngSequence)
    }

    /// Has sound: everything but GIFs and image sequences.
    pub fn has_sound(self) -> bool {
        !matches!(self, VideoCodec::Gif { .. } | VideoCodec::PngSequence)
    }

    /// For an image sequence, the numbered file pattern ffmpeg writes to (`x.png` →
    /// `x_%05d.png`); the frames are numbered from 0.
    pub fn sequence_pattern(self, out: &Path) -> Option<std::path::PathBuf> {
        (self == VideoCodec::PngSequence).then(|| {
            let stem = out.file_stem().map_or_else(|| "frame".to_string(), |s| s.to_string_lossy().to_string());
            out.with_file_name(format!("{stem}_%05d.png"))
        })
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
            VideoCodec::PngSequence => v(&["-c:v", "png", "-pix_fmt", "rgba", "-start_number", "0", "-f", "image2"]),
            VideoCodec::Audio => Vec::new(),
        }
    }
}

/// A GPU's own H.264/HEVC encoder, reached through ffmpeg: how Linux (and macOS) export
/// on the graphics card, and Windows' second choice after Media Foundation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HwEncoder {
    /// NVIDIA.
    Nvenc,
    /// Intel and AMD on Linux (Mesa / intel-media-driver).
    Vaapi,
    /// Intel Quick Sync.
    Qsv,
    /// AMD on Windows.
    Amf,
    /// Apple.
    VideoToolbox,
}

/// The render node VA-API encodes on (the first GPU).
const VAAPI_DEVICE: &str = "/dev/dri/renderD128";

impl HwEncoder {
    /// The ones worth trying on this platform, in order.
    fn candidates() -> &'static [HwEncoder] {
        if cfg!(target_os = "macos") {
            &[HwEncoder::VideoToolbox]
        } else if cfg!(windows) {
            &[HwEncoder::Nvenc, HwEncoder::Qsv, HwEncoder::Amf]
        } else {
            &[HwEncoder::Nvenc, HwEncoder::Vaapi, HwEncoder::Qsv]
        }
    }

    fn label(self) -> &'static str {
        match self {
            HwEncoder::Nvenc => "NVENC",
            HwEncoder::Vaapi => "VA-API",
            HwEncoder::Qsv => "Quick Sync",
            HwEncoder::Amf => "AMF",
            HwEncoder::VideoToolbox => "VideoToolbox",
        }
    }

    /// Options that go before the inputs.
    fn global_args(self) -> Vec<String> {
        match self {
            HwEncoder::Vaapi => vec!["-vaapi_device".into(), VAAPI_DEVICE.into()],
            _ => Vec::new(),
        }
    }

    /// The output options encoding `codec` (H.264 or HEVC only) at about `bitrate`
    /// bits per second. Frames arrive as NV12, which all of them take as is (VA-API after
    /// an upload to the GPU).
    fn encode_args(self, codec: VideoCodec, bitrate: u64) -> Option<Vec<String>> {
        let hevc = match codec {
            VideoCodec::H264 => false,
            VideoCodec::Hevc => true,
            _ => return None,
        };
        let name = match self {
            HwEncoder::Nvenc => "nvenc",
            HwEncoder::Vaapi => "vaapi",
            HwEncoder::Qsv => "qsv",
            HwEncoder::Amf => "amf",
            HwEncoder::VideoToolbox => "videotoolbox",
        };
        let mut args: Vec<String> = Vec::new();
        if self == HwEncoder::Vaapi {
            args.extend(["-vf".into(), "format=nv12,hwupload".into()]);
        }
        let bitrate = bitrate.max(100_000);
        args.extend([
            "-c:v".into(),
            format!("{}_{name}", if hevc { "hevc" } else { "h264" }),
            "-b:v".into(),
            bitrate.to_string(),
            "-maxrate".into(),
            (bitrate * 3 / 2).to_string(),
            "-bufsize".into(),
            (bitrate * 2).to_string(),
        ]);
        if hevc {
            // Plays in QuickTime and on Apple devices.
            args.extend(["-tag:v".into(), "hvc1".into()]);
        }
        Some(args)
    }

    /// Whether ffmpeg has this encoder and the GPU/driver can actually encode `codec` at
    /// `size` with it — a few blank frames, encoded and thrown away. Remembered per run.
    fn works(self, codec: VideoCodec, size: [u32; 2]) -> bool {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        /// (encoder, HEVC, size) → whether it worked.
        type Tried = HashMap<(HwEncoder, bool, [u32; 2]), bool>;
        static TRIED: OnceLock<Mutex<Tried>> = OnceLock::new();
        let key = (self, codec == VideoCodec::Hevc, size);
        if let Some(known) = TRIED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return *known;
        }
        let Some(encode) = self.encode_args(codec, 2_000_000) else { return false };
        if self == HwEncoder::Vaapi && !Path::new(VAAPI_DEVICE).exists() {
            return false;
        }
        let mut command = tool();
        command
            .args(["-v", "error", "-nostdin"])
            .args(self.global_args())
            .args(["-f", "lavfi", "-i", &format!("color=c=black:s={}x{}:r=30:d=0.2,format=nv12", size[0], size[1])])
            .args(encode)
            .args(["-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let works = command.spawn().is_ok_and(|mut child| {
            // A driver that hangs counts as not working.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break false;
                    }
                }
            }
        });
        TRIED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(key, works);
        works
    }
}

/// The first GPU encoder that can take `codec` at `size` here, if any.
pub fn find_hardware(codec: VideoCodec, size: [u32; 2]) -> Option<HwEncoder> {
    HwEncoder::candidates().iter().copied().find(|e| e.works(codec, size))
}

/// `ffmpeg`, without a console window flashing up (and taking the keyboard) on Windows.
pub(crate) fn tool() -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

pub struct FfmpegSink {
    child: Child,
    stdin: Option<ChildStdin>,
    output: std::path::PathBuf,
    /// Where the picture is encoded: `output`, or a file beside it when there's sound.
    video: std::path::PathBuf,
    /// The sound's encoded file and its codec's options, when there's sound.
    sound: Option<(std::path::PathBuf, Vec<String>)>,
    /// MP4/MOV: the index goes at the front.
    faststart: bool,
    rate: FrameRate,
    /// Frames handed over so far.
    frames: u64,
    rgba: bool,
    /// The GPU encoder used, or `None` for software.
    hardware: Option<HwEncoder>,
}

impl FfmpegSink {
    pub fn start(
        out: &Path,
        size: [u32; 2],
        rate: FrameRate,
        codec: VideoCodec,
        crf: u32,
        audio: bool,
    ) -> Result<Self, ExportError> {
        Self::start_with(out, size, rate, codec, crf, audio, None)
    }

    /// Like [`FfmpegSink::start`], encoding H.264/HEVC on the GPU with `hardware` at about
    /// `bitrate` bits per second.
    pub fn start_hardware(out: &Path, size: [u32; 2], rate: FrameRate, codec: VideoCodec, audio: bool, hardware: HwEncoder, bitrate: u64) -> Result<Self, ExportError> {
        Self::start_with(out, size, rate, codec, 0, audio, Some((hardware, bitrate)))
    }

    /// `audio`: sound will be mixed for it (see [`VideoSink::audio_target`]).
    fn start_with(out: &Path, size: [u32; 2], rate: FrameRate, codec: VideoCodec, crf: u32, audio: bool, hardware: Option<(HwEncoder, u64)>) -> Result<Self, ExportError> {
        if codec == VideoCodec::Audio {
            return Err(ExportError::Encode("the sound alone has no picture to encode".into()));
        }
        let rgba = codec.has_alpha();
        let encode = match hardware {
            Some((hw, bitrate)) => hw.encode_args(codec, bitrate).ok_or_else(|| ExportError::Encode(format!("{} can't encode {codec:?}", hw.label())))?,
            None => codec.args(crf),
        };
        let mut command = tool();
        command.args(["-v", "error", "-nostdin", "-y"]);
        if let Some((hw, _)) = hardware {
            command.args(hw.global_args());
        }
        command
            // Raw frames arrive on stdin in exactly the format the GPU produced: NV12, or
            // straight-alpha sRGB RGBA for formats that keep transparency.
            .args(["-f", "rawvideo", "-pix_fmt", if rgba { "rgba" } else { "nv12" }])
            .args(["-s", &format!("{}x{}", size[0], size[1])])
            .args(["-r", &format!("{}/{}", rate.num, rate.den)])
            .args(["-i", "-"]);
        // With sound, the picture goes to a file of its own and the sound is encoded beside
        // it as it's mixed (see mixdown.rs); `finish` puts the two together.
        let extension = out.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_ascii_lowercase();
        let with_sound = audio && codec.has_sound();
        let video = match codec.sequence_pattern(out) {
            Some(pattern) => pattern,
            None if with_sound => out.with_extension(format!("export-video.{extension}")),
            None => out.to_path_buf(),
        };
        let sound = with_sound.then(|| {
            let (args, ext) = crate::mixdown::audio_codec(&extension);
            (crate::mixdown::temp_beside(out, ext), args)
        });
        command.args(encode);
        match codec {
            VideoCodec::Gif { .. } | VideoCodec::PngSequence | VideoCodec::Audio => {}
            VideoCodec::ProRes4444 | VideoCodec::WebmAlpha => {
                command.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
            }
            _ => {
                // Software encoders get 4:2:0 planar; GPU encoders take the NV12 as is.
                if hardware.is_none() {
                    command.args(["-pix_fmt", "yuv420p"]);
                }
                // We convert with BT.709 limited range, so say so in the file: without
                // these tags players (and our own probe) guess by frame height.
                command.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]);
                // The index at the front, for streaming — done by the final mux instead
                // when there's sound (the same rewrite, once).
                if !with_sound {
                    command.args(["-movflags", "+faststart"]);
                }
            }
        }
        command.arg(&video).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| ExportError::Encode(format!("could not run ffmpeg: {e}")))?;
        let stdin = child.stdin.take();
        let faststart = matches!(codec, VideoCodec::H264 | VideoCodec::Hevc);
        Ok(FfmpegSink { child, stdin, output: out.to_path_buf(), video, sound, faststart, rate, frames: 0, rgba, hardware: hardware.map(|h| h.0) })
    }

    /// Puts the picture and the sound together into the output, copying both.
    fn mux(&self, sound: &Path) -> Result<(), ExportError> {
        // As long as the picture: the sound is exactly that long, unless the export was
        // stopped early (the mix may have got further). Cut on the frame count, which
        // keeps every packet that starts before the end.
        let seconds = self.frames as f64 * self.rate.den as f64 / self.rate.num.max(1) as f64;
        let mut command = tool();
        command
            .args(["-v", "error", "-nostdin", "-y"])
            .arg("-i")
            .arg(&self.video)
            .arg("-i")
            .arg(sound)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-t", &format!("{seconds:.6}")]);
        if self.faststart {
            command.args(["-movflags", "+faststart"]);
        }
        let output = command
            .arg(&self.output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map_err(|e| ExportError::Encode(format!("could not run ffmpeg: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(ExportError::Encode(format!("putting the sound in: {}", String::from_utf8_lossy(&output.stderr).trim())))
        }
    }

    fn write(&mut self, parts: &[&[u8]]) -> Result<(), ExportError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| ExportError::Encode("encoder already closed".into()))?;
        for p in parts {
            stdin.write_all(p).map_err(|e| ExportError::Encode(format!("the encoder stopped reading ({e}); output: {}", self.output.display())))?;
        }
        Ok(())
    }
}

impl Drop for FfmpegSink {
    /// Not finished (a canceled export): the picture's and the sound's own files go.
    fn drop(&mut self) {
        if let Some((sound, _)) = self.sound.take() {
            drop(self.stdin.take());
            let _ = self.child.wait();
            let _ = std::fs::remove_file(&sound);
            let _ = std::fs::remove_file(&self.video);
        }
    }
}

impl VideoSink for FfmpegSink {
    fn push_nv12(&mut self, luma: &[u8], chroma: &[u8]) -> Result<(), ExportError> {
        self.write(&[luma, chroma])?;
        self.frames += 1;
        Ok(())
    }

    fn wants_rgba(&self) -> bool {
        self.rgba
    }

    fn push_rgba(&mut self, rgba: &[u8]) -> Result<(), ExportError> {
        self.write(&[rgba])?;
        self.frames += 1;
        Ok(())
    }

    fn audio_target(&self) -> Option<AudioTarget> {
        self.sound.as_ref().map(|(path, args)| AudioTarget::Encoded { path: path.clone(), args: args.clone() })
    }

    fn describe(&self) -> String {
        match self.hardware {
            Some(hw) => format!("ffmpeg ({}, hardware)", hw.label()),
            None => "ffmpeg (software)".into(),
        }
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        drop(self.stdin.take()); // EOF tells ffmpeg to flush and mux
        let mut errors = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut errors);
        }
        let status = self.child.wait().map_err(|e| ExportError::Encode(e.to_string()))?;
        if !status.success() {
            return Err(ExportError::Encode(errors.trim().to_string()));
        }
        // The sound was finished before this was called (the exporter waits for the mix).
        let Some((sound, _)) = self.sound.take() else { return Ok(()) };
        let muxed = self.mux(&sound);
        let _ = std::fs::remove_file(&sound);
        let _ = std::fs::remove_file(&self.video);
        muxed
    }
}
