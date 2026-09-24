//! The programs the editor runs: `ffmpeg` and `ffprobe` read, decode and write media.
//! The Windows packages carry them next to the program (found there first); elsewhere
//! they come from the system. Checked once at start, on a thread; if one is missing, a
//! bar says so and how to get it, instead of imports failing with a puzzling error.

use crate::App;
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};

/// Whether each program runs: the missing ones, by name.
fn missing_tools() -> Vec<&'static str> {
    ["ffmpeg", "ffprobe"]
        .into_iter()
        .filter(|tool| !oa_media::tool(tool).arg("-version").stdin(std::process::Stdio::null()).output().is_ok_and(|o| o.status.success()))
        .collect()
}

/// How to get them on this system.
fn how_to_install() -> &'static str {
    if cfg!(windows) {
        "Reinstall OpenAtelier with its installer (it includes them), or put ffmpeg.exe and ffprobe.exe next to OpenAtelier.exe."
    } else if cfg!(target_os = "macos") {
        "Install them with Homebrew: brew install ffmpeg"
    } else {
        "Install them with your package manager, e.g. sudo apt install ffmpeg (Debian, Ubuntu) or sudo dnf install ffmpeg (Fedora)."
    }
}

#[derive(Default)]
pub struct Deps {
    checking: Option<Receiver<Vec<&'static str>>>,
    missing: Vec<&'static str>,
    dismissed: bool,
}

impl App {
    pub(crate) fn check_deps(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(missing_tools());
        });
        self.deps.checking = Some(rx);
    }

    /// A bar across the top when ffmpeg or ffprobe can't be run.
    pub(crate) fn deps_banner(&mut self, root: &mut egui::Ui) {
        if let Some(rx) = &self.deps.checking
            && let Ok(missing) = rx.try_recv()
        {
            self.deps.missing = missing;
            self.deps.checking = None;
        }
        if self.deps.missing.is_empty() || self.deps.dismissed {
            return;
        }
        let names = self.deps.missing.join(" and ");
        egui::Panel::top("deps-banner").show(root, |ui| {
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("⚠ {names} can't be found: importing, playing and exporting video need it.")).color(crate::style::WARNING));
                ui.label(how_to_install());
                if ui.button("Check again").clicked() {
                    self.check_deps();
                }
                if ui.button("Dismiss").clicked() {
                    self.deps.dismissed = true;
                }
            });
            ui.add_space(3.0);
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_program_that_is_not_there_counts_as_missing() {
        let runs = |tool: &str| oa_media::tool(tool).arg("-version").output().is_ok_and(|o| o.status.success());
        assert!(!runs("oa-no-such-program-anywhere"));
    }
}
