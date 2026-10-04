//! Coming back from a lost GPU (a driver reset, a GPU removed, a crash elsewhere).
//!
//! The window draws with the same device the renderer uses (eframe owns it), so when it
//! goes, nothing in this process can draw again. What can be done is to start over
//! without losing a thing: the work is saved (the project file if it had no unsaved
//! changes, an autosave otherwise), a new OpenAtelier starts and opens it where the
//! playhead was, and this one closes. The new one says what happened.
//!
//! A GPU that keeps failing would restart forever: a reset again within a few minutes of
//! the last restart stops there and says so instead, pointing at Settings → Graphics.

use crate::i18n::{tr, trf};
use crate::App;
use oa_time::Time;
use std::path::PathBuf;

/// What a restart after a reset is asked to open (command-line arguments).
pub const RECOVER_ARG: &str = "--after-gpu-reset";
pub const OPEN_ARG: &str = "--after-gpu-reset-open";
pub const PLAYHEAD_ARG: &str = "--playhead";
/// Plugins turned off because they were drawing when the GPU was lost (names, one
/// argument, separated by newlines).
pub const TURNED_OFF_ARG: &str = "--gpu-turned-off";

/// How many restarts in a row, and when the last was: passed down to the new process.
const RESETS_ENV: &str = "OA_GPU_RESETS";
const RESET_AT_ENV: &str = "OA_GPU_RESET_AT";

/// A second reset within this long of a restart doesn't restart again.
const TOO_SOON_SECS: u64 = 180;

/// What a restart was asked to bring back (from the command line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resume {
    /// An autosave to recover (there were unsaved changes).
    pub recover: Option<PathBuf>,
    /// A project to open (everything was saved).
    pub open: Option<PathBuf>,
    pub playhead: Option<Time>,
    pub turned_off: Vec<String>,
}

impl Resume {
    /// Takes this module's arguments out of `args`, leaving the rest (files to open…).
    pub fn take_from(args: &mut Vec<String>) -> Resume {
        let mut value = |flag: &str| {
            let i = args.iter().position(|a| a == flag)?;
            let v = args.get(i + 1).cloned();
            args.drain(i..(i + 2).min(args.len()));
            v
        };
        Resume {
            recover: value(RECOVER_ARG).map(PathBuf::from),
            open: value(OPEN_ARG).map(PathBuf::from),
            playhead: value(PLAYHEAD_ARG).and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite() && *s >= 0.0).map(Time::from_seconds_f64),
            turned_off: value(TURNED_OFF_ARG).map(|s| s.lines().map(str::to_string).filter(|l| !l.is_empty()).collect()).unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.recover.is_none() && self.open.is_none()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Whether this process itself was a restart after a reset that happened `within`
/// seconds ago (from its environment).
fn restarted_recently(resets: Option<&str>, at: Option<&str>, now: u64, within: u64) -> bool {
    let resets: u32 = resets.and_then(|s| s.parse().ok()).unwrap_or(0);
    let at: u64 = at.and_then(|s| s.parse().ok()).unwrap_or(0);
    resets > 0 && now.saturating_sub(at) < within
}

impl App {
    /// The device is gone: save the work and start again in a new process (see the
    /// module notes). Doesn't return when it restarts; an error means it didn't (lost again
    /// right after a restart, or the program couldn't be started): the caller says so.
    pub(crate) fn restart_after_gpu_reset(&mut self) -> Result<(), String> {
        let env = |k: &str| std::env::var(k).ok();
        if restarted_recently(env(RESETS_ENV).as_deref(), env(RESET_AT_ENV).as_deref(), now_secs(), TOO_SOON_SECS) {
            return Err("the graphics device was lost again right after restarting".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut command = std::process::Command::new(exe);
        // Everything saved: open the project itself. Otherwise the autosave, which is
        // tied back to its project when recovered.
        match self.editor.path.clone().filter(|_| !self.editor.dirty()) {
            Some(project) => {
                command.arg(OPEN_ARG).arg(project);
            }
            None => {
                self.autosave.write_now(&self.editor.doc.snapshot(), self.editor.path.as_deref())?;
                command.arg(RECOVER_ARG).arg(self.autosave.path());
            }
        }
        command.arg(PLAYHEAD_ARG).arg(format!("{:.6}", self.playhead.as_seconds_f64()));
        if !self.gpu_turned_off.is_empty() {
            command.arg(TURNED_OFF_ARG).arg(self.gpu_turned_off.join("\n"));
        }
        let resets: u32 = env(RESETS_ENV).and_then(|s| s.parse().ok()).unwrap_or(0);
        command.env(RESETS_ENV, (resets + 1).to_string()).env(RESET_AT_ENV, now_secs().to_string());
        command.spawn().map_err(|e| format!("couldn't start OpenAtelier again: {e}"))?;
        // This process can't draw any more; what it runs on the side goes with it.
        self.proxies.stop();
        std::process::exit(0);
    }

    /// At start: bring back what a restart after a reset was asked to (see [`Resume`]).
    pub(crate) fn resume_after_gpu_reset(&mut self, resume: Resume) {
        if resume.is_empty() {
            return;
        }
        if let Some(file) = &resume.recover {
            let found = self.recoveries.iter().find(|r| &r.file == file).cloned();
            match found {
                Some(r) => self.recover(r, true),
                None => self.report_error(trf("The graphics driver reset, and the work it saved ({0}) couldn't be found.", &[("0", &(file.display()).to_string())])),
            }
        } else if let Some(project) = &resume.open {
            self.open_project(project);
            self.screen = crate::home::Screen::Editor;
        }
        self.resume_playhead = resume.playhead;
        self.notify(tr("The graphics driver reset. OpenAtelier restarted and brought your work back."));
        if !resume.turned_off.is_empty() {
            self.notify(trf("{0} was drawing when it happened, so it's been turned off. Turn it back on in the Plugins window if you trust it.", &[("0", &(resume.turned_off.join(" and ")).to_string())]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_arguments_are_taken_out_of_the_rest() {
        let mut args: Vec<String> = ["a.mp4", RECOVER_ARG, "C:/x/1-2.oaproj.json", PLAYHEAD_ARG, "12.5", "--autoplay"].map(String::from).to_vec();
        let r = Resume::take_from(&mut args);
        assert_eq!(args, vec!["a.mp4".to_string(), "--autoplay".into()]);
        assert_eq!(r.recover, Some(PathBuf::from("C:/x/1-2.oaproj.json")));
        assert_eq!(r.open, None);
        assert_eq!(r.playhead, Some(Time::from_seconds_f64(12.5)));
        let mut none: Vec<String> = vec!["b.mov".into()];
        assert!(Resume::take_from(&mut none).is_empty());
        assert_eq!(none, vec!["b.mov".to_string()]);
    }

    #[test]
    fn a_reset_right_after_a_restart_stops_the_loop() {
        assert!(!restarted_recently(None, None, 1000, TOO_SOON_SECS), "a first reset restarts");
        assert!(restarted_recently(Some("1"), Some("900"), 1000, TOO_SOON_SECS), "again 100 s later: stop");
        assert!(!restarted_recently(Some("1"), Some("500"), 1000, TOO_SOON_SECS), "minutes later: restart");
    }
}
