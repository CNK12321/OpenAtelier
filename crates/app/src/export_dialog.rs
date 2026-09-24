//! Exporting: the Export window (what to write and how) and the Exporting window (a
//! live view of the frame being written, with progress).
//!
//! The Export window has the file type as cards (MP4 H.264, MP4 HEVC, MOV ProRes, GIF),
//! then which format (or every format, one file each), resolution, which part of the
//! timeline (all of it, the selected clips, or from–to), quality, encoder and sound —
//! with a picture and a summary of what it'll make beside them. Exports run one after
//! another from a queue; the choices are remembered (`ExportPrefs`).

use crate::style;
use crate::App;
use eframe::egui;
use oa_doc::{SeqId, VariantId};
use oa_export::{Encoder, ExportOptions, VideoCodec};
use oa_time::{Time, TimeRange};
use std::path::PathBuf;

/// One file to write.
pub struct ExportJob {
    pub seq: SeqId,
    pub path: PathBuf,
    pub options: ExportOptions,
}

/// The export in progress, as the Exporting window shows it.
#[derive(Default)]
pub struct ExportView {
    /// The latest exported frame, drawn for egui (texture, its id, size).
    pub picture: Option<(wgpu::Texture, egui::TextureId, [u32; 2])>,
    /// The timeline time of that frame.
    pub at: Time,
    /// When the running export began (for speed and time left).
    pub began: Option<std::time::Instant>,
    /// The window is hidden (the top bar still shows progress).
    pub hidden: bool,
}

/// File types: (key, name, detail, extension, what it's for).
const TYPES: [(&str, &str, &str, &str, &str); 6] = [
    ("h264", "MP4", "H.264", "mp4", "Plays everywhere: the web, phones, social apps"),
    ("hevc", "MP4", "HEVC", "mp4", "About half the size at the same quality; newer devices"),
    ("prores", "MOV", "ProRes 422 HQ", "mov", "Large, near-lossless: for handing to another editor"),
    ("prores4444", "MOV", "ProRes 4444 · alpha", "mov", "Keeps transparency: overlays and titles for other editors"),
    ("webm", "WebM", "VP9 · alpha", "webm", "Keeps transparency, small: overlays for the web"),
    ("gif", "GIF", "Animated", "gif", "Loops anywhere, no sound — keep it short and small"),
];

fn codec_of(key: &str, gif_fps: u32) -> VideoCodec {
    match key {
        "hevc" => VideoCodec::Hevc,
        "prores" => VideoCodec::ProRes,
        "prores4444" => VideoCodec::ProRes4444,
        "webm" => VideoCodec::WebmAlpha,
        "gif" => VideoCodec::Gif { fps: gif_fps },
        _ => VideoCodec::H264,
    }
}

/// Resolutions offered, as short edges (0 = the format's own size).
const RESOLUTIONS: [(u32, &str); 6] = [(0, "Full"), (480, "480p"), (720, "720p"), (1080, "1080p"), (1440, "1440p"), (2160, "4K")];

/// "1:05.20" for a time.
fn clock(t: Time) -> String {
    let s = t.as_seconds_f64().max(0.0);
    format!("{}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
}

/// A heading for one part of the window.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(style::GAP);
    ui.label(egui::RichText::new(text).size(style::TEXT_S).strong().color(ui.visuals().weak_text_color()));
    ui.add_space(2.0);
}

/// A card for a file type; returns whether it was clicked.
fn type_card(ui: &mut egui::Ui, name: &str, detail: &str, hint: &str, on: bool) -> bool {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(104.0, 62.0), egui::Sense::click());
    let v = ui.visuals();
    let fill = if on { style::ACCENT.gamma_multiply(0.22) } else if r.hovered() { v.widgets.hovered.bg_fill } else { v.widgets.inactive.bg_fill };
    let stroke = if on { egui::Stroke::new(2.0, style::ACCENT) } else { egui::Stroke::new(1.0, v.widgets.inactive.bg_stroke.color) };
    ui.painter().rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
    ui.painter().text(rect.center_top() + egui::vec2(0.0, 12.0), egui::Align2::CENTER_TOP, name, egui::FontId::proportional(20.0), v.strong_text_color());
    ui.painter().text(rect.center_bottom() - egui::vec2(0.0, 9.0), egui::Align2::CENTER_BOTTOM, detail, egui::FontId::proportional(11.0), v.text_color());
    r.on_hover_text(hint).clicked()
}

/// A chip: a small selectable pill.
fn chip(ui: &mut egui::Ui, on: bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::selectable(on, text).corner_radius(10.0).min_size(egui::vec2(0.0, 22.0)))
}

/// The encoded size for a format at a short edge (0 = its own), even on both sides.
fn out_size(size: oa_doc::CanvasSize, short_edge: u32) -> [u32; 2] {
    let k = if short_edge == 0 { 1.0 } else { short_edge as f64 / size.short_edge().max(1) as f64 };
    let even = |x: u32| ((((x as f64 * k).round() as u32).max(2)) / 2) * 2;
    [even(size.width), even(size.height)]
}

/// A format name usable in a file name.
fn safe_name(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '.' { c } else { '-' }).collect::<String>().trim().to_string()
}

/// Rough file size in bytes.
fn estimate(prefs: &crate::settings::ExportPrefs, [w, h]: [u32; 2], rate: f64, seconds: f64) -> f64 {
    let px = w as f64 * h as f64 * seconds;
    let sound = if prefs.audio && prefs.codec != "gif" { 24_000.0 * seconds } else { 0.0 };
    match prefs.codec.as_str() {
        // ProRes 422 HQ runs about 10 bits per pixel.
        "prores" => 10.0 * px * rate / 8.0 + sound,
        "prores4444" => 15.0 * px * rate / 8.0 + sound,
        // Palette GIFs: very roughly a byte per pixel per frame, less with little motion.
        "gif" => 0.35 * px * prefs.gif_fps.clamp(1, 50) as f64,
        codec => {
            let bpp = 0.12 * 2f64.powf((18.0 - prefs.crf as f64) / 6.0);
            let bpp = if codec == "hevc" || codec == "webm" { bpp * 0.6 } else { bpp };
            bpp * px * rate / 8.0 + sound
        }
    }
}

impl App {
    pub(crate) fn export_window(&mut self, ctx: &egui::Context) {
        self.exporting_window(ctx);
        if !self.export_dialog {
            return;
        }
        let (seq, current) = self.main_timeline();
        let Some(sequence) = self.editor.doc.project().sequence(seq).cloned() else {
            self.export_dialog = false;
            return;
        };
        let whole = sequence.duration();
        let key = egui::Id::new("export-which");
        // Which format: an index, or `usize::MAX` for every format.
        let mut which: usize = ctx.data(|d| d.get_temp(key)).unwrap_or_else(|| sequence.variants.iter().position(|v| v.id == current).unwrap_or(0));
        let mut prefs = self.settings.export.clone();
        // Which part: None = all of it.
        let mut range = self.export_range.map(|(a, b)| (a.max(Time::ZERO).min(whole), b.min(whole)));
        let selection = {
            let items: Vec<_> = self.selected.iter().filter_map(|id| self.editor.item(*id)).map(|i| i.range).collect();
            let lo = items.iter().map(|r| r.start).min();
            let hi = items.iter().map(|r| r.end()).max();
            lo.zip(hi).filter(|_| self.compound_trail.is_empty())
        };
        // Is anything behind the clips see-through? (A solid or gradient background with
        // alpha below 1 — blurred and texture backgrounds are opaque.)
        let transparent = {
            let bg = sequence.params.eval(oa_doc::schema::background(), None, &oa_params::EvalContext::at(Time::ZERO, Time::ZERO));
            let solid = !matches!(bg.get(oa_doc::schema::BG_MODE), Some(oa_params::Value::Enum(m)) if m != "solid");
            solid && bg.get(oa_doc::schema::BG_COLOR).and_then(|v| v.as_gradient()).is_some_and(|g| g.stops.iter().any(|s| s.color[3] < 1.0))
        };
        let mut go = false;
        let mut open = true;
        let gif = prefs.codec == "gif";
        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(700.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // Left: the choices.
                    ui.vertical(|ui| {
                        ui.set_width(450.0);
                        heading(ui, "FILE TYPE");
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                            for (k, name, detail, _, hint) in TYPES {
                                if type_card(ui, name, detail, hint, prefs.codec == k) {
                                    prefs.codec = k.to_string();
                                    if k == "gif" && prefs.short_edge == 0 {
                                        prefs.short_edge = 480;
                                    }
                                }
                            }
                        });
                        // A see-through background only survives in formats with alpha.
                        let keeps_alpha = codec_of(&prefs.codec, prefs.gif_fps).has_alpha();
                        if transparent && !keeps_alpha {
                            egui::Frame::new().fill(style::WARNING.gamma_multiply(0.15)).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.colored_label(style::WARNING, "The background is transparent, but this type can't keep it — it'll be black.");
                                    if ui.small_button("Use ProRes 4444").clicked() {
                                        prefs.codec = "prores4444".into();
                                    }
                                    if ui.small_button("Use WebM").clicked() {
                                        prefs.codec = "webm".into();
                                    }
                                });
                            });
                        } else if transparent {
                            ui.label(egui::RichText::new("✓ Transparent background kept").small().color(egui::Color32::from_rgb(90, 200, 120)));
                        }

                        heading(ui, "FORMAT");
                        ui.horizontal_wrapped(|ui| {
                            for (i, v) in sequence.variants.iter().enumerate() {
                                chip(ui, which == i, &v.name).on_hover_text(format!("{}×{}", v.size.width, v.size.height)).clicked().then(|| which = i);
                            }
                            if sequence.variants.len() > 1 {
                                chip(ui, which == usize::MAX, format!("Every format ({})", sequence.variants.len())).on_hover_text("One file each, into a folder").clicked().then(|| which = usize::MAX);
                            }
                        });

                        heading(ui, "RESOLUTION");
                        ui.horizontal_wrapped(|ui| {
                            for (edge, label) in RESOLUTIONS {
                                chip(ui, prefs.short_edge == edge, label).clicked().then(|| prefs.short_edge = edge);
                            }
                        });

                        heading(ui, "PART");
                        ui.horizontal_wrapped(|ui| {
                            if chip(ui, range.is_none(), format!("Whole timeline · {}", clock(whole))).clicked() {
                                range = None;
                            }
                            if let Some((a, b)) = selection
                                && chip(ui, range == Some((a, b)), format!("Selected clips · {}", clock(b - a))).clicked()
                            {
                                range = Some((a, b));
                            }
                            if chip(ui, range.is_some() && range != selection, "From – to").clicked() && (range.is_none() || range == selection) {
                                let at = self.playhead.min(whole);
                                range = Some((at, (at + Time::from_seconds(5)).min(whole).max(at + Time::from_seconds_f64(0.1))));
                            }
                        });
                        let custom = range.is_some() && range != selection;
                        if let Some((a, b)) = range.as_mut().filter(|_| custom) {
                            ui.horizontal(|ui| {
                                let mut fa = a.as_seconds_f64();
                                let mut fb = b.as_seconds_f64();
                                let limit = whole.as_seconds_f64();
                                ui.label("from");
                                ui.add(egui::DragValue::new(&mut fa).range(0.0..=limit).speed(0.05).custom_formatter(|v, _| clock(Time::from_seconds_f64(v))));
                                if ui.small_button("⏵ here").on_hover_text("Start at the playhead").clicked() {
                                    fa = self.playhead.as_seconds_f64();
                                }
                                ui.label("to");
                                ui.add(egui::DragValue::new(&mut fb).range(0.0..=limit).speed(0.05).custom_formatter(|v, _| clock(Time::from_seconds_f64(v))));
                                if ui.small_button("here ⏴").on_hover_text("End at the playhead").clicked() {
                                    fb = self.playhead.as_seconds_f64();
                                }
                                let (lo, hi) = (fa.min(fb).clamp(0.0, limit), fa.max(fb).clamp(0.0, limit));
                                *a = Time::from_seconds_f64(lo);
                                *b = Time::from_seconds_f64(hi.max(lo + 0.04).min(limit.max(lo + 0.04)));
                            });
                        }

                        if gif {
                            heading(ui, "FRAME RATE");
                            ui.horizontal(|ui| {
                                for fps in [10, 12, 15, 20, 24, 30] {
                                    chip(ui, prefs.gif_fps == fps, format!("{fps} fps")).clicked().then(|| prefs.gif_fps = fps);
                                }
                            });
                        } else if !prefs.codec.starts_with("prores") {
                            heading(ui, "QUALITY");
                            ui.horizontal(|ui| {
                                // Shown as better → smaller; stored on x264's scale (lower is better).
                                let mut q = 40 - prefs.crf as i32;
                                ui.add(egui::Slider::new(&mut q, 8..=28).show_value(false));
                                prefs.crf = (40 - q).clamp(12, 32) as u32;
                                ui.label(match prefs.crf {
                                    0..=15 => "Very high",
                                    16..=19 => "High",
                                    20..=24 => "Standard",
                                    _ => "Small file",
                                });
                            });
                        }

                        if !gif {
                            heading(ui, "ENCODER & SOUND");
                            ui.horizontal(|ui| {
                                // Only H.264 and HEVC have a hardware encoder.
                                let prores = !matches!(prefs.codec.as_str(), "h264" | "hevc");
                                ui.add_enabled_ui(!prores, |ui| {
                                    chip(ui, prefs.encoder == "auto" && !prores, "Automatic").on_hover_text("The graphics card's encoder when it can, else software").clicked().then(|| prefs.encoder = "auto".into());
                                    chip(ui, prefs.encoder == "hardware" && !prores, "Hardware").on_hover_text("Fastest (the GPU's own encoder)").clicked().then(|| prefs.encoder = "hardware".into());
                                });
                                chip(ui, prefs.encoder == "software" || prores, "Software").on_hover_text("x264/x265: slower, slightly better per byte").clicked().then(|| prefs.encoder = "software".into());
                                ui.separator();
                                ui.checkbox(&mut prefs.audio, "Sound");
                            });
                        }
                    });

                    ui.separator();

                    // Right: a picture and what it'll make.
                    ui.vertical(|ui| {
                        ui.set_width(220.0);
                        heading(ui, "PREVIEW");
                        let v = &sequence.variants[if which == usize::MAX { 0 } else { which.min(sequence.variants.len() - 1) }];
                        let aspect = v.size.width as f32 / v.size.height.max(1) as f32;
                        let box_size = if aspect >= 1.0 { egui::vec2(220.0, 220.0 / aspect) } else { egui::vec2(140.0 * aspect, 140.0) };
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(220.0, box_size.y.max(90.0)), egui::Sense::hover());
                        let pic = egui::Rect::from_center_size(rect.center(), box_size);
                        ui.painter().rect_filled(pic, 6.0, egui::Color32::BLACK);
                        if let Some(p) = &self.preview {
                            ui.painter().image(p.id, pic, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        ui.painter().rect_stroke(pic, 6.0, egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color), egui::StrokeKind::Outside);

                        heading(ui, "YOU'LL GET");
                        let (a, b) = range.unwrap_or((Time::ZERO, whole));
                        let seconds = (b - a).as_seconds_f64();
                        let rate = if gif { prefs.gif_fps as f64 } else { sequence.rate.as_f64() };
                        let targets: Vec<usize> = if which == usize::MAX { (0..sequence.variants.len()).collect() } else { vec![which.min(sequence.variants.len() - 1)] };
                        let (_, name, detail, _, _) = TYPES.iter().copied().find(|t| t.0 == prefs.codec).unwrap_or(TYPES[0]);
                        ui.label(egui::RichText::new(format!("{name} · {detail}")).strong());
                        let mut total = 0.0;
                        for &i in &targets {
                            let v = &sequence.variants[i];
                            let size = out_size(v.size, prefs.short_edge);
                            total += estimate(&prefs, size, sequence.rate.as_f64(), seconds);
                            ui.label(egui::RichText::new(format!("{} — {}×{}", v.name, size[0], size[1])).small());
                        }
                        ui.label(egui::RichText::new(format!("{} · {:.3} fps{}", clock(b - a), rate, if range.is_some() { format!(" · from {}", clock(a)) } else { String::new() })).small());
                        ui.label(egui::RichText::new(format!("about {}{}", crate::bytes(total as u64), if targets.len() > 1 { format!(" in {} files", targets.len()) } else { String::new() })).small().weak());
                        if gif && seconds > 20.0 {
                            ui.colored_label(style::WARNING, "Long GIFs get very large — a short part is best.");
                        }

                        ui.add_space(style::GAP_L);
                        let busy = self.export.is_some();
                        let label = if busy { "Add to the queue…" } else { "Export…" };
                        let button = egui::Button::new(egui::RichText::new(label).strong().size(style::TEXT_L).color(egui::Color32::WHITE))
                            .fill(style::ACCENT)
                            .corner_radius(8.0)
                            .min_size(egui::vec2(220.0, 36.0));
                        if ui.add(button).on_hover_text("Choose where it goes").clicked() {
                            go = true;
                        }
                        if ui.add(egui::Button::new("Cancel").frame(false).min_size(egui::vec2(220.0, 24.0))).clicked() {
                            self.export_dialog = false;
                        }
                    });
                });
            });
        ctx.data_mut(|d| d.insert_temp(key, which));
        self.export_range = range;
        if prefs != self.settings.export {
            self.settings.export = prefs.clone();
            self.settings.save();
        }
        if !open {
            self.export_dialog = false;
        }
        if go {
            self.queue_exports(seq, &sequence, which, &prefs, range);
        }
    }

    /// Asks where the file(s) go, then queues them.
    fn queue_exports(&mut self, seq: SeqId, sequence: &oa_doc::Sequence, which: usize, prefs: &crate::settings::ExportPrefs, range: Option<(Time, Time)>) {
        let (_, _, _, ext, _) = TYPES.iter().copied().find(|t| t.0 == prefs.codec).unwrap_or(TYPES[0]);
        let codec = codec_of(&prefs.codec, prefs.gif_fps);
        let stem = self
            .editor
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().trim_end_matches(".json").trim_end_matches(".oaproj").to_string())
            .unwrap_or_else(|| "export".into());
        let targets: Vec<(VariantId, String)> = if which == usize::MAX {
            sequence.variants.iter().map(|v| (v.id, v.name.clone())).collect()
        } else {
            let v = &sequence.variants[which.min(sequence.variants.len() - 1)];
            vec![(v.id, v.name.clone())]
        };
        let paths: Vec<PathBuf> = if targets.len() == 1 {
            let filter = match ext {
                "mov" => "QuickTime movie",
                "gif" => "Animated GIF",
                "webm" => "WebM video",
                _ => "MP4 video",
            };
            let Some(p) = rfd::FileDialog::new().add_filter(filter, &[ext]).set_file_name(format!("{stem}.{ext}")).save_file() else { return };
            vec![p.with_extension(ext)]
        } else {
            let Some(dir) = rfd::FileDialog::new().set_title("Folder for the exports").pick_folder() else { return };
            targets.iter().map(|(_, name)| dir.join(format!("{stem} - {}.{ext}", safe_name(name)))).collect()
        };
        let encoder = match (prefs.encoder.as_str(), codec) {
            (_, c) if !matches!(c, VideoCodec::H264 | VideoCodec::Hevc) => Encoder::Ffmpeg,
            ("software", _) => Encoder::Ffmpeg,
            ("hardware", _) => Encoder::MediaFoundation,
            _ => Encoder::Auto,
        };
        let (audio, buses) = if prefs.audio && !matches!(codec, VideoCodec::Gif { .. }) { self.audio_mix_of(seq) } else { Default::default() };
        for ((variant, _), path) in targets.into_iter().zip(paths) {
            let size = sequence.variant(variant).map(|v| v.size).unwrap_or(sequence.canvas());
            let scale = if prefs.short_edge == 0 { 1.0 } else { prefs.short_edge as f64 / size.short_edge().max(1) as f64 };
            let options = ExportOptions {
                variant: Some(variant),
                scale,
                codec,
                crf: prefs.crf,
                reference: self.reference,
                audio: audio.clone(),
                buses: buses.clone(),
                encoder,
                range: range.map(|(a, b)| TimeRange::new(a, b - a)),
            };
            self.export_queue.push_back(ExportJob { seq, path, options });
        }
        self.set_playing(false);
        self.export_dialog = false;
        self.export_view.hidden = false;
        self.next_export();
    }

    /// After each slice of an export: draw its latest frame for the Exporting window.
    pub(crate) fn update_export_view(&mut self, render_state: Option<&eframe::egui_wgpu::RenderState>) {
        let Some(render_state) = render_state else { return };
        // The export thread hands over a display-ready picture a few times a second.
        let Some(preview) = self.export.as_ref().and_then(|e| e.take_preview()) else { return };
        self.export_view.at = preview.at;
        let view = oa_gpu::readback::display_view(&preview.texture);
        let id = match self.export_view.picture.take() {
            Some((_, id, _)) => {
                render_state.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, id);
                id
            }
            None => render_state.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
        };
        self.export_view.picture = Some((preview.texture, id, preview.size));
    }

    /// The Exporting window: the frame being written, where it is, how far along, how
    /// fast and how long is left.
    fn exporting_window(&mut self, ctx: &egui::Context) {
        let Some(exporter) = self.export.as_ref() else { return };
        if self.export_view.hidden {
            return;
        }
        let (done, total) = exporter.progress();
        let encoder = exporter.encoder();
        let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        let elapsed = self.export_view.began.map_or(0.0, |b| b.elapsed().as_secs_f64());
        let fps = if elapsed > 0.2 { done as f64 / elapsed } else { 0.0 };
        let left = if fps > 0.0 { (total - done) as f64 / fps } else { 0.0 };
        let name = self.export_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let queued = self.export_queue.len();
        let (mut hide, mut cancel) = (false, false);
        egui::Window::new("Exporting")
            .id(egui::Id::new("exporting"))
            .collapsible(true)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&name).strong());
                let width = 360.0;
                let size = self.export_view.picture.as_ref().map_or([16, 9], |p| p.2);
                let h = (width * size[1] as f32 / size[0].max(1) as f32).min(240.0);
                let w = h * size[0] as f32 / size[1].max(1) as f32;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
                let pic = egui::Rect::from_center_size(rect.center(), egui::vec2(w, h));
                ui.painter().rect_filled(pic, 4.0, egui::Color32::BLACK);
                if let Some((_, id, _)) = &self.export_view.picture {
                    ui.painter().image(*id, pic, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                ui.add(egui::ProgressBar::new(fraction).desired_width(width).text(format!("{:.0}%", fraction * 100.0)));
                ui.horizontal(|ui| {
                    ui.label(format!("Frame {} of {}", done.min(total), total));
                    ui.label(egui::RichText::new(format!("at {}", clock(self.export_view.at))).weak());
                });
                ui.label(
                    egui::RichText::new(format!(
                        "{:.0} fps · {} left{}",
                        fps,
                        if fps > 0.0 { clock(Time::from_seconds_f64(left)) } else { "…".into() },
                        if queued > 0 { format!(" · {queued} more queued") } else { String::new() }
                    ))
                    .small()
                    .weak(),
                );
                if !encoder.is_empty() {
                    ui.label(egui::RichText::new(format!("with {encoder}")).small().weak());
                }
                ui.horizontal(|ui| {
                    if ui.button("Hide").on_hover_text("Keep exporting; progress stays in the top bar").clicked() {
                        hide = true;
                    }
                    if ui.button(if queued > 0 { "Cancel all" } else { "Cancel" }).clicked() {
                        cancel = true;
                    }
                });
            });
        if hide {
            self.export_view.hidden = true;
        }
        if cancel {
            self.cancel_exports();
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_names_and_clock() {
        assert_eq!(out_size(oa_doc::CanvasSize::new(1920, 1080), 0), [1920, 1080]);
        assert_eq!(out_size(oa_doc::CanvasSize::new(1920, 1080), 720), [1280, 720]);
        assert_eq!(out_size(oa_doc::CanvasSize::new(1080, 1920), 2160), [2160, 3840]);
        assert_eq!(safe_name("Vertical 9:16"), "Vertical 9-16");
        assert_eq!(clock(Time::from_seconds_f64(65.2)), "1:05.20");
        assert_eq!(codec_of("gif", 12), VideoCodec::Gif { fps: 12 });
    }
}
