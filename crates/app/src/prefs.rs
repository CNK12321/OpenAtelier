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
        self.viewer_render.set_budget(self.settings.vram_budget());
        if let Some(audio) = &self.audio {
            audio.set_output_delay(std::time::Duration::from_millis(self.settings.output_delay_ms as u64));
        }
    }

    /// The Settings window: a list of pages on the left, the page on the right. It has a
    /// fixed size that fits the screen (the page scrolls when it's taller), opens in the
    /// middle, and can't be dragged off the screen — a tall window used to push its own
    /// title bar out of reach.
    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let before = serde_json::to_string(&self.settings).unwrap_or_default();
        let screen = ctx.content_rect();
        let size = egui::vec2(620.0f32.min(screen.width() - 32.0), 440.0f32.min(screen.height() - 64.0)).max(egui::vec2(240.0, 160.0));
        let page_id = egui::Id::new("settings-page");
        let mut page: Page = ctx.data(|d| d.get_temp(page_id)).unwrap_or_default();
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size(size)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .constrain_to(screen)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(128.0);
                        for p in Page::ALL {
                            if ui.add_sized([124.0, 26.0], egui::Button::selectable(page == p, p.name())).clicked() {
                                page = p;
                            }
                        }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Kept on this computer; they apply to every project.").small().weak());
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.heading(page.name());
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().id_salt(("settings-scroll", page as u8)).auto_shrink([false, false]).show(ui, |ui| match page {
                            Page::Editing => self.settings_editing(ui),
                            Page::Interface => self.settings_interface(ui),
                            Page::Performance => self.settings_performance(ui),
                            Page::Graphics => self.settings_graphics(ui),
                            Page::Audio => self.settings_audio(ui),
                            Page::Updates => self.update_settings(ui),
                            Page::Tracker => self.tracker_settings(ui),
                        });
                    });
                });
            });
        ctx.data_mut(|d| d.insert_temp(page_id, page));
        if serde_json::to_string(&self.settings).unwrap_or_default() != before {
            self.settings.save();
            self.apply_settings(ctx);
        }
        if !open {
            self.settings_open = false;
        }
    }

    fn settings_editing(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        ui.checkbox(&mut s.advanced_transform, "Advanced transformations")
            .on_hover_text("Squash and the crop sliders in Transform. Cropping by double-clicking a clip in the viewer works either way.");
        ui.checkbox(&mut s.advanced_color, "Advanced color")
            .on_hover_text("Source color (log and HDR curves, gamut, levels) and the Output tone map. Off: files are read by their own tags.");
        ui.add_space(6.0);
        ui.strong("Masking");
        ui.checkbox(&mut s.masking, "Masks tab").on_hover_text(
            "A Masks tab in the inspector: draw masks on a clip (rectangle, ellipse, brush, magic select, fill, or a black-and-white picture), then give properties their own value inside them or run effects only there.",
        );
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
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Pictures and titles last");
            ui.add(egui::DragValue::new(&mut s.still_seconds).range(0.1..=3600.0).speed(0.1).suffix(" s"));
        });
        ui.checkbox(&mut s.save_as_you_go, "Save as you go").on_hover_text("Keeps the project file up to date while you edit. The crash-recovery autosave happens either way.");
    }

    fn settings_interface(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
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
    }

    fn settings_performance(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        let mut mb = s.vram_budget() >> 20;
        if ui.add(egui::Slider::new(&mut mb, 256..=16384).logarithmic(true).text("GPU memory (MB)")).changed() {
            s.vram_budget_mb = Some(mb);
        }
        ui.horizontal(|ui| {
            ui.label("Preview up to");
            let limits = [(720, "720p"), (1080, "1080p"), (1440, "1440p"), (2160, "4K"), (0, "Full size")];
            let shown = limits.iter().find(|l| l.0 == s.preview_limit).map_or("1080p", |l| l.1);
            egui::ComboBox::from_id_salt("preview-limit").selected_text(shown).show_ui(ui, |ui| {
                for (limit, name) in limits {
                    ui.selectable_value(&mut s.preview_limit, limit, name);
                }
            })
            .response
            .on_hover_text("Large formats and 4K files preview at this size at most, so playback stays smooth. Exports always use the full size.");
        });
    }

    fn settings_audio(&mut self, ui: &mut egui::Ui) {
        let device = self.audio.as_ref().map_or_else(|| "none open yet (it opens with the first sound)".to_string(), |a| a.device_name().to_string());
        ui.label(egui::RichText::new(format!("Playing through: {device}")).small());
        ui.add_space(4.0);
        let s = &mut self.settings;
        ui.add(egui::Slider::new(&mut s.output_delay_ms, 0..=500).suffix(" ms").text("output delay"))
            .on_hover_text("Bluetooth headphones and some TVs play sound later than they say: the picture waits this long so they line up. Wireless headphones: try 150–250 ms. Wired speakers: 0.");
        ui.label(egui::RichText::new("If lips move before you hear the words, raise it; if the sound comes first, lower it.").small().weak());
    }

    fn settings_graphics(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(format!("Using {}", self.gpu.describe())).small());
        let video = if self.decoders.hardware() { "Media Foundation (hardware), ffmpeg for files it can't take" } else { "ffmpeg" };
        ui.label(egui::RichText::new(format!("Video decoding: {video}")).small().weak());
        ui.add_space(4.0);
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
    }
}

/// The Settings window's pages.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Editing,
    Interface,
    Performance,
    Graphics,
    Audio,
    Updates,
    Tracker,
}

impl Page {
    const ALL: [Page; 7] = [Page::Editing, Page::Interface, Page::Performance, Page::Graphics, Page::Audio, Page::Updates, Page::Tracker];

    fn name(self) -> &'static str {
        match self {
            Page::Editing => "Editing",
            Page::Interface => "Interface",
            Page::Performance => "Performance",
            Page::Graphics => "Graphics",
            Page::Audio => "Audio",
            Page::Updates => "Updates",
            Page::Tracker => "AI tracker",
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
