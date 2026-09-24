//! Media: probing, frame indexing, fingerprints and GPU-first decoding.
//!
//! Decoded frames go from the hardware decoder to a GPU texture to the render graph
//! without touching CPU memory:
//!
//! * Windows: Media Foundation (DXVA) decodes into D3D11 NV12 surfaces on the same
//!   adapter as the wgpu device; each shown frame is GPU-copied into a shared texture
//!   that D3D12/wgpu imports, then converted to linear RGB by a shader.
//! * Everywhere (Linux, macOS, and Windows when Media Foundation can't take a file):
//!   an `ffmpeg` process decodes (with the platform's hardware decoder when it can) and
//!   the planes are uploaded as two textures every GPU backend can sample (`ffmpeg`).
//!   [`frame_source`] picks per file.
//!
//! Probing uses `ffprobe` for metadata and the frame index only.

mod any;
pub mod ffmpeg;
mod fingerprint;
pub mod import;
mod probe;
mod source;
mod still;
#[cfg(windows)]
pub mod windows;

pub use any::{frame_source, AnyDecoder, AnyFrame, DecoderChoice};
pub use fingerprint::fingerprint;
pub use import::{import, Imported, MediaKind};
pub use probe::{probe, AudioTrack, FrameIndex, MediaProbe, VideoTrack};
pub use source::{DecodeStats, Lease, MediaFrameSource, Surface, VideoDecoder, LOOKAHEAD};
pub use still::StillSource;

use std::fmt;

/// Where OpenAtelier keeps its own files (settings, caches, autosaves, engines):
/// `%LOCALAPPDATA%\OpenAtelier` on Windows, `~/Library/Application Support/OpenAtelier`
/// on macOS, `$XDG_DATA_HOME/OpenAtelier` (`~/.local/share/OpenAtelier`) elsewhere. Only
/// when none of those can be found does it fall back to the temporary folder.
pub fn app_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local").join("share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("OpenAtelier")
}

/// A command for one of the helper tools (ffmpeg, ffprobe) that never opens a console
/// window. On Windows, a child of an app without a console gets one of its own: it
/// flashes up on every probe, import and seek, and takes the keyboard from the editor.
pub fn tool(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaError {
    Probe(String),
    Io(String),
    /// Hardware decoding isn't available for this file/platform/GPU backend.
    Unsupported(String),
    Decode(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::Probe(e) => write!(f, "probe failed: {e}"),
            MediaError::Io(e) => write!(f, "i/o error: {e}"),
            MediaError::Unsupported(e) => write!(f, "unsupported: {e}"),
            MediaError::Decode(e) => write!(f, "decode failed: {e}"),
        }
    }
}

impl std::error::Error for MediaError {}
