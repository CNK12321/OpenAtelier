//! Transient texture pool.
//!
//! Lifetime analysis falls out of reference counting: a texture is reusable exactly when
//! the pool holds the only `Arc` to it — no node output, cache entry or caller still
//! refers to it. Textures idle for a few frames are released.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const WORKING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const BYTES_PER_TEXEL: u64 = 8;

pub struct PooledTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
    last_used: AtomicU64,
}

impl PooledTexture {
    pub fn bytes(&self) -> u64 {
        self.size[0] as u64 * self.size[1] as u64 * BYTES_PER_TEXEL
    }
}

#[derive(Default)]
pub struct TexturePool {
    textures: Mutex<Vec<Arc<PooledTexture>>>,
    frame: AtomicU64,
    created: AtomicU64,
}

impl TexturePool {
    pub fn acquire(&self, device: &wgpu::Device, size: [u32; 2]) -> Arc<PooledTexture> {
        let frame = self.frame.load(Ordering::Relaxed);
        let mut textures = self.textures.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = textures.iter().find(|t| t.size == size && Arc::strong_count(t) == 1) {
            t.last_used.store(frame, Ordering::Relaxed);
            return t.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("oa-pool"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let t = Arc::new(PooledTexture { texture, view, size, last_used: AtomicU64::new(frame) });
        textures.push(t.clone());
        self.created.fetch_add(1, Ordering::Relaxed);
        t
    }

    /// Advances the frame counter and frees textures nobody uses that have been idle
    /// for more than `keep_idle_frames`.
    pub fn end_frame(&self, keep_idle_frames: u64) {
        let frame = self.frame.fetch_add(1, Ordering::Relaxed) + 1;
        self.textures.lock().unwrap_or_else(|e| e.into_inner()).retain(|t| {
            Arc::strong_count(t) > 1 || frame - t.last_used.load(Ordering::Relaxed) <= keep_idle_frames
        });
    }

    /// (live textures, bytes, textures ever created)
    pub fn stats(&self) -> (usize, u64, u64) {
        let textures = self.textures.lock().unwrap_or_else(|e| e.into_inner());
        (textures.len(), textures.iter().map(|t| t.bytes()).sum(), self.created.load(Ordering::Relaxed))
    }
}
