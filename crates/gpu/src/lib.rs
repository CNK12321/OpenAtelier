//! GPU executor: runs a render graph on the GPU with wgpu.
//!
//! GPU-first rules:
//! * Every node produces a [`GpuImage`] — a texture plus where its texel (0,0) sits in
//!   layer space. Frames never touch CPU memory between nodes.
//! * [`FrameSource`]s (decoders, generators) hand back GPU images too. CPU readback
//!   exists only at explicit edges ([`readback`]).
//! * Working format: `Rgba16Float`, scene-linear, premultiplied alpha.
//!
//! Executor contract with the planner/optimizer:
//! * `Transform` nodes are consumed by `Composite` layers (drawn directly into the
//!   composite target — no intermediate texture). A standalone `Transform` is an error.
//! * Stateful effects and effects without a shader render as passthrough and are
//!   reported in [`RenderStats::unsupported`].

mod context;
pub mod health;
mod pipelines;
mod pool;
pub mod readback;
mod renderer;
mod shaders;
mod test_pattern;
pub mod text;
mod yuv;

pub use context::{GpuContext, GpuPreference};
/// Choosing and describing adapters (shared with the window, which makes its own device).
pub mod select {
    pub use crate::context::{choose, describe, device_descriptor, parse_backend, shortcomings};
}
pub use health::{GpuHealth, Memory, Pressure};
pub use pipelines::{pass_layout_entries, PipelineSpec, Pipelines};
pub use pool::{PooledTexture, TexturePool, WORKING_FORMAT};
pub use shaders::Nv12Plane;
pub use renderer::{FusionMode, GpuServices, RenderOptions, RenderStats, Renderer, Uniforms};
pub use test_pattern::TestPatternSource;
pub use yuv::{Nv12Frame, TransferFn, VideoColor, YuvMatrix};

use oa_graph::Representation;
use oa_time::Time;
use std::fmt;
use std::sync::Arc;

/// A texture in the working format plus its placement in layer space.
#[derive(Clone)]
pub struct GpuImage {
    pub tex: Arc<PooledTexture>,
    /// Layer-space position of texel (0, 0)'s corner.
    pub origin: [f64; 2],
    pub size: [u32; 2],
}

impl fmt::Debug for GpuImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GpuImage({}x{} @ {:?})", self.size[0], self.size[1], self.origin)
    }
}

/// What a [`FrameSource`] is asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceRequest {
    pub media: u64,
    pub fingerprint: Option<Arc<str>>,
    pub source_time: Time,
    pub rep: Representation,
    /// Exact texture size the graph expects (already scaled by decode scale).
    pub size: [u32; 2],
    /// YCbCr overrides `[matrix, range]` for video (see `NodeOp::Source::yuv`).
    pub yuv: [u8; 2],
}

/// Produces media frames as GPU images. Hardware decoders implement this by wrapping
/// their output surfaces; the test pattern renders a shader.
pub trait FrameSource {
    fn frame(&mut self, gpu: &mut GpuServices<'_>, req: &SourceRequest) -> Result<GpuImage, RenderError>;

    /// One line about what this source has been doing, for status displays.
    fn status(&self) -> Option<String> {
        None
    }

    /// Called right after the GPU work that used this frame's images was submitted.
    /// Sources that lend out their own textures (e.g. decoder surfaces) can use
    /// `queue.on_submitted_work_done` to learn when those textures are free again.
    fn submitted(&mut self, _queue: &wgpu::Queue) {}

    /// Whether the last `frame` call returned exactly the frame asked for. Sources that
    /// answer instantly with a stand-in while the real frame decodes (scrubbing) return
    /// false; the renderer then caches nothing built on it.
    fn exact(&self) -> bool {
        true
    }

    /// False if any stand-ins were served since the last call: render again shortly and
    /// the exact frames will be there.
    fn settled(&mut self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    NoAdapter(String),
    Device(String),
    Pipeline { label: String, message: String },
    Unsupported(String),
    TooLarge([u32; 2]),
    Source(String),
    Readback(String),
    /// Something the frame needs (a shader, glyphs) is still being prepared in the
    /// background; nothing was drawn. Only when not waiting (`RenderOptions::wait`).
    NotReady,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::NoAdapter(e) => write!(f, "no GPU adapter: {e}"),
            RenderError::Device(e) => write!(f, "GPU device error: {e}"),
            RenderError::Pipeline { label, message } => write!(f, "pipeline {label} failed: {message}"),
            RenderError::Unsupported(e) => write!(f, "unsupported graph: {e}"),
            RenderError::TooLarge(s) => write!(f, "texture {}x{} exceeds GPU limits", s[0], s[1]),
            RenderError::Source(e) => write!(f, "frame source: {e}"),
            RenderError::Readback(e) => write!(f, "readback: {e}"),
            RenderError::NotReady => write!(f, "still preparing"),
        }
    }
}

impl std::error::Error for RenderError {}
