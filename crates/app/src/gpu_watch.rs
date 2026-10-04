//! Keeping plugin shaders from taking the GPU down.
//!
//! A plugin's WGSL runs on the GPU, where nothing can stop a shader that runs too long:
//! after about two seconds the system resets the driver (Windows' TDR), and the device
//! is lost (`gpu_reset.rs` then restarts the app). So the viewer's frames are watched:
//!
//! * When a frame is handed to the GPU, the effects it uses are noted; when the GPU has
//!   finished it, the note is cleared (`on_submitted_work_done`).
//! * A frame still unfinished after [`STALL`] (well under the reset's two seconds) that
//!   uses effects of plugins other than Atelier Core: those plugins are turned off at
//!   once, and the user is told. A slow shader stops there; one that never ends still
//!   resets the GPU, but comes back turned off.
//! * The device lost while such a frame was on the GPU: the same, before restarting.
//!
//! Atelier Core's own effects are never blamed; a stall without plugin effects is left
//! alone (a big frame on a slow GPU).
//!
//! Limits on what a plugin may ask for (shader size, passes, parameters) are checked when
//! it's loaded (`oa_graph::plugin`).

use crate::i18n::tr;
use crate::i18n::trf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A frame on the GPU this long is stuck.
pub const STALL: Duration = Duration::from_millis(1200);

#[derive(Default)]
struct InFlight {
    /// Which frame (the watch's own count), since when, and the effects it uses.
    frame: u64,
    since: Option<Instant>,
    effects: Vec<Arc<str>>,
}

/// Shared by the render thread (which notes frames) and the UI (which checks).
#[derive(Clone, Default)]
pub struct GpuWatch {
    state: Arc<Mutex<InFlight>>,
}

impl GpuWatch {
    /// A frame using `effects` was just submitted to `queue`.
    pub fn submitted(&self, queue: &wgpu::Queue, effects: Vec<Arc<str>>) {
        let frame = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.frame += 1;
            s.since = Some(Instant::now());
            s.effects = effects;
            s.frame
        };
        let state = self.state.clone();
        queue.on_submitted_work_done(move || {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            // Only this frame's own finish clears it (a later one has its own note).
            if s.frame == frame {
                s.since = None;
                s.effects.clear();
            }
        });
    }

    /// The effects of a frame the GPU has had longer than `limit` without finishing, if
    /// there is one (taken: reported once). `device` is polled first, so finished work is
    /// known about even when nothing else is submitting.
    pub fn stalled(&self, device: &wgpu::Device, limit: Duration) -> Option<Vec<Arc<str>>> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.since.is_some_and(|t| t.elapsed() > limit) {
            s.since = None;
            return Some(std::mem::take(&mut s.effects));
        }
        None
    }

    /// The effects of the frame on the GPU now, if one is (the device just went away).
    pub fn in_flight(&self) -> Vec<Arc<str>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.since.is_some() { s.effects.clone() } else { Vec::new() }
    }
}

impl crate::App {
    /// The plugins (other than Atelier Core) that provide any of `effects`: (id, name).
    pub(crate) fn plugins_providing(&self, effects: &[Arc<str>]) -> Vec<(String, String)> {
        self.plugins
            .list
            .iter()
            .filter(|p| !p.builtin && p.effects.iter().any(|e| effects.contains(&e.type_id)))
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect()
    }

    /// Turns off the plugins behind `effects` that stalled or lost the GPU, and says so.
    /// Returns their names (none: nothing of a plugin's was running).
    pub(crate) fn turn_off_gpu_suspects(&mut self, effects: &[Arc<str>], what: &str) -> Vec<String> {
        let suspects = self.plugins_providing(effects);
        for (id, _) in &suspects {
            self.settings.set_plugin_enabled(id, false);
        }
        let names: Vec<String> = suspects.into_iter().map(|(_, name)| name).collect();
        if !names.is_empty() {
            self.reload_plugins();
            self.notify(trf("{what} while {0} was drawing, so it's been turned off to keep the editor safe. Turn it back on in the Plugins window if you trust it.", &[("what", (what)), ("0", &(names.join(" and ")).to_string())]));
        }
        names
    }

    /// Each frame: a viewer frame stuck on the GPU with plugin effects in it turns those
    /// plugins off before the system resets the driver.
    pub(crate) fn watch_gpu(&mut self) {
        if self.gpu_lost {
            return;
        }
        if let Some(effects) = self.gpu_watch.stalled(&self.gpu.device, STALL) {
            self.turn_off_gpu_suspects(&effects, tr("The GPU took too long on a frame"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's note stays while the GPU works on it, clears when it's done, and a frame
    /// older than the limit is reported once.
    #[test]
    fn frames_are_watched_until_the_gpu_finishes_them() {
        let Ok(ctx) = oa_gpu::GpuContext::new_headless() else { return };
        let watch = GpuWatch::default();
        let effects: Vec<Arc<str>> = vec!["com.example.slow".into()];
        // Nothing submitted: nothing stalled.
        assert!(watch.stalled(&ctx.device, Duration::ZERO).is_none());
        ctx.queue.submit([]);
        watch.submitted(&ctx.queue, effects.clone());
        // Done (an empty submission finishes at once): cleared, never reported.
        let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(watch.in_flight().is_empty());
        assert!(watch.stalled(&ctx.device, Duration::ZERO).is_none());
        // A note whose finish hasn't come in: stalled past a zero limit, reported once.
        {
            let mut s = watch.state.lock().unwrap();
            s.frame += 1;
            s.since = Some(Instant::now());
            s.effects = effects.clone();
        }
        assert_eq!(watch.stalled(&ctx.device, Duration::ZERO), Some(effects));
        assert!(watch.stalled(&ctx.device, Duration::ZERO).is_none());
    }
}
