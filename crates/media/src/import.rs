//! Importing: work out what a file is, and make it playable.
//!
//! Some containers the platform decoder can't open (animated GIF, for instance) are
//! **conformed**: remuxed, or re-encoded if that fails, into an MP4 kept in a cache keyed
//! by the file's fingerprint. The project keeps pointing at the original file; only
//! decoding uses the conformed copy.

use crate::{fingerprint, probe, MediaError, MediaProbe};
use oa_time::Time;
use std::path::{Path, PathBuf};

/// How a file will be played back.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MediaKind {
    /// Moving pictures (possibly with sound).
    Video,
    /// A single image.
    Still,
    /// Sound only.
    Audio,
}

#[derive(Clone, Debug)]
pub struct Imported {
    /// The file the user picked; this is what the project stores.
    pub path: PathBuf,
    /// The file to decode — the original, or a conformed copy.
    pub decode_path: PathBuf,
    pub fingerprint: String,
    /// Probe of `decode_path` (its frame index is the one decoding uses).
    pub probe: MediaProbe,
    pub kind: MediaKind,
    /// Set when the file had to be conformed, with the reason.
    pub conformed: Option<String>,
}

impl Imported {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    /// Default length for a clip of this media: its own duration, or 5s for a still.
    pub fn default_duration(&self) -> Time {
        match self.kind {
            MediaKind::Still => Time::from_seconds(5),
            _ if self.probe.duration > Time::ZERO => self.probe.duration,
            _ => Time::from_seconds(5),
        }
    }
}

/// Probes `path` and classifies it. `can_decode` answers whether the platform decoder can
/// open a video file as-is; when it can't, the file is conformed to MP4 first.
pub fn import(path: &Path, can_decode: impl Fn(&Path, &MediaProbe) -> bool) -> Result<Imported, MediaError> {
    let path = std::fs::canonicalize(path)
        .map_err(|e| MediaError::Io(format!("{}: {e}", path.display())))?
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .into();
    let path: PathBuf = path;
    let fingerprint = fingerprint(&path)?;
    let mut probe = probe(&path)?;

    let kind = match (&probe.video, &probe.audio) {
        (Some(v), _) if v.still => MediaKind::Still,
        (Some(_), _) => MediaKind::Video,
        (None, Some(_)) => MediaKind::Audio,
        (None, None) => return Err(MediaError::Probe("file has no audio or video".into())),
    };

    let mut decode_path = path.clone();
    let mut conformed = None;
    if kind == MediaKind::Video && !can_decode(&path, &probe) {
        let reason = format!("{} is not playable directly", probe.container);
        decode_path = conform(&path, &fingerprint)?;
        probe = crate::probe(&decode_path)?;
        conformed = Some(reason);
    }
    Ok(Imported { path, decode_path, fingerprint, probe, kind, conformed })
}

/// Where conformed copies live.
pub fn cache_dir() -> PathBuf {
    crate::app_dir().join("conformed")
}

/// Remuxes into MP4, re-encoding only if the streams can't be copied. Cached by
/// fingerprint, so importing the same file twice is instant.
pub fn conform(path: &Path, fingerprint: &str) -> Result<PathBuf, MediaError> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| MediaError::Io(format!("{}: {e}", dir.display())))?;
    let out = dir.join(format!("{fingerprint}.mp4"));
    if out.exists() {
        return Ok(out);
    }
    let attempt = |args: &[&str]| -> Result<bool, MediaError> {
        let result = crate::tool("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(path)
            .args(args)
            .arg(&out)
            .output()
            .map_err(|e| MediaError::Decode(format!("could not run ffmpeg: {e}")))?;
        Ok(result.status.success())
    };
    if attempt(&["-c", "copy", "-movflags", "+faststart"])? {
        return Ok(out);
    }
    let encoded = attempt(&[
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-movflags", "+faststart",
    ])?;
    if encoded {
        Ok(out)
    } else {
        let _ = std::fs::remove_file(&out);
        Err(MediaError::Unsupported(format!("could not conform {}", path.display())))
    }
}

/// Finds a moved file: same name, or any file with the same fingerprint, under
/// `search_dirs` (searched one level deep).
pub fn relink(missing: &Path, fingerprint: &str, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = missing.file_name()?;
    let mut candidates = Vec::new();
    for dir in search_dirs {
        let by_name = dir.join(name);
        if by_name.is_file() {
            candidates.push(by_name);
        }
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.file_name() == Some(name) {
                candidates.push(p);
            } else if p.is_dir() {
                let nested = p.join(name);
                if nested.is_file() {
                    candidates.push(nested);
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.into_iter().find(|c| crate::fingerprint(c).is_ok_and(|f| f == fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relink_matches_by_content_not_just_name() {
        let dir = std::env::temp_dir().join("oa-relink-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let real = dir.join("nested").join("clip.bin");
        std::fs::write(&real, b"the actual media bytes").unwrap();
        // A same-named decoy with different content must be rejected.
        std::fs::write(dir.join("clip.bin"), b"something else entirely").unwrap();

        let fp = fingerprint(&real).unwrap();
        let missing = PathBuf::from("D:/gone/clip.bin");
        assert_eq!(relink(&missing, &fp, std::slice::from_ref(&dir)), Some(real));
        assert_eq!(relink(&missing, "xxh3-not-a-match-0", &[dir]), None);
    }
}
