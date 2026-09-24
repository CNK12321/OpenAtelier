use crate::pipelines::{PipelineSpec, Pipelines};
use crate::pool::{PooledTexture, TexturePool, WORKING_FORMAT};
use crate::shaders::{self, Nv12Plane, PARAM_BASE, UNIFORM_SLOTS};
use crate::yuv::{self, Nv12Frame};
use crate::{FrameSource, GpuContext, GpuImage, RenderError, SourceRequest};
use oa_graph::registry::{EffectDescriptor, Registry};
use oa_graph::{BlendMode, CacheKey, EffectKind, Graph, NodeId, NodeOp, WorkingSpace};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FusionMode {
    /// Fused pipelines compile in the background; the unfused chain renders meanwhile.
    Async,
    /// Compile fused pipelines on first use (tests, export).
    Blocking,
    /// Always run point ops one pass each (debugging, reference comparisons).
    Off,
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub fusion: FusionMode,
    /// Node-output cache budget in bytes (0 disables caching).
    pub cache_budget: u64,
    /// All the GPU memory the renderer may hold — pool plus cache. As it fills, the
    /// cache is trimmed and idle textures are released; past it, the host is asked to
    /// render smaller (see [`crate::health`]).
    pub vram_budget: u64,
    /// Pool textures unused for this many frames are freed.
    pub keep_idle_frames: u64,
    /// Wait for shaders (and glyphs) that aren't ready yet. Off for interactive use: the
    /// frame fails with `RenderError::NotReady` while they're prepared in the background,
    /// and the caller shows what it had. On for export and tests.
    pub wait: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { fusion: FusionMode::Async, cache_budget: 1 << 30, vram_budget: 2 << 30, keep_idle_frames: 4, wait: true }
    }
}

/// Counters for the most recent frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderStats {
    pub passes: u32,
    pub cache_hits: u32,
    pub fused_chains: u32,
    /// Fused chains rendered unfused because their pipeline wasn't ready.
    pub fallback_chains: u32,
    pub unsupported: Vec<String>,
    pub pool_textures: usize,
    pub pool_bytes: u64,
    pub cache_bytes: u64,
    /// What the renderer holds against its budget.
    pub memory: crate::health::Memory,
}

/// The 64-float uniform block shared by every pass (layout in `shaders.rs`).
#[derive(Clone)]
pub struct Uniforms(pub [f32; UNIFORM_SLOTS]);

impl Default for Uniforms {
    fn default() -> Self {
        Uniforms([0.0; UNIFORM_SLOTS])
    }
}

impl Uniforms {
    pub fn output(mut self, origin: [f64; 2], size: [u32; 2]) -> Self {
        self.0[0..4].copy_from_slice(&[origin[0] as f32, origin[1] as f32, size[0] as f32, size[1] as f32]);
        self
    }

    pub fn input(mut self, img: &GpuImage) -> Self {
        self.0[4..8].copy_from_slice(&[img.origin[0] as f32, img.origin[1] as f32, img.size[0] as f32, img.size[1] as f32]);
        self
    }

    pub fn affine(mut self, m: [f64; 6], opacity: f32) -> Self {
        for (i, v) in m.iter().enumerate() {
            self.0[8 + i] = *v as f32;
        }
        self.0[14] = opacity;
        self
    }

    pub fn pass_index(mut self, i: u32) -> Self {
        self.0[15] = i as f32;
        self
    }

    /// Writes params starting at `PARAM_BASE + offset`. Returns an error if they don't fit.
    pub fn params(mut self, offset: usize, values: &[f32]) -> Result<Self, RenderError> {
        let start = PARAM_BASE + offset;
        let end = start + values.len();
        if end > UNIFORM_SLOTS {
            return Err(RenderError::Unsupported(format!("{} effect params exceed the uniform block", end - PARAM_BASE)));
        }
        self.0[start..end].copy_from_slice(values);
        Ok(self)
    }
}

/// GPU resources a frame (and any [`FrameSource`]) renders with.
pub struct GpuServices<'a> {
    pub ctx: &'a GpuContext,
    pub pipelines: &'a Pipelines,
    yuv_pipelines: &'a Pipelines,
    mix_pipelines: &'a Pipelines,
    pool: &'a TexturePool,
    sampler: &'a wgpu::Sampler,
    /// Nearest-neighbor, for pixelated layers.
    nearest: &'a wgpu::Sampler,
    dummy: &'a GpuImage,
    uniform_buffers: &'a mut Vec<wgpu::Buffer>,
    uniforms_used: usize,
    pub(crate) encoder: wgpu::CommandEncoder,
    pub(crate) stats: RenderStats,
    /// See [`RenderOptions::wait`].
    pub(crate) wait: bool,
}

impl GpuServices<'_> {
    /// Whether this frame waits for things still being prepared (see
    /// [`RenderOptions::wait`]). Sources that load in the background check it.
    pub fn waits(&self) -> bool {
        self.wait
    }

    pub fn target(&self, size: [u32; 2]) -> Result<Arc<PooledTexture>, RenderError> {
        let max = self.ctx.max_texture_size();
        if size[0] == 0 || size[1] == 0 || size[0] > max || size[1] > max {
            return Err(RenderError::TooLarge(size));
        }
        Ok(self.pool.acquire(&self.ctx.device, size))
    }

    /// Renders one NV12 plane of `img` for an encoder: `Luma` at full size, `Chroma` at
    /// half. Color is BT.709, limited range.
    pub fn to_nv12_plane(&mut self, img: &GpuImage, plane: Nv12Plane) -> Result<wgpu::Texture, RenderError> {
        match plane {
            Nv12Plane::Luma => self.encode_pass(img, "nv12-luma", wgpu::TextureFormat::R8Unorm, img.size, || shaders::to_nv12(Nv12Plane::Luma)),
            Nv12Plane::Chroma => {
                self.encode_pass(img, "nv12-chroma", wgpu::TextureFormat::Rg8Unorm, [img.size[0] / 2, img.size[1] / 2], || shaders::to_nv12(Nv12Plane::Chroma))
            }
        }
    }

    /// The working-format image as 8-bit sRGB RGBA with **straight** (not premultiplied)
    /// alpha — what formats that keep transparency (GIF, ProRes 4444, WebM) take.
    pub fn to_rgba8(&mut self, img: &GpuImage) -> Result<wgpu::Texture, RenderError> {
        self.encode_pass(img, "rgba8-straight", wgpu::TextureFormat::Rgba8UnormSrgb, img.size, shaders::to_rgba8_straight)
    }

    /// One full-screen pass from `img` into a fresh `format` texture of `size`, readable
    /// back to the CPU.
    fn encode_pass(&mut self, img: &GpuImage, key: &'static str, format: wgpu::TextureFormat, size: [u32; 2], source: impl FnOnce() -> String) -> Result<wgpu::Texture, RenderError> {
        let pipeline = self.pipelines.get_blocking(key, || PipelineSpec {
            label: key.into(),
            source: source(),
            vertex_entry: "vs_full",
            fragment_entry: "fs_main",
            format,
            blend: None,
        })?;
        let target = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(key),
            size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let u = Uniforms::default().output([0.0; 2], size);
        let buf = self.uniform_buffer(&u);
        let input = img.tex.view.clone();
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(key),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(self.sampler) },
            ],
        });
        {
            let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(key),
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }
        self.stats.passes += 1;
        Ok(target)
    }

    /// Uploads nothing: takes an existing 8-bit sRGB texture (a decoded still image) and
    /// returns it in the working format at `out_size`, premultiplied.
    pub fn blit_srgb(&mut self, texture: &wgpu::Texture, out_size: [u32; 2]) -> Result<GpuImage, RenderError> {
        let pipeline = self.working_pipeline("blit-srgb", shaders::blit_srgb)?;
        let target = self.target(out_size)?;
        let view = texture.create_view(&Default::default());
        let u = Uniforms::default().output([0.0; 2], out_size);
        let buf = self.uniform_buffer(&u);
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-blit"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(self.sampler) },
            ],
        });
        self.draw_fullscreen(&pipeline, &target, &bg);
        Ok(GpuImage { tex: target, origin: [0.0; 2], size: out_size })
    }

    /// Converts a decoded NV12 frame into a working-format image of `out_size`
    /// (cropping decoder padding and resampling in the same pass).
    pub fn convert_nv12(&mut self, frame: &Nv12Frame<'_>, out_size: [u32; 2]) -> Result<GpuImage, RenderError> {
        // One NV12 texture viewed as its two planes, or the planes as two textures.
        let (luma, chroma) = match frame.chroma {
            None if frame.texture.format() == wgpu::TextureFormat::NV12 => {
                let plane = |aspect, format| frame.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(format), aspect, ..Default::default() });
                (plane(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm), plane(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm))
            }
            Some(chroma) if frame.texture.format() == wgpu::TextureFormat::R8Unorm && chroma.format() == wgpu::TextureFormat::Rg8Unorm => {
                (frame.texture.create_view(&Default::default()), chroma.create_view(&Default::default()))
            }
            _ => return Err(RenderError::Source(format!("expected NV12 or R8 + RG8 planes, got {:?}", frame.texture.format()))),
        };
        let pipeline = self.yuv_pipelines.get_blocking("yuv-nv12", yuv::spec)?;
        let target = self.target(out_size)?;
        let buf = self.uniform_buffer(&Uniforms(yuv::uniforms(frame, out_size)));
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-yuv"),
            layout: &self.yuv_pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&luma) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&chroma) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(self.sampler) },
            ],
        });
        self.draw_fullscreen(&pipeline, &target, &bg);
        Ok(GpuImage { tex: target, origin: [0.0; 2], size: out_size })
    }

    /// Runs a transition shader over `from` and `to` into a new image of `size` at `origin`.
    #[allow(clippy::too_many_arguments)]
    pub fn mix_pass(
        &mut self,
        shader: &oa_graph::EffectShader,
        origin: [f64; 2],
        size: [u32; 2],
        from: &GpuImage,
        to: &GpuImage,
        progress: f32,
        params: &[f32],
    ) -> Result<GpuImage, RenderError> {
        let key = format!("transition:{}", shader.entry);
        let pipeline = self.mix_pipelines.get(&key, self.wait, || working_spec(&key, shaders::transition(shader)))?;
        let target = self.target(size)?;
        let mut u = Uniforms::default().output(origin, size).input(from).params(0, params)?;
        u.0[14] = progress;
        self.two_input_pass(&pipeline, &target, u, from, to);
        Ok(GpuImage { tex: target, origin, size })
    }

    /// A fullscreen pass on the two-texture layout: `a` as the usual input, `b` at
    /// binding 2 with its origin and size in uniform slots 8–11.
    pub fn two_input_pass(&mut self, pipeline: &wgpu::RenderPipeline, target: &PooledTexture, mut u: Uniforms, a: &GpuImage, b: &GpuImage) {
        u.0[8] = b.origin[0] as f32;
        u.0[9] = b.origin[1] as f32;
        u.0[10] = b.size[0] as f32;
        u.0[11] = b.size[1] as f32;
        let buf = self.uniform_buffer(&u);
        let bg = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-two-input"),
            layout: &self.mix_pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&a.tex.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&b.tex.view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(self.sampler) },
            ],
        });
        self.draw_fullscreen(pipeline, target, &bg);
    }

    fn uniform_buffer(&mut self, u: &Uniforms) -> wgpu::Buffer {
        if self.uniforms_used == self.uniform_buffers.len() {
            self.uniform_buffers.push(self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("oa-pass-uniforms"),
                size: (UNIFORM_SLOTS * 4) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.uniform_buffers[self.uniforms_used].clone();
        self.uniforms_used += 1;
        self.ctx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(&u.0));
        buf
    }

    fn bind_group(&mut self, u: &Uniforms, input: Option<&GpuImage>) -> wgpu::BindGroup {
        self.bind_group_sampled(u, input, false)
    }

    /// `nearest`: sample the input without smoothing (pixel art).
    fn bind_group_sampled(&mut self, u: &Uniforms, input: Option<&GpuImage>, nearest: bool) -> wgpu::BindGroup {
        let sampler = if nearest { self.nearest } else { self.sampler };
        let buf = self.uniform_buffer(u);
        let input = input.unwrap_or(self.dummy);
        self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("oa-pass"),
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&input.tex.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// [`GpuServices::fullscreen_pass`] reading the input without smoothing when
    /// `nearest` (pixel art).
    pub fn sampled_pass(
        &mut self,
        pipeline: &wgpu::RenderPipeline,
        target: &PooledTexture,
        u: &Uniforms,
        input: Option<&GpuImage>,
        nearest: bool,
    ) {
        let bind = self.bind_group_sampled(u, input, nearest);
        self.draw_fullscreen(pipeline, target, &bind);
    }

    /// Runs a fullscreen pass into `target`, clearing it to transparent first.
    pub fn fullscreen_pass(
        &mut self,
        pipeline: &wgpu::RenderPipeline,
        target: &PooledTexture,
        uniforms: &Uniforms,
        input: Option<&GpuImage>,
    ) {
        let bg = self.bind_group(uniforms, input);
        self.draw_fullscreen(pipeline, target, &bg);
    }

    fn draw_fullscreen(&mut self, pipeline: &wgpu::RenderPipeline, target: &PooledTexture, bg: &wgpu::BindGroup) {
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oa-fullscreen"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bg, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.stats.passes += 1;
    }

    pub fn working_pipeline(&self, key: &str, fragment_source: impl FnOnce() -> String) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
        self.pipelines.get_blocking(key, || PipelineSpec {
            label: key.into(),
            source: fragment_source(),
            vertex_entry: "vs_full",
            fragment_entry: "fs_main",
            format: WORKING_FORMAT,
            blend: None,
        })
    }
}

struct CacheEntry {
    image: GpuImage,
    last_used: u64,
}

pub struct Renderer {
    ctx: Arc<GpuContext>,
    pipelines: Pipelines,
    yuv_pipelines: Pipelines,
    /// Two-input passes (transitions).
    mix_pipelines: Pipelines,
    pool: TexturePool,
    sampler: wgpu::Sampler,
    nearest: wgpu::Sampler,
    dummy: GpuImage,
    uniform_buffers: Vec<wgpu::Buffer>,
    text: crate::text::TextState,
    cache: HashMap<CacheKey, CacheEntry>,
    /// Keys seen once. A node is only worth caching when it comes back (scrubbing and
    /// playback produce a new key per frame, which would otherwise fill VRAM with
    /// entries nothing ever reads).
    seen: HashMap<CacheKey, u64>,
    frame: u64,
    pub options: RenderOptions,
    pub stats: RenderStats,
}

impl Renderer {
    pub fn new(ctx: Arc<GpuContext>, options: RenderOptions) -> Self {
        let pipelines = Pipelines::new(&ctx.device);
        let yuv_pipelines = yuv::pipelines(&ctx.device);
        let mix_pipelines = Pipelines::with_layout(&ctx.device, "oa-mix", &crate::pipelines::pass_layout_entries(2));
        let pool = TexturePool::default();
        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("oa-linear-clamp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let nearest = ctx.device.create_sampler(&wgpu::SamplerDescriptor { label: Some("oa-nearest-clamp"), ..Default::default() });
        let dummy = GpuImage { tex: pool.acquire(&ctx.device, [1, 1]), origin: [0.0; 2], size: [1, 1] };
        let text = crate::text::TextState::new(&ctx.device);
        Renderer {
            ctx,
            pipelines,
            yuv_pipelines,
            mix_pipelines,
            pool,
            sampler,
            nearest,
            dummy,
            uniform_buffers: Vec::new(),
            text,
            cache: HashMap::new(),
            seen: HashMap::new(),
            frame: 0,
            options,
            stats: RenderStats::default(),
        }
    }

    pub fn context(&self) -> &Arc<GpuContext> {
        &self.ctx
    }

    pub fn pipelines(&self) -> &Pipelines {
        &self.pipelines
    }

    /// Compiles the per-effect (unfused) pipelines for every registered effect, so the
    /// fallback path never stalls playback.
    pub fn warm_up(&self, registry: &Registry) -> Result<(), RenderError> {
        // Without `wait`, everything compiles in the background (interactive startup).
        let wait = self.options.wait;
        let ready = |r: Result<Arc<wgpu::RenderPipeline>, RenderError>| match r {
            Err(RenderError::NotReady) => Ok(()),
            r => r.map(|_| ()),
        };
        for d in registry.effects() {
            if let Some(shader) = &d.shader {
                match d.kind {
                    EffectKind::PointOp => {
                        let len = d.uniform_len();
                        let key = point_key(&[(shader.entry.as_str(), len)], d.space);
                        ready(self.pipelines.get(&key, wait, || working_spec(&key, shaders::point_chain(&[(shader, len)], d.space))))?;
                    }
                    EffectKind::Spatial { .. } | EffectKind::UvWarp => {
                        let len = d.uniform_len();
                        let key = spatial_key(&shader.entry, len, false);
                        ready(self.pipelines.get(&key, wait, || working_spec(&key, shaders::spatial(shader, len, false))))?;
                    }
                    _ => {}
                }
            }
        }
        for blend in BlendMode::ALL {
            layer_pipeline(&self.pipelines, blend)?;
        }
        // Plain text, and text with each text effect on its own.
        ready(crate::text::warm(&self.text, &[], wait))?;
        for d in registry.effects() {
            if let (Some(shader), Some(stage)) = (&d.shader, crate::text::stage(&d.kind)) {
                ready(crate::text::warm(&self.text, &[(shader, stage, d.uniform_len())], wait))?;
            }
        }
        Ok(())
    }

    /// Runs GPU work outside a graph render (color conversion for an encoder, say) and
    /// submits it. The images it returns stay valid while the caller holds them.
    pub fn run<R>(&mut self, f: impl FnOnce(&mut GpuServices<'_>) -> R) -> R {
        let encoder = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("oa-run") });
        let mut gpu = GpuServices {
            ctx: &self.ctx,
            pipelines: &self.pipelines,
            yuv_pipelines: &self.yuv_pipelines,
            mix_pipelines: &self.mix_pipelines,
            pool: &self.pool,
            sampler: &self.sampler,
            nearest: &self.nearest,
            dummy: &self.dummy,
            uniform_buffers: &mut self.uniform_buffers,
            uniforms_used: 0,
            encoder,
            stats: RenderStats::default(),
            wait: self.options.wait,
        };
        let result = f(&mut gpu);
        self.ctx.queue.submit([gpu.encoder.finish()]);
        result
    }

    /// Renders the graph's output. The returned image stays valid (and out of the pool)
    /// for as long as the caller holds it.
    pub fn render(&mut self, graph: &Graph, registry: &Registry, source: &mut dyn FrameSource) -> Result<GpuImage, RenderError> {
        let encoder = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("oa-frame") });
        self.frame += 1;
        let mut exec = Exec {
            gpu: GpuServices {
                ctx: &self.ctx,
                pipelines: &self.pipelines,
                yuv_pipelines: &self.yuv_pipelines,
                mix_pipelines: &self.mix_pipelines,
                pool: &self.pool,
                sampler: &self.sampler,
                nearest: &self.nearest,
                dummy: &self.dummy,
                uniform_buffers: &mut self.uniform_buffers,
                uniforms_used: 0,
                encoder,
                stats: RenderStats::default(),
                wait: self.options.wait,
            },
            graph,
            registry,
            source,
            memo: HashMap::new(),
            cache: &mut self.cache,
            seen: &mut self.seen,
            cache_enabled: self.options.cache_budget > 0,
            fusion: self.options.fusion,
            frame: self.frame,
            tainted: false,
            text: &mut self.text,
        };
        let result = exec.node(graph.output);
        let Exec { gpu, source, .. } = exec;
        let mut stats = gpu.stats;
        self.ctx.queue.submit([gpu.encoder.finish()]);
        source.submitted(&self.ctx.queue);
        let image = result?;

        self.evict_cache();
        // Forget sightings older than a few frames so the set stays small.
        let frame = self.frame;
        self.seen.retain(|_, last| frame.saturating_sub(*last) < 4);
        // An allocation failed somewhere: give everything reclaimable back at once.
        if self.ctx.health.take_out_of_memory() {
            self.clear_cache();
            self.pool.end_frame(0);
        }
        self.pool.end_frame(self.options.keep_idle_frames);
        stats.memory = self.settle_memory();
        let (textures, bytes, _) = self.pool.stats();
        stats.pool_textures = textures;
        stats.pool_bytes = bytes;
        stats.cache_bytes = self.cache.values().map(|e| e.image.tex.bytes()).sum();
        self.stats = stats;
        Ok(image)
    }

    /// What the renderer is holding against its budget, after giving back whatever it
    /// can: idle textures first, then the oldest cache entries. At `Pressure::Over` the
    /// host should render the preview smaller — there's nothing left to free.
    fn settle_memory(&mut self) -> crate::health::Memory {
        use crate::health::{Memory, Pressure};
        let budget = self.options.vram_budget;
        let cached = |c: &HashMap<CacheKey, CacheEntry>| c.values().map(|e| e.image.tex.bytes()).sum::<u64>();
        let mut memory = Memory::of(self.pool.stats().1, cached(&self.cache), budget);
        if memory.pressure == Pressure::Easy || budget == 0 {
            return memory;
        }
        // Textures nothing is using go back now rather than in a few frames.
        self.pool.end_frame(0);
        // Then the cache, down to whatever room is left under the budget.
        let pool_bytes = self.pool.stats().1;
        let room = budget.saturating_sub(pool_bytes).min(self.options.cache_budget);
        self.evict_to(room);
        memory = Memory::of(pool_bytes, cached(&self.cache), budget);
        memory
    }

    /// What the renderer holds, and how close that is to the budget.
    pub fn memory(&self) -> crate::health::Memory {
        crate::health::Memory::of(
            self.pool.stats().1,
            self.cache.values().map(|e| e.image.tex.bytes()).sum(),
            self.options.vram_budget,
        )
    }

    /// The device's own report: loss, errors, failed allocations.
    pub fn health(&self) -> &Arc<crate::health::GpuHealth> {
        &self.ctx.health
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.seen.clear();
    }

    fn evict_cache(&mut self) {
        self.evict_to(self.options.cache_budget);
    }

    /// Drops the least recently used entries until the cache fits in `budget`.
    fn evict_to(&mut self, budget: u64) {
        let mut total: u64 = self.cache.values().map(|e| e.image.tex.bytes()).sum();
        if total <= budget {
            return;
        }
        let mut entries: Vec<_> = self.cache.iter().map(|(k, e)| (e.last_used, *k, e.image.tex.bytes())).collect();
        entries.sort_by_key(|e| e.0);
        for (_, key, bytes) in entries {
            if total <= budget {
                break;
            }
            self.cache.remove(&key);
            total -= bytes;
        }
    }
}

fn working_spec(label: &str, source: String) -> PipelineSpec {
    PipelineSpec {
        label: label.into(),
        source,
        vertex_entry: "vs_full",
        fragment_entry: "fs_main",
        format: WORKING_FORMAT,
        blend: None,
    }
}

fn spatial_key(entry: &str, len: usize, media: bool) -> String {
    format!("spatial{}:{entry}/{len}", if media { "+media" } else { "" })
}

fn point_key(chain: &[(&str, usize)], space: WorkingSpace) -> String {
    let parts: Vec<String> = chain.iter().map(|(e, n)| format!("{e}/{n}")).collect();
    format!("point:{space:?}:{}", parts.join("+"))
}

fn layer_pipeline(pipelines: &Pipelines, blend: BlendMode) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
    use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
    let over_alpha = C { src_factor: F::One, dst_factor: F::OneMinusSrcAlpha, operation: O::Add };
    let color = match blend {
        BlendMode::Normal => over_alpha,
        BlendMode::Add => C { src_factor: F::One, dst_factor: F::One, operation: O::Add },
        BlendMode::Screen => C { src_factor: F::One, dst_factor: F::OneMinusSrc, operation: O::Add },
        // src·dst + dst·(1 − αs): exact over an opaque backdrop.
        BlendMode::Multiply => C { src_factor: F::Dst, dst_factor: F::OneMinusSrcAlpha, operation: O::Add },
        // min(layer over white, dst) / max(layer over black, dst): exact where the layer
        // is opaque, no change where it's transparent.
        BlendMode::Darken => C { src_factor: F::One, dst_factor: F::One, operation: O::Min },
        BlendMode::Lighten => C { src_factor: F::One, dst_factor: F::One, operation: O::Max },
    };
    let key = format!("layer:{blend:?}");
    pipelines.get_blocking(&key, || PipelineSpec {
        label: key.clone(),
        source: shaders::layer(blend == BlendMode::Darken),
        vertex_entry: "vs_layer",
        fragment_entry: "fs_layer",
        format: WORKING_FORMAT,
        blend: Some(wgpu::BlendState { color, alpha: over_alpha }),
    })
}

struct Exec<'a> {
    gpu: GpuServices<'a>,
    graph: &'a Graph,
    registry: &'a Registry,
    source: &'a mut dyn FrameSource,
    memo: HashMap<NodeId, GpuImage>,
    cache: &'a mut HashMap<CacheKey, CacheEntry>,
    seen: &'a mut HashMap<CacheKey, u64>,
    cache_enabled: bool,
    fusion: FusionMode,
    frame: u64,
    /// Set once a source serves a stand-in: nothing rendered after that is cached (it may
    /// be built on the stand-in).
    tainted: bool,
    text: &'a mut crate::text::TextState,
}

impl Exec<'_> {
    fn node(&mut self, id: NodeId) -> Result<GpuImage, RenderError> {
        if let Some(img) = self.memo.get(&id) {
            return Ok(img.clone());
        }
        let key = self.graph.node(id).key.filter(|_| self.cache_enabled);
        if let Some(entry) = key.and_then(|k| self.cache.get_mut(&k)) {
            entry.last_used = self.frame;
            self.gpu.stats.cache_hits += 1;
            let img = entry.image.clone();
            self.memo.insert(id, img.clone());
            return Ok(img);
        }
        let img = self.execute(id)?;
        if let Some(k) = key.filter(|_| !self.tainted) {
            // Second sighting: worth keeping. First sighting: just remember the key.
            if self.seen.insert(k, self.frame).is_some() {
                self.cache.insert(k, CacheEntry { image: img.clone(), last_used: self.frame });
            }
        }
        self.memo.insert(id, img.clone());
        Ok(img)
    }

    fn execute(&mut self, id: NodeId) -> Result<GpuImage, RenderError> {
        let node = self.graph.node(id);
        match &node.op {
            NodeOp::Source { media, fingerprint, source_time, rep, size, yuv, .. } => {
                let req = SourceRequest { media: *media, fingerprint: fingerprint.clone(), source_time: *source_time, rep: *rep, size: *size, yuv: *yuv };
                let img = self.source.frame(&mut self.gpu, &req)?;
                if !self.source.exact() {
                    self.tainted = true;
                }
                if img.size != req.size {
                    return Err(RenderError::Source(format!("asked for {:?}, got {:?}", req.size, img.size)));
                }
                Ok(img)
            }
            NodeOp::Solid { color, size } => {
                let size = [size[0].round().max(1.0) as u32, size[1].round().max(1.0) as u32];
                let target = self.gpu.target(size)?;
                let pipeline = self.gpu.working_pipeline("solid", shaders::solid)?;
                let u = Uniforms::default().output([0.0; 2], size).params(0, color)?;
                self.gpu.fullscreen_pass(&pipeline, &target, &u, None);
                Ok(GpuImage { tex: target, origin: [0.0; 2], size })
            }
            NodeOp::Effect { type_id, stateful, uniforms, nearest, .. } => {
                let input = self.node(node.inputs[0])?;
                // A media parameter's picture (a mask's matte…) arrives as a second input.
                let media = node.inputs.get(1).map(|&m| self.node(m)).transpose()?;
                let descriptor = self.registry.effect(type_id).cloned();
                match descriptor {
                    Some(d) if !*stateful && d.shader.is_some() => self.effect(id, &d, uniforms, input, media, *nearest),
                    _ => {
                        let why = if *stateful { "stateful effects not yet executed" } else { "no GPU shader" };
                        self.gpu.stats.unsupported.push(format!("{type_id}: {why}"));
                        Ok(input)
                    }
                }
            }
            NodeOp::FusedPointOps { space, chain, nearest } => {
                let (space, nearest) = (*space, *nearest);
                let input = self.node(node.inputs[0])?;
                self.fused(space, chain, input, nearest)
            }
            NodeOp::Composite { size, background, layers } => {
                let target = self.gpu.target(*size)?;
                let mut draws = Vec::with_capacity(layers.len());
                for (info, &input) in layers.iter().zip(&node.inputs) {
                    let (img, matrix) = match &self.graph.node(input).op {
                        NodeOp::Transform { matrix } => (self.node(self.graph.node(input).inputs[0])?, matrix.m),
                        _ => (self.node(input)?, oa_graph::Affine2::IDENTITY.m),
                    };
                    let pipeline = layer_pipeline(self.gpu.pipelines, info.blend)?;
                    let u = Uniforms::default().output([0.0; 2], *size).input(&img).affine(matrix, info.opacity);
                    let bg = self.gpu.bind_group_sampled(&u, Some(&img), info.pixelated);
                    draws.push((pipeline, bg, img));
                }
                let [r, g, b, a] = background.map(|c| c as f64);
                let mut pass = self.gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("oa-composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: r * a, g: g * a, b: b * a, a }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                for (pipeline, bg, _) in &draws {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, bg, &[]);
                    pass.draw(0..6, 0..1);
                }
                drop(pass);
                self.gpu.stats.passes += 1;
                Ok(GpuImage { tex: target, origin: [0.0; 2], size: *size })
            }
            // A transform is normally drawn straight into the composite it feeds (above).
            // Used anywhere else — an effect's input — it's drawn into an image of its
            // own, covering its bounds, rather than failing the frame.
            NodeOp::Transform { matrix } => {
                let img = self.node(node.inputs[0])?;
                let b = node.bounds;
                let origin = [b.x0.floor(), b.y0.floor()];
                let size = [((b.x1 - origin[0]).ceil().max(1.0)) as u32, ((b.y1 - origin[1]).ceil().max(1.0)) as u32];
                let target = self.gpu.target(size)?;
                // The layer shader places by the matrix alone: move the bounds' corner to 0,0.
                let m = matrix.then(&oa_graph::Affine2::translate(-origin[0], -origin[1]));
                let pipeline = layer_pipeline(self.gpu.pipelines, BlendMode::Normal)?;
                let u = Uniforms::default().output([0.0; 2], size).input(&img).affine(m.m, 1.0);
                let bg = self.gpu.bind_group_sampled(&u, Some(&img), false);
                let mut pass = self.gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("oa-transform"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.draw(0..6, 0..1);
                drop(pass);
                self.gpu.stats.passes += 1;
                Ok(GpuImage { tex: target, origin, size })
            }
            NodeOp::Text { spec, scale, style, spoken_style, spoken_word, chain } => {
                let b = node.bounds;
                let draw = crate::text::TextDraw {
                    spec,
                    scale: *scale,
                    style,
                    spoken_style,
                    spoken_word: *spoken_word,
                    chain,
                    origin: [b.x0, b.y0],
                    size: [(b.x1 - b.x0).round().max(1.0) as u32, (b.y1 - b.y0).round().max(1.0) as u32],
                };
                self.gpu.text_pass(self.text, self.registry, &draw)
            }
            NodeOp::Transition { type_id, uniforms, progress, .. } => {
                let from = self.node(node.inputs[0])?;
                let to = self.node(node.inputs[1])?;
                let shader = self.registry.effect(type_id).and_then(|d| d.shader.clone());
                let Some(shader) = shader else {
                    // Unknown transition: cut at the midpoint rather than failing the frame.
                    self.gpu.stats.unsupported.push(format!("{type_id}: no GPU shader"));
                    return Ok(if *progress < 0.5 { from } else { to });
                };
                let b = node.bounds;
                let (origin, size) = ([b.x0, b.y0], [(b.x1 - b.x0).round().max(1.0) as u32, (b.y1 - b.y0).round().max(1.0) as u32]);
                self.gpu.mix_pass(&shader, origin, size, &from, &to, *progress, uniforms)
            }
        }
    }

    /// `nearest`: the layer is pixel art, so the shader reads whole picture pixels.
    fn effect(&mut self, id: NodeId, d: &EffectDescriptor, uniforms: &[f32], input: GpuImage, media: Option<GpuImage>, nearest: bool) -> Result<GpuImage, RenderError> {
        let shader = d.shader.as_ref().expect("checked by caller");
        match d.kind {
            EffectKind::PointOp => {
                let key = point_key(&[(shader.entry.as_str(), uniforms.len())], d.space);
                let pipeline = self
                    .gpu
                    .pipelines
                    .get(&key, self.gpu.wait, || working_spec(&key, shaders::point_chain(&[(shader, uniforms.len())], d.space)))?;
                self.point_pass(&pipeline, uniforms, input, nearest)
            }
            EffectKind::Spatial { .. } | EffectKind::UvWarp => {
                let len = uniforms.len();
                let with_media = media.is_some();
                let key = spatial_key(&shader.entry, len, with_media);
                let pipelines = if with_media { self.gpu.mix_pipelines } else { self.gpu.pipelines };
                let pipeline = pipelines.get(&key, self.gpu.wait, || working_spec(&key, shaders::spatial(shader, len, with_media)))?;
                // Output grows by however much the planner expanded the bounds.
                let node = self.graph.node(id);
                let in_bounds = self.graph.node(node.inputs[0]).bounds;
                // Each side on its own (a blur grows evenly; a tile fills a whole canvas).
                let b = node.bounds;
                let grow = |d: f64| d.max(0.0).ceil();
                let (left, top) = (grow(in_bounds.x0 - b.x0), grow(in_bounds.y0 - b.y0));
                let (right, bottom) = (grow(b.x1 - in_bounds.x1), grow(b.y1 - in_bounds.y1));
                let origin = [input.origin[0] - left, input.origin[1] - top];
                let size = [input.size[0] + (left + right) as u32, input.size[1] + (top + bottom) as u32];
                let mut current = input;
                let passes = d.pass_count.map_or(shader.passes, |count| count(uniforms)).clamp(1, 32);
                for pass in 0..passes {
                    // A pass may run on a coarser grid (its target divided); the layer area it
                    // covers is the same, the shader works out its cells.
                    let div = d.pass_divisor.map_or(1, |f| f(uniforms, pass)).clamp(1, 64);
                    let pass_size = [size[0].div_ceil(div).max(1), size[1].div_ceil(div).max(1)];
                    let target = self.gpu.target(pass_size)?;
                    let u = Uniforms::default().output(origin, pass_size).input(&current).pass_index(pass).params(0, uniforms)?;
                    match &media {
                        Some(m) => self.gpu.two_input_pass(&pipeline, &target, u, &current, m),
                        None => self.gpu.sampled_pass(&pipeline, &target, &u, Some(&current), nearest),
                    }
                    current = GpuImage { tex: target, origin, size: pass_size };
                }
                Ok(current)
            }
            _ => {
                self.gpu.stats.unsupported.push(format!("{}: {:?} effects not yet executed", d.type_id, d.kind));
                Ok(input)
            }
        }
    }

    fn point_pass(&mut self, pipeline: &wgpu::RenderPipeline, uniforms: &[f32], input: GpuImage, nearest: bool) -> Result<GpuImage, RenderError> {
        let target = self.gpu.target(input.size)?;
        let u = Uniforms::default().output(input.origin, input.size).input(&input).params(0, uniforms)?;
        self.gpu.sampled_pass(pipeline, &target, &u, Some(&input), nearest);
        Ok(GpuImage { tex: target, origin: input.origin, size: input.size })
    }

    fn fused(&mut self, space: WorkingSpace, chain: &[(Arc<str>, u32, Vec<f32>)], input: GpuImage, nearest: bool) -> Result<GpuImage, RenderError> {
        let mut shaders_in_chain = Vec::with_capacity(chain.len());
        for (type_id, _, uniforms) in chain {
            match self.registry.effect(type_id).and_then(|d| d.shader.clone()) {
                Some(s) => shaders_in_chain.push((s, uniforms.len())),
                None => {
                    self.gpu.stats.unsupported.push(format!("{type_id}: no GPU shader"));
                    return self.unfused(space, chain, input, nearest);
                }
            }
        }
        let total: usize = chain.iter().map(|c| c.2.len()).sum();
        if total > UNIFORM_SLOTS - PARAM_BASE {
            return self.unfused(space, chain, input, nearest);
        }
        let key = point_key(&shaders_in_chain.iter().map(|(s, n)| (s.entry.as_str(), *n)).collect::<Vec<_>>(), space);
        let spec = || {
            let refs: Vec<_> = shaders_in_chain.iter().map(|(s, n)| (s, *n)).collect();
            working_spec(&key, shaders::point_chain(&refs, space))
        };
        let pipeline = match self.fusion {
            FusionMode::Off => None,
            FusionMode::Blocking => self.gpu.pipelines.get_blocking(&key, spec).ok(),
            FusionMode::Async => self.gpu.pipelines.get_async(&key, spec),
        };
        let Some(pipeline) = pipeline else {
            self.gpu.stats.fallback_chains += 1;
            return self.unfused(space, chain, input, nearest);
        };
        let flat: Vec<f32> = chain.iter().flat_map(|c| c.2.iter().copied()).collect();
        self.gpu.stats.fused_chains += 1;
        self.point_pass(&pipeline, &flat, input, nearest)
    }

    /// The reference path for a fused chain: one pass per effect.
    fn unfused(&mut self, space: WorkingSpace, chain: &[(Arc<str>, u32, Vec<f32>)], mut img: GpuImage, nearest: bool) -> Result<GpuImage, RenderError> {
        for (type_id, _, uniforms) in chain {
            let Some(shader) = self.registry.effect(type_id).and_then(|d| d.shader.clone()) else { continue };
            let key = point_key(&[(shader.entry.as_str(), uniforms.len())], space);
            let pipeline = self
                .gpu
                .pipelines
                .get(&key, self.gpu.wait, || working_spec(&key, shaders::point_chain(&[(&shader, uniforms.len())], space)))?;
            img = self.point_pass(&pipeline, uniforms, img, nearest)?;
        }
        Ok(img)
    }
}
