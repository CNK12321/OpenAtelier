//! A portable decoder: an `ffmpeg` process decodes the file and streams NV12 frames,
//! which are uploaded into two plain textures (R8 luma, RG8 chroma) that every GPU
//! backend can sample — Vulkan, Metal, OpenGL and DX12 alike. It is the decoder on Linux
//! and macOS, and on Windows it takes over whenever Media Foundation can't (no DX12
//! device to share frames with, or a codec the system can't hardware-decode).
//!
//! ffmpeg itself uses the platform's hardware decoder when it can (`-hwaccel auto`: VA-API,
//! NVDEC, D3D11VA, VideoToolbox) and the CPU otherwise. Each frame's exact presentation
//! time comes from the `showinfo` filter on stderr, in the same order as the frames on
//! stdout; `-copyts` keeps the file's own timestamps, so the frame index lines up.

use crate::source::{Lease, Surface, VideoDecoder};
use crate::{MediaError, VideoTrack};
use oa_gpu::VideoColor;
use oa_time::Time;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, OnceLock};

/// Upload textures kept per file at most (frames in flight plus look-ahead).
const MAX_SLOTS: usize = 24;

/// One reusable pair of upload textures.
struct Slot {
    luma: Arc<wgpu::Texture>,
    chroma: Arc<wgpu::Texture>,
    free: Arc<AtomicBool>,
}

/// A decoded frame, still in CPU memory: NV12 bytes, luma then interleaved chroma.
pub struct CpuFrame(Vec<u8>);

pub struct FfmpegDecoder {
    path: PathBuf,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The picture as stored (before rotation), and its chroma plane.
    size: [u32; 2],
    chroma: [u32; 2],
    rotation: u32,
    color: VideoColor,
    hwaccel: bool,
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    times: Option<Receiver<Time>>,
    /// The last frame's time, for a frame whose `showinfo` line went missing.
    last: Option<Time>,
    slots: Vec<Slot>,
}

impl FfmpegDecoder {
    /// Opens `path` for decoding (the process starts at the first frame).
    pub fn open(device: wgpu::Device, queue: wgpu::Queue, path: &Path, video: &VideoTrack) -> Result<Self, MediaError> {
        // Stored size: the display size with the rotation undone.
        let (w, h) = if video.rotation_quarter_turns % 2 == 1 { (video.height, video.width) } else { (video.width, video.height) };
        if w == 0 || h == 0 {
            return Err(MediaError::Unsupported("the video has no size".into()));
        }
        let mut d = FfmpegDecoder {
            path: path.to_path_buf(),
            device,
            queue,
            size: [w, h],
            chroma: [w.div_ceil(2), h.div_ceil(2)],
            rotation: video.rotation_quarter_turns % 4,
            color: video.color,
            hwaccel: hwaccel_allowed(),
            child: None,
            stdout: None,
            times: None,
            last: None,
            slots: Vec::new(),
        };
        d.spawn(None)?;
        Ok(d)
    }

    fn frame_bytes(&self) -> usize {
        (self.size[0] * self.size[1] + 2 * self.chroma[0] * self.chroma[1]) as usize
    }

    /// (Re)starts ffmpeg, from the keyframe at or before `from` (decoder time) if given.
    fn spawn(&mut self, from: Option<Time>) -> Result<(), MediaError> {
        self.stop();
        let mut c = crate::tool("ffmpeg");
        c.args(["-hide_banner", "-nostdin", "-loglevel", "info"]);
        if self.hwaccel {
            c.args(["-hwaccel", "auto"]);
        }
        // Rotation is applied when the frame is converted, as for hardware frames.
        c.arg("-noautorotate");
        c.args(["-copyts"]);
        if let Some(t) = from {
            // The file's own timestamps, landing on the keyframe at or before them.
            c.args(["-seek_timestamp", "1", "-noaccurate_seek", "-ss", &format!("{:.6}", t.as_seconds_f64().max(0.0))]);
        }
        c.arg("-i").arg(&self.path);
        c.args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", "showinfo"]);
        // One frame out per frame decoded: no duplicates or drops to fit a frame rate.
        c.args(passthrough_args());
        c.args(["-pix_fmt", "nv12", "-f", "rawvideo", "-"]);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| MediaError::Decode(format!("could not run ffmpeg: {e}")))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = channel();
        if let Some(stderr) = stderr {
            std::thread::spawn(move || {
                // Lines end in \n (and progress in \r); `showinfo` lines carry pts_time.
                for line in std::io::BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                    let line = String::from_utf8_lossy(&line);
                    if let Some(t) = showinfo_time(&line)
                        && tx.send(t).is_err()
                    {
                        return;
                    }
                }
            });
        }
        self.child = Some(child);
        self.stdout = stdout;
        self.times = Some(rx);
        Ok(())
    }

    fn stop(&mut self) {
        self.stdout = None;
        self.times = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// A pair of upload textures not in use, making one if needed.
    fn free_slot(&mut self) -> Result<usize, MediaError> {
        if let Some(i) = self.slots.iter().position(|s| s.free.load(std::sync::atomic::Ordering::Acquire)) {
            return Ok(i);
        }
        if self.slots.len() >= MAX_SLOTS {
            return Err(MediaError::Decode("every upload texture is still in use".into()));
        }
        let make = |label, size: [u32; 2], format| {
            Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }))
        };
        self.slots.push(Slot {
            luma: make("oa-ffmpeg-luma", self.size, wgpu::TextureFormat::R8Unorm),
            chroma: make("oa-ffmpeg-chroma", self.chroma, wgpu::TextureFormat::Rg8Unorm),
            free: Arc::new(AtomicBool::new(true)),
        });
        Ok(self.slots.len() - 1)
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

impl VideoDecoder for FfmpegDecoder {
    type Frame = CpuFrame;

    fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        self.last = None;
        self.spawn(Some(t))
    }

    fn next(&mut self) -> Result<Option<(Time, CpuFrame)>, MediaError> {
        let bytes = self.frame_bytes();
        let Some(stdout) = self.stdout.as_mut() else { return Ok(None) };
        let mut buf = vec![0u8; bytes];
        let mut got = 0;
        while got < bytes {
            match stdout.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => return Err(MediaError::Decode(format!("reading from ffmpeg: {e}"))),
            }
        }
        if got < bytes {
            // The end of the file (a partial frame means ffmpeg stopped mid-way).
            self.stdout = None;
            return Ok(None);
        }
        // Its time: normally already reported by the time the frame's bytes arrive.
        let t = self
            .times
            .as_ref()
            .and_then(|rx| rx.recv_timeout(std::time::Duration::from_secs(5)).ok())
            .or(self.last.map(|t| t + Time(1)))
            .ok_or_else(|| MediaError::Decode("ffmpeg gave a frame without its time".into()))?;
        self.last = Some(t);
        Ok(Some((t, CpuFrame(buf))))
    }

    fn publish(&mut self, frame: &CpuFrame) -> Result<Surface, MediaError> {
        let i = self.free_slot()?;
        let slot = &self.slots[i];
        let lease = Lease::take(&slot.free);
        let luma_len = (self.size[0] * self.size[1]) as usize;
        let upload = |texture: &wgpu::Texture, data: &[u8], size: [u32; 2], bpp: u32| {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0] * bpp), rows_per_image: Some(size[1]) },
                wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            );
        };
        upload(&slot.luma, &frame.0[..luma_len], self.size, 1);
        upload(&slot.chroma, &frame.0[luma_len..], self.chroma, 2);
        Ok(Surface {
            texture: slot.luma.clone(),
            chroma: Some(slot.chroma.clone()),
            coded_size: self.size,
            visible_size: self.size,
            rotation_quarter_turns: self.rotation,
            color: self.color,
            lease: Arc::new(lease),
        })
    }
}

/// `pts_time` from a `showinfo` line ("[Parsed_showinfo_0 @ …] n: 3 pts: 3072 pts_time:0.1 …").
fn showinfo_time(line: &str) -> Option<Time> {
    if !line.contains("showinfo") {
        return None;
    }
    let rest = &line[line.find("pts_time:")? + "pts_time:".len()..];
    let value = rest.split_whitespace().next()?;
    value.parse::<f64>().ok().filter(|v| v.is_finite()).map(Time::from_seconds_f64)
}

/// Whether ffmpeg may use the platform's hardware decoder (`OA_FFMPEG_HWACCEL=0` turns
/// it off, for drivers that decode wrongly).
fn hwaccel_allowed() -> bool {
    std::env::var("OA_FFMPEG_HWACCEL").map_or(true, |v| v != "0")
}

/// "Pass frames through as decoded": `-fps_mode` on ffmpeg 5.1 and later, `-vsync` before.
fn passthrough_args() -> [&'static str; 2] {
    static NEW: OnceLock<bool> = OnceLock::new();
    let new = *NEW.get_or_init(|| {
        crate::tool("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "nullsrc=s=16x16:d=0.04", "-fps_mode", "passthrough", "-f", "null", "-"])
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if new { ["-fps_mode", "passthrough"] } else { ["-vsync", "passthrough"] }
}

/// A frame source decoding every file through ffmpeg, uploading to `ctx`'s device.
pub fn source(ctx: &oa_gpu::GpuContext) -> crate::MediaFrameSource<FfmpegDecoder> {
    let (device, queue) = (ctx.device.clone(), ctx.queue.clone());
    crate::MediaFrameSource::new(move |path, video| FfmpegDecoder::open(device.clone(), queue.clone(), path, video))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showinfo_lines_give_their_time() {
        let line = "[Parsed_showinfo_0 @ 0000023a] n:  12 pts:  6144 pts_time:0.4     duration:512 fmt:yuv420p";
        assert_eq!(showinfo_time(line), Some(Time::from_seconds_f64(0.4)));
        assert_eq!(showinfo_time("frame=  120 fps=0.0 q=-0.0 size=N/A time=00:00:04.00"), None);
        assert_eq!(showinfo_time("[Parsed_showinfo_0 @ 0x1] config in time_base: 1/15360"), None);
    }
}
