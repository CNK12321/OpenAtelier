//! How the editor looks: the few numbers and colors everything else is built from.
//!
//! Spacing sits on a 4 px grid, text on a four-step scale, and one accent color drives
//! selection, focus and the playhead. Keeping them here — rather than sprinkling
//! magic numbers through each panel — is what stops the UI drifting as it grows.

use eframe::egui;

/// The 4 px grid: gaps between controls, inside cards, between sections.
pub const GAP_S: f32 = 4.0;
pub const GAP: f32 = 8.0;
pub const GAP_L: f32 = 12.0;
pub const GAP_XL: f32 = 16.0;

/// Text sizes: small print, body, section heading, page title.
pub const TEXT_S: f32 = 11.0;
pub const TEXT: f32 = 13.0;
pub const TEXT_L: f32 = 15.0;
pub const TITLE: f32 = 20.0;

/// Icon buttons are all this size, so rows of them line up.
pub const ICON: f32 = 24.0;

pub const ROUNDING: f32 = 5.0;

/// Selection, focus, the playhead: one color, used for "this is the thing".
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 160, 240);
/// Groups, keyframes and the playhead's own marker.
pub const GOLD: egui::Color32 = egui::Color32::from_rgb(240, 200, 80);
pub const WARNING: egui::Color32 = egui::Color32::from_rgb(235, 180, 90);
pub const ERROR: egui::Color32 = egui::Color32::from_rgb(225, 110, 110);

/// Applies the look to a context. Called once at startup (and again if the user
/// changes the theme).
pub fn install(ctx: &egui::Context) {
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(GAP, GAP_S + 2.0);
        style.spacing.button_padding = egui::vec2(GAP, GAP_S + 1.0);
        style.spacing.menu_margin = egui::Margin::same(GAP_S as i8);
        style.spacing.indent = GAP_XL;
        style.spacing.slider_width = 140.0;
        style.spacing.interact_size.y = ICON;

        use egui::{FontFamily::Proportional, FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Small, FontId::new(TEXT_S, Proportional)),
            (TextStyle::Body, FontId::new(TEXT, Proportional)),
            (TextStyle::Button, FontId::new(TEXT, Proportional)),
            (TextStyle::Heading, FontId::new(TEXT_L, Proportional)),
            (TextStyle::Monospace, FontId::new(TEXT, egui::FontFamily::Monospace)),
        ]
        .into();

        let v = &mut style.visuals;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.window_corner_radius = egui::CornerRadius::same(ROUNDING as u8);
        v.menu_corner_radius = egui::CornerRadius::same(ROUNDING as u8);
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open, &mut v.widgets.noninteractive] {
            w.corner_radius = egui::CornerRadius::same(ROUNDING as u8);
        }
        // A quieter window: panels a shade darker than the content they hold.
        v.panel_fill = egui::Color32::from_gray(24);
        v.window_fill = egui::Color32::from_gray(30);
        v.extreme_bg_color = egui::Color32::from_gray(16);
        v.faint_bg_color = egui::Color32::from_gray(34);

    });
}
