//! Where the panels go, and remembering it per project.
//!
//! Two layouts: **Standard** (the viewer in the middle, the properties in the tall column
//! on the right) and **Vertical**, for 9:16 work, which swaps them — the tall column is
//! just the shape of a phone video. **Auto** picks Vertical while the format being
//! edited is taller than it is wide. A project can pick its own (the viewer's toolbar);
//! otherwise Settings' default applies.
//!
//! Each saved project also remembers its panel sizes, its layout and the format it was
//! showing (`Settings::project_views`, by file path), put back when it's opened again.
//! Panels are keyed by an epoch that changes on every restore, so egui takes the saved
//! sizes as fresh defaults instead of the sizes it remembered for the last project.

use crate::settings::{Layout, ProjectView, MAX_PROJECT_VIEWS};
use crate::App;
use eframe::egui;

impl App {
    /// The layout in effect: the project's own, or the default from Settings.
    pub(crate) fn layout(&self) -> Layout {
        self.project_view.layout.unwrap_or(self.settings.layout)
    }

    /// Whether the viewer is in the tall right-hand column.
    pub(crate) fn vertical_layout(&self) -> bool {
        match self.layout() {
            Layout::Vertical => true,
            Layout::Standard => false,
            Layout::Auto => {
                let s = self.editor.sequence();
                let size = s.variants[self.variant.min(s.variants.len() - 1)].size;
                size.height > size.width
            }
        }
    }

    /// Switches this project between the standard and vertical layouts.
    pub(crate) fn toggle_layout(&mut self) {
        let vertical = self.vertical_layout();
        self.project_view.layout = Some(if vertical { Layout::Standard } else { Layout::Vertical });
        self.view_epoch += 1;
    }

    /// Puts back how the open project was last seen (sizes, layout, format), or the
    /// defaults for a new one.
    pub(crate) fn restore_view(&mut self) {
        let saved = self.editor.path.as_ref().and_then(|p| self.settings.project_views.get(&key(p)).cloned());
        self.project_view = saved.unwrap_or_default();
        if let Some(id) = self.project_view.variant {
            let variants = &self.editor.sequence().variants;
            self.variant = variants.iter().position(|v| v.id.0 == id).unwrap_or(0);
        }
        self.view_epoch += 1;
    }

    /// Records panel sizes as drawn this frame (`right` is the right-hand column: the
    /// properties, or the viewer in the vertical layout).
    pub(crate) fn note_panel_sizes(&mut self, right: f32, media: f32, timeline: f32) {
        let vertical = self.vertical_layout();
        let v = &mut self.project_view;
        if vertical {
            v.viewer = right;
        } else {
            v.inspector = right;
        }
        v.media = media;
        v.timeline = timeline;
    }

    /// Saves the open project's view when it has changed, once the pointer is up (not
    /// on every step of a panel drag).
    pub(crate) fn remember_view(&mut self, ctx: &egui::Context) {
        let Some(path) = self.editor.path.clone() else { return };
        let s = self.editor.sequence();
        self.project_view.variant = s.variants.get(self.variant).map(|v| v.id.0);
        // The key is worked out once per path (it touches the disk), not every frame.
        let k = match &self.view_key {
            Some((p, k)) if *p == path => k.clone(),
            _ => {
                let k = key(&path);
                self.view_key = Some((path, k.clone()));
                k
            }
        };
        let round = |v: &ProjectView| ProjectView {
            inspector: v.inspector.round(),
            media: v.media.round(),
            timeline: v.timeline.round(),
            viewer: v.viewer.round(),
            ..v.clone()
        };
        let now = round(&self.project_view);
        if self.settings.project_views.get(&k) == Some(&now) || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.settings.project_views.insert(k, now);
        while self.settings.project_views.len() > MAX_PROJECT_VIEWS {
            let Some(first) = self.settings.project_views.keys().next().cloned() else { break };
            self.settings.project_views.remove(&first);
        }
        self.settings.save();
    }
}

impl App {
    /// The first frames: puts the window the way it should open. Asking for "maximized"
    /// when the window is created doesn't always take (it came back at the small size it
    /// had before being maximized), so it's asked again once the window exists; and a
    /// window much smaller than the screen (a size remembered from a small window, a
    /// first run on a big screen) or bigger than it is fitted to 85% of the screen,
    /// centered.
    pub(crate) fn fit_window(&mut self, ctx: &egui::Context) {
        let Some(frames) = self.window_fitting else { return };
        if self.script.is_some() {
            self.window_fitting = None;
            return;
        }
        self.window_fitting = Some(frames + 1);
        let (inner, maximized, monitor) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().maximized, i.viewport().monitor_size));
        let Some(inner) = inner else { return };
        // The screen's size can take a few frames to be known (and some systems never
        // tell): then settle for a sensible minimum.
        let monitor = match monitor {
            Some(m) if m.x > 0.0 && m.y > 0.0 => m,
            _ if frames < 30 => return,
            _ => egui::vec2(1600.0, 1000.0),
        };
        self.window_fitting = None;
        if self.settings.window.as_ref().is_some_and(|w| w.maximized) {
            if maximized != Some(true) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            return;
        }
        if maximized == Some(true) {
            return;
        }
        let small = inner.width() < monitor.x * 0.6 || inner.height() < monitor.y * 0.6;
        let big = inner.width() > monitor.x || inner.height() > monitor.y;
        if small || big {
            let size = egui::vec2((monitor.x * 0.85).max(800.0), (monitor.y * 0.85).max(500.0));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(((monitor - size) * 0.5).to_pos2()));
        }
    }

    /// Keeps the main window's size and maximized state for the next launch (the size
    /// only while it's a normal window, so un-maximizing goes back to it).
    pub(crate) fn remember_window(&mut self, ctx: &egui::Context) {
        if self.script.is_some() {
            return;
        }
        let (inner, maximized, down) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().maximized.unwrap_or(false), i.pointer.any_down()));
        let Some(inner) = inner else { return };
        let mut now = self.settings.window.clone().unwrap_or(crate::settings::WindowState { size: [1440.0, 900.0], maximized: false });
        now.maximized = maximized;
        // Not while it's being fitted, and never a size too small to work in.
        if self.window_fitting.is_some() {
            return;
        }
        if !maximized && inner.width() >= 800.0 && inner.height() >= 500.0 {
            now.size = [inner.width().round(), inner.height().round()];
        }
        if down || self.settings.window.as_ref() == Some(&now) {
            return;
        }
        // An OS resize drag doesn't show as a pointer press: wait until the size has
        // held for half a second before writing it.
        let t = ctx.input(|i| i.time);
        match self.window_pending.take() {
            Some((pending, since)) if pending == now && t - since >= 0.5 => {
                self.settings.window = Some(now);
                self.settings.save();
            }
            Some((pending, since)) if pending == now => {
                self.window_pending = Some((pending, since));
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
            _ => {
                self.window_pending = Some((now, t));
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }
}

fn key(path: &std::path::Path) -> String {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_string()
}
