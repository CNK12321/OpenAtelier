//! Autosave and crash recovery.
//!
//! While a project has unsaved changes it's written every few seconds to
//! `<app folder>/autosave/<session>.oaproj.json` (with a `.origin` file
//! naming the project it came from). Saving, starting a new project or quitting cleanly
//! removes it; so a file still there at startup means a session ended without saving —
//! a crash, a kill, a power cut — and the app offers to recover it.
//!
//! All writes are atomic (temp file + rename), so a crash mid-write never leaves a
//! half-written project, autosave or real.

use oa_doc::{Project, ProjectFile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Writes `bytes` to `path` so readers see either the old file or the new one, never a
/// partial write.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `OA_AUTOSAVE_DIR` overrides the location (tests, portable installs).
pub fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OA_AUTOSAVE_DIR") {
        return PathBuf::from(dir);
    }
    oa_media::app_dir().join("autosave")
}

/// An autosave left behind by a session that didn't end cleanly.
#[derive(Clone, Debug)]
pub struct Recovery {
    pub file: PathBuf,
    /// The project file it was editing, if it had been saved before.
    pub original: Option<PathBuf>,
    pub modified: SystemTime,
}

impl Recovery {
    pub fn name(&self) -> String {
        match &self.original {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            None => "an untitled project".into(),
        }
    }

    pub fn age(&self) -> String {
        let secs = self.modified.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        match secs {
            0..=59 => "moments ago".into(),
            60..=3599 => format!("{} min ago", secs / 60),
            3600..=86_399 => format!("{} h ago", secs / 3600),
            _ => format!("{} days ago", secs / 86_400),
        }
    }

    /// Deletes the autosave. A missing file is fine (another window may have taken it);
    /// anything else — a lock, no permission — comes back to be shown.
    pub fn discard(&self) -> std::io::Result<()> {
        let _ = std::fs::remove_file(self.file.with_extension("origin"));
        match std::fs::remove_file(&self.file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

pub struct Autosave {
    dir: PathBuf,
    session: String,
    /// The snapshot last written, so an unchanged document isn't written again.
    written: Option<Arc<Project>>,
    /// A write running on its own thread.
    pending: Option<std::thread::JoinHandle<Result<(), String>>>,
    last: Instant,
    pub interval: Duration,
}

impl Autosave {
    pub fn new(dir: PathBuf) -> Self {
        let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Autosave {
            dir,
            session: format!("{stamp}-{}", std::process::id()),
            written: None,
            pending: None,
            last: Instant::now(),
            // `OA_AUTOSAVE_SECS` shortens it for testing.
            interval: Duration::from_secs(std::env::var("OA_AUTOSAVE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(15)),
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join(format!("{}.oaproj.json", self.session))
    }

    /// Autosaves left by other sessions, newest first.
    pub fn recoverable(&self) -> Vec<Recovery> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mine = self.file();
        let mut found: Vec<Recovery> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(".oaproj.json") && *p != mine)
            .map(|file| {
                let original = std::fs::read_to_string(file.with_extension("origin")).ok().map(|s| PathBuf::from(s.trim()));
                let modified = std::fs::metadata(&file).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                Recovery { file, original: original.filter(|p| !p.as_os_str().is_empty()), modified }
            })
            .collect();
        found.sort_by_key(|r| std::cmp::Reverse(r.modified));
        found
    }

    /// Writes the project if it changed since the last autosave and the interval passed.
    /// `dirty` is whether it differs from the saved file (nothing to protect otherwise).
    ///
    /// The write happens on a thread of its own: turning a big project into JSON takes a
    /// noticeable moment (~0.2 s for an hour-long edit), which every 15 s was a regular
    /// hitch in the editor. The snapshot it writes is immutable, so edits carry on
    /// meanwhile. A finished write that failed comes back from a later tick.
    pub fn tick(&mut self, project: &Arc<Project>, dirty: bool, original: Option<&Path>) -> Option<Result<(), String>> {
        if let Some(done) = self.finished() {
            return Some(done);
        }
        if self.pending.is_some() {
            return None; // one write at a time
        }
        if !dirty {
            // Saved: the real file is the safe copy now.
            if self.written.take().is_some() {
                self.clear();
            }
            return None;
        }
        if self.last.elapsed() < self.interval || self.written.as_ref().is_some_and(|w| Arc::ptr_eq(w, project)) {
            return None;
        }
        self.last = Instant::now();
        let (dir, file, project_copy) = (self.dir.clone(), self.file(), project.clone());
        let origin = original.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        self.written = Some(project.clone());
        self.pending = std::thread::Builder::new().name("oa-autosave".into()).spawn(move || write(&dir, &file, &project_copy, &origin)).ok();
        None
    }

    /// Writes now, on this thread — when the app may be about to go down (a lost GPU, a
    /// panic) and there's no later to wait for.
    pub fn write_now(&mut self, project: &Arc<Project>, original: Option<&Path>) -> Result<(), String> {
        let _ = self.wait();
        let origin = original.map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        write(&self.dir, &self.file(), project, &origin)?;
        self.written = Some(project.clone());
        self.last = Instant::now();
        Ok(())
    }

    /// A background write that has finished, and how it went.
    fn finished(&mut self) -> Option<Result<(), String>> {
        if !self.pending.as_ref()?.is_finished() {
            return None;
        }
        self.wait()
    }

    /// Waits for a background write, if there is one.
    fn wait(&mut self) -> Option<Result<(), String>> {
        let result = self.pending.take()?.join().unwrap_or_else(|_| Err("the autosave stopped unexpectedly".into()));
        if result.is_err() {
            self.written = None; // try again next time
        }
        Some(result)
    }

    /// Writes at the next tick instead of waiting for the interval (e.g. right after a
    /// recovery, whose own autosave was just consumed).
    pub fn soon(&mut self) {
        self.last = Instant::now().checked_sub(self.interval).unwrap_or(self.last);
        self.written = None;
    }

    /// Removes this session's autosave (after a save, a new project, or a clean exit).
    pub fn clear(&mut self) {
        // A write still going would put the file back after it's removed.
        let _ = self.wait();
        let file = self.file();
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("origin"));
        self.written = None;
    }
}

/// Writes one autosave: the project, and beside it which file it belongs to.
fn write(dir: &Path, file: &Path, project: &Project, origin: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = ProjectFile::to_json(project).map_err(|e| e.to_string())?;
    write_atomic(file, json.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
    write_atomic(&file.with_extension("origin"), origin.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autosaves_only_changed_dirty_work_and_others_are_recoverable() {
        let dir = std::env::temp_dir().join(format!("oa-autosave-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = Autosave::new(dir.clone());
        a.interval = Duration::ZERO;
        let p1 = Arc::new(Project::new("one"));

        assert!(a.tick(&p1, false, None).is_none(), "clean projects aren't autosaved");
        // The write goes off on its own thread; the tick doesn't wait for it.
        assert!(a.tick(&p1, true, Some(Path::new("C:/x/one.oaproj.json"))).is_none());
        assert!(a.pending.is_some(), "writing in the background");
        assert!(matches!(a.wait(), Some(Ok(()))), "and it gets there");
        assert!(a.tick(&p1, true, None).is_none(), "unchanged since the last autosave");
        assert!(a.pending.is_none(), "so nothing is written");

        // A second session sees the first one's file as recoverable, with its origin.
        let b = Autosave::new(dir.clone());
        let b = Autosave { session: format!("{}-other", b.session), ..b };
        let found = b.recoverable();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].original.as_deref(), Some(Path::new("C:/x/one.oaproj.json")));
        let recovered = ProjectFile::from_json(&std::fs::read_to_string(&found[0].file).unwrap()).unwrap();
        assert_eq!(recovered.name, "one");
        assert!(a.recoverable().is_empty(), "a session doesn't offer its own autosave");

        // Saving (no longer dirty) removes the autosave.
        a.tick(&p1, false, None);
        assert!(b.recoverable().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_writes_replace_whole_files() {
        let dir = std::env::temp_dir().join(format!("oa-atomic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p.oaproj.json");
        write_atomic(&f, b"first").unwrap();
        write_atomic(&f, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"second, longer");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
