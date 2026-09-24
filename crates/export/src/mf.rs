//! Encoding with Media Foundation's sink writer (Windows).
//!
//! The sink writer picks an encoder transform and, with hardware transforms enabled, that
//! is the GPU vendor's encoder (NVENC, Quick Sync, AMF) shipped with the driver — so this
//! works where ffmpeg's NVENC doesn't (it needs a newer driver API). It also muxes the
//! sound itself (PCM in, AAC out), so export needs no external tools.
//!
//! Frames arrive as CPU NV12 today (the same 1.5 bytes/pixel readback the ffmpeg sink
//! uses). Handing the encoder GPU textures through the DXGI device manager is the next
//! step; it only changes how `push_nv12` builds its sample.

use crate::{ExportError, VideoCodec, VideoSink};
use oa_time::{FrameRate, Time};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use windows::core::{GUID, HSTRING, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

fn err(context: &'static str) -> impl Fn(windows::core::Error) -> ExportError {
    move |e| ExportError::Encode(format!("{context}: {e}"))
}

fn startup() {
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    });
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

/// 100 ns units, as Media Foundation counts time.
fn hns(t: Time) -> i64 {
    (t.0 as i128 * 10_000_000 / oa_time::FLICKS_PER_SECOND as i128) as i64
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// The sound to mux: a 16-bit PCM WAV written by the exporter.
struct Audio {
    file: std::fs::File,
    stream: u32,
    sample_rate: u32,
    channels: u16,
    /// Sample frames written so far.
    written: u64,
    total: u64,
}

pub struct MfSink {
    writer: IMFSinkWriter,
    video: u32,
    size: [u32; 2],
    rate: FrameRate,
    frame: i64,
    audio: Option<Audio>,
    /// Which encoder the sink writer chose, e.g. "hardware: NVIDIA H.264 Encoder MFT".
    pub encoder: String,
}

impl MfSink {
    /// `bits_per_pixel` sets the average bitrate (0.1 ≈ 12 Mbit/s for 1080p60).
    pub fn start(
        out: &Path,
        size: [u32; 2],
        rate: FrameRate,
        codec: VideoCodec,
        bits_per_pixel: f64,
        audio_wav: Option<&Path>,
    ) -> Result<Self, ExportError> {
        let subtype = match codec {
            VideoCodec::H264 => MFVideoFormat_H264,
            VideoCodec::Hevc => MFVideoFormat_HEVC,
            _ => return Err(ExportError::Encode("Media Foundation writes H.264 and HEVC only; use the ffmpeg encoder".into())),
        };
        startup();
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4).map_err(err("MFCreateAttributes"))?;
            let attrs = attrs.expect("attributes");
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(err("hardware transforms"))?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).map_err(err("throttling"))?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4).map_err(err("container"))?;
            let _ = std::fs::remove_file(out);
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(out.as_os_str()), None, &attrs)
                .map_err(|e| ExportError::Encode(format!("{}: {e}", out.display())))?;

            let video_type = |subtype: &GUID| -> Result<IMFMediaType, ExportError> {
                let t = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(err("major type"))?;
                t.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(err("subtype"))?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(size[0], size[1])).map_err(err("frame size"))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(rate.num, rate.den)).map_err(err("frame rate"))?;
                t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(err("aspect"))?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(err("interlace"))?;
                // We convert with BT.709 limited range; say so in the stream.
                t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32).map_err(err("primaries"))?;
                t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32).map_err(err("transfer"))?;
                t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).map_err(err("matrix"))?;
                t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).map_err(err("range"))?;
                Ok(t)
            };
            let output = video_type(&subtype)?;
            let bitrate = (size[0] as f64 * size[1] as f64 * rate.as_f64() * bits_per_pixel).clamp(1e6, 4e8) as u32;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(err("bitrate"))?;
            if codec == VideoCodec::H264 {
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32).map_err(err("profile"))?;
            }
            let video = writer.AddStream(&output).map_err(err("AddStream(video)"))?;
            let input = video_type(&MFVideoFormat_NV12)?;
            writer
                .SetInputMediaType(video, &input, None)
                .map_err(|e| ExportError::Encode(format!("no {codec:?} encoder accepts {}x{} NV12: {e}", size[0], size[1])))?;

            let audio = match audio_wav {
                Some(path) => Some(Self::add_audio(&writer, path)?),
                None => None,
            };

            writer.BeginWriting().map_err(err("BeginWriting"))?;
            let encoder = describe_encoder(&writer, video);
            Ok(MfSink { writer, video, size, rate, frame: 0, audio, encoder })
        }
    }

    unsafe fn add_audio(writer: &IMFSinkWriter, wav: &Path) -> Result<Audio, ExportError> {
        let mut file = std::fs::File::open(wav).map_err(|e| ExportError::Io(format!("{}: {e}", wav.display())))?;
        // Our own WAV: a fixed 44-byte header (see wav.rs).
        let mut header = [0u8; 44];
        file.read_exact(&mut header).map_err(|e| ExportError::Io(e.to_string()))?;
        let channels = u16::from_le_bytes([header[22], header[23]]);
        let sample_rate = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
        let data = u32::from_le_bytes([header[40], header[41], header[42], header[43]]) as u64;
        let block = channels as u32 * 2;
        unsafe {
            let out = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(err("audio major"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).map_err(err("aac"))?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate).map_err(err("rate"))?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32).map_err(err("channels"))?;
            out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(err("bits"))?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000).map_err(err("aac bitrate"))?; // 192 kbit/s
            let stream = writer.AddStream(&out).map_err(err("AddStream(audio)"))?;
            let input = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(err("audio major"))?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(err("pcm"))?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate).map_err(err("rate"))?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32).map_err(err("channels"))?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(err("bits"))?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block).map_err(err("align"))?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, sample_rate * block).map_err(err("bytes/s"))?;
            writer.SetInputMediaType(stream, &input, None).map_err(err("no AAC encoder for this audio"))?;
            Ok(Audio { file, stream, sample_rate, channels, written: 0, total: data / block as u64 })
        }
    }

    /// Writes sound up to `until` (sample-exact), in chunks of about 100 ms, so audio and
    /// video stay interleaved in the file.
    fn write_audio_until(&mut self, until: Time) -> Result<(), ExportError> {
        let Some(a) = self.audio.as_mut() else { return Ok(()) };
        let target = ((until.0 as i128 * a.sample_rate as i128 / oa_time::FLICKS_PER_SECOND as i128) as u64).min(a.total);
        let chunk = (a.sample_rate / 10) as u64;
        let block = a.channels as u64 * 2;
        while a.written < target {
            let frames = chunk.min(target - a.written);
            let bytes = (frames * block) as usize;
            let mut data = vec![0u8; bytes];
            a.file.seek(SeekFrom::Start(44 + a.written * block)).map_err(|e| ExportError::Io(e.to_string()))?;
            a.file.read_exact(&mut data).map_err(|e| ExportError::Io(e.to_string()))?;
            let start = a.written as i128 * 10_000_000 / a.sample_rate as i128;
            let end = (a.written + frames) as i128 * 10_000_000 / a.sample_rate as i128;
            unsafe { write_sample(&self.writer, a.stream, &data, start as i64, (end - start) as i64)? };
            a.written += frames;
        }
        Ok(())
    }
}

unsafe fn write_sample(writer: &IMFSinkWriter, stream: u32, parts: &[u8], time: i64, duration: i64) -> Result<(), ExportError> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(parts.len() as u32).map_err(err("MFCreateMemoryBuffer"))?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None).map_err(err("Lock"))?;
        std::ptr::copy_nonoverlapping(parts.as_ptr(), ptr, parts.len());
        buffer.Unlock().map_err(err("Unlock"))?;
        buffer.SetCurrentLength(parts.len() as u32).map_err(err("SetCurrentLength"))?;
        let sample = MFCreateSample().map_err(err("MFCreateSample"))?;
        sample.AddBuffer(&buffer).map_err(err("AddBuffer"))?;
        sample.SetSampleTime(time).map_err(err("SetSampleTime"))?;
        sample.SetSampleDuration(duration).map_err(err("SetSampleDuration"))?;
        writer.WriteSample(stream, &sample).map_err(err("WriteSample"))
    }
}

/// "hardware: <name>" or "software: <name>", from the transform the sink writer chose.
unsafe fn describe_encoder(writer: &IMFSinkWriter, stream: u32) -> String {
    unsafe {
        let Ok(ex) = windows::core::Interface::cast::<IMFSinkWriterEx>(writer) else { return "unknown encoder".into() };
        let string = |attrs: &IMFAttributes, key: &GUID| -> Option<String> {
            let mut value = PWSTR::null();
            let mut len = 0u32;
            attrs.GetAllocatedString(key, &mut value, &mut len).ok()?;
            let s = value.to_string().ok();
            CoTaskMemFree(Some(value.0 as _));
            s
        };
        for index in 0.. {
            let mut category = GUID::zeroed();
            let mut transform = None;
            if ex.GetTransformForStream(stream, index, Some(&mut category), &mut transform).is_err() {
                break;
            }
            if category != MFT_CATEGORY_VIDEO_ENCODER {
                continue;
            }
            let Some(attrs) = transform.and_then(|t| t.GetAttributes().ok()) else { continue };
            let name = string(&attrs, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "encoder".into());
            let hardware = string(&attrs, &MFT_ENUM_HARDWARE_URL_Attribute).is_some();
            return format!("{}: {name}", if hardware { "hardware" } else { "software" });
        }
        "unknown encoder".into()
    }
}

impl VideoSink for MfSink {
    fn push_nv12(&mut self, luma: &[u8], chroma: &[u8]) -> Result<(), ExportError> {
        let expected = (self.size[0] * self.size[1]) as usize;
        if luma.len() != expected || chroma.len() != expected / 2 {
            return Err(ExportError::Encode(format!("frame is {}+{} bytes, expected {}+{}", luma.len(), chroma.len(), expected, expected / 2)));
        }
        let (t0, t1) = (self.rate.frame_start(self.frame), self.rate.frame_start(self.frame + 1));
        // Keep the sound slightly ahead of the picture, as the muxer likes.
        self.write_audio_until(t1)?;
        // The exporter reads a frame back with its planes one after the other: that's
        // already NV12's layout, so it goes to the encoder without being joined again.
        let joined;
        let data: &[u8] = if std::ptr::eq(luma.as_ptr_range().end, chroma.as_ptr()) {
            // SAFETY: two adjacent slices of one allocation (checked above).
            unsafe { std::slice::from_raw_parts(luma.as_ptr(), luma.len() + chroma.len()) }
        } else {
            joined = [luma, chroma].concat();
            &joined
        };
        unsafe { write_sample(&self.writer, self.video, data, hns(t0), hns(t1) - hns(t0))? };
        self.frame += 1;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        self.write_audio_until(Time::MAX)?;
        unsafe { self.writer.Finalize().map_err(err("Finalize")) }
    }

    fn describe(&self) -> String {
        format!("Media Foundation ({})", self.encoder)
    }
}
