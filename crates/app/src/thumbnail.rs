//! A picture of the project, for the start page.
//!
//! Saving a project also writes `<project>.thumb.png` beside it: one frame from the
//! timeline, rendered the same way the viewer renders, small. The start page draws
//! those instead of a wall of identical file icons — a project is much easier to
//! recognize by what it looks like than by what it's called.
//!
//! It's rendered, read back and encoded on the preview thread (`preview_worker`), so
//! saving never stalls the editor. It's a nicety, never a failure: anything that goes
//! wrong (no frame, a read-only folder) just means no picture this time.

use crate::App;
use oa_time::Time;
use std::path::{Path, PathBuf};

/// How wide the picture is; the height follows the format.
const WIDTH: u32 = 480;

/// Where a project's picture lives.
pub fn path_for(project: &Path) -> PathBuf {
    let name = project.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    project.with_file_name(format!("{name}.thumb.png"))
}

impl App {
    /// Asks for a frame of the project to be written beside the file (in the
    /// background). Returns where it goes, or `None` if there's nothing to draw.
    pub(crate) fn write_thumbnail(&mut self, project_path: &Path) -> Option<PathBuf> {
        let duration = self.editor.duration();
        if duration <= Time::ZERO {
            return None;
        }
        // A frame with something on it: the playhead if it's inside the sequence,
        // otherwise a little way in (the first frame is often black).
        let at = if self.playhead > Time::ZERO && self.playhead < duration {
            self.playhead
        } else {
            Time::from_seconds_f64(duration.as_seconds_f64() * 0.2)
        };
        let canvas = self.editor.sequence().canvas();
        let scale = (WIDTH as f64 / canvas.width.max(1) as f64).min(1.0);
        let out = path_for(project_path);
        self.preview_worker.request(crate::preview_worker::Request {
            slot: crate::preview_worker::PROJECT_PICTURE,
            tag: 0,
            // The project as saved: edits made meanwhile don't leak into the picture.
            project: self.editor.doc.snapshot(),
            registry: self.registry.clone(),
            seq: self.editor.seq,
            variant: self.variant_id(),
            at,
            scale,
            wanted: None,
            png: Some(out.clone()),
            see_through: false,
        });
        Some(out)
    }
}

/// A sensible name for a project saved to `path`: the file name without its extensions,
/// with separators turned into spaces — "my_holiday_edit.oaproj.json" becomes
/// "my holiday edit".
pub fn name_from_path(path: &Path) -> String {
    let stem = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let stem = stem.split('.').next().unwrap_or(&stem);
    let name: String = stem.replace(['_', '-'], " ").trim().to_string();
    if name.is_empty() { "Untitled".into() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths_are_sensible() {
        assert_eq!(name_from_path(Path::new("/a/my_holiday_edit.oaproj.json")), "my holiday edit");
        assert_eq!(name_from_path(Path::new("cut-two.json")), "cut two");
        assert_eq!(name_from_path(Path::new("")), "Untitled");
        assert_eq!(path_for(Path::new("/a/b.oaproj.json")), Path::new("/a/b.oaproj.json.thumb.png"));
    }
}
