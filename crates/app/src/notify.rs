//! Messages to the user, in the bottom-right corner.
//!
//! Anything that fails where the user can't be interrupted — a file that won't delete, an
//! edit the document refused, a plugin that wouldn't load — becomes a card down there
//! instead of a silent failure or a panic. Errors stay a good while and can be clicked
//! away; notes fade on their own. Everything also goes to the Messages list in the
//! inspector, so nothing is lost if a card goes before it's read.

use crate::App;
use eframe::egui;

/// How long a card stays before it fades out, by kind.
const ERROR_SECS: f64 = 12.0;
const NOTE_SECS: f64 = 5.0;
const FADE: f64 = 0.5;
const WIDTH: f32 = 330.0;


#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Level {
    Error,
    Note,
}

pub struct Toast {
    pub text: String,
    pub level: Level,
    /// UI time it appeared; `None` until the first frame that draws it (the clock only
    /// exists inside the UI).
    pub since: Option<f64>,
}

impl Toast {
    fn life(&self) -> f64 {
        match self.level {
            Level::Error => ERROR_SECS,
            Level::Note => NOTE_SECS,
        }
    }
}

impl App {
    /// Something went wrong, in words the user can act on.
    pub(crate) fn report_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.messages.push(text.clone());
        self.toasts.push(Toast { text, level: Level::Error, since: None });
    }

    /// Something worth mentioning went right ("saved …", "recovered …").
    pub(crate) fn notify(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.messages.push(text.clone());
        self.toasts.push(Toast { text, level: Level::Note, since: None });
    }

    /// Draws the cards, oldest at the bottom, and drops the ones that have had their
    /// time. Called every frame, on the start page as well as in the editor.
    pub(crate) fn toasts(&mut self, ctx: &egui::Context) {
        // Errors set the old way still reach the corner.
        if let Some(e) = self.error.take() {
            self.report_error(e);
        }
        if self.toasts.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let mut dismissed = Vec::new();
        let mut y = 12.0;
        for (i, toast) in self.toasts.iter_mut().enumerate().rev() {
            let since = *toast.since.get_or_insert(now);
            let age = now - since;
            let left = toast.life() - age;
            if left <= 0.0 {
                dismissed.push(i);
                continue;
            }
            let fade = (left / FADE).clamp(0.0, 1.0) as f32;
            let (fill, edge) = match toast.level {
                Level::Error => (egui::Color32::from_rgb(70, 28, 28), crate::style::ERROR),
                Level::Note => (egui::Color32::from_rgb(34, 38, 44), egui::Color32::from_rgb(120, 140, 160)),
            };
            let shown = egui::Area::new(egui::Id::new(("toast", i)))
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -y))
                .interactable(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let frame = egui::Frame::new()
                        .fill(fill.gamma_multiply(fade))
                        .stroke(egui::Stroke::new(1.0, edge.gamma_multiply(fade)))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(WIDTH);
                            ui.horizontal(|ui| {
                                let icon = if toast.level == Level::Error { "⚠" } else { "ⓘ" };
                                ui.label(egui::RichText::new(icon).color(edge.gamma_multiply(fade)));
                                ui.label(egui::RichText::new(&toast.text).color(egui::Color32::WHITE.gamma_multiply(fade)));
                            });
                        });
                    // Its own id: interacting with the area's response would reuse the
                    // area's id and egui flags that as a widget used twice.
                    ui.interact(frame.response.rect, ui.id().with("dismiss"), egui::Sense::click())
                });
            let card = shown.inner;
            y += shown.response.rect.height() + 8.0;
            if card.clicked() {
                dismissed.push(i);
            }
            if card.hovered() {
                // Keep it up while it's being read.
                toast.since = Some(now - 0.1);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        for i in dismissed {
            self.toasts.remove(i);
        }
        // Only the newest handful are worth the screen.
        while self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }
}

/// Checks a name typed into a text box: something, not just spaces, and not absurd.
/// Returns the trimmed name or why it can't be used.
pub fn check_name(kind: &str, text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(crate::i18n::args("error.name_empty", &[("kind", kind)]));
    }
    if trimmed.chars().count() > 120 {
        return Err(crate::i18n::args("error.name_long", &[("kind", kind)]));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        assert_eq!(check_name("track", "  V2 "), Ok("V2".into()));
        assert!(check_name("track", "   ").is_err());
        assert!(check_name("track", &"x".repeat(200)).is_err());
    }
}
