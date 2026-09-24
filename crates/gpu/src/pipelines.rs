//! Pipeline cache with optional background compilation.
//!
//! Compiling a pipeline can take 50–500 ms on some backends. Fused pipelines are new
//! combinations the user creates while editing, so they compile on a worker thread;
//! until they're ready the renderer runs the equivalent unfused chain.

use crate::RenderError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PipelineSpec {
    pub label: String,
    pub source: String,
    pub vertex_entry: &'static str,
    pub fragment_entry: &'static str,
    pub format: wgpu::TextureFormat,
    pub blend: Option<wgpu::BlendState>,
}

enum Slot {
    Ready(Arc<wgpu::RenderPipeline>),
    Pending,
    Failed(String),
}

struct Inner {
    device: wgpu::Device,
    layout: wgpu::PipelineLayout,
    slots: Mutex<HashMap<String, Slot>>,
}

#[derive(Clone)]
pub struct Pipelines {
    inner: Arc<Inner>,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

/// Bind group layout entries: a uniform block at binding 0, then `textures` filterable
/// 2D float textures, then one filtering sampler.
pub fn pass_layout_entries(textures: u32) -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut entries = vec![wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }];
    for i in 0..textures {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 1 + i,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
    }
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 1 + textures,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    });
    entries
}

impl Pipelines {
    /// The standard pass layout: uniforms, one input texture, sampler.
    pub fn new(device: &wgpu::Device) -> Self {
        Self::with_layout(device, "oa-pass", &pass_layout_entries(1))
    }

    pub fn with_layout(device: &wgpu::Device, label: &str, entries: &[wgpu::BindGroupLayoutEntry]) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        Pipelines {
            inner: Arc::new(Inner { device: device.clone(), layout, slots: Mutex::new(HashMap::new()) }),
            bind_group_layout,
        }
    }

    /// Returns the pipeline, compiling it on this thread if needed.
    pub fn get_blocking(&self, key: &str, spec: impl FnOnce() -> PipelineSpec) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
        loop {
            {
                let mut slots = self.inner.slots.lock().unwrap_or_else(|e| e.into_inner());
                match slots.get(key) {
                    Some(Slot::Ready(p)) => return Ok(p.clone()),
                    Some(Slot::Failed(e)) => {
                        return Err(RenderError::Pipeline { label: key.into(), message: e.clone() });
                    }
                    Some(Slot::Pending) => {}
                    None => {
                        slots.insert(key.into(), Slot::Pending);
                        drop(slots);
                        let spec = spec();
                        let result = build(&self.inner, &spec);
                        return finish(&self.inner, key, result);
                    }
                }
            }
            // Another thread is compiling it.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Returns the pipeline if it's ready; otherwise starts a background compile (once)
    /// and returns `None`. Failed compiles also return `None` so callers fall back.
    pub fn get_async(&self, key: &str, spec: impl FnOnce() -> PipelineSpec) -> Option<Arc<wgpu::RenderPipeline>> {
        let mut slots = self.inner.slots.lock().unwrap_or_else(|e| e.into_inner());
        match slots.get(key) {
            Some(Slot::Ready(p)) => Some(p.clone()),
            Some(Slot::Pending) | Some(Slot::Failed(_)) => None,
            None => {
                slots.insert(key.into(), Slot::Pending);
                drop(slots);
                let (inner, key, spec) = (self.inner.clone(), key.to_string(), spec());
                std::thread::spawn(move || {
                    let result = build(&inner, &spec);
                    let _ = finish(&inner, &key, result);
                });
                None
            }
        }
    }

    /// Blocks until no background compiles are pending (tests, export start).
    /// `get_blocking` when `wait`, otherwise starts compiling in the background and
    /// reports [`RenderError::NotReady`] until it's done (a failed build is an error
    /// either way). Interactive callers keep showing what they had and ask again.
    pub fn get(&self, key: &str, wait: bool, spec: impl FnOnce() -> PipelineSpec) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
        if wait {
            return self.get_blocking(key, spec);
        }
        if let Some(p) = self.get_async(key, spec) {
            return Ok(p);
        }
        match self.inner.slots.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
            Some(Slot::Failed(e)) => Err(RenderError::Pipeline { label: key.into(), message: e.clone() }),
            _ => Err(RenderError::NotReady),
        }
    }

    pub fn wait_idle(&self) {
        while self.inner.slots.lock().unwrap_or_else(|e| e.into_inner()).values().any(|s| matches!(s, Slot::Pending)) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub fn failures(&self) -> Vec<(String, String)> {
        let slots = self.inner.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Failed(e) => Some((k.clone(), e.clone())),
                _ => None,
            })
            .collect()
    }
}

fn finish(inner: &Inner, key: &str, result: Result<wgpu::RenderPipeline, String>) -> Result<Arc<wgpu::RenderPipeline>, RenderError> {
    let mut slots = inner.slots.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(p) => {
            let p = Arc::new(p);
            slots.insert(key.into(), Slot::Ready(p.clone()));
            Ok(p)
        }
        Err(e) => {
            slots.insert(key.into(), Slot::Failed(e.clone()));
            Err(RenderError::Pipeline { label: key.into(), message: e })
        }
    }
}

fn build(inner: &Inner, spec: &PipelineSpec) -> Result<wgpu::RenderPipeline, String> {
    let device = &inner.device;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&spec.label),
        source: wgpu::ShaderSource::Wgsl(spec.source.as_str().into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&spec.label),
        layout: Some(&inner.layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some(spec.vertex_entry),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(spec.fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: spec.format,
                blend: spec.blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(pipeline),
    }
}
