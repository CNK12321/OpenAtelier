//! Media Foundation hardware decoding into wgpu (DX12) textures.
//!
//! Frame path, all on the GPU:
//! 1. MF (DXVA) decodes into NV12 surfaces of a D3D11 device created on the *same
//!    adapter* as the wgpu device (matched by LUID).
//! 2. For each frame that is shown, the surface is GPU-copied (on the decode thread) into
//!    one of a few shared NV12 textures (`SHARED_NTHANDLE`). The copy is synced with a
//!    D3D11 event query. A texture is written again only once the renderer's GPU work
//!    reading it has finished (its [`Lease`] dropped).
//! 3. D3D12 opens the shared handle once per slot; wgpu wraps it as an NV12 texture.
//! 4. `GpuServices::convert_nv12` converts it to linear RGB in the render graph.

use crate::source::{Lease, Surface, VideoDecoder};
use crate::{MediaError, VideoTrack};
use oa_gpu::{GpuContext, VideoColor};
use oa_time::{Rational, Time};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use windows::core::{Interface, BOOL, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HMODULE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4, IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn win(context: &'static str) -> impl Fn(windows::core::Error) -> MediaError {
    move |e| MediaError::Decode(format!("{context}: {e}"))
}

/// Converts a decoder timestamp in 100 ns units to exact timeline time.
fn time_from_hns(hns: i64) -> Time {
    Time::from_rational_floor(Rational::new(hns, 10_000_000))
}

/// Converts time to 100 ns units, rounding down.
fn hns_from_time(t: Time) -> i64 {
    (t.0 as i128 * 10_000_000 / oa_time::FLICKS_PER_SECOND as i128) as i64
}

fn startup() {
    static START: Once = Once::new();
    START.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    });
    // Per-thread; "already initialized in another mode" is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

/// A D3D11 video device on the wgpu device's adapter, plus the D3D12 device to import into.
pub struct D3D11Bridge {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: ID3D11Multithread,
    manager: IMFDXGIDeviceManager,
    d3d12: ID3D12Device,
    wgpu: wgpu::Device,
}

// SAFETY: everything here is free-threaded. D3D11 devices are thread-safe, the immediate
// context is only used between `multithread.Enter/Leave` (protection is switched on at
// creation), MF's DXGI device manager exists to be shared between components on
// different threads, and D3D12 devices are thread-safe. One bridge serves every decode
// thread.
unsafe impl Send for D3D11Bridge {}
unsafe impl Sync for D3D11Bridge {}

impl D3D11Bridge {
    pub fn new(ctx: &GpuContext) -> Result<Arc<Self>, MediaError> {
        if !ctx.is_dx12() || !ctx.supports_nv12() {
            return Err(MediaError::Unsupported(format!(
                "hardware decode import needs the DX12 backend with NV12 support (have {:?}, nv12: {})",
                ctx.info.backend,
                ctx.supports_nv12()
            )));
        }
        startup();
        unsafe {
            let d3d12: ID3D12Device = {
                let hal = ctx
                    .device
                    .as_hal::<wgpu::hal::api::Dx12>()
                    .ok_or_else(|| MediaError::Unsupported("wgpu device is not DX12".into()))?;
                hal.raw_device().clone()
            };
            let factory: IDXGIFactory4 = CreateDXGIFactory1().map_err(win("CreateDXGIFactory1"))?;
            let adapter: IDXGIAdapter = factory.EnumAdapterByLuid(d3d12.GetAdapterLuid()).map_err(win("EnumAdapterByLuid"))?;
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(win("D3D11CreateDevice"))?;
            let (device, context) = (device.expect("device"), context.expect("context"));
            let multithread: ID3D11Multithread = device.cast().map_err(win("ID3D11Multithread"))?;
            let _ = multithread.SetMultithreadProtected(true);
            let (mut token, mut manager) = (0u32, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(win("MFCreateDXGIDeviceManager"))?;
            let manager = manager.expect("manager");
            manager.ResetDevice(&device, token).map_err(win("ResetDevice"))?;
            Ok(Arc::new(D3D11Bridge { device, context, multithread, manager, d3d12, wgpu: ctx.device.clone() }))
        }
    }

    /// A D3D11 NV12 texture shared with D3D12 and wrapped as a wgpu texture.
    fn shared_nv12(&self, size: [u32; 2]) -> Result<Slot, MediaError> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size[0],
                Height: size[1],
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0) as u32,
            };
            let mut texture = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut texture)).map_err(win("CreateTexture2D(shared NV12)"))?;
            let texture = texture.expect("texture");
            let handle = texture
                .cast::<IDXGIResource1>()
                .and_then(|r| r.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null()))
                .map_err(win("CreateSharedHandle"))?;
            let mut resource: Option<ID3D12Resource> = None;
            let opened = self.d3d12.OpenSharedHandle(handle, &mut resource);
            let _ = CloseHandle(handle);
            opened.map_err(win("OpenSharedHandle"))?;
            let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
            let hal = wgpu::hal::dx12::Device::texture_from_raw(
                resource.expect("resource"),
                wgpu::TextureFormat::NV12,
                wgpu::TextureDimension::D2,
                extent,
                1,
                1,
            );
            let wgpu_texture = self.wgpu.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("oa-decoded-nv12"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::NV12,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            );
            Ok(Slot { d3d11: texture, wgpu: Arc::new(wgpu_texture), size, free: Arc::new(AtomicBool::new(true)) })
        }
    }

    /// GPU copy of one decoder surface into a shared slot, waiting for completion so the
    /// D3D12 side never reads a half-written frame.
    fn copy(&self, dst: &ID3D11Texture2D, src: &ID3D11Texture2D, subresource: u32) -> Result<(), MediaError> {
        unsafe {
            self.multithread.Enter();
            let result = (|| {
                self.context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, subresource, None);
                let mut query = None;
                self.device
                    .CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut query))
                    .map_err(win("CreateQuery"))?;
                let query = query.expect("query");
                self.context.End(&query);
                self.context.Flush();
                Ok(query)
            })();
            self.multithread.Leave();
            let query = result?;
            loop {
                let mut done = BOOL(0);
                self.multithread.Enter();
                let r = self.context.GetData(&query, Some(&mut done as *mut BOOL as *mut _), size_of::<BOOL>() as u32, 0);
                self.multithread.Leave();
                r.map_err(win("GetData"))?;
                if done.as_bool() {
                    return Ok(());
                }
                std::thread::yield_now();
            }
        }
    }
}

struct Slot {
    d3d11: ID3D11Texture2D,
    wgpu: Arc<wgpu::Texture>,
    size: [u32; 2],
    /// False while a [`Surface`] using this texture is alive anywhere: the decoded-ahead
    /// queue, the renderer, or GPU work that hasn't finished.
    free: Arc<AtomicBool>,
}

/// Enough for the lookahead queue, the frame on screen and a few in flight on the GPU.
const MAX_SLOTS: usize = crate::source::LOOKAHEAD + 6;

pub struct MfDecoder {
    bridge: Arc<D3D11Bridge>,
    reader: IMFSourceReader,
    visible: [u32; 2],
    rotation: u32,
    color: VideoColor,
    slots: Vec<Slot>,
}

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

impl MfDecoder {
    pub fn open(bridge: Arc<D3D11Bridge>, path: &Path, video: &VideoTrack) -> Result<Self, MediaError> {
        startup();
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 3).map_err(win("MFCreateAttributes"))?;
            let attributes = attributes.expect("attributes");
            attributes.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &bridge.manager).map_err(win("D3D manager"))?;
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(win("hardware transforms"))?;
            // MF's URL resolver rejects paths over MAX_PATH (260) and the `\\?\` form, so open
            // the file as a byte stream ourselves and tell MF its name for format sniffing.
            let full = std::fs::canonicalize(path).map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
            let stream = MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, &HSTRING::from(full.as_os_str()))
                .map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?;
            if let Ok(stream_attributes) = stream.cast::<IMFAttributes>() {
                let name = path.file_name().map(HSTRING::from).unwrap_or_default();
                let _ = stream_attributes.SetString(&MF_BYTESTREAM_ORIGIN_NAME, &name);
            }
            let reader = MFCreateSourceReaderFromByteStream(&stream, &attributes).map_err(win("MFCreateSourceReaderFromByteStream"))?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false).map_err(win("deselect streams"))?;
            reader.SetStreamSelection(VIDEO, true).map_err(win("select video"))?;
            let ty = MFCreateMediaType().map_err(win("MFCreateMediaType"))?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(win("major type"))?;
            ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(win("subtype"))?;
            reader
                .SetCurrentMediaType(VIDEO, None, &ty)
                .map_err(|e| MediaError::Unsupported(format!("no NV12 hardware decode for {} ({}): {e}", video.codec, video.pixel_format)))?;
            Ok(MfDecoder {
                bridge,
                reader,
                visible: [video.coded_width, video.coded_height],
                rotation: video.rotation_quarter_turns,
                color: video.color,
                slots: Vec::new(),
            })
        }
    }

    /// A shared texture nobody is using, allocating one while there's room. If every
    /// slot is busy (the GPU hasn't reported finishing with them), waits briefly.
    fn free_slot(&mut self, size: [u32; 2]) -> Result<usize, MediaError> {
        // Textures of another size (a resolution change mid-stream) go once released.
        self.slots.retain(|s| s.size == size || !s.free.load(Ordering::Acquire));
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if let Some(i) = self.slots.iter().position(|s| s.size == size && s.free.load(Ordering::Acquire)) {
                return Ok(i);
            }
            if self.slots.len() < MAX_SLOTS || std::time::Instant::now() > deadline {
                // Past the deadline nothing is polling the device; growing beats stalling
                // or overwriting a texture that may still be read.
                self.slots.push(self.bridge.shared_nv12(size)?);
                return Ok(self.slots.len() - 1);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl VideoDecoder for MfDecoder {
    type Frame = IMFSample;

    fn seek(&mut self, t: Time) -> Result<(), MediaError> {
        unsafe {
            let position = PROPVARIANT::from(hns_from_time(t));
            self.reader.SetCurrentPosition(&GUID::zeroed(), &position).map_err(win("SetCurrentPosition"))
        }
    }

    fn next(&mut self) -> Result<Option<(Time, IMFSample)>, MediaError> {
        unsafe {
            loop {
                let (mut flags, mut timestamp, mut sample) = (0u32, 0i64, None);
                self.reader
                    .ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut timestamp), Some(&mut sample))
                    .map_err(win("ReadSample"))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Ok(None);
                }
                if let Some(sample) = sample {
                    return Ok(Some((time_from_hns(timestamp), sample)));
                }
            }
        }
    }

    fn publish(&mut self, sample: &IMFSample) -> Result<Surface, MediaError> {
        let (surface, subresource, coded) = unsafe {
            let buffer: IMFDXGIBuffer = sample.GetBufferByIndex(0).and_then(|b| b.cast()).map_err(|e| {
                MediaError::Unsupported(format!("decoder did not output GPU surfaces (software decode?): {e}"))
            })?;
            let mut raw = std::ptr::null_mut();
            buffer.GetResource(&ID3D11Texture2D::IID, &mut raw).map_err(win("GetResource"))?;
            let surface = ID3D11Texture2D::from_raw(raw);
            let subresource = buffer.GetSubresourceIndex().map_err(win("GetSubresourceIndex"))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            surface.GetDesc(&mut desc);
            (surface, subresource, [desc.Width, desc.Height])
        };
        let i = self.free_slot(coded)?;
        let slot = &self.slots[i];
        let lease = Lease::take(&slot.free);
        self.bridge.copy(&slot.d3d11, &surface, subresource)?;
        Ok(Surface {
            texture: slot.wgpu.clone(),
            chroma: None,
            coded_size: coded,
            visible_size: self.visible,
            rotation_quarter_turns: self.rotation,
            color: self.color,
            lease: Arc::new(lease),
        })
    }
}

/// A frame source backed by Media Foundation hardware decoding on `ctx`'s adapter.
pub fn hardware_source(ctx: &GpuContext) -> Result<crate::MediaFrameSource<MfDecoder>, MediaError> {
    Ok(hardware_source_with(D3D11Bridge::new(ctx)?))
}

/// Same, reusing a bridge that's already open (one D3D11 device per app, not per file).
pub fn hardware_source_with(bridge: Arc<D3D11Bridge>) -> crate::MediaFrameSource<MfDecoder> {
    crate::MediaFrameSource::new(move |path, video| MfDecoder::open(bridge.clone(), path, video))
}
