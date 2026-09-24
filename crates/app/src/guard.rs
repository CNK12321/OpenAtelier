//! The last line of defense for whatever the user does: a bug reached by some click
//! must not close the window and lose work. Each frame runs under `catch_unwind`; if it
//! panics, the project is autosaved at once, every half-finished gesture is dropped, the
//! editor is put back on a timeline that exists, and a notification says what happened.
//! If frames keep failing, the editor steps back to the start page (the project stays
//! recoverable from the autosave).

use crate::App;
use eframe::egui;

/// Failing frames in a row before giving up on the editor view.
const MAX_FAILED_FRAMES: u32 = 3;

impl App {
    pub(crate) fn guarded_ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.frame_ui(root, frame)));
        match result {
            Ok(()) => self.failed_frames = 0,
            Err(payload) => {
                let why = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown error".into());
                self.recover_from_panic(&why);
                root.ctx().request_repaint();
            }
        }
    }

    fn recover_from_panic(&mut self, why: &str) {
        self.failed_frames += 1;
        eprintln!("recovered from a panic (frame {} in a row): {why}", self.failed_frames);
        // Save first: whatever happens next, the work is on disk.
        if self.editor.dirty() {
            let _ = self.autosave.write_now(&self.editor.doc.snapshot(), self.editor.path.as_deref());
        }
        // Drop anything mid-gesture; it may be what broke.
        self.viewer_drag = None;
        self.timeline_drag = None;
        self.canvas_text = None;
        self.curve_editor = None;
        self.crop_mode = None;
        self.crop_drag = None;
        self.renaming = None;
        self.timeline_view.menu = None;
        self.set_playing(false);
        // Back onto a timeline that exists, with a selection that exists.
        if self.editor.doc.project().sequence(self.editor.seq).is_none() {
            self.close_compound(usize::MAX);
            if let Some(&first) = self.editor.doc.project().sequences.keys().next()
                && self.editor.doc.project().sequence(self.editor.seq).is_none()
            {
                self.editor.seq = first;
            }
        }
        self.selection = self.selection.filter(|id| self.editor.item(*id).is_some());
        self.selected.retain(|id| self.editor.item(*id).is_some());
        let seq = self.editor.sequence();
        self.variant = self.variant.min(seq.variants.len().saturating_sub(1));
        self.rendered = None;
        if self.failed_frames >= MAX_FAILED_FRAMES {
            self.failed_frames = 0;
            self.screen = crate::home::Screen::Home;
            self.report_error(format!("The editor kept failing ({why}). Your work was autosaved — reopen the project to continue."));
        } else {
            self.report_error(format!("Something went wrong ({why}). Your work was autosaved and the editor recovered."));
        }
    }
}
