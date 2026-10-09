//! A glance at a picture already on the GPU: shrunk there to a few pixels (each halving
//! averages four, so it comes out softly blurred) and read back without waiting — for
//! things that only need its overall colors, like the viewer's glow.
//!
//! Nothing is rendered again: it reads the texture as shown. One read is in flight at a
//! time; [`Glance::poll`] picks it up when the GPU is done, never blocking.

use crate::GpuContext;
use std::sync::mpsc::{Receiver, TryRecvError};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
struct V { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2f(p.x, 1.0 - p.y);
    return o;
}
@fragment fn fs(v: V) -> @location(0) vec4f { return textureSampleLevel(src, smp, v.uv, 0.0); }
"#;

struct InFlight {
    buffer: wgpu::Buffer,
    padded: u32,
    size: [u32; 2],
    mapped: Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// Shrinks pictures and reads them back (see the module docs).
pub struct Glance {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The halvings, kept while the pictures stay the same size.
    chain: Vec<wgpu::Texture>,
    chain_for: [u32; 2],
    pending: Option<InFlight>,
}

/// The sizes a picture of `size` halves through until its short side is at most
/// `short` (each at least 1 px), not counting the picture itself.
pub fn halvings(size: [u32; 2], short: u32) -> Vec<[u32; 2]> {
    let mut out = Vec::new();
    let mut s = size;
    while s[0].min(s[1]) > short.max(1) {
        s = [(s[0] / 2).max(1), (s[1] / 2).max(1)];
        out.push(s);
    }
    out
}

impl Glance {
    pub fn new(ctx: &GpuContext) -> Self {
        let device = &ctx.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("oa-glance"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("oa-glance"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("oa-glance"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("oa-glance"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("oa-glance"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Glance { pipeline, layout, sampler, chain: Vec::new(), chain_for: [0, 0], pending: None }
    }

    /// Whether a read is still on its way.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Shrinks `texture` (a sampleable RGBA picture) until its short side is at most
    /// `short` px and starts reading it back. Does nothing while a read is in flight.
    pub fn start(&mut self, ctx: &GpuContext, texture: &wgpu::Texture, short: u32) {
        if self.pending.is_some() {
            return;
        }
        let size = [texture.width(), texture.height()];
        if self.chain_for != size {
            self.chain = halvings(size, short)
                .into_iter()
                .map(|s| {
                    ctx.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("oa-glance"),
                        size: wgpu::Extent3d { width: s[0], height: s[1], depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    })
                })
                .collect();
            self.chain_for = size;
        }
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("oa-glance") });
        let mut from = texture.create_view(&Default::default());
        for target in &self.chain {
            let view = target.create_view(&Default::default());
            let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("oa-glance"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&from) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("oa-glance"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            from = view;
        }
        let last = self.chain.last().unwrap_or(texture);
        let size = [last.width(), last.height()];
        let unpadded = size[0] * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("oa-glance"),
            size: padded as u64 * size[1] as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: last, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None } },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        ctx.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.pending = Some(InFlight { buffer, padded, size, mapped: rx });
    }

    /// The shrunk picture once it's back — RGBA bytes, tightly packed, and its size —
    /// or `None` while it's still on its way (or the read failed).
    pub fn poll(&mut self, ctx: &GpuContext) -> Option<(Vec<u8>, [u32; 2])> {
        let p = self.pending.as_ref()?;
        let _ = ctx.device.poll(wgpu::PollType::Poll);
        match p.mapped.try_recv() {
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) | Ok(Err(_)) => {
                self.pending = None;
                None
            }
            Ok(Ok(())) => {
                let p = self.pending.take()?;
                let unpadded = (p.size[0] * 4) as usize;
                let bytes = {
                    let view = p.buffer.get_mapped_range(..).ok()?;
                    view.chunks_exact(p.padded as usize).flat_map(|row| row[..unpadded].iter().copied()).collect()
                };
                p.buffer.unmap();
                Some((bytes, p.size))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::halvings;

    #[test]
    fn pictures_halve_down_to_a_glance() {
        assert_eq!(halvings([1920, 1080], 16), [[960, 540], [480, 270], [240, 135], [120, 67], [60, 33], [30, 16]]);
        assert!(halvings([16, 9], 16).is_empty(), "already small enough");
        assert_eq!(halvings([4000, 1], 16), Vec::<[u32; 2]>::new(), "a sliver's short side is already small");
    }
}
