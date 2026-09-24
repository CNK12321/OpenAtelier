//! The Settings window (OpenAtelier menu → Settings…, or Ctrl+,): preferences kept on
//! this computer in `settings.json`, applied the moment they change.

use crate::settings::CurveShape;
use crate::App;
use eframe::egui;
use oa_time::Time;

impl App {
    /// How long a picture or a title is when it's added.
    pub(crate) fn still_length(&self) -> Time {
        let s = self.settings.still_seconds;
        Time::from_seconds_f64(if s.is_finite() { s.clamp(0.1, 3600.0) } else { 5.0 })
    }

    /// Puts the settings that live outside the settings file into effect.
    pub(crate) fn apply_settings(&mut self, ctx: &egui::Context) {
        oa_params::set_default_interp(self.settings.default_curve.interp());
        let scale = if self.settings.ui_scale.is_finite() { self.settings.ui_scale.clamp(0.6, 2.0) } else { 1.0 };
        if (ctx.zoom_factor() - scale).abs() > 1e-3 {
            ctx.set_zoom_factor(scale);
        }
        self.renderer.options.vram_budget = self.settings.vram_budget();
    }

    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let before = serde_json::to_string(&self.settings).unwrap_or_default();
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).default_width(380.0).show(ctx, |ui| {
            let s = &mut self.settings;
            ui.heading("Editing");
            ui.checkbox(&mut s.advanced_transform, "Advanced transformations")
                .on_hover_text("Squash and the crop sliders in Transform. Cropping by double-clicking a clip in the viewer works either way.");
            ui.checkbox(&mut s.advanced_color, "Advanced color")
                .on_hover_text("Source color (log and HDR curves, gamut, levels) and the Output tone map. Off: files are read by their own tags.");
            ui.add_space(6.0);
            ui.label("Default curve for new keyframes");
            ui.horizontal(|ui| {
                let label = |c: CurveShape| match c {
                    CurveShape::Linear => "Linear",
                    CurveShape::EaseIn => "Ease in",
                    CurveShape::EaseOut => "Ease out",
                    CurveShape::EaseInOut => "Ease in-out",
                    CurveShape::Hold => "Hold",
                };
                egui::ComboBox::from_id_salt("default-curve").selected_text(label(s.default_curve.shape)).show_ui(ui, |ui| {
                    for c in [CurveShape::Linear, CurveShape::EaseIn, CurveShape::EaseOut, CurveShape::EaseInOut, CurveShape::Hold] {
                        ui.selectable_value(&mut s.default_curve.shape, c, label(c));
                    }
                });
                let powered = matches!(s.default_curve.shape, CurveShape::EaseIn | CurveShape::EaseOut | CurveShape::EaseInOut);
                ui.add_enabled(powered, egui::Slider::new(&mut s.default_curve.power, 1.0..=5.0).step_by(0.1).text("power"));
            });
            curve_preview(ui, s.default_curve.interp());
            ui.horizontal(|ui| {
                ui.label("Pictures and titles last");
                ui.add(egui::DragValue::new(&mut s.still_seconds).range(0.1..=3600.0).speed(0.1).suffix(" s"));
            });

            ui.separator();
            ui.heading("Interface");
            // Resizing the interface under the pointer mid-drag would move the slider away
            // from it: the size applies when you let go.
            let pending = egui::Id::new("ui-scale-pending");
            let mut scale: f32 = ui.data(|d| d.get_temp(pending)).unwrap_or(s.ui_scale);
            let r = ui
                .add(egui::Slider::new(&mut scale, 0.6..=2.0).step_by(0.05).text("interface size").custom_formatter(|v, _| format!("{:.0}%", v * 100.0)))
                .on_hover_text("Applies when you let go");
            if r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus()) {
                s.ui_scale = scale;
                ui.data_mut(|d| d.remove::<f32>(pending));
            } else if r.changed() {
                ui.data_mut(|d| d.insert_temp(pending, scale));
            }
            ui.horizontal(|ui| {
                ui.label("Layout").on_hover_text("Where the viewer and the properties go. Each project can switch with the viewer's Vertical button, and remembers its choice.");
                use crate::settings::Layout;
                for (l, name, tip) in [
                    (Layout::Standard, "Standard", "The viewer in the middle, the properties on the right"),
                    (Layout::Vertical, "Vertical", "For vertical video: the viewer in the tall column on the right, the properties in the middle"),
                    (Layout::Auto, "Auto", "Vertical while the format being edited is taller than it is wide"),
                ] {
                    ui.selectable_value(&mut s.layout, l, name).on_hover_text(tip);
                }
            });
            ui.checkbox(&mut s.compact_properties, "Compact properties panel")
                .on_hover_text("Tighter rows and smaller controls in the properties panel, so more fits without scrolling; section notes show on hover");
            ui.checkbox(&mut s.show_performance, "Performance panel").on_hover_text("Frame times, GPU memory and renderer details under the inspector");

            ui.separator();
            ui.heading("Saving");
            ui.checkbox(&mut s.save_as_you_go, "Save as you go").on_hover_text("Keeps the project file up to date while you edit. The crash-recovery autosave happens either way.");

            ui.separator();
            ui.heading("Performance");
            let mut mb = s.vram_budget() >> 20;
            if ui.add(egui::Slider::new(&mut mb, 256..=16384).logarithmic(true).text("GPU memory (MB)")).changed() {
                s.vram_budget_mb = Some(mb);
            }

            ui.separator();
            ui.heading("Graphics");
            ui.label(egui::RichText::new(format!("Using {}", self.gpu.describe())).small());
            let video = if self.decoders.hardware() { "Media Foundation (hardware), ffmpeg for files it can't take" } else { "ffmpeg" };
            ui.label(egui::RichText::new(format!("Video decoding: {video}")).small().weak());
            let s = &mut self.settings;
            let apis: &[(&str, &str)] = if cfg!(windows) {
                &[("auto", "Automatic"), ("dx12", "DirectX 12"), ("vulkan", "Vulkan"), ("gl", "OpenGL")]
            } else if cfg!(target_os = "macos") {
                &[("auto", "Automatic"), ("metal", "Metal")]
            } else {
                &[("auto", "Automatic"), ("vulkan", "Vulkan"), ("gl", "OpenGL")]
            };
            ui.horizontal(|ui| {
                ui.label("Graphics API");
                let shown = apis.iter().find(|a| a.0 == s.gpu_backend).map_or("Automatic", |a| a.1);
                egui::ComboBox::from_id_salt("gpu-api").selected_text(shown).show_ui(ui, |ui| {
                    for (id, name) in apis {
                        ui.selectable_value(&mut s.gpu_backend, id.to_string(), *name);
                    }
                })
                .response
                .on_hover_text("Automatic picks the best one this computer has. Try another if the picture is wrong or the app won't start.");
            });
            ui.horizontal(|ui| {
                ui.label("Graphics card");
                let shown = if s.gpu_adapter.is_empty() { "The fastest".to_string() } else { s.gpu_adapter.clone() };
                egui::ComboBox::from_id_salt("gpu-card").selected_text(shown).width(240.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.gpu_adapter, String::new(), "The fastest");
                    let mut seen = std::collections::BTreeSet::new();
                    for (name, _) in &self.gpu_names {
                        if seen.insert(name.clone()) {
                            ui.selectable_value(&mut s.gpu_adapter, name.clone(), name);
                        }
                    }
                })
                .response
                .on_hover_text("On laptops with two GPUs, the dedicated one is used unless you pick another here");
            });
            ui.checkbox(&mut s.decode_with_ffmpeg, "Decode video with ffmpeg")
                .on_hover_text("Instead of the system's hardware decoder. For drivers that show glitches, green frames or wrong colors.");
            let running = oa_gpu::GpuPreference::new(&s.gpu_backend, &s.gpu_adapter);
            let pending = running.backend.is_some_and(|b| b != self.gpu.info.backend)
                || running.adapter.as_ref().is_some_and(|a| !self.gpu.info.name.to_lowercase().contains(&a.to_lowercase()))
                || (cfg!(windows) && s.decode_with_ffmpeg != self.decoders.force_ffmpeg);
            if pending {
                ui.label(egui::RichText::new("Restart OpenAtelier to use these.").small().color(crate::style::WARNING));
            }

            ui.separator();
            ui.heading("Updates");
            self.update_settings(ui);

            // After the last use of `s`: it runs setup jobs of its own.
            ui.separator();
            ui.heading("AI tracker");
            self.tracker_settings(ui);
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Settings are kept on this computer and apply to every project.").small().weak());
        });
        if serde_json::to_string(&self.settings).unwrap_or_default() != before {
            self.settings.save();
            self.apply_settings(ctx);
        }
        if !open {
            self.settings_open = false;
        }
    }
}

/// A small drawing of an easing curve.
fn curve_preview(ui: &mut egui::Ui, interp: oa_params::Interp) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 60.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let r = rect.shrink(6.0);
    let pts: Vec<egui::Pos2> = (0..=40)
        .map(|i| {
            let u = i as f64 / 40.0;
            let y = if interp == oa_params::Interp::Hold { 0.0 } else { interp.ease(u) };
            egui::pos2(r.left() + u as f32 * r.width(), r.bottom() - y as f32 * r.height())
        })
        .collect();
    p.add(egui::Shape::line(pts, egui::Stroke::new(2.0, crate::style::ACCENT)));
}
