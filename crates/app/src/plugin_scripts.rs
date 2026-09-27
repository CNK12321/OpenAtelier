//! Plugins' own scripts: **overlays** drawn over the viewer and **actions** run over the
//! selected clips (`oa_graph::script`), from the Plugins menu — and the security
//! confirmation none of a plugin's scripts get past.
//!
//! A plugin carrying any OA script (its sound effects, motion and other effect scripts,
//! overlays, actions) runs none of them until the user has allowed that plugin's scripts
//! — exactly those: the allowance is the scripts' fingerprint (`Plugin::script_digest`),
//! so an updated plugin asks again. Until then it loads without them (`Plugins::registry`).
//! OA script itself can't loop, touch files or the network, or see anything but what its
//! host hands it; the confirmation is for what the host hands it — an action edits the
//! project, an overlay draws over the picture — and for code from someone else running
//! at all. Atelier Core is part of the editor and needs no allowing.

use crate::App;
use eframe::egui;
use oa_doc::schema;
use oa_graph::plugin::{Plugin, PluginScript, ScriptKind};
use oa_graph::script::{ActionClip, OverlayInput, OverlayShape};
use oa_params::Value;
use std::sync::Arc;

/// The question on screen: may this plugin's scripts run?
pub struct ScriptConsent {
    pub plugin: String,
    pub name: String,
    pub author: String,
    pub digest: u64,
    /// What would run, in words.
    pub what: Vec<String>,
}

/// Parameter values a script runs with: its defaults.
fn defaults(s: &PluginScript) -> oa_params::Evaluated {
    oa_params::Evaluated(s.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect())
}

impl App {
    fn scripts_allowed(&self, p: &Plugin) -> bool {
        crate::plugins::Plugins::scripts_allowed(p, &self.settings.trusted_scripts)
    }

    /// Asks whether `plugin`'s scripts may run (the dialog is `script_consent_window`).
    pub(crate) fn ask_script_consent(&mut self, plugin: &str) {
        let Some(p) = self.plugins.list.iter().find(|p| p.id == plugin) else { return };
        let Some(digest) = p.script_digest else { return };
        let mut what = Vec::new();
        for d in &p.effects {
            let kinds: Vec<&str> = [
                (d.kind == oa_graph::EffectKind::Sound, "sound processing"),
                (d.motion.is_some(), "motion"),
                (d.bounds.is_some() || d.pass_count.is_some() || d.pass_divisor.is_some(), "drawing bounds"),
            ]
            .into_iter()
            .filter_map(|(on, k)| on.then_some(k))
            .collect();
            if !kinds.is_empty() {
                what.push(format!("Effect “{}” — {}", d.name, kinds.join(", ")));
            }
        }
        for s in &p.scripts {
            what.push(match s.kind {
                ScriptKind::Overlay(_) => format!("Overlay “{}” — draws over the viewer", s.name),
                ScriptKind::Action(_) => format!("Action “{}” — changes the selected clips when you run it", s.name),
            });
        }
        self.script_consent = Some(ScriptConsent { plugin: p.id.clone(), name: p.name.clone(), author: p.author.clone(), digest, what });
    }

    /// The security confirmation: nothing of the plugin's runs until "Allow".
    pub(crate) fn script_consent_window(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.script_consent else { return };
        let (mut allow, mut close) = (false, false);
        egui::Modal::new(egui::Id::new("script-consent")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(format!("Allow scripts from “{}”?", c.name));
            if !c.author.is_empty() {
                ui.label(egui::RichText::new(format!("by {}", c.author)).weak());
            }
            ui.add_space(6.0);
            ui.label("This plugin carries scripts that run on this computer:");
            for w in &c.what {
                ui.label(format!("  •  {w}"));
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Plugin scripts can't read or write files, reach the network, or see anything but what the editor hands them — \
                     but an action changes your project and an overlay draws over the picture. Only allow plugins you trust. \
                     If the plugin's scripts change (an update), you'll be asked again.",
                )
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(egui::RichText::new("Allow its scripts").strong()).clicked() {
                    allow = true;
                }
                if ui.button("Not now").clicked() {
                    close = true;
                }
            });
        });
        if allow {
            let (id, digest) = (c.plugin.clone(), c.digest);
            self.settings.trusted_scripts.insert(id, digest);
            self.settings.save();
            self.script_consent = None;
            self.rebuild_registry();
        } else if close {
            self.script_consent = None;
        }
    }

    /// Takes back a plugin's allowance: its scripts stop at once.
    pub(crate) fn revoke_scripts(&mut self, plugin: &str) {
        if self.settings.trusted_scripts.remove(plugin).is_some() {
            self.settings.overlays_on.retain(|k| !k.starts_with(&format!("{plugin}/")));
            self.settings.save();
            self.rebuild_registry();
        }
    }

    /// The Plugins menu: each allowed plugin's actions (run over the selection) and
    /// overlays (on or off), and the plugins still waiting to be allowed.
    pub(crate) fn plugins_menu(&mut self, ui: &mut egui::Ui) {
        let scripted: Vec<(String, String, bool, Vec<Arc<PluginScript>>)> = self
            .plugins
            .enabled()
            .filter(|p| !p.scripts.is_empty() || (!p.builtin && p.has_scripts()))
            .map(|p| (p.id.clone(), p.name.clone(), self.scripts_allowed(p), p.scripts.clone()))
            .collect();
        ui.menu_button("Plugins", |ui| {
            ui.set_min_width(260.0);
            if scripted.is_empty() {
                ui.label(egui::RichText::new("No plugin actions or overlays").weak());
            }
            for (id, name, allowed, scripts) in &scripted {
                ui.label(egui::RichText::new(name).strong());
                if !allowed {
                    if ui.button("⚠ Its scripts need your OK…").on_hover_text("Nothing of this plugin's scripts runs until you allow it").clicked() {
                        self.ask_script_consent(id);
                        ui.close();
                    }
                    continue;
                }
                for s in scripts {
                    match &s.kind {
                        ScriptKind::Action(_) => {
                            let any = !self.selected.is_empty() || self.selection.is_some();
                            let r = ui.add_enabled(any, egui::Button::new(format!("▶ {}", s.name)));
                            let r = if s.description.is_empty() { r } else { r.on_hover_text(&s.description) };
                            if r.clicked() {
                                self.run_plugin_action(id, s);
                                ui.close();
                            }
                        }
                        ScriptKind::Overlay(_) => {
                            let key = format!("{id}/{}", s.id);
                            let mut on = self.settings.overlays_on.contains(&key);
                            let r = ui.checkbox(&mut on, &s.name);
                            let r = if s.description.is_empty() { r } else { r.on_hover_text(&s.description) };
                            if r.changed() {
                                if on {
                                    self.settings.overlays_on.insert(key);
                                } else {
                                    self.settings.overlays_on.remove(&key);
                                }
                                self.settings.save();
                            }
                        }
                    }
                }
                ui.separator();
            }
            if ui.button("Manage plugins…").clicked() {
                self.go_home();
                self.home_tab = crate::home::HomeTab::Plugins;
                ui.close();
            }
        });
    }

    /// Draws the overlays that are on (from allowed plugins) over the viewer.
    pub(crate) fn draw_overlays(&self, painter: &egui::Painter, to_screen: &dyn Fn([f64; 2]) -> egui::Pos2, zoom: f64, input: &OverlayInput) {
        if self.settings.overlays_on.is_empty() {
            return;
        }
        for p in self.plugins.enabled().filter(|p| self.scripts_allowed(p)) {
            for s in &p.scripts {
                let ScriptKind::Overlay(overlay) = &s.kind else { continue };
                if !self.settings.overlays_on.contains(&format!("{}/{}", p.id, s.id)) {
                    continue;
                }
                let color = |c: [f32; 4]| egui::Color32::from_rgba_unmultiplied((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, (c[3] * 255.0) as u8);
                let at = |p: [f32; 2]| to_screen([p[0] as f64, p[1] as f64]);
                let px = |w: f32| (w as f64 * zoom).max(1.0) as f32;
                for shape in overlay.draw(&defaults(s), input) {
                    match shape {
                        OverlayShape::Line { from, to, width, color: c } => {
                            painter.line_segment([at(from), at(to)], egui::Stroke::new(px(width), color(c)));
                        }
                        OverlayShape::Rect { min, max, width, color: c } => {
                            let r = egui::Rect::from_two_pos(at(min), at(max));
                            if width > 0.0 {
                                painter.rect_stroke(r, 0.0, egui::Stroke::new(px(width), color(c)), egui::StrokeKind::Middle);
                            } else {
                                painter.rect_filled(r, 0.0, color(c));
                            }
                        }
                        OverlayShape::Circle { center, radius, width, color: c } => {
                            let r = (radius as f64 * zoom) as f32;
                            if width > 0.0 {
                                painter.circle_stroke(at(center), r, egui::Stroke::new(px(width), color(c)));
                            } else {
                                painter.circle_filled(at(center), r, color(c));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Runs a plugin's action over the selected clips (in timeline order), as one edit.
    fn run_plugin_action(&mut self, plugin: &str, s: &Arc<PluginScript>) {
        use oa_doc::ParamTarget;
        let Some(p) = self.plugins.list.iter().find(|p| p.id == plugin) else { return };
        if !self.scripts_allowed(p) {
            self.ask_script_consent(plugin);
            return;
        }
        let ScriptKind::Action(action) = &s.kind else { return };
        let mut ids: Vec<oa_doc::ItemId> = self.selected_clips();
        if ids.is_empty() {
            ids.extend(self.selection);
        }
        ids.sort_by_key(|id| self.editor.item(*id).map(|i| i.range.start));
        let (t, seq, variant, scope) = (self.playhead, self.editor.seq, self.variant_id(), self.transform_scope());
        let canvas = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
        let values = defaults(s);
        let project = self.editor.doc.snapshot();
        let mut ops = Vec::new();
        let count = ids.len();
        for (index, id) in ids.into_iter().enumerate() {
            let Some(item) = self.editor.item(id).cloned() else { continue };
            let value = |param: &str| self.editor.param_value(id, &ParamTarget::Item, param, t);
            let position = value(schema::POSITION).and_then(|v| v.as_vec2()).unwrap_or([0.0, 0.0]);
            let scale = value(schema::SCALE).and_then(|v| v.as_vec2()).unwrap_or([1.0, 1.0]);
            let before = ActionClip {
                start: item.range.start.as_seconds_f64(),
                duration: item.range.duration.as_seconds_f64(),
                position,
                scale: scale[0],
                rotation: value(schema::ROTATION).and_then(|v| v.as_float()).unwrap_or(0.0),
                opacity: value(schema::OPACITY).and_then(|v| v.as_float()).unwrap_or(1.0),
                volume_db: value(schema::AUDIO_GAIN).and_then(|v| v.as_float()).unwrap_or(0.0),
            };
            let after = action.apply(&values, before, index, count, t.as_seconds_f64(), [canvas.width as f64, canvas.height as f64]);
            let changed = |a: f64, b: f64| (a - b).abs() > 1e-9;
            if changed(before.start, after.start) || changed(before.duration, after.duration) {
                let rate = self.editor.sequence().rate;
                let snap = |s: f64| rate.frame_start(rate.frame_at(oa_time::Time::from_seconds_f64(s)));
                let range = oa_time::TimeRange::new(snap(after.start), (snap(after.start + after.duration) - snap(after.start)).max(rate.frame_start(1)));
                ops.push(oa_doc::Op::SetItemTiming { seq, item: id, range, time_map: item.time_map });
            }
            let mut set = |param: &str, v: Value| match oa_edit::transform::write_param(&project, seq, id, variant, scope, t, param, v) {
                Ok(op) => ops.push(op),
                Err(e) => self.error = Some(e.to_string()),
            };
            if changed(before.position[0], after.position[0]) || changed(before.position[1], after.position[1]) {
                set(schema::POSITION, Value::Vec2(after.position));
            }
            if changed(before.scale, after.scale) {
                let k = if scale[0].abs() > 1e-9 { after.scale / scale[0] } else { 1.0 };
                set(schema::SCALE, Value::Vec2([after.scale, scale[1] * k]));
            }
            if changed(before.rotation, after.rotation) {
                set(schema::ROTATION, Value::Float(after.rotation));
            }
            if changed(before.opacity, after.opacity) {
                set(schema::OPACITY, Value::Float(after.opacity));
            }
            if changed(before.volume_db, after.volume_db) {
                set(schema::AUDIO_GAIN, Value::Float(after.volume_db));
            }
        }
        if ops.is_empty() {
            self.notify(format!("{} changed nothing", s.name));
            return;
        }
        if let Err(e) = self.editor.apply(&s.name, ops) {
            self.error = Some(format!("{}: {e}", s.name));
        }
    }
}
