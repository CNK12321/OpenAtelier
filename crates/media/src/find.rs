//! Finding moved media: one pass over likely places on this computer for every missing
//! file at once.
//!
//! A file counts as found when it has the same name and the same content (the
//! [`crate::fingerprint`]: its size and a hash of three slices of it, taken when it was
//! imported) — or, renamed, just the same content: only files of exactly the same size
//! are hashed, so that's cheap. A same-named file with other content is a different file
//! and is passed over; only for media imported before fingerprints were kept is the name
//! alone enough. The search stops at a time limit, nearest places first.

use crate::fingerprint;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A file being looked for: where it was, and its fingerprint if one was kept.
#[derive(Clone, Debug)]
pub struct Missing {
    pub path: PathBuf,
    pub fingerprint: Option<String>,
}

/// Folders never worth going into: the system's, programs', caches and build output.
fn skipped(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('$')
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "windows" | "program files" | "program files (x86)" | "programdata" | "appdata" | "system volume information" | "node_modules" | "target" | "__pycache__" | "library" | "proc" | "sys" | "dev"
        )
}

/// How far into each place the search goes.
const DEPTH: usize = 8;

/// The size a fingerprint records (its last part).
fn size_of(fp: &str) -> Option<u64> {
    fp.rsplit('-').next()?.parse().ok()
}

/// Looks for each of `missing` under `roots` (nearest first; each searched breadth-first,
/// a few levels deep), until everything is found or `budget` runs out. Returns where each
/// one is, in order (`None`: not found).
pub fn find_moved(missing: &[Missing], roots: &[PathBuf], budget: Duration) -> Vec<Option<PathBuf>> {
    let deadline = Instant::now() + budget;
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    // What's looked for, by name and by size.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, m) in missing.iter().enumerate() {
        if let Some(n) = name_of(&m.path) {
            by_name.entry(n).or_default().push(i);
        }
        if let Some(size) = m.fingerprint.as_deref().and_then(size_of) {
            by_size.entry(size).or_default().push(i);
        }
    }
    let mut found: Vec<Option<PathBuf>> = vec![None; missing.len()];
    // Same name, other content (or no fingerprint to tell): the last resort, for media
    // without a fingerprint.
    let mut named: Vec<Option<PathBuf>> = vec![None; missing.len()];
    let mut prints: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut print = |p: &Path| prints.entry(p.to_path_buf()).or_insert_with(|| fingerprint(p).ok()).clone();
    let mut visited = std::collections::HashSet::new();
    'roots: for root in roots {
        let mut queue = std::collections::VecDeque::from([(root.clone(), 0usize)]);
        while let Some((dir, depth)) = queue.pop_front() {
            if Instant::now() > deadline || found.iter().all(Option::is_some) {
                break 'roots;
            }
            // A folder reached from two places is searched once.
            let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            if !visited.insert(key) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let Ok(kind) = e.file_type() else { continue };
                let p = e.path();
                if kind.is_dir() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if depth < DEPTH && !skipped(&n) {
                        queue.push_back((p, depth + 1));
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                // The same name: the same content settles it.
                if let Some(ids) = name_of(&p).and_then(|n| by_name.get(&n)) {
                    for &i in ids {
                        if found[i].is_some() {
                            continue;
                        }
                        match &missing[i].fingerprint {
                            Some(fp) if print(&p).as_deref() == Some(fp) => found[i] = Some(p.clone()),
                            Some(_) => {}
                            None => {
                                named[i].get_or_insert_with(|| p.clone());
                            }
                        }
                    }
                }
                // Renamed: the same size, then the same content.
                let size = e.metadata().map(|m| m.len()).ok();
                if let Some(ids) = size.and_then(|s| by_size.get(&s)) {
                    for &i in ids {
                        if found[i].is_none() && print(&p).as_deref() == missing[i].fingerprint.as_deref() {
                            found[i] = Some(p.clone());
                        }
                    }
                }
            }
        }
    }
    found.into_iter().zip(named).map(|(f, n)| f.or(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by name and content, renamed by content alone; a same-named file with other
    /// content is passed over; without a fingerprint the name will do.
    #[test]
    fn moved_files_are_found_by_name_or_content() {
        let root = std::env::temp_dir().join(format!("oa-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        // The originals, measured where they were, then moved (and one renamed).
        let write = |p: &Path, bytes: &[u8]| std::fs::write(p, bytes).unwrap();
        let (clip, music, old) = (root.join("clip.mp4"), root.join("music.wav"), root.join("old.mov"));
        write(&clip, b"clip content");
        write(&music, b"music content, longer");
        let (clip_fp, music_fp) = (fingerprint(&clip).unwrap(), fingerprint(&music).unwrap());
        std::fs::rename(&clip, deep.join("clip.mp4")).unwrap();
        std::fs::rename(&music, deep.join("renamed.wav")).unwrap();
        std::fs::remove_file(&old).ok();
        // Decoys: a same-named file with other content; a match inside a skipped folder.
        write(&root.join("a").join("clip.mp4"), b"another clip entirely");
        write(&root.join(".cache").join("old.mov"), b"cached");
        write(&root.join("a").join("old.mov"), b"the old one");
        let missing = [
            Missing { path: clip.clone(), fingerprint: Some(clip_fp) },
            Missing { path: music.clone(), fingerprint: Some(music_fp) },
            Missing { path: old.clone(), fingerprint: None },
            Missing { path: root.join("gone.mp4"), fingerprint: Some("xxh3-0-99".into()) },
        ];
        let found = find_moved(&missing, std::slice::from_ref(&root), Duration::from_secs(5));
        assert_eq!(found[0].as_deref(), Some(deep.join("clip.mp4").as_path()), "the same name and content, past the decoy");
        assert_eq!(found[1].as_deref(), Some(deep.join("renamed.wav").as_path()), "renamed: by content");
        assert_eq!(found[2].as_deref(), Some(root.join("a").join("old.mov").as_path()), "no fingerprint: by name, not in .cache");
        assert_eq!(found[3], None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
