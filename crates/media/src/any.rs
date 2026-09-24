//! The decoder for a file, chosen when it's opened: on Windows, Media Foundation's
//! hardware decoder when it can take the file (frames stay on the GPU), else ffmpeg
//! (`crate::ffmpeg`, every platform, any codec ffmpeg knows).

use crate::ffmpeg::{CpuFrame, FfmpegDecoder};
use crate::source::{Surface, VideoDecoder};
use crate::{MediaError, MediaFrameSource};
use oa_time::Time;

pub enum AnyDecoder {
    #[cfg(windows)]
    Mf(crate::windows::MfDecoder),
    Ffmpeg(FfmpegDecoder),
}

pub enum AnyFrame {
    #[cfg(windows)]
    Mf(<crate::windows::MfDecoder as VideoDecoder>::Frame),
    Ffmpeg(CpuFrame),
}

impl AnyDecoder {
    /// Which decoder this is, for the status line.
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(windows)]
            AnyDecoder::Mf(_) => "Media Foundation",
            AnyDecoder::Ffmpeg(_) => "ffmpeg",
        }
    }
}

impl VideoDecoder for AnyDecoder {
    type Frame = AnyFrame;

    fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        match self {
            #[cfg(windows)]
            AnyDecoder::Mf(d) => d.seek(t),
            AnyDecoder::Ffmpeg(d) => d.seek(t),
        }
    }

    fn next(&mut self) -> Result<Option<(Time, AnyFrame)>, MediaError> {
        Ok(match self {
            #[cfg(windows)]
            AnyDecoder::Mf(d) => d.next()?.map(|(t, f)| (t, AnyFrame::Mf(f))),
            AnyDecoder::Ffmpeg(d) => d.next()?.map(|(t, f)| (t, AnyFrame::Ffmpeg(f))),
        })
    }

    fn publish(&mut self, frame: &AnyFrame) -> Result<Surface, MediaError> {
        match (self, frame) {
            #[cfg(windows)]
            (AnyDecoder::Mf(d), AnyFrame::Mf(f)) => d.publish(f),
            (AnyDecoder::Ffmpeg(d), AnyFrame::Ffmpeg(f)) => d.publish(f),
            #[cfg(windows)]
            _ => Err(MediaError::Decode("a frame from another decoder".into())),
        }
    }
}

/// How files get decoded.
#[derive(Clone, Default)]
pub struct DecoderChoice {
    /// The D3D11 device Media Foundation decodes on (Windows, DX12 backend only).
    #[cfg(windows)]
    pub bridge: Option<std::sync::Arc<crate::windows::D3D11Bridge>>,
    /// Always ffmpeg, even where Media Foundation could decode (a setting, for systems
    /// whose hardware decoder misbehaves).
    pub force_ffmpeg: bool,
}

impl DecoderChoice {
    /// Whether the platform's zero-copy hardware decoder is in use at all.
    pub fn hardware(&self) -> bool {
        #[cfg(windows)]
        {
            self.bridge.is_some() && !self.force_ffmpeg
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// A frame source that opens each file with the best decoder `choice` allows.
pub fn frame_source(ctx: &oa_gpu::GpuContext, choice: DecoderChoice) -> MediaFrameSource<AnyDecoder> {
    let (device, queue) = (ctx.device.clone(), ctx.queue.clone());
    MediaFrameSource::new(move |path, video| {
        #[cfg(windows)]
        if let Some(bridge) = choice.bridge.clone().filter(|_| !choice.force_ffmpeg)
            && let Ok(d) = crate::windows::MfDecoder::open(bridge, path, video)
        {
            return Ok(AnyDecoder::Mf(d));
        }
        let _ = &choice;
        FfmpegDecoder::open(device.clone(), queue.clone(), path, video).map(AnyDecoder::Ffmpeg)
    })
}
