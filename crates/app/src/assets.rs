//! The asset library: the files you reach for in every project.
//!
//! A folder on this computer (`<config>/assets`, or wherever the settings point) holding
//! the whoosh you always use, your logo, a grain plate. It isn't part of any project —
//! the media bin has an **Assets** tab that lists what's in the folder, and adding one to
//! the project imports it the same way dropping the file in would. Saving to assets
//! copies the file in, so the library keeps working when the original is moved or the
//! project is thrown away.
//!
//! Nothing here touches the document: the library is a place on disk, scanned on demand.

use oa_media::MediaKind;
use std::path::{Path, PathBuf};

/// One file in the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
    /// Which folder inside the library it's in ("" is the top).
    pub folder: String,
}

impl Asset {
    /// A stable id for preview caches, from the path. The high bit keeps it clear of
    /// the project's own media ids, which count up from 1.
    pub fn preview_id(&self) -> oa_doc::MediaId {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for b in self.path.to_string_lossy().as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        oa_doc::MediaId(hash | (1 << 63))
    }
}

/// What kind of file this is, by extension — enough for the bin to draw it.
fn kind_of(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !crate::MEDIA_EXTENSIONS.iter().any(|e| *e == ext) {
        return None;
    }
    Some(match ext.as_str() {
        "mp3" | "m4a" | "wav" | "flac" | "aac" | "ogg" => MediaKind::Audio,
        "png" | "jpg" | "jpeg" | "bmp" | "webp" => MediaKind::Still,
        _ => MediaKind::Video,
    })
}

/// The library as it is on disk.
#[derive(Default)]
pub struct Assets {
    pub dir: PathBuf,
    pub items: Vec<Asset>,
    /// Folders inside the library, as paths.
    pub folders: Vec<String>,
    pub error: Option<String>,
}

impl Assets {
    pub fn new(dir: PathBuf) -> Self {
        let mut assets = Assets { dir, ..Default::default() };
        assets.rescan();
        assets
    }

    /// Reads the folder again (after adding, removing, or the user editing it outside).
    pub fn rescan(&mut self) {
        self.items.clear();
        self.folders.clear();
        self.error = None;
        let dir = self.dir.clone();
        self.walk(&dir, "");
        self.items.sort_by_key(|a| (a.folder.to_lowercase(), a.name.to_lowercase()));
        self.folders.sort();
    }

    fn walk(&mut self, dir: &Path, folder: &str) {
        // Deep enough for organizing, shallow enough that a stray symlink can't spin.
        if folder.matches('/').count() > 6 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                let inside = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
                self.folders.push(inside.clone());
                self.walk(&path, &inside);
            } else if let Some(kind) = kind_of(&path) {
                self.items.push(Asset { path, name, kind, folder: folder.to_string() });
            }
        }
    }

    /// What's directly inside `folder`.
    pub fn in_folder<'a>(&'a self, folder: &'a str) -> impl Iterator<Item = &'a Asset> {
        self.items.iter().filter(move |a| a.folder == folder)
    }

    /// The folders directly inside `parent`, with how many files each holds.
    pub fn child_folders(&self, parent: &str) -> Vec<(String, usize)> {
        self.folders
            .iter()
            .filter(|f| match parent {
                "" => !f.contains('/'),
                p => f.strip_prefix(p).is_some_and(|rest| rest.starts_with('/') && !rest[1..].contains('/')),
            })
            .map(|f| {
                let inside = format!("{f}/");
                let n = self.items.iter().filter(|a| a.folder == *f || a.folder.starts_with(&inside)).count();
                (f.clone(), n)
            })
            .collect()
    }

    /// Copies a file into the library (into `folder`), keeping its name unless that's
    /// taken. Returns where it landed.
    pub fn add(&mut self, source: &Path, folder: &str) -> Result<PathBuf, String> {
        let name = source.file_name().ok_or("that isn't a file")?.to_string_lossy().to_string();
        let dir = if folder.is_empty() { self.dir.clone() } else { self.dir.join(folder) };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut target = dir.join(&name);
        if target.exists() {
            // "whoosh.wav" already there: "whoosh 2.wav", "whoosh 3.wav"…
            let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());
            let ext = Path::new(&name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            for n in 2..100 {
                let candidate = dir.join(format!("{stem} {n}{ext}"));
                if !candidate.exists() {
                    target = candidate;
                    break;
                }
            }
        }
        if target == source {
            return Ok(target);
        }
        std::fs::copy(source, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        self.rescan();
        Ok(target)
    }

    /// Deletes a file from the library. It's a copy, so this doesn't touch the original
    /// the user added — but it is a real delete, so the caller asks first.
    pub fn remove(&mut self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.rescan();
        Ok(())
    }

    /// Makes a folder in the library.
    pub fn new_folder(&mut self, parent: &str, name: &str) -> Result<(), String> {
        let name = crate::notify::check_name("folder", name)?;
        if name.contains(['/', '\\']) {
            return Err("A folder name can't contain a slash.".into());
        }
        let path = if parent.is_empty() { self.dir.join(&name) } else { self.dir.join(parent).join(&name) };
        std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.rescan();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oa-assets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The library is just a folder: files in it are assets, folders are folders, and
    /// anything that isn't media is ignored.
    #[test]
    fn the_library_is_a_folder_on_disk() {
        let dir = temp("scan");
        std::fs::write(dir.join("whoosh.wav"), b"x").expect("write");
        std::fs::write(dir.join("notes.txt"), b"x").expect("write");
        std::fs::create_dir_all(dir.join("logos")).expect("dir");
        std::fs::write(dir.join("logos/mark.png"), b"x").expect("write");

        let assets = Assets::new(dir.clone());
        assert_eq!(assets.items.len(), 2, "the text file isn't media");
        assert_eq!(assets.folders, vec!["logos".to_string()]);
        assert_eq!(assets.in_folder("").count(), 1);
        assert_eq!(assets.child_folders(""), vec![("logos".to_string(), 1)]);
        let logo = assets.items.iter().find(|a| a.name == "mark.png").expect("the logo");
        assert_eq!((logo.kind, logo.folder.as_str()), (MediaKind::Still, "logos"));
        // Preview ids are stable per path and never look like a project's own media id.
        assert_eq!(logo.preview_id(), logo.preview_id());
        assert!(logo.preview_id().0 > (1 << 62));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saving to the library copies the file in, and a second copy of the same name
    /// gets its own.
    #[test]
    fn saving_copies_and_never_overwrites() {
        let dir = temp("add");
        let source = dir.join("source");
        std::fs::create_dir_all(&source).expect("dir");
        std::fs::write(source.join("beep.wav"), b"one").expect("write");

        let mut assets = Assets::new(dir.join("library"));
        let first = assets.add(&source.join("beep.wav"), "").expect("first copy");
        std::fs::write(source.join("beep.wav"), b"two").expect("write");
        let second = assets.add(&source.join("beep.wav"), "").expect("second copy");
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).expect("read"), b"one", "the first copy is untouched");
        assert_eq!(assets.items.len(), 2);

        assets.remove(&second).expect("remove");
        assert_eq!(assets.items.len(), 1);
        assert!(source.join("beep.wav").exists(), "removing a copy leaves the original alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
