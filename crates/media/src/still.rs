//! Still images (PNG, JPEG, …).
//!
//! An image is decoded once with ffmpeg, uploaded to the GPU and then reused for every
//! frame that shows it — so the one CPU copy happens at import, not per frame. Alpha is
//! preserved (the NV12 video path cannot carry it).

use crate::{MediaError, VideoTrack};
use oa_gpu::{FrameSource, GpuImage, GpuServices, RenderError, SourceRequest};
use std::collections::HashMap;
use std::path::Path;

struct Image {
    texture: wgpu::Texture,
    size: [u32; 2],
}

type Decoded = Result<Vec<u8>, MediaError>;

/// Serves `Source` nodes for still images.
#[derive(Default)]
pub struct StillSource {
    images: HashMap<u64, Image>,
    /// Images still being decoded on a worker thread, with their size.
    pending: HashMap<u64, ([u32; 2], std::sync::mpsc::Receiver<Decoded>)>,
    pub uploads: u64,
}

impl StillSource {
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.pending.is_empty()
    }

    pub fn contains(&self, media_id: u64) -> bool {
        self.images.contains_key(&media_id) || self.pending.contains_key(&media_id)
    }

    /// Starts decoding `path` on a worker thread; the first frame that needs it uploads
    /// it (or, when that frame can't wait, reports `NotReady` until it's decoded).
    pub fn add_in_background(&mut self, media_id: u64, path: &Path, video: &VideoTrack) {
        let size = [video.width.max(1), video.height.max(1)];
        let (tx, rx) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(decode_rgba(&path, size));
        });
        self.images.remove(&media_id);
        self.pending.insert(media_id, (size, rx));
    }

    /// Moves a finished background decode onto the GPU. `Err(NotReady)` while it's still
    /// running and `wait` is off.
    fn settle(&mut self, gpu: &oa_gpu::GpuContext, media_id: u64, wait: bool) -> Result<(), RenderError> {
        let Some((size, rx)) = self.pending.get(&media_id) else { return Ok(()) };
        let size = *size;
        let result = if wait {
            rx.recv().unwrap_or_else(|_| Err(MediaError::Decode("image decode stopped".into())))
        } else {
            match rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return Err(RenderError::NotReady),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(MediaError::Decode("image decode stopped".into())),
            }
        };
        self.pending.remove(&media_id);
        let pixels = result.map_err(|e| RenderError::Source(e.to_string()))?;
        self.upload(gpu, media_id, size, &pixels);
        Ok(())
    }

    /// Decodes `path` and keeps it on the GPU as an sRGB texture with straight alpha.
    pub fn add(&mut self, gpu: &oa_gpu::GpuContext, media_id: u64, path: &Path, video: &VideoTrack) -> Result<(), MediaError> {
        let size = [video.width.max(1), video.height.max(1)];
        let pixels = decode_rgba(path, size)?;
        self.upload(gpu, media_id, size, &pixels);
        Ok(())
    }

    fn upload(&mut self, gpu: &oa_gpu::GpuContext, media_id: u64, size: [u32; 2], pixels: &[u8]) {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oa-still"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0] * 4), rows_per_image: Some(size[1]) },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        self.images.insert(media_id, Image { texture, size });
    }
}

impl FrameSource for StillSource {
    fn frame(&mut self, gpu: &mut GpuServices<'_>, req: &SourceRequest) -> Result<GpuImage, RenderError> {
        let wait = gpu.waits();
        self.settle(gpu.ctx, req.media, wait)?;
        let image = self.images.get(&req.media).ok_or_else(|| RenderError::Source(format!("still {} not loaded", req.media)))?;
        self.uploads += 1;
        let _ = image.size;
        gpu.blit_srgb(&image.texture, req.size)
    }

    fn status(&self) -> Option<String> {
        (!self.images.is_empty()).then(|| format!("{} still image(s) on the GPU", self.images.len()))
    }
}

/// Decodes one frame as straight-alpha RGBA8 at exactly `size`.
fn decode_rgba(path: &Path, size: [u32; 2]) -> Result<Vec<u8>, MediaError> {
    let out = crate::tool("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .map_err(|e| MediaError::Decode(format!("could not run ffmpeg: {e}")))?;
    if !out.status.success() {
        return Err(MediaError::Decode(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let expected = size[0] as usize * size[1] as usize * 4;
    if out.stdout.len() < expected {
        return Err(MediaError::Decode(format!(
            "expected {expected} bytes for {}x{}, got {}",
            size[0],
            size[1],
            out.stdout.len()
        )));
    }
    let mut pixels = out.stdout;
    pixels.truncate(expected);
    Ok(pixels)
}
