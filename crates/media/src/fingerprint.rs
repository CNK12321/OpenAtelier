use crate::MediaError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use xxhash_rust::xxh3::Xxh3;

const CHUNK: u64 = 1 << 20;

/// Fast content fingerprint: file size plus hashes of the first, middle and last MiB.
/// Stable across renames/moves, so caches and relinking key off content, not paths.
pub fn fingerprint(path: &Path) -> Result<String, MediaError> {
    let io = |e: std::io::Error| MediaError::Io(format!("{}: {e}", path.display()));
    let mut f = File::open(path).map_err(io)?;
    let len = f.metadata().map_err(io)?.len();
    let mut h = Xxh3::new();
    h.update(&len.to_le_bytes());
    let mut buf = vec![0u8; CHUNK as usize];
    for offset in [0, len.saturating_sub(CHUNK) / 2, len.saturating_sub(CHUNK)] {
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        let n = f.read(&mut buf).map_err(io)?;
        h.update(&buf[..n]);
    }
    Ok(format!("xxh3-{:032x}-{len}", h.digest128()))
}
