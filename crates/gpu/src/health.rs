//! Watching the GPU: memory pressure, and the device going away.
//!
//! wgpu can't tell us how much VRAM is free, so the renderer keeps its own count of what
//! it allocated — the texture pool and the node cache — and works to a budget. As that
//! budget fills, the renderer gives memory back instead of allocating past it: idle
//! textures are released the same frame, the cache is trimmed, and the host is told to
//! render the preview smaller. Running out is a last resort, not the first thing that
//! happens on a big project.
//!
//! The device itself can still go away — a driver reset, a GPU unplugged, a crash in
//! another process. The callbacks here notice, so the host can save the user's work and
//! say what happened rather than looping on failed frames.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// How close the renderer is to its memory budget.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Pressure {
    /// Plenty of room.
    #[default]
    Easy,
    /// Over three quarters: the cache is being trimmed.
    Tight,
    /// At or past the budget: render smaller.
    Over,
}

/// What the renderer is holding, and what it's allowed to hold.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Transient render targets held by the pool.
    pub pool_bytes: u64,
    /// Cached node outputs.
    pub cache_bytes: u64,
    pub budget: u64,
    pub pressure: Pressure,
}

impl Memory {
    pub fn used(&self) -> u64 {
        self.pool_bytes + self.cache_bytes
    }

    /// 0…1+ of the budget.
    pub fn fraction(&self) -> f32 {
        if self.budget == 0 {
            return 0.0;
        }
        self.used() as f32 / self.budget as f32
    }

    pub fn of(pool_bytes: u64, cache_bytes: u64, budget: u64) -> Self {
        let mut m = Memory { pool_bytes, cache_bytes, budget, pressure: Pressure::Easy };
        m.pressure = match m.fraction() {
            f if f >= 1.0 => Pressure::Over,
            f if f >= 0.75 => Pressure::Tight,
            _ => Pressure::Easy,
        };
        m
    }
}

/// Shared with the device's callbacks: what has gone wrong, if anything.
#[derive(Default)]
pub struct GpuHealth {
    lost: AtomicBool,
    out_of_memory: AtomicBool,
    errors: AtomicU64,
    message: Mutex<Option<String>>,
}

impl GpuHealth {
    /// The device is gone. Nothing will render again on this one.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// An allocation failed since this was last asked, and the flag is cleared: the
    /// caller is expected to free what it can.
    pub fn take_out_of_memory(&self) -> bool {
        self.out_of_memory.swap(false, Ordering::Relaxed)
    }

    /// How many errors the device has reported (validation included).
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// What went wrong most recently, for the user.
    pub fn message(&self) -> Option<String> {
        self.message.lock().ok().and_then(|m| m.clone())
    }

    fn note(&self, text: String) {
        if let Ok(mut m) = self.message.lock() {
            *m = Some(text);
        }
    }

    /// Hooks the device's error and device-lost callbacks up to this.
    pub fn watch(self: &Arc<Self>, device: &wgpu::Device) {
        let on_error = Arc::clone(self);
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            on_error.errors.fetch_add(1, Ordering::Relaxed);
            if matches!(error, wgpu::Error::OutOfMemory { .. }) {
                on_error.out_of_memory.store(true, Ordering::Relaxed);
            }
            on_error.note(error.to_string());
            eprintln!("gpu error: {error}");
        }));
        let on_lost = Arc::clone(self);
        device.set_device_lost_callback(move |reason, message| {
            // Dropping the device on purpose (shutdown) isn't a failure.
            if matches!(reason, wgpu::DeviceLostReason::Destroyed) {
                return;
            }
            on_lost.lost.store(true, Ordering::Relaxed);
            on_lost.note(format!("the graphics device was lost ({reason:?}): {message}"));
            eprintln!("gpu lost: {reason:?}: {message}");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_follows_the_budget() {
        assert_eq!(Memory::of(10, 10, 100).pressure, Pressure::Easy);
        assert_eq!(Memory::of(40, 40, 100).pressure, Pressure::Tight);
        assert_eq!(Memory::of(60, 60, 100).pressure, Pressure::Over);
        // No budget set: nothing to be over.
        assert_eq!(Memory::of(10, 10, 0).pressure, Pressure::Easy);
        assert_eq!(Memory::of(3, 4, 100).used(), 7);
    }

    #[test]
    fn health_starts_clean_and_clears_its_flags() {
        let h = GpuHealth::default();
        assert!(!h.is_lost() && !h.take_out_of_memory() && h.errors() == 0 && h.message().is_none());
        h.out_of_memory.store(true, Ordering::Relaxed);
        assert!(h.take_out_of_memory(), "reported once");
        assert!(!h.take_out_of_memory(), "and then cleared");
        h.note("bang".into());
        assert_eq!(h.message().as_deref(), Some("bang"));
    }
}
