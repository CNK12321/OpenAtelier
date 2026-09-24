use crate::health::GpuHealth;
use crate::RenderError;
use std::sync::Arc;

pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    /// Errors and device loss reported by the driver (see [`crate::health`]).
    pub health: Arc<GpuHealth>,
}

/// Which GPU to use, when there's a choice: a graphics API and part of an adapter's name.
/// Empty means automatic. `OA_GPU_BACKEND` (`dx12`, `vulkan`, `metal`, `gl`) and
/// `OA_GPU_ADAPTER` (part of a name) override what's passed in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuPreference {
    pub backend: Option<wgpu::Backend>,
    pub adapter: Option<String>,
}

impl GpuPreference {
    /// From the settings' strings ("auto", "dx12", "vulkan", "metal", "gl"; a name or ""),
    /// with the environment winning.
    pub fn new(backend: &str, adapter: &str) -> Self {
        let env_backend = std::env::var("OA_GPU_BACKEND").ok();
        let env_adapter = std::env::var("OA_GPU_ADAPTER").ok();
        let backend = parse_backend(env_backend.as_deref().unwrap_or(backend));
        let adapter = env_adapter.or_else(|| Some(adapter.to_string())).map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        GpuPreference { backend, adapter }
    }

    /// The graphics APIs worth trying on this platform (or just the one asked for).
    pub fn backends(&self) -> wgpu::Backends {
        match self.backend {
            Some(b) => wgpu::Backends::from(b),
            None if cfg!(windows) => wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL,
            None if cfg!(target_os = "macos") => wgpu::Backends::METAL,
            None => wgpu::Backends::VULKAN | wgpu::Backends::GL,
        }
    }
}

/// `dx12`, `vulkan`, `metal`, `gl` (anything else: automatic).
pub fn parse_backend(s: &str) -> Option<wgpu::Backend> {
    match s.trim().to_ascii_lowercase().as_str() {
        "dx12" | "d3d12" => Some(wgpu::Backend::Dx12),
        "vulkan" | "vk" => Some(wgpu::Backend::Vulkan),
        "metal" => Some(wgpu::Backend::Metal),
        "gl" | "opengl" | "gles" => Some(wgpu::Backend::Gl),
        _ => None,
    }
}

/// Why `adapter` can't run the renderer, if it can't: half-float render targets that
/// filter and blend (the working format), and storage buffers in vertex shaders (text).
pub fn shortcomings(adapter: &wgpu::Adapter) -> Vec<String> {
    let mut out = Vec::new();
    let f = adapter.get_texture_format_features(crate::WORKING_FORMAT);
    if !f.allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING) {
        out.push("can't render to half-float textures".into());
    }
    if !f.flags.contains(wgpu::TextureFormatFeatureFlags::FILTERABLE | wgpu::TextureFormatFeatureFlags::BLENDABLE) {
        out.push("can't filter or blend half-float textures".into());
    }
    let down = adapter.get_downlevel_capabilities();
    if !down.flags.contains(wgpu::DownlevelFlags::VERTEX_STORAGE) || adapter.limits().max_storage_buffers_per_shader_stage < 2 {
        out.push("no storage buffers in vertex shaders (titles can't be drawn)".into());
    }
    out
}

/// How good a choice `adapter` is: a real GPU over software, a dedicated one over an
/// integrated one, then the API that suits the platform best (DX12 on Windows: decoded
/// video is shared with it without a copy).
fn score(adapter: &wgpu::Adapter, pref: &GpuPreference) -> i64 {
    let info = adapter.get_info();
    let kind = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 4,
        wgpu::DeviceType::IntegratedGpu => 3,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 1,
        wgpu::DeviceType::Cpu => 0,
    };
    let api = match info.backend {
        wgpu::Backend::Dx12 if cfg!(windows) => 3,
        wgpu::Backend::Metal => 3,
        wgpu::Backend::Vulkan => 2,
        wgpu::Backend::Gl => 1,
        _ => 0,
    };
    let named = pref.adapter.as_ref().is_some_and(|n| info.name.to_lowercase().contains(&n.to_lowercase()));
    let capable = shortcomings(adapter).is_empty();
    (named as i64) * 10_000 + (capable as i64) * 1_000 + kind * 10 + api
}

/// The best of `adapters` for `pref` (see [`score`]); adapters that fall short are only
/// taken when nothing else is there.
pub fn choose(adapters: &[wgpu::Adapter], pref: &GpuPreference) -> Option<wgpu::Adapter> {
    adapters.iter().filter(|a| pref.backend.is_none_or(|b| a.get_info().backend == b)).max_by_key(|a| score(a, pref)).cloned()
}

/// What to ask of the device: all the adapter allows (large canvases where the hardware
/// can), NV12 textures where there are any (zero-copy hardware decode).
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("oa-gpu"),
        required_features: adapter.features() & wgpu::Features::TEXTURE_FORMAT_NV12,
        required_limits: adapter.limits(),
        ..Default::default()
    }
}

/// One line about the GPU in use: "NVIDIA GeForce RTX 3070 (Vulkan, discrete)".
pub fn describe(info: &wgpu::AdapterInfo) -> String {
    let api = match info.backend {
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Gl => "OpenGL",
        _ => "other",
    };
    let kind = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => "dedicated",
        wgpu::DeviceType::IntegratedGpu => "integrated",
        wgpu::DeviceType::VirtualGpu => "virtual",
        wgpu::DeviceType::Cpu => "software",
        wgpu::DeviceType::Other => "other",
    };
    format!("{} ({api}, {kind})", info.name)
}

impl GpuContext {
    /// A device with no window, for rendering, export and tests, chosen by
    /// [`GpuPreference`] from the environment (see [`GpuContext::new_preferred`]).
    pub fn new_headless() -> Result<Self, RenderError> {
        Self::new_preferred(&GpuPreference::new("auto", ""))
    }

    /// The best adapter for `pref` among every API this platform has, falling back to a
    /// software renderer (WARP, llvmpipe, lavapipe) so there's always a picture; if a
    /// device can't be made with the adapter's full limits, the conservative defaults are
    /// tried.
    pub fn new_preferred(pref: &GpuPreference) -> Result<Self, RenderError> {
        pollster::block_on(async {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = pref.backends();
            let instance = wgpu::Instance::new(desc);
            let adapters = instance.enumerate_adapters(pref.backends()).await;
            let adapter = match choose(&adapters, pref) {
                Some(a) => a,
                None => instance
                    .request_adapter(&wgpu::RequestAdapterOptions { force_fallback_adapter: true, ..Default::default() })
                    .await
                    .map_err(|e| RenderError::NoAdapter(format!("no GPU found, and no software renderer either: {e}")))?,
            };
            Self::open(instance, adapter).await
        })
    }

    async fn open(instance: wgpu::Instance, adapter: wgpu::Adapter) -> Result<Self, RenderError> {
        let (device, queue) = match adapter.request_device(&device_descriptor(&adapter)).await {
            Ok(dq) => dq,
            Err(first) => {
                let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()).using_alignment(adapter.limits());
                let desc = wgpu::DeviceDescriptor { label: Some("oa-gpu"), required_limits: limits, ..Default::default() };
                adapter.request_device(&desc).await.map_err(|e| RenderError::Device(format!("{first}; with lower limits: {e}")))?
            }
        };
        Ok(Self::from_parts(instance, adapter, device, queue))
    }

    /// Wraps a device made elsewhere (the window's, made by eframe with
    /// [`device_descriptor`]).
    pub fn from_parts(instance: wgpu::Instance, adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let info = adapter.get_info();
        let health = Arc::new(GpuHealth::default());
        health.watch(&device);
        GpuContext { instance, adapter, device, queue, info, health }
    }

    /// A device on exactly `backends` (tests and tools pinning one API).
    pub fn new_headless_with(backends: wgpu::Backends) -> Result<Self, RenderError> {
        let backend = [wgpu::Backend::Dx12, wgpu::Backend::Vulkan, wgpu::Backend::Metal, wgpu::Backend::Gl].into_iter().find(|b| backends.contains(wgpu::Backends::from(*b)));
        Self::new_preferred(&GpuPreference { backend, adapter: None })
    }

    pub fn is_dx12(&self) -> bool {
        self.info.backend == wgpu::Backend::Dx12
    }

    pub fn supports_nv12(&self) -> bool {
        self.device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12)
    }

    pub fn max_texture_size(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// "NVIDIA GeForce RTX 3070 (Vulkan, dedicated)".
    pub fn describe(&self) -> String {
        describe(&self.info)
    }
}
