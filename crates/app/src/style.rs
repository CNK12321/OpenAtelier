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
    // What both themes share: spacing, text sizes, rounding, the accent.
    ctx.all_styles_mut(|style| {
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
    });
    dark(ctx, false);
    // Light: soft gray panels around white fields, so the picture and the clips stand
    // out rather than the chrome; the accent darker, to read on white.
    ctx.style_mut_of(egui::Theme::Light, |style| {
        let v = &mut style.visuals;
        v.panel_fill = egui::Color32::from_gray(238);
        v.window_fill = egui::Color32::from_gray(248);
        v.extreme_bg_color = egui::Color32::from_gray(255);
        v.faint_bg_color = egui::Color32::from_gray(228);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.hyperlink_color = egui::Color32::from_rgb(30, 110, 200);
    });
}

/// The dark theme's panels: a quieter window, panels a shade darker than the content
/// they hold — or, `black`, true black with controls a step above it.
pub fn dark(ctx: &egui::Context, black: bool) {
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        let gray = egui::Color32::from_gray;
        // egui's own dark controls to start from (so Black can be switched back off).
        let base = egui::Visuals::dark().widgets;
        for (w, b) in [
            (&mut v.widgets.noninteractive, base.noninteractive),
            (&mut v.widgets.inactive, base.inactive),
            (&mut v.widgets.hovered, base.hovered),
            (&mut v.widgets.active, base.active),
            (&mut v.widgets.open, base.open),
        ] {
            (w.bg_fill, w.weak_bg_fill, w.bg_stroke) = (b.bg_fill, b.weak_bg_fill, b.bg_stroke);
        }
        if black {
            (v.panel_fill, v.window_fill, v.extreme_bg_color, v.faint_bg_color) = (gray(0), gray(8), gray(0), gray(14));
            v.widgets.noninteractive.bg_fill = gray(0);
            v.widgets.noninteractive.weak_bg_fill = gray(0);
            v.widgets.noninteractive.bg_stroke.color = gray(40);
            for (w, fill) in [(&mut v.widgets.inactive, 26), (&mut v.widgets.open, 30), (&mut v.widgets.hovered, 38), (&mut v.widgets.active, 48)] {
                (w.bg_fill, w.weak_bg_fill) = (gray(fill), gray(fill));
            }
        } else {
            (v.panel_fill, v.window_fill, v.extreme_bg_color, v.faint_bg_color) = (gray(24), gray(30), gray(16), gray(34));
        }
    });
}

/// Whether the dark theme's panels are the black ones now.
pub fn is_black(ctx: &egui::Context) -> bool {
    ctx.style_of(egui::Theme::Dark).visuals.panel_fill == egui::Color32::from_gray(0)
}
