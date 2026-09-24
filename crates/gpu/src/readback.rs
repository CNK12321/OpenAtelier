//! CPU readback — only for edges: tests, still export, thumbnails.

use crate::pipelines::PipelineSpec;
use crate::renderer::Uniforms;
use crate::{shaders, GpuContext, GpuImage, Pipelines, RenderError};

/// Reads a working-format image as linear premultiplied RGBA floats, row by row.
pub fn read_linear(ctx: &GpuContext, img: &GpuImage) -> Result<Vec<[f32; 4]>, RenderError> {
    let bytes = read_texture(ctx, &img.tex.texture, img.size, 8)?;
    Ok(bytes
        .chunks_exact(8)
        .map(|px| {
            let c = |i: usize| f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]]));
            [c(0), c(2), c(4), c(6)]
        })
        .collect())
}

/// Converts to display sRGB on the GPU, then reads 8-bit RGBA (for PNGs/thumbnails).
pub fn read_srgb8(ctx: &GpuContext, pipelines: &Pipelines, img: &GpuImage) -> Result<Vec<u8>, RenderError> {
    let target = display_texture(ctx, pipelines, img)?;
    read_texture(ctx, &target, img.size, 4)
}

/// A view of a [`display_texture`] that reads its sRGB-encoded bytes as they are, with no
/// decoding — what egui and most UI toolkits expect of user textures.
pub fn display_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    // The texture is plain RGBA8 holding sRGB-encoded bytes (the display shader encodes):
    // its own view already reads them as they are, on every backend.
    texture.create_view(&Default::default())
}

/// Applies the display transform into a fresh `Rgba8Unorm` texture of sRGB-encoded
/// bytes — what a preview window or thumbnail shows.
pub fn display_texture(ctx: &GpuContext, pipelines: &Pipelines, img: &GpuImage) -> Result<wgpu::Texture, RenderError> {
    display_texture_into(ctx, pipelines, img, None)
}

/// Like [`display_texture`], drawing into `reuse` when it's the right size — a preview
/// updating every frame then allocates nothing. (Writes are queued after anything that
/// already read the texture, so reusing it while it's on screen is safe.)
pub fn display_texture_into(ctx: &GpuContext, pipelines: &Pipelines, img: &GpuImage, reuse: Option<wgpu::Texture>) -> Result<wgpu::Texture, RenderError> {
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let pipeline = pipelines.get_blocking("display-srgb8", || PipelineSpec {
        label: "display-srgb8".into(),
        source: shaders::display(),
        vertex_entry: "vs_full",
        fragment_entry: "fs_main",
        format,
        blend: None,
    })?;
    let fits = |t: &wgpu::Texture| t.width() == img.size[0] && t.height() == img.size[1] && t.format() == format;
    let target = match reuse.filter(fits) {
        Some(t) => t,
        None => new_display_target(ctx, img.size, format),
    };
    draw_display(ctx, pipelines, &pipeline, img, &target);
    Ok(target)
}

fn new_display_target(ctx: &GpuContext, size: [u32; 2], format: wgpu::TextureFormat) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("oa-display"),
        size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn draw_display(ctx: &GpuContext, pipelines: &Pipelines, pipeline: &wgpu::RenderPipeline, img: &GpuImage, target: &wgpu::Texture) {
    let view = target.create_view(&Default::default());
    let u = Uniforms::default().output(img.origin, img.size).input(img);
    let uniforms = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oa-display-uniforms"),
        size: (u.0.len() * 4) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.queue.write_buffer(&uniforms, 0, bytemuck::cast_slice(&u.0));
    let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor::default());
    let bg = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("oa-display"),
        layout: &pipelines.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&img.tex.view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });
    let mut encoder = ctx.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oa-display"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
    ctx.queue.submit([encoder.finish()]);
}

/// Copies any texture back to CPU bytes, row by row without the padding.
pub fn read_texture(ctx: &GpuContext, texture: &wgpu::Texture, size: [u32; 2], bytes_per_px: u32) -> Result<Vec<u8>, RenderError> {
    let mut parts = start_read(ctx, &[(texture, size, bytes_per_px)]).wait(ctx)?;
    Ok(parts.remove(0))
}

/// Readbacks in flight: the copies are submitted, the CPU hasn't waited for them yet.
/// Lets a caller render the next frame while this one travels to the CPU.
pub struct PendingRead {
    parts: Vec<(wgpu::Buffer, u32, u32, u32)>, // buffer, unpadded row, padded row, rows
    submission: wgpu::SubmissionIndex,
    mapped: Vec<std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
}

/// Copies each `(texture, size, bytes per pixel)` into a mappable buffer in one submission
/// and starts mapping; nothing blocks until [`PendingRead::wait`].
pub fn start_read(ctx: &GpuContext, textures: &[(&wgpu::Texture, [u32; 2], u32)]) -> PendingRead {
    start_read_reusing(ctx, textures, &mut Vec::new())
}

/// [`start_read`], taking its staging buffers from `spare` when one of the right size is
/// there (a stream of same-sized frames — an export — then allocates none after the
/// first few; give them back with [`PendingRead::wait_packed`]).
pub fn start_read_reusing(ctx: &GpuContext, textures: &[(&wgpu::Texture, [u32; 2], u32)], spare: &mut Vec<wgpu::Buffer>) -> PendingRead {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("oa-readback") });
    let mut parts = Vec::new();
    for &(texture, size, bytes_per_px) in textures {
        let unpadded = size[0] * bytes_per_px;
        let padded = unpadded.div_ceil(align) * align;
        let bytes = padded as u64 * size[1] as u64;
        let buffer = match spare.iter().position(|b| b.size() == bytes) {
            Some(i) => spare.swap_remove(i),
            None => ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("oa-readback"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        };
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        parts.push((buffer, unpadded, padded, size[1]));
    }
    let submission = ctx.queue.submit([encoder.finish()]);
    let mapped = parts
        .iter()
        .map(|(buffer, ..)| {
            let (tx, rx) = std::sync::mpsc::channel();
            buffer.map_async(wgpu::MapMode::Read, .., move |r| {
                let _ = tx.send(r);
            });
            rx
        })
        .collect();
    PendingRead { parts, submission, mapped }
}

impl PendingRead {
    /// Waits for this readback and writes every texture's rows, tightly packed and one
    /// after another, into `out` (reused: no allocation once it's big enough). Returns
    /// where each texture starts in it. The staging buffers go back to `spare`.
    pub fn wait_packed(self, ctx: &GpuContext, out: &mut Vec<u8>, spare: &mut Vec<wgpu::Buffer>) -> Result<Vec<usize>, RenderError> {
        ctx.device
            .poll(wgpu::PollType::Wait { submission_index: Some(self.submission), timeout: None })
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        out.clear();
        let mut starts = Vec::with_capacity(self.parts.len());
        for ((buffer, unpadded, padded, _rows), mapped) in self.parts.into_iter().zip(self.mapped) {
            mapped
                .recv()
                .map_err(|e| RenderError::Readback(e.to_string()))?
                .map_err(|e| RenderError::Readback(e.to_string()))?;
            starts.push(out.len());
            {
                let view = buffer.get_mapped_range(..).map_err(|e| RenderError::Readback(e.to_string()))?;
                if unpadded == padded {
                    out.extend_from_slice(&view);
                } else {
                    for row in view.chunks_exact(padded as usize) {
                        out.extend_from_slice(&row[..unpadded as usize]);
                    }
                }
            }
            buffer.unmap();
            spare.push(buffer);
        }
        Ok(starts)
    }

    /// Waits for this readback's submission only (not later work) and returns each
    /// texture's tightly packed bytes, in the order given.
    pub fn wait(self, ctx: &GpuContext) -> Result<Vec<Vec<u8>>, RenderError> {
        ctx.device
            .poll(wgpu::PollType::Wait { submission_index: Some(self.submission), timeout: None })
            .map_err(|e| RenderError::Readback(e.to_string()))?;
        let mut out = Vec::with_capacity(self.parts.len());
        for ((buffer, unpadded, padded, rows), mapped) in self.parts.into_iter().zip(self.mapped) {
            mapped
                .recv()
                .map_err(|e| RenderError::Readback(e.to_string()))?
                .map_err(|e| RenderError::Readback(e.to_string()))?;
            let view = buffer.get_mapped_range(..).map_err(|e| RenderError::Readback(e.to_string()))?;
            let mut bytes = Vec::with_capacity((unpadded * rows) as usize);
            for row in view.chunks_exact(padded as usize) {
                bytes.extend_from_slice(&row[..unpadded as usize]);
            }
            out.push(bytes);
        }
        Ok(out)
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => {
            if frac == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::f16_to_f32;

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0), 0.0);
        assert!(f16_to_f32(0x7c01).is_nan());
    }
}
