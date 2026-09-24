//! Listening to a sound without putting it on the timeline.
//!
//! The bin's audio cards have a play button: it opens a second output stream with that
//! one file on it, so you can hear what a whoosh is before deciding where it goes. It
//! stops when the file ends, when you click it again, when another one starts, or when
//! the timeline starts playing — the sequence always wins, since that's the thing being
//! edited.

use crate::App;
use oa_audio::{AudioClip, AudioEngine, TimelineAudio};
use oa_time::{Time, TimeRange};
use std::path::{Path, PathBuf};

/// The longest a preview runs before stopping itself.
const MOST: Time = Time::from_seconds(120);

pub struct Audition {
    engine: AudioEngine,
    pub path: PathBuf,
}

impl App {
    /// Whether this file is the one currently playing.
    pub(crate) fn auditioning(&self, path: &Path) -> bool {
        self.audition.as_ref().is_some_and(|a| a.path == path)
    }

    /// Plays `path` on its own, from the start. Clicking the same one again stops it.
    pub(crate) fn audition(&mut self, path: &Path, duration: Time) {
        if self.auditioning(path) {
            self.audition = None;
            return;
        }
        self.audition = None;
        let length = if duration > Time::ZERO { duration.min(MOST) } else { MOST };
        let mut clip = AudioClip::new(0, 0, path.to_path_buf(), TimeRange::new(Time::ZERO, length), Time::ZERO);
        clip.fade_out = Time::from_seconds_f64(0.05);
        let gain = self.volume;
        match AudioEngine::new(move |format| Box::new(TimelineAudio::new(vec![clip], format, Time::ZERO, length)), Time::ZERO) {
            Ok(engine) => {
                engine.set_gain(gain);
                engine.set_playing(true);
                self.audition = Some(Audition { engine, path: path.to_path_buf() });
            }
            Err(e) => self.report_error(format!("can't play that: {e}")),
        }
    }

    /// Called every frame: stops the preview when it's done, or when the sequence itself
    /// starts playing.
    pub(crate) fn audition_tick(&mut self) {
        let done = match &self.audition {
            Some(a) => self.playing || a.engine.ended(),
            None => return,
        };
        if done {
            self.audition = None;
        }
    }
}
