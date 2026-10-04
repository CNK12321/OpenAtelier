//! The plugin window: click a plugin on the Plugins page and it opens over the page (the
//! rest dimmed) — on the left its name, version, author, what it is, its switch and its
//! scripts; on the right all its effects, in tabs by category, each with a preview.
//!
//! Previews are rendered on a sample picture (a sky at sunset, `assets/preview/sky.jpg`,
//! with a big "Aa" over it) since there's no clip to show them on there. Picture effects
//! apply to the whole scene (a compound clip with the effect on it), text effects to the
//! letters. Hover a preview to play it: intros run through, animated effects move, still
//! ones sweep from off to full. Transitions and sound effects list with an icon.
//! Previews render on the preview thread like the pickers' do.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{CanvasSize, EffectId, EffectInstance, EffectRole, FormatVariant, Item, ItemId, ItemKind, MediaId, MediaInfo, MediaRef, Project, SeqId, Sequence, Track, TrackId, TrackKind, VariantId};
use oa_graph::registry::{EffectDescriptor, EffectUsage};
use oa_graph::EffectKind;
use oa_params::{Gradient, GradientStop, ParamSource, Value};
use oa_time::{FrameRate, Time, TimeRange};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const OUTER: SeqId = SeqId(1);
const INNER: SeqId = SeqId(10);
const VARIANT: VariantId = VariantId(4);
const SCENE: ItemId = ItemId(3);
const TITLE: ItemId = ItemId(12);
const PHOTO: ItemId = ItemId(13);
/// The sample picture's media id: far from any a project hands out.
const SKY: u64 = u64::MAX - 42;
/// The sample picture, built in; written out once for the decoder to read.
static SKY_JPG: &[u8] = include_bytes!("../../../assets/preview/sky.jpg");
/// The sample scene's canvas: the picture's own shape (3:4).
const CANVAS: [u32; 2] = [360, 480];
/// How long the sample scene is; animated previews loop over it.
const LENGTH: Time = Time::from_seconds(4);
/// An intro/outro previewed over this long, then held a moment before it loops.
const IN_OUT: f64 = 1.0;
const HOLD: f64 = 0.6;
/// Preview cells, px wide (3:4).
const CELL: f32 = 132.0;

/// The preview thread's slot for the hovered cell's animation.
const LIVE_SLOT: u64 = 4;

#[derive(Default)]
pub struct PluginPreviews {
    /// The plugin whose window is open, and the tab it's on ("" = all its effects).
    pub open: Option<String>,
    tab: String,
    images: HashMap<String, (egui::TextureId, wgpu::Texture)>,
    requested: HashSet<String>,
    /// The registry the previews were rendered with (plugins changed: render again).
    registry: usize,
    hover: Option<(String, f64, bool)>,
    live: Option<(egui::TextureId, wgpu::Texture, u64)>,
    /// The sample picture, once written out and handed to the preview thread (its size),
    /// or `None` if that failed (previews then show a plain backdrop).
    sky: Option<Option<[u32; 2]>>,
}

/// What kind of effect, for its label.
fn kind_label(d: &EffectDescriptor) -> &'static str {
    match (&d.kind, d.usage) {
        (EffectKind::Sound, _) => "Sound",
        (EffectKind::Transition, _) | (_, EffectUsage::Cut) => "Transition",
        (_, EffectUsage::InOut) => tr("Intro / outro"),
        (EffectKind::Motion, _) => "Motion",
        (k, _) if k.text_only() => "Text",
        _ => "Picture",
    }
}

/// Whether the sample scene can show it.
fn previewable(d: &EffectDescriptor) -> bool {
    !matches!(d.kind, EffectKind::Sound | EffectKind::Transition) && d.usage != EffectUsage::Cut
}

/// Its tab: its category, or what kind it is.
fn category(d: &EffectDescriptor) -> String {
    d.category.clone().unwrap_or_else(|| kind_label(d).to_string())
}

fn slot(type_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("plugin-preview", type_id).hash(&mut h);
    h.finish() | 1 << 62
}

/// The sample scene with `d` on it, and the moment to show: `phase` is `None` for the
/// still preview, else seconds into the hover animation. `sky`: the sample picture's
/// size, when it's available (else a plain backdrop stands in).
pub fn sample_scene(d: &EffectDescriptor, phase: Option<f64>, sky: Option<[u32; 2]>) -> (Project, Time) {
    let mut p = Project::new("sample");
    let variant = FormatVariant { id: VARIANT, name: "3:4".into(), size: CanvasSize::new(CANVAS[0], CANVAS[1]), overrides: Default::default() };
    // Inside: the picture (or a warm-to-cool backdrop) and a big two-tone "Aa".
    let mut inner = Sequence::new(INNER, "Sample", FrameRate::FPS_30, variant.clone());
    let stop = |pos: f64, color: [f64; 4]| GradientStop { pos, color };
    inner.params.set(
        oa_doc::schema::BG_COLOR,
        ParamSource::Static(Value::Gradient(Gradient { angle: 60.0, stops: vec![stop(0.0, [0.08, 0.2, 0.5, 1.0]), stop(0.55, [0.85, 0.3, 0.35, 1.0]), stop(1.0, [0.98, 0.8, 0.3, 1.0])] })),
    );
    if let Some([width, height]) = sky {
        let info = MediaInfo { width, height, duration: Time::ZERO, rate: None, has_video: true, has_audio: false, still: true, ..Default::default() };
        p.media.insert(
            MediaId(SKY),
            Arc::new(MediaRef { id: MediaId(SKY), path: String::new(), fingerprint: Some("oa-preview-sky-1".into()), info: Some(info), scaling: Default::default(), folder: String::new(), color: Default::default() }),
        );
        let mut v0 = Track::new(TrackId(14), "Picture", TrackKind::Video);
        v0.items.push(Item::new(PHOTO, "sky", ItemKind::Media { media: MediaId(SKY) }, TimeRange::new(Time::ZERO, LENGTH)));
        inner.tracks.push(Arc::new(v0));
    }
    let mut title = Item::new(TITLE, "Aa", ItemKind::Text, TimeRange::new(Time::ZERO, LENGTH));
    title.params.set(oa_doc::schema::TEXT_CONTENT, ParamSource::Static(Value::Text("Aa".into())));
    title.params.set(oa_doc::schema::TEXT_SIZE, ParamSource::Static(Value::Float(130.0)));
    title.params.set(oa_doc::schema::TEXT_BOLD, ParamSource::Static(Value::Bool(true)));
    title.params.set(
        oa_doc::schema::TEXT_COLOR,
        ParamSource::Static(Value::Gradient(Gradient { angle: 90.0, stops: vec![stop(0.0, [1.0, 1.0, 1.0, 1.0]), stop(1.0, [1.0, 0.85, 0.55, 1.0])] })),
    );
    let mut fx = EffectInstance::new(EffectId(99), &d.type_id);
    for (param, value) in &d.preview {
        fx.params.set(param.as_str(), ParamSource::Static(value.clone()));
    }
    // When: an intro plays through its second; moving effects run over the scene; a
    // still one sweeps its strength from off to full and back.
    let t = match (d.usage, phase) {
        (EffectUsage::InOut, phase) => {
            fx.role = EffectRole::In { duration: Time::from_seconds_f64(IN_OUT) };
            let at = phase.map_or(IN_OUT * 0.5, |p| (p % (IN_OUT + HOLD)).min(IN_OUT - 0.01));
            Time::from_seconds_f64(at)
        }
        (_, None) => Time::from_seconds_f64(1.5),
        (_, Some(p)) if d.time_varying || d.kind == EffectKind::Motion => Time::from_seconds_f64(p % LENGTH.as_seconds_f64()),
        (_, Some(p)) => {
            let k = 0.5 - 0.5 * (p * std::f64::consts::PI).cos();
            for s in &d.params {
                let (Value::Float(default), Some((lo, _))) = (&s.default, s.range) else { continue };
                let preset = d.preview.iter().find(|(id, _)| *id == s.id).and_then(|(_, v)| v.as_float());
                let (from, to) = match preset {
                    Some(to) => (*default, to),
                    None => (lo.max(0.0).min(*default), *default),
                };
                fx.params.set(s.id.as_str(), ParamSource::Static(Value::Float(from + (to - from) * k)));
            }
            Time::from_seconds_f64(1.5)
        }
    };
    let text_effect = d.kind.text_only();
    if text_effect {
        title.effects.push(fx.clone());
    }
    let mut v1 = Track::new(TrackId(11), "Title", TrackKind::Video);
    v1.items.push(title);
    inner.tracks.push(Arc::new(v1));
    p.sequences.insert(INNER, Arc::new(inner));
    // Outside: the scene as one clip, carrying a picture effect.
    let mut outer = Sequence::new(OUTER, "Preview", FrameRate::FPS_30, variant);
    let mut scene = Item::new(SCENE, "scene", ItemKind::Nested { sequence: INNER }, TimeRange::new(Time::ZERO, LENGTH));
    if !text_effect {
        scene.effects.push(fx);
    }
    let mut v = Track::new(TrackId(2), "V1", TrackKind::Video);
    v.items.push(scene);
    outer.tracks.push(Arc::new(v));
    p.sequences.insert(OUTER, Arc::new(outer));
    (p, t)
}

impl App {
    /// The sample picture's size, getting it ready the first time: written beside the
    /// settings (once), probed, and handed to the preview thread to keep.
    fn preview_sky(&mut self) -> Option<[u32; 2]> {
        if let Some(known) = self.plugin_previews.sky {
            return known;
        }
        let ready = (|| {
            let path = crate::settings::config_dir().join("cache").join("preview-sky.jpg");
            if std::fs::metadata(&path).map_or(true, |m| m.len() != SKY_JPG.len() as u64) {
                std::fs::create_dir_all(path.parent()?).ok()?;
                crate::autosave::write_atomic(&path, SKY_JPG).ok()?;
            }
            let track = oa_media::probe(&path).ok()?.video?;
            let size = [track.width, track.height];
            self.preview_worker.keep(crate::export_worker::MediaEntry { id: SKY, kind: oa_media::MediaKind::Still, path, track: Some(track) });
            Some(size)
        })();
        self.plugin_previews.sky = Some(ready);
        ready
    }

    /// The plugin window, over the Plugins page, while one is open.
    pub(crate) fn plugin_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.plugin_previews.open.clone() else { return };
        let Some(plugin) = self.plugins.list.iter().find(|p| p.id == id).cloned() else {
            self.plugin_previews.open = None;
            return;
        };
        let enabled = self.plugins.is_enabled(&plugin.id);
        let scripts = (!plugin.builtin && plugin.has_scripts()).then(|| crate::plugins::Plugins::scripts_allowed(&plugin, &self.settings.trusted_scripts));
        let effects: Vec<EffectDescriptor> = plugin.effects.iter().filter(|d| !oa_graph::registry::is_internal_effect(&d.type_id)).cloned().collect();
        // The tabs: every category, in the plugin's order.
        let mut tabs: Vec<String> = Vec::new();
        for d in &effects {
            let c = category(d);
            if !tabs.contains(&c) {
                tabs.push(c);
            }
        }
        if !self.plugin_previews.tab.is_empty() && !tabs.contains(&self.plugin_previews.tab) {
            self.plugin_previews.tab.clear();
        }
        let screen = ctx.content_rect();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("plugin-window")).show(ctx, |ui| {
            let (w, h) = ((screen.width() * 0.88).min(1320.0), (screen.height() * 0.84).min(920.0));
            ui.set_width(w);
            ui.set_height(h);
            ui.horizontal_top(|ui| {
                // Left: what it is.
                ui.vertical(|ui| {
                    ui.set_width(290.0);
                    ui.set_min_height(h);
                    ui.horizontal(|ui| {
                        let color = if enabled { crate::style::ACCENT } else { ui.visuals().weak_text_color() };
                        crate::widgets::icon_badge(ui, crate::icons::PLUGINS, color, 44.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&plugin.name).size(crate::style::TITLE).strong());
                            ui.horizontal_wrapped(|ui| {
                                crate::widgets::pill(ui, &format!("v{}", plugin.version), ui.visuals().weak_text_color());
                                if plugin.builtin {
                                    crate::widgets::pill(ui, crate::i18n::t("plugins.builtin"), crate::style::ACCENT);
                                }
                                if !enabled {
                                    crate::widgets::pill(ui, crate::i18n::t("plugins.off"), crate::style::WARNING);
                                }
                            });
                        });
                    });
                    if !plugin.author.is_empty() {
                        ui.label(egui::RichText::new(crate::i18n::args("plugins.by", &[("author", &plugin.author)])).weak());
                    }
                    ui.add_space(crate::style::GAP);
                    if !plugin.description.is_empty() {
                        ui.label(&plugin.description);
                        ui.add_space(crate::style::GAP_S);
                    }
                    ui.label(egui::RichText::new(plugin.summary()).small().weak());
                    let source = match &plugin.path {
                        Some(p) => p.parent().map(|p| p.display().to_string()).unwrap_or_default(),
                        None => crate::i18n::t("plugins.built_in_source").to_string(),
                    };
                    ui.label(egui::RichText::new(source).small().weak().monospace());
                    ui.add_space(crate::style::GAP);
                    ui.horizontal(|ui| {
                        let mut on = enabled;
                        if crate::widgets::toggle(ui, &mut on).changed() {
                            self.set_plugin_enabled(&plugin.id, on);
                        }
                        ui.label(if enabled { "On" } else { tr("Off — its effects aren't offered, and clips using them skip them") });
                    });
                    match scripts {
                        Some(false) => {
                            ui.add_space(crate::style::GAP_S);
                            ui.label(egui::RichText::new(tr("⚠ Carries scripts — none run until you allow them")).small().color(crate::style::WARNING));
                            if ui.small_button(tr("Review and allow…")).clicked() {
                                self.ask_script_consent(&plugin.id);
                            }
                        }
                        Some(true) => {
                            ui.add_space(crate::style::GAP_S);
                            ui.label(egui::RichText::new(tr("Scripts allowed")).small().weak());
                            if ui.small_button(tr("Stop allowing")).clicked() {
                                self.revoke_scripts(&plugin.id);
                            }
                        }
                        None => {}
                    }
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        if ui.button(tr("Close")).on_hover_text(tr("Esc, or click outside")).clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                // Right: its effects, in tabs.
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        let all = trf("All · {n}", &[("n", &effects.len().to_string())]);
                        if ui.selectable_label(self.plugin_previews.tab.is_empty(), all).clicked() {
                            self.plugin_previews.tab.clear();
                        }
                        for tab in &tabs {
                            let n = effects.iter().filter(|d| category(d) == *tab).count();
                            if ui.selectable_label(self.plugin_previews.tab == *tab, format!("{tab} · {n}")).clicked() {
                                self.plugin_previews.tab = tab.clone();
                            }
                        }
                    });
                    ui.separator();
                    if !enabled {
                        ui.label(egui::RichText::new(tr("Turned off: its previews show once it's on.")).small().weak());
                    }
                    let shown: Vec<&EffectDescriptor> = effects.iter().filter(|d| self.plugin_previews.tab.is_empty() || category(d) == self.plugin_previews.tab).collect();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
                            for d in shown {
                                self.plugin_effect_cell(ui, d);
                            }
                        });
                    });
                });
            });
        });
        if close || modal.should_close() {
            self.plugin_previews.open = None;
            self.plugin_previews.hover = None;
        }
    }

    fn plugin_effect_cell(&mut self, ui: &mut egui::Ui, d: &EffectDescriptor) {
        // Plugins changed: the previews are of what the effects were.
        let registry = Arc::as_ptr(&self.registry) as usize;
        if self.plugin_previews.registry != registry {
            self.plugin_previews.registry = registry;
            self.plugin_previews.requested.clear();
            let old: Vec<_> = self.plugin_previews.images.drain().map(|(_, v)| v).collect();
            self.free_plugin_textures(old);
        }
        if let Some(h) = &mut self.plugin_previews.hover
            && !std::mem::replace(&mut h.2, false)
        {
            self.plugin_previews.hover = None;
        }
        let picture = egui::vec2(CELL, CELL * CANVAS[1] as f32 / CANVAS[0] as f32);
        let (rect, response) = ui.allocate_exact_size(picture + egui::vec2(0.0, 36.0), egui::Sense::hover());
        let image_rect = egui::Rect::from_min_size(rect.min, picture);
        let painter = ui.painter_at(rect);
        painter.rect_filled(image_rect, 8.0, ui.visuals().extreme_bg_color);
        if previewable(d) {
            let texture = if response.hovered() {
                let now = ui.input(|i| i.time);
                let since = match &self.plugin_previews.hover {
                    Some((id, since, _)) if **id == *d.type_id => *since,
                    _ => now,
                };
                self.plugin_previews.hover = Some((d.type_id.to_string(), since, true));
                ui.ctx().request_repaint();
                self.plugin_live(d, now - since).or_else(|| self.plugin_still(d))
            } else {
                self.plugin_still(d)
            };
            match texture {
                Some(id) => {
                    painter.image(id, image_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                None => crate::widgets::skeleton(ui, &painter, image_rect),
            }
        } else {
            let icon = if d.kind == EffectKind::Sound { crate::icons::AUDIO_TRACK } else { crate::icons::EFFECTS };
            crate::icons::paint(&painter, egui::Rect::from_center_size(image_rect.center(), egui::vec2(36.0, 36.0)), icon, ui.visuals().weak_text_color());
        }
        let outline = if response.hovered() { ui.visuals().widgets.hovered.fg_stroke.color } else { ui.visuals().widgets.noninteractive.bg_stroke.color };
        painter.rect_stroke(image_rect, 8.0, egui::Stroke::new(1.0, outline), egui::StrokeKind::Inside);
        painter.text(egui::pos2(rect.left() + 2.0, image_rect.bottom() + 5.0), egui::Align2::LEFT_TOP, &d.name, egui::FontId::proportional(13.0), ui.visuals().text_color());
        painter.text(egui::pos2(rect.left() + 2.0, image_rect.bottom() + 21.0), egui::Align2::LEFT_TOP, kind_label(d), egui::FontId::proportional(10.5), ui.visuals().weak_text_color());
        let tip = if d.description.is_empty() { format!("{} — {}", d.name, kind_label(d)) } else { format!("{}\n{}", d.name, d.description) };
        response.on_hover_text(tip);
    }

    /// The still preview of `d`: asked for once, picked up when done.
    fn plugin_still(&mut self, d: &EffectDescriptor) -> Option<egui::TextureId> {
        let key = d.type_id.to_string();
        if let Some((id, _)) = self.plugin_previews.images.get(&key) {
            return Some(*id);
        }
        let slot = slot(&key);
        let tag = self.plugin_previews.registry as u64;
        if let Some(done) = self.preview_worker.take(slot).filter(|r| r.tag == tag)
            && let Some(rs) = self.render_state.as_ref()
        {
            let view = oa_gpu::readback::display_view(&done.texture);
            let id = rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear);
            self.plugin_previews.images.insert(key, (id, done.texture));
            return Some(id);
        }
        if self.plugin_previews.requested.insert(key) {
            let sky = self.preview_sky();
            let (project, at) = sample_scene(d, None, sky);
            self.request_sample(slot, tag, project, at);
        }
        None
    }

    /// The hovered cell's animation, `phase` seconds in (the latest frame done).
    fn plugin_live(&mut self, d: &EffectDescriptor, phase: f64) -> Option<egui::TextureId> {
        let cell = slot(&d.type_id);
        let sky = self.preview_sky();
        let (project, at) = sample_scene(d, Some(phase), sky);
        self.request_sample(LIVE_SLOT, cell, project, at);
        if let Some(done) = self.preview_worker.take(LIVE_SLOT)
            && done.tag == cell
            && let Some(rs) = self.render_state.as_ref()
        {
            let view = oa_gpu::readback::display_view(&done.texture);
            let id = match self.plugin_previews.live.take() {
                Some((id, ..)) => {
                    rs.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, id);
                    id
                }
                None => rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
            };
            self.plugin_previews.live = Some((id, done.texture, cell));
        }
        self.plugin_previews.live.as_ref().filter(|(.., of)| *of == cell).map(|(id, ..)| *id)
    }

    fn request_sample(&self, slot: u64, tag: u64, project: Project, at: Time) {
        self.preview_worker.request(crate::preview_worker::Request {
            slot,
            tag,
            project: Arc::new(project),
            registry: self.registry.clone(),
            seq: OUTER,
            variant: VARIANT,
            at,
            scale: (CELL as f64 * 1.5 / CANVAS[0] as f64).min(1.0),
            wanted: None,
            png: None,
            see_through: false,
        });
    }

    fn free_plugin_textures(&self, textures: Vec<(egui::TextureId, wgpu::Texture)>) {
        if let Some(rs) = &self.render_state {
            let mut renderer = rs.renderer.write();
            for (id, _) in textures {
                renderer.free_texture(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_graph::registry::Registry;

    /// Every Atelier Core effect the window previews builds a scene that plans, with the
    /// sample picture and without it: picture effects on the whole scene, text effects on
    /// the letters.
    #[test]
    fn every_previewable_effect_plans_on_the_sample_scene() {
        let registry = Registry::with_builtins();
        let core = oa_graph::plugin::core();
        let mut shown = 0;
        for d in core.effects.iter().filter(|d| previewable(d) && !oa_graph::registry::is_internal_effect(&d.type_id)) {
            for (phase, sky) in [(None, Some([767, 1024])), (Some(0.3), None)] {
                let (project, at) = sample_scene(d, phase, sky);
                let plan = oa_plan::plan_frame(&project, OUTER, at, &oa_plan::PlanOptions::default(), &registry).unwrap_or_else(|e| panic!("{}: {e}", d.type_id));
                assert!(plan.report.missing_effects.is_empty(), "{}: {:?}", d.type_id, plan.report);
                let on_title = project.sequence(INNER).unwrap().item(TITLE).unwrap().effects.len();
                let on_scene = project.sequence(OUTER).unwrap().item(SCENE).unwrap().effects.len();
                assert_eq!((on_title, on_scene), if d.kind.text_only() { (1, 0) } else { (0, 1) }, "{}", d.type_id);
                assert_eq!(project.sequence(INNER).unwrap().item(PHOTO).is_some(), sky.is_some());
            }
            shown += 1;
        }
        assert!(shown > 30, "only {shown} previewed");
    }

    /// The sample scene renders with the picture in it: the preview thread keeps it (as
    /// the window hands it over) and the sky's colors come out — warm low down, cooler
    /// up high — rather than the plain backdrop.
    #[test]
    fn the_sample_picture_renders() {
        let Ok(gpu) = oa_gpu::GpuContext::new_headless() else { return };
        let gpu = Arc::new(gpu);
        let path = std::env::temp_dir().join(format!("oa-sky-{}.jpg", std::process::id()));
        std::fs::write(&path, SKY_JPG).unwrap();
        let Ok(probe) = oa_media::probe(&path) else { return }; // no ffprobe here
        let track = probe.video.unwrap();
        let size = [track.width, track.height];
        let worker = crate::preview_worker::PreviewWorker::start(gpu, oa_media::DecoderChoice::default(), 512 << 20, egui::Context::default());
        worker.keep(crate::export_worker::MediaEntry { id: SKY, kind: oa_media::MediaKind::Still, path: path.clone(), track: Some(track) });
        let registry = Arc::new(Registry::with_builtins());
        let d = registry.effect("oa.color.saturation").unwrap().clone();
        let (project, at) = sample_scene(&d, None, Some(size));
        let (tx, rx) = std::sync::mpsc::channel();
        worker.pixels(crate::preview_worker::Request { slot: 0, tag: 0, project: Arc::new(project), registry, seq: OUTER, variant: VARIANT, at, scale: 0.25, wanted: None, png: None, see_through: false }, tx);
        let frame = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("answered").expect("rendered");
        let [w, h] = frame.size;
        assert_eq!([w, h], [90, 120]);
        let px = |x: usize, y: usize| &frame.pixels[(y * w + x) * 4..(y * w + x) * 4 + 3];
        // Up high, left of the tree: the blue-grey sky. Low down: the golden band.
        let (high, low) = (px(80, 10), px(45, 105));
        assert!(high[2] > high[0], "blue above: {high:?}");
        assert!(low[0] > low[2], "gold below: {low:?}");
        let _ = std::fs::remove_file(&path);
    }

    /// The built-in picture is the one in the repository, a portrait 3:4 JPEG.
    #[test]
    fn the_sample_picture_is_built_in() {
        assert_eq!(&SKY_JPG[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
        assert!(SKY_JPG.len() > 10_000);
    }
}
