//! The clip inspector, in tabs: **Properties** (what the clip is and where it sits),
//! **Transitions** (how it arrives and leaves), **Effects** (what is applied for its
//! whole length) and **Sound**. One job per tab, rather than one scroll holding all of
//! them — a clip with five effects used to bury its opacity.
//!
//! With nothing selected the panel shows the **Scene** instead: the background, which
//! belongs to the sequence rather than to any clip, and its effects (the background is
//! edited as a clip of its own, `oa_doc::BACKGROUND`).
//!
//! Picture and sound effects share one kind of card: drag its grip to reorder, or onto
//! another clip to copy it; **Apply to All Selected** copies it onto the other selected
//! clips; "+ Add…" adds to every selected clip.
//!
//! Every property shows its value at the playhead. The ◇/◆ button turns keyframing on
//! or off; while it's on, changing the value (here or by dragging in the viewer) sets a
//! key at the playhead. ◀ ▶ jump between the clip's keyframes.

use crate::App;
use eframe::egui;
use oa_doc::{schema, ItemId, ItemKind, ParamTarget};
use crate::thumbs::ThumbKind;
use oa_edit::transform;
use oa_params::{KeyframeAnchor, ParamSource, Value};
use oa_time::Time;

/// Whether the properties panel is drawn compact this frame (Settings → Interface).
fn compact(ui: &egui::Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new(COMPACT)).unwrap_or(false))
}

const COMPACT: &str = "inspector-compact";

/// Draws the properties panel compact or not: tighter spacing and smaller controls, the
/// section notes moving into tooltips (see [`section`]).
pub(crate) fn set_compact(ui: &mut egui::Ui, on: bool) {
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(COMPACT), on));
    if !on {
        return;
    }
    let style = ui.style_mut();
    style.spacing.item_spacing = egui::vec2(6.0, 2.0);
    style.spacing.button_padding = egui::vec2(4.0, 1.0);
    style.spacing.interact_size.y = 16.0;
    style.spacing.indent = 12.0;
    style.spacing.slider_width = (style.spacing.slider_width * 0.85).max(80.0);
    for (text_style, font) in style.text_styles.iter_mut() {
        if matches!(text_style, egui::TextStyle::Body | egui::TextStyle::Button | egui::TextStyle::Monospace) {
            font.size = (font.size - 1.5).max(10.0);
        }
    }
}

/// A section heading with a one-line explanation (compact: a smaller heading, and the
/// explanation shows on hover).
fn section(ui: &mut egui::Ui, title: &str, hint: &str) {
    if compact(ui) {
        ui.add_space(3.0);
        ui.separator();
        let r = ui.label(egui::RichText::new(title).size(13.0).strong());
        if !hint.is_empty() {
            r.on_hover_text(hint);
        }
        return;
    }
    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new(title).size(15.0).strong());
    if !hint.is_empty() {
        ui.label(egui::RichText::new(hint).small().weak());
    }
    ui.add_space(2.0);
}

/// Dragging the effect shown `from`-th to before the one shown `slot`-th (or past the
/// last): its new index in the clip's whole effect list, counted after taking it out.
/// `positions` are the shown effects' indices in that list (other kinds sit between).
/// `None` when it wouldn't move.
fn move_target(positions: &[usize], from: usize, slot: usize) -> Option<usize> {
    if from >= positions.len() || slot == from || slot == from + 1 {
        return None;
    }
    let src = positions[from];
    let at = positions.get(slot).copied().unwrap_or_else(|| positions.last().map_or(0, |i| i + 1));
    Some(if src < at { at - 1 } else { at })
}

#[cfg(test)]
mod tests {
    use super::move_target;

    /// Moves in a list shaped [intro, A, B, sound, C]: A B C are the shown effects.
    #[test]
    fn drag_targets() {
        let shown = [1, 2, 4];
        let apply = |from: usize, slot: usize| {
            let mut list = vec!["intro", "A", "B", "sound", "C"];
            let at = move_target(&shown, from, slot)?;
            let x = list.remove(shown[from]);
            list.insert(at, x);
            Some(list)
        };
        assert_eq!(apply(0, 3), Some(vec!["intro", "B", "sound", "C", "A"]), "A to the end");
        assert_eq!(apply(2, 0), Some(vec!["intro", "C", "A", "B", "sound"]), "C to the top");
        assert_eq!(apply(0, 2), Some(vec!["intro", "B", "sound", "A", "C"]), "A between B and C");
        assert_eq!(apply(1, 1), None);
        assert_eq!(apply(1, 2), None, "just below itself is where it already is");
    }
}

/// An effect being dragged out of the inspector (`App::effect_drag`): let go over
/// another clip, in the timeline or the viewer, and a copy goes onto it. The inspector
/// decides the drop itself, from where the clips were drawn (`App::clip_on_screen`),
/// so it doesn't depend on another panel seeing the release.
#[derive(Clone)]
pub struct EffectDrag {
    pub from: ItemId,
    pub effect: oa_doc::EffectInstance,
}

/// Which of a clip's effects a list of effect cards shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum List {
    /// Passive picture effects.
    Effects,
    /// Sound effects, in any role.
    Sound,
    /// Picture intros and outros.
    Intro,
    Outro,
}

/// A framed box grouping one effect's controls.
fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let tight = compact(ui);
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(if tight { 3 } else { 6 })).corner_radius(4.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(if tight { 2.0 } else { 4.0 });
}

/// A property row: label, keyframe toggle, and the editor widget.
struct Prop<'a> {
    label: &'a str,
    target: ParamTarget,
    param: &'a str,
    default: Value,
}

/// Which part of the clip the inspector is showing.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Properties,
    Transitions,
    Effects,
    Sound,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Properties => "Properties",
            Tab::Transitions => "Transitions",
            Tab::Effects => "Effects",
            Tab::Sound => "Sound",
        }
    }
}

impl App {
    pub(crate) fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(item) = self.selection.filter(|i| self.editor.item(*i).is_some()) else {
            ui.label(egui::RichText::new("Select a clip — in the viewer or on the timeline.").weak());
            section(ui, "Background", "What shows behind the clips, in every format.");
            self.background_section(ui);
            section(ui, "Background effects", "Run on the background only, under the clips — keyframable like a clip's.");
            self.effects_section(ui, oa_doc::BACKGROUND, self.playhead);
            if self.settings.advanced_color {
                section(ui, "Output", "How the picture meets an SDR screen and the exported file — the same for both.");
                self.output_section(ui);
            }
            return;
        };
        let it = self.editor.item(item).expect("checked").clone();
        let t = self.playhead;
        let inside = it.range.contains(t);
        ui.horizontal(|ui| {
            ui.strong(&it.name);
            ui.label(egui::RichText::new(format!("{:.2}s – {:.2}s", it.range.start.as_seconds_f64(), it.range.end().as_seconds_f64())).weak());
        });
        let group = self.editor.linked.len();
        if group > 1 {
            ui.label(egui::RichText::new(format!("Editing {group} selected clips together: a change here changes each of them (the position moves them all by the same amount).")).small().color(crate::style::ACCENT))
                .on_hover_text("The values shown are this clip's. Each clip keeps its own keyframes.");
        }
        if !inside {
            ui.label(egui::RichText::new("The playhead is outside this clip; values shown at its nearest edge.").small().weak());
        }
        // Evaluate at the playhead, clamped into the clip.
        let t = t.max(it.range.start).min(it.range.end() - Time(1));

        // An effect container: no picture or sound of its own, just its effects.
        if let Some(kind) = self.container_kind(item) {
            self.container_panel(ui, item, kind, t);
            return;
        }

        // Keyframe navigation over every animated property of the clip.
        let mut keys: Vec<Time> = it
            .params
            .0
            .values()
            .chain(it.effects.iter().flat_map(|e| e.params.0.values()))
            .filter_map(|s| s.curve().filter(|c| c.anchor == KeyframeAnchor::ClipStart))
            .flat_map(|c| c.keys.iter().map(|k| it.range.start + k.t))
            .collect();
        keys.sort();
        keys.dedup();
        ui.horizontal(|ui| {
            let prev = keys.iter().rev().find(|k| **k < self.playhead).copied();
            let next = keys.iter().find(|k| **k > self.playhead).copied();
            if ui.add_enabled(prev.is_some(), egui::Button::new("◀ key")).clicked() {
                self.set_playhead(prev.expect("enabled"));
            }
            if ui.add_enabled(next.is_some(), egui::Button::new("key ▶")).clicked() {
                self.set_playhead(next.expect("enabled"));
            }
            ui.label(egui::RichText::new(format!("{} keyframes", keys.len())).weak());
        });

        let visual = !matches!(it.kind, ItemKind::Media { .. }) || self.is_visual(item);
        let audible = self.is_audible(item);

        // The tabs: only the ones this clip has anything in.
        let tabs: Vec<Tab> = [Tab::Properties, Tab::Transitions, Tab::Effects, Tab::Sound]
            .into_iter()
            .filter(|tab| match tab {
                Tab::Properties => true,
                Tab::Transitions | Tab::Effects => visual,
                Tab::Sound => audible,
            })
            .collect();
        if !tabs.contains(&self.inspector_tab) {
            self.inspector_tab = Tab::Properties;
        }
        ui.add_space(crate::style::GAP_S);
        let titled: Vec<(Tab, &str)> = tabs.iter().map(|t| (*t, t.title())).collect();
        crate::widgets::tabs(ui, &mut self.inspector_tab, &titled);
        ui.add_space(crate::style::GAP);

        match self.inspector_tab {
            Tab::Properties => self.properties_tab(ui, item, visual, t),
            Tab::Transitions => self.transitions_tab(ui, item, t),
            Tab::Effects => {
                self.effects_section(ui, item, t);
            }
            Tab::Sound => self.sound_tab(ui, item, t),
        }
    }

    /// What the clip is and where it sits: its text, if it has any, how fast it plays,
    /// then its transform.
    fn properties_tab(&mut self, ui: &mut egui::Ui, item: ItemId, visual: bool, t: Time) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        // Footage plays at a speed; a title or a solid has nothing to speed up.
        if matches!(it.kind, ItemKind::Media { .. } | ItemKind::Nested { .. }) {
            section(ui, "Speed", "The whole clip, picture and sound together. It gets shorter or longer on the timeline.");
            self.speed_row(ui, item);
        }
        if it.kind == ItemKind::Text {
            section(ui, "Text", "Per-letter and per-pixel text effects are in the Transitions and Effects tabs.");
            self.text_section(ui, item, t);
        }
        if visual {
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Transform");
                let scope = self.transform_scope();
                ui.label(egui::RichText::new(match scope {
                    transform::Scope::AllFormats => "all formats",
                    transform::Scope::Variant(_) => "this format only",
                }).small().weak());
                if ui.small_button("reset").clicked() {
                    match transform::reset_transform(self.editor.doc.project(), self.editor.seq, item, scope) {
                        Ok(ops) => self.apply_or_report("Reset transform", ops),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                if ui.small_button("copy").on_hover_text("Copy this clip's transform (keyframes and per-format changes too)").clicked() {
                    self.copy_transform(item);
                }
                let pastable = self.transform_clipboard.is_some();
                if ui.add_enabled(pastable, egui::Button::new("paste").small()).on_hover_text("Paste the copied transform onto the selected clips").clicked() {
                    let targets = self.selected_clips();
                    self.paste_transform(&targets);
                }
            });
            let canvas = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
            if let Ok(p) = transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), item, t) {
                egui::Grid::new("transform").num_columns(3).spacing([6.0, 4.0]).show(ui, |ui| {
                    let v = &p.values;
                    // Following a track: what's shown and edited is the offset from it.
                    let source = self.editor.param_source(item, &ParamTarget::Item, schema::POSITION);
                    let following = source.as_ref().and_then(|s| Some((s.find_track()?.0.clone(), matches!(s.track_use(), Some(oa_params::TrackUse::Stabilize(_))))));
                    let on_track = source.as_ref().and_then(|s| s.track_contribution(&it.eval_context(t))).unwrap_or([0.0; 2]);
                    let pos = v.vec2(schema::POSITION);
                    let pos = [pos[0] - on_track[0], pos[1] - on_track[1]];
                    let mut px = [pos[0] * canvas.width as f64, pos[1] * canvas.height as f64];
                    self.key_toggle(ui, item, schema::POSITION, Value::Vec2([0.0, 0.0]), t);
                    match &following {
                        Some((tr, stabilized)) => {
                            let how = if *stabilized { format!("Stabilized with “{}”: this is where it sits, the shake taken out on top.", tr.name) } else { format!("Following “{}”: this is the offset from the track.", tr.name) };
                            ui.label(egui::RichText::new("offset").color(crate::style::GOLD)).on_hover_text(format!("{how} Right-click the numbers → Animate → Edit track… to change or stop it."));
                        }
                        None => {
                            ui.label("position");
                        }
                    }
                    ui.horizontal(|ui| {
                        let a = ui.add(egui::DragValue::new(&mut px[0]).speed(1.0).suffix(" px"));
                        let b = ui.add(egui::DragValue::new(&mut px[1]).speed(1.0).suffix(" px"));
                        if a.changed() || b.changed() {
                            let full = [px[0] / canvas.width as f64 + on_track[0], px[1] / canvas.height as f64 + on_track[1]];
                            self.write_transform(item, t, schema::POSITION, Value::Vec2(full));
                        }
                        self.seal_on_release(&a);
                        self.seal_on_release(&b);
                        for r in [&a, &b] {
                            self.property_menu(r, item, &ParamTarget::Item, schema::POSITION, &Value::Vec2([0.0, 0.0]), None, t);
                        }
                    });
                    ui.end_row();

                    let scale = v.vec2(schema::SCALE);
                    let mut pct = [scale[0] * 100.0, scale[1] * 100.0];
                    self.key_toggle(ui, item, schema::SCALE, Value::Vec2([1.0, 1.0]), t);
                    ui.label("scale");
                    ui.horizontal(|ui| {
                        // One linked value by default; right-click for X and Y separately
                        // (shown anyway once they differ).
                        let key = ui.id().with(("scale-advanced", item.0));
                        let mut advanced = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) || (pct[0] - pct[1]).abs() > 1e-6;
                        let responses = if advanced {
                            let a = ui.add(egui::DragValue::new(&mut pct[0]).speed(0.5).prefix("X ").suffix(" %"));
                            let b = ui.add(egui::DragValue::new(&mut pct[1]).speed(0.5).prefix("Y ").suffix(" %"));
                            vec![a, b]
                        } else {
                            let r = ui.add(egui::DragValue::new(&mut pct[0]).speed(0.5).suffix(" %"));
                            pct[1] = pct[0];
                            vec![r]
                        };
                        if responses.iter().any(|r| r.changed()) {
                            self.write_transform(item, t, schema::SCALE, Value::Vec2([pct[0] / 100.0, pct[1] / 100.0]));
                        }
                        for r in &responses {
                            self.seal_on_release(r);
                            crate::widgets::context_menu(r, |ui| {
                                self.property_menu_items(ui, item, &ParamTarget::Item, schema::SCALE, &Value::Vec2([1.0, 1.0]), None, t);
                                ui.separator();
                                if !advanced && ui.button("Advanced: separate X and Y").clicked() {
                                    advanced = true;
                                    ui.close();
                                }
                                if advanced && ui.button("Simple: one value (uses X)").clicked() {
                                    advanced = false;
                                    self.write_transform(item, t, schema::SCALE, Value::Vec2([pct[0] / 100.0, pct[0] / 100.0]));
                                    self.editor.doc.seal();
                                    ui.close();
                                }
                            });
                        }
                        ui.data_mut(|d| d.insert_temp(key, advanced));
                    });
                    ui.end_row();

                    for (param, label, default, speed, suffix, factor) in [
                        (schema::ROTATION, "rotation", 0.0, 0.5, "°", 1.0),
                        (schema::SQUASH, "squash", 0.0, 0.01, "", 1.0),
                        (schema::OPACITY, "opacity", 1.0, 0.5, " %", 100.0),
                        // Crop: the share of each edge cut away (the clip keeps its place).
                        (schema::CROP_LEFT, "crop left", 0.0, 0.25, " %", 100.0),
                        (schema::CROP_RIGHT, "crop right", 0.0, 0.25, " %", 100.0),
                        (schema::CROP_TOP, "crop top", 0.0, 0.25, " %", 100.0),
                        (schema::CROP_BOTTOM, "crop bottom", 0.0, 0.25, " %", 100.0),
                    ] {
                        let crop = schema::CROPS.contains(&param);
                        // Squash and crop are advanced (Settings); still shown once they
                        // hold something, e.g. after cropping in the viewer.
                        let animated = self.editor.param_source(item, &ParamTarget::Item, param).is_some_and(|s| s.is_animated());
                        if (crop || param == schema::SQUASH) && !self.settings.advanced_transform && v.float(param) == default && !animated {
                            continue;
                        }
                        let mut value = v.float(param) * factor;
                        self.key_toggle(ui, item, param, Value::Float(default), t);
                        ui.label(label);
                        let limits = if crop { 0.0..=100.0 } else { f64::NEG_INFINITY..=f64::INFINITY };
                        let r = ui.add(egui::DragValue::new(&mut value).speed(speed).suffix(suffix).range(limits));
                        if r.changed() {
                            self.write_transform(item, t, param, Value::Float(value / factor));
                        }
                        self.seal_on_release(&r);
                        let band = match param {
                            p if p == schema::OPACITY || crop => Some((0.0, 1.0)),
                            p if p == schema::ROTATION => Some((-180.0, 180.0)),
                            _ => Some((-0.9, 2.0)),
                        };
                        self.property_menu(&r, item, &ParamTarget::Item, param, &Value::Float(default), band, t);
                        ui.end_row();
                        if param == schema::OPACITY {
                            ui.label("");
                            ui.label("blend");
                            self.blend_combo(ui, item, t);
                            ui.end_row();
                        }
                    }
                });
            }
        }
        // How the file's values are read as light (a picture file, not a title/solid).
        if self.settings.advanced_color
            && let ItemKind::Media { media } = it.kind
            && self.editor.pool_item(media).is_some_and(|p| p.probe.video.is_some())
        {
            section(ui, "Source color", "How this file's values become light: its curve (log, HDR…), gamut and exposure.");
            self.source_color_section(ui, media);
        }
    }

    /// How the clip arrives and leaves.
    fn transitions_tab(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        section(ui, "Intro", "How the clip arrives: plays over its first seconds. Add several to combine them.");
        self.in_out_section(ui, item, true, t);
        section(ui, "Outro", "How the clip leaves: plays over its last seconds. Add several to combine them.");
        self.in_out_section(ui, item, false, t);
    }

    /// Level and sound effects. The speed control is only here for sound on its own —
    /// on a video clip it lives with the picture, and the sound follows it.
    fn sound_tab(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        self.slider(ui, item, Prop { label: "volume (dB)", target: ParamTarget::Item, param: schema::AUDIO_GAIN, default: Value::Float(0.0) }, -60.0..=12.0, t);
        ui.label(
            egui::RichText::new("◇ keyframes the volume — or drag its line on the clip in the timeline to fade by hand.")
                .small()
                .weak(),
        );
        if self.is_visual(item) {
            let speed = self.editor.item(item).map_or(1.0, |i| i.time_map.speed.num() as f64 / i.time_map.speed.den() as f64);
            ui.label(
                egui::RichText::new(format!("Speed {speed:.2}× — set with the picture, in Properties. Extract the audio to give it its own."))
                    .small()
                    .weak(),
            );
        } else {
            self.speed_row(ui, item);
        }
        section(ui, "Sound effects", "Applied top to bottom, in playback and export alike.");
        self.sound_effects_section(ui, item, t);
    }

    /// An effect container: what it covers, then its effects — the same cards as a
    /// clip's (drag to reorder or onto other clips, keyframes, waves), with intros and
    /// outros to bring it in and out.
    fn container_panel(&mut self, ui: &mut egui::Ui, item: ItemId, kind: oa_doc::TrackKind, t: Time) {
        let (what, list) = if kind == oa_doc::TrackKind::Video {
            ("Its effects run over the whole picture below this track — every clip under it, and the background — while it lasts.", List::Effects)
        } else {
            ("Its sound effects run over everything heard on the audio tracks below it, and the sound of picture tracks, while it lasts — ringing on after for echoes and rooms.", List::Sound)
        };
        ui.label(egui::RichText::new(what).small().weak());
        if kind == oa_doc::TrackKind::Video {
            section(ui, "Mix", "How much of the result shows over the untouched picture, and how it combines with it. Intros and outros like Fade fade it in and out.");
            self.slider(ui, item, Prop { label: "opacity", target: ParamTarget::Item, param: schema::OPACITY, default: Value::Float(1.0) }, 0.0..=1.0, t);
            ui.horizontal(|ui| {
                ui.label("blend");
                self.blend_combo(ui, item, t);
            });
        }
        section(ui, if list == List::Sound { "Sound effects" } else { "Effects" }, "Applied top to bottom.");
        self.effect_list(ui, item, list, t);
        if kind == oa_doc::TrackKind::Video {
            section(ui, "Intro", "Brings the effects in over the container's first moments.");
            self.in_out_section(ui, item, true, t);
            section(ui, "Outro", "Takes them away over its last moments.");
            self.in_out_section(ui, item, false, t);
        }
    }

    /// How the clip (or an effect container's result) combines with what's under it.
    fn blend_combo(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        let current = self.editor.param_value(item, &ParamTarget::Item, schema::BLEND, t).and_then(|v| v.as_enum().map(str::to_string)).unwrap_or_else(|| "normal".into());
        let title = |m: &str| match m {
            "add" => "Add",
            "multiply" => "Multiply",
            "screen" => "Screen",
            "darken" => "Darken",
            "lighten" => "Lighten",
            _ => "Normal",
        };
        let tip = |m: &str| match m {
            "add" => "Adds its light to what's under it: glows, flares, fire",
            "multiply" => "Darkens by its colors, white disappears: shadows, paper textures",
            "screen" => "Lightens by its colors, black disappears: light leaks, smoke",
            "darken" => "Keeps the darker of it and what's under it",
            "lighten" => "Keeps the lighter of it and what's under it",
            _ => "Covers what's under it",
        };
        egui::ComboBox::from_id_salt(("blend", item.0)).selected_text(title(&current)).show_ui(ui, |ui| {
            for m in schema::BLEND_MODES {
                if ui.selectable_label(current == m, title(m)).on_hover_text(tip(m)).clicked() && current != m {
                    self.editor.set_value_at(item, ParamTarget::Item, schema::BLEND, Value::Enum(m.into()), t, "blend");
                    self.editor.doc.seal();
                }
            }
        })
        .response
        .on_hover_text(tip(&current));
    }

    /// The clip's intros (`intro`) or outros, in the same cards as its other effects.
    fn in_out_section(&mut self, ui: &mut egui::Ui, item: ItemId, intro: bool, t: Time) {
        self.effect_list(ui, item, if intro { List::Intro } else { List::Outro }, t);
    }

    /// What a text clip says and how it's set: the words, font, size, colors and
    /// outline. Numbers and colors are keyframable.
    fn text_section(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        let target = ParamTarget::Item;
        let value = |app: &Self, id: &str| app.editor.param_value(item, &target, id, t);
        let mut content = value(self, schema::TEXT_CONTENT).and_then(|v| v.as_text().map(str::to_string)).unwrap_or_default();
        let r = ui.add(egui::TextEdit::multiline(&mut content).desired_rows(2).desired_width(f32::INFINITY).hint_text("Type your title"));
        if std::mem::take(&mut self.focus_text) {
            r.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(content.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), r.id);
            }
        }
        if r.changed() {
            self.editor.set_param(item, target.clone(), schema::TEXT_CONTENT, ParamSource::Static(Value::Text(content)), "text-content");
        }
        self.seal_on_release(&r);

        // Font family, with bold and italic.
        let family = value(self, schema::TEXT_FONT).and_then(|v| v.as_text().map(str::to_string)).unwrap_or_default();
        let mut chosen = family.clone();
        ui.horizontal(|ui| {
            // Searchable, with each name drawn in its own font.
            if let Some(f) = self.font_menu(ui, egui::Id::new(("font", item.0)), &family) {
                chosen = f;
            }
            for (param, label, tip) in [(schema::TEXT_BOLD, "B", "Bold"), (schema::TEXT_ITALIC, "I", "Italic")] {
                let on = matches!(value(self, param), Some(Value::Bool(true)));
                let text = if param == schema::TEXT_BOLD { egui::RichText::new(label).strong() } else { egui::RichText::new(label).italics() };
                if ui.selectable_label(on, text).on_hover_text(tip).clicked() {
                    self.editor.set_param(item, target.clone(), param, ParamSource::Static(Value::Bool(!on)), param);
                    self.editor.doc.seal();
                }
            }
            let align = value(self, schema::TEXT_ALIGN).and_then(|v| v.as_enum().map(str::to_string)).unwrap_or_else(|| "center".into());
            for (option, label) in [("left", "Left"), ("center", "Center"), ("right", "Right")] {
                if ui.selectable_label(align == option, label).clicked() && align != option {
                    self.editor.set_param(item, target.clone(), schema::TEXT_ALIGN, ParamSource::Static(Value::Enum(option.into())), "text-align");
                    self.editor.doc.seal();
                }
            }
        });
        if chosen != family {
            self.editor.set_param(item, target.clone(), schema::TEXT_FONT, ParamSource::Static(Value::Text(chosen)), "text-font");
            self.editor.doc.seal();
        }
        for id in [
            schema::TEXT_SIZE,
            schema::TEXT_COLOR,
            schema::TEXT_TRACKING,
            schema::TEXT_LINE_HEIGHT,
            schema::TEXT_OUTLINE,
            schema::TEXT_OUTLINE_COLOR,
        ] {
            let Some(s) = schema::text().iter().find(|s| s.id.as_str() == id) else { continue };
            if id == schema::TEXT_OUTLINE_COLOR && value(self, schema::TEXT_OUTLINE).and_then(|v| v.as_float()).unwrap_or(0.0) <= 0.0 {
                continue;
            }
            self.param_widget(ui, item, &target, s, t, "text");
        }
    }

    /// Type, duration and settings of the transition on one end of a clip.
    pub(crate) fn param_widget(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, schema: &oa_params::ParamSchema, t: Time, salt: &str) {
        let id = schema.id.as_str();
        let current = self.editor.param_value(item, target, id, t).filter(|v| v.ty() == schema.ty).unwrap_or_else(|| schema.default.clone());
        let pretty = |id: &str| id.rsplit('.').next().unwrap_or(id).replace(['_', '-'], " ");
        let label = match id.strip_suffix(schema::SPOKEN_SUFFIX) {
            Some(base) => format!("{} when spoken", pretty(base)),
            None => pretty(id),
        };
        match &schema.default {
            Value::Float(_) if schema.unit == oa_params::Unit::Direction => {
                // A direction: a pinwheel to spin plus a 0–360° field.
                let mut degrees = current.as_float().unwrap_or(0.0);
                ui.horizontal(|ui| {
                    self.key_toggle_for(ui, item, target.clone(), id, schema.default.clone(), t);
                    let (changed, finished) = crate::widgets::direction(ui, &mut degrees);
                    if changed {
                        self.editor.set_value_at(item, target.clone(), id, Value::Float(degrees), t, id);
                    }
                    if finished {
                        self.editor.doc.seal();
                    }
                    ui.label(&label);
                });
            }
            Value::Gradient(_) => {
                // A plain color by default; right-click for the directional gradient
                // (shown anyway once it has more than one color).
                let mut g = current.as_gradient().cloned().unwrap_or_else(|| oa_params::Gradient::solid([1.0; 4]));
                let key = ui.id().with(("gradient-advanced", salt, id, item.0));
                let mut advanced = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) || g.stops.len() > 1;
                let was_advanced = advanced;
                let mut write: Option<(oa_params::Gradient, bool)> = None;
                ui.horizontal(|ui| {
                    self.key_toggle_for(ui, item, target.clone(), id, schema.default.clone(), t);
                    let r = if advanced {
                        ui.add(egui::Label::new(&label).sense(egui::Sense::click())).on_hover_text("Right-click for a single color")
                    } else {
                        let mut c = g.sorted_stops()[0].color;
                        let (r, changed) = crate::widgets::color_swatch(ui, &mut c);
                        let r = r.on_hover_text("Click to pick · right-click for a gradient");
                        if changed {
                            write = Some((oa_params::Gradient { angle: g.angle, stops: vec![oa_params::GradientStop { pos: 0.0, color: c }] }, false));
                        }
                        ui.label(&label);
                        r
                    };
                    crate::widgets::context_menu(&r, |ui| {
                        if !advanced && ui.button("Advanced: directional gradient").clicked() {
                            advanced = true;
                            ui.close();
                        }
                        if advanced && ui.button("Simple: one color (keeps the first)").clicked() {
                            advanced = false;
                            let first = g.sorted_stops()[0].clone();
                            write = Some((oa_params::Gradient { angle: g.angle, stops: vec![oa_params::GradientStop { pos: 0.0, ..first }] }, true));
                            ui.close();
                        }
                        self.spoken_entry(ui, item, target, id, &schema.default, t);
                    });
                });
                if was_advanced && advanced {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        let (changed, finished) = crate::widgets::gradient(ui, ui.id().with(("gradient", salt, id, item.0)), &mut g);
                        if changed {
                            write = Some((g.clone(), finished));
                        } else if finished {
                            self.editor.doc.seal();
                        }
                    });
                }
                ui.data_mut(|d| d.insert_temp(key, advanced));
                if let Some((g, seal)) = write {
                    self.editor.set_value_at(item, target.clone(), id, Value::Gradient(g), t, id);
                    if seal {
                        self.editor.doc.seal();
                    }
                }
            }
            Value::Float(_) => {
                let (lo, hi) = schema.range.unwrap_or((-360.0, 360.0));
                let prop = Prop { label: &label, target: target.clone(), param: id, default: schema.default.clone() };
                self.slider(ui, item, prop, lo..=hi, t);
            }
            Value::Bool(_) => {
                let mut on = matches!(current, Value::Bool(true));
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    if ui.checkbox(&mut on, &label).changed() {
                        self.editor.set_value_at(item, target.clone(), id, Value::Bool(on), t, id);
                        self.editor.doc.seal();
                    }
                });
            }
            Value::Enum(_) => {
                let mut selected = current.as_enum().unwrap_or_default().to_string();
                let was = selected.clone();
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    egui::ComboBox::from_id_salt(("param-enum", salt, id, item.0)).selected_text(&selected).show_ui(ui, |ui| {
                        for option in &schema.options {
                            ui.selectable_value(&mut selected, option.clone(), option);
                        }
                    });
                    let lr = ui.add(egui::Label::new(&label).sense(egui::Sense::click()));
                    self.property_menu(&lr, item, target, id, &schema.default, None, t);
                });
                if selected != was {
                    self.editor.set_value_at(item, target.clone(), id, Value::Enum(selected), t, id);
                    self.editor.doc.seal();
                }
            }
            Value::Color(_) => {
                let c = if let Value::Color(c) = current { c } else { [1.0; 4] };
                let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32);
                ui.horizontal(|ui| {
                    self.key_toggle_for(ui, item, target.clone(), id, schema.default.clone(), t);
                    if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::OnlyBlend).changed() {
                        let [r, g, b, a] = rgba.to_rgba_unmultiplied();
                        let value = Value::Color([r as f64, g as f64, b as f64, a as f64]);
                        self.editor.set_value_at(item, target.clone(), id, value, t, id);
                    }
                    let lr = ui.add(egui::Label::new(&label).sense(egui::Sense::click()));
                    self.property_menu(&lr, item, target, id, &schema.default, None, t);
                });
            }
            Value::Text(_) => {
                let mut text = current.as_text().unwrap_or_default().to_string();
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(160.0));
                    if r.changed() {
                        self.editor.set_param(item, target.clone(), id, ParamSource::Static(Value::Text(text)), id);
                    }
                    self.seal_on_release(&r);
                    ui.label(&label);
                });
            }
            Value::Media(_) => {
                // Anything in the pool with a picture can feed an effect (a matte, a map…).
                // Compound clips (groups made into media) too.
                let options: Vec<(u64, String)> = self
                    .editor
                    .pool
                    .iter()
                    .filter(|m| !m.missing && m.probe.video.is_some())
                    .map(|m| (m.id.0, m.name.clone()))
                    .chain(self.editor.compounds().into_iter().map(|s| (s.id.0, format!("▣ {}", s.name))))
                    .collect();
                let mut chosen = current.as_media();
                let was = chosen;
                let name = |m: Option<u64>| m.and_then(|m| options.iter().find(|o| o.0 == m)).map_or("none".to_string(), |o| o.1.clone());
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    egui::ComboBox::from_id_salt(("param-media", salt, id, item.0)).selected_text(name(chosen)).show_ui(ui, |ui| {
                        ui.selectable_value(&mut chosen, None, "none");
                        for (m, n) in &options {
                            ui.selectable_value(&mut chosen, Some(*m), n);
                        }
                    });
                    ui.label(&label);
                });
                if chosen != was {
                    self.editor.set_param(item, target.clone(), id, ParamSource::Static(Value::Media(chosen)), id);
                    self.editor.doc.seal();
                }
            }
            Value::Vec2(_) => self.vec2_row(ui, item, target, schema, t, &label),
            _ => {
                ui.label(egui::RichText::new(format!("{label}: not editable here yet")).small().weak());
            }
        }
        // Highlighted when spoken: the spoken word's value, just under the property.
        let spoken = schema::spoken(id);
        if !id.ends_with(schema::SPOKEN_SUFFIX) && self.editor.param_source(item, target, &spoken).is_some() {
            let mut alt = schema.clone();
            alt.id = oa_params::ParamId::new(&spoken);
            ui.indent(("spoken", salt, id), |ui| self.param_widget(ui, item, target, &alt, t, salt));
        }
    }

    /// A point or offset: keyframe toggle, X and Y (fractions shown as percentages), label.
    fn vec2_row(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, schema: &oa_params::ParamSchema, t: Time, label: &str) {
        use oa_params::Unit;
        let id = schema.id.as_str();
        let current = self.editor.param_value(item, target, id, t).and_then(|v| v.as_vec2()).or_else(|| schema.default.as_vec2()).unwrap_or([0.0; 2]);
        let (k, speed, suffix) = match schema.unit {
            Unit::SourceFraction | Unit::CanvasFraction => (100.0, 0.2, " %"),
            Unit::LayerPixels => (1.0, 1.0, " px"),
            _ => (1.0, 0.01, ""),
        };
        let mut shown = [current[0] * k, current[1] * k];
        ui.horizontal(|ui| {
            self.key_toggle_for(ui, item, target.clone(), id, schema.default.clone(), t);
            let a = ui.add(egui::DragValue::new(&mut shown[0]).speed(speed).max_decimals(2).prefix("X ").suffix(suffix));
            let b = ui.add(egui::DragValue::new(&mut shown[1]).speed(speed).max_decimals(2).prefix("Y ").suffix(suffix));
            if a.changed() || b.changed() {
                self.editor.set_value_at(item, target.clone(), id, Value::Vec2([shown[0] / k, shown[1] / k]), t, id);
            }
            self.seal_on_release(&a);
            self.seal_on_release(&b);
            let lr = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
            for r in [&a, &b, &lr] {
                self.property_menu(r, item, target, id, &schema.default, None, t);
            }
        });
    }

    /// Surface: how many points, a way back to the flat picture, and each point in use
    /// (they're dragged in the viewer, where they replace the clip's handles).
    fn surface_settings(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &oa_doc::EffectInstance, d: &oa_graph::registry::EffectDescriptor, t: Time) {
        use oa_graph::registry::{surface_grid, surface_point_id};
        let target = ParamTarget::Effect(fx.id);
        let salt = fx.id.0.to_string();
        let Some(it) = self.editor.item(item) else { return };
        let n = surface_grid(&fx.params.eval(&d.params, None, &it.eval_context(t)));
        if let Some(grid) = d.params.first() {
            self.param_widget(ui, item, &target, grid, t, &salt);
        }
        ui.horizontal(|ui| {
            let (spot, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), spot, crate::icons::MOVE, ui.visuals().weak_text_color());
            ui.label(egui::RichText::new("Drag its points in the viewer").small().weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let moved = (0..16).any(|i| fx.params.get(&surface_point_id(i / 4, i % 4)).is_some());
                if ui.add_enabled(moved, egui::Button::new("Reset").small()).on_hover_text("Put every point back where it rests").clicked() {
                    let ops = (0..16)
                        .map(|i| oa_doc::Op::SetParam { seq: self.editor.seq, item, target: target.clone(), param: oa_params::ParamId::new(&surface_point_id(i / 4, i % 4)), source: None })
                        .collect();
                    if let Err(e) = self.editor.apply("Reset surface", ops) {
                        self.error = Some(e.to_string());
                    }
                }
            });
        });
        egui::CollapsingHeader::new(egui::RichText::new("Points").small()).id_salt(("surface-points", fx.id.0)).show(ui, |ui| {
            for r in 0..n {
                for c in 0..n {
                    let id = surface_point_id(r, c);
                    let Some(schema) = d.params.iter().find(|p| p.id.as_str() == id) else { continue };
                    let label = match (n, r, c) {
                        (2, 0, 0) => "top left".to_string(),
                        (2, 0, 1) => "top right".to_string(),
                        (2, 1, 0) => "bottom left".to_string(),
                        (2, 1, 1) => "bottom right".to_string(),
                        _ => format!("row {}, column {}", r + 1, c + 1),
                    };
                    self.vec2_row(ui, item, &target, schema, t, &label);
                }
            }
        });
    }

    /// The clip's (or the background's) passive picture effects.
    /// A bounded text effect's range (right-click its name to bound it): the unit, and
    /// where it starts and ends and its blend — keyframable, so a highlight can sweep
    /// across a title. Switching units keeps the range where it was.
    fn bounds_settings(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &oa_doc::EffectInstance, t: Time) {
        let Some(it) = self.editor.item(item) else { return };
        let v = fx.params.eval(schema::bounds(), None, &it.eval_context(t));
        if schema::bounds_of(&v).is_none() {
            return;
        }
        // Letters as the title draws them (spaces aren't letters).
        let text = it.params.get(schema::TEXT_CONTENT).map(|s| s.eval(&it.eval_context(t)));
        let count = match &text {
            Some(Value::Text(s)) => s.chars().filter(|c| !c.is_whitespace()).count().max(1),
            _ => 1,
        } as f64;
        let letters = v.get(schema::BOUND_UNIT).and_then(Value::as_enum) == Some(schema::BOUND_UNITS[1]);
        let target = ParamTarget::Effect(fx.id);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Bounded").strong()).on_hover_text("Only these letters; right-click the effect's name to turn it off");
            for (unit, label) in [(false, "Percent"), (true, "Letters")] {
                if ui.selectable_label(letters == unit, label).clicked() && letters != unit {
                    // The same range in the other unit.
                    let (start, end, blend) = (v.float(schema::BOUND_START), v.float(schema::BOUND_END), v.float(schema::BOUND_BLEND));
                    let (start, end, blend) = if unit {
                        ((start / 100.0 * count).floor() + 1.0, (end / 100.0 * count).ceil(), (blend / 100.0 * count).round())
                    } else {
                        ((start - 1.0) / count * 100.0, end / count * 100.0, blend / count * 100.0)
                    };
                    let unit_name = schema::BOUND_UNITS[unit as usize];
                    let ops = [
                        (schema::BOUND_UNIT, Value::Enum(unit_name.into())),
                        (schema::BOUND_START, Value::Float(start.max(if unit { 1.0 } else { 0.0 }))),
                        (schema::BOUND_END, Value::Float(end)),
                        (schema::BOUND_BLEND, Value::Float(blend.max(0.0))),
                    ];
                    for (id, value) in ops {
                        self.editor.set_param(item, target.clone(), id, oa_params::ParamSource::Static(value), "bounded-unit");
                    }
                }
            }
            if letters {
                ui.label(egui::RichText::new(format!("of {count}")).weak());
            }
        });
        // In letters, the sliders run over the title's letters.
        for id in [schema::BOUND_START, schema::BOUND_END, schema::BOUND_BLEND] {
            let Some(mut s) = schema::bounds().iter().find(|s| s.id.as_str() == id).cloned() else { continue };
            if letters {
                s.range = Some(if id == schema::BOUND_BLEND { (0.0, count / 2.0) } else { (1.0, count) });
            }
            self.param_widget(ui, item, &target, &s, t, &fx.id.0.to_string());
        }
    }

    fn effects_section(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        self.effect_list(ui, item, List::Effects, t);
    }

    /// One card per effect in chain order — the clip's passive picture effects, its sound
    /// effects (any role), or its intros or outros (which all play together, each over
    /// its own duration). Each card: a grip to drag it (up or down the list to reorder,
    /// out onto another clip in the timeline or viewer to copy it there), on/off, its
    /// name (right-click to copy or paste), an intro's or outro's duration, **Apply to
    /// All Selected** when other clips are selected, remove, then its settings. "+ Add…"
    /// adds to every selected clip the effect fits.
    fn effect_list(&mut self, ui: &mut egui::Ui, item: ItemId, list: List, t: Time) {
        use oa_doc::{EffectRole, Op};
        let Some(it) = self.editor.item(item) else { return };
        let length = it.range.duration;
        let is_text = it.kind == ItemKind::Text;
        let sound = list == List::Sound;
        let in_out = matches!(list, List::Intro | List::Outro);
        let reversing = list == List::Outro && it.outro_reverses_intro;
        let registry = &self.registry;
        let in_list = |e: &oa_doc::EffectInstance| {
            let heard = registry.is_sound(&e.type_id);
            match list {
                List::Effects => e.role == EffectRole::Passive && !heard,
                List::Sound => heard,
                List::Intro => matches!(e.role, EffectRole::In { .. }) && !heard,
                List::Outro => matches!(e.role, EffectRole::Out { .. }) && !heard,
            }
        };
        let effects: Vec<(usize, oa_doc::EffectInstance)> = it.effects.iter().cloned().enumerate().filter(|(_, e)| in_list(e)).filter(|_| !reversing).collect();
        // With several clips selected, the cards are every effect any of them has — this
        // clip's first, then the others' it doesn't — each saying how many have it, and
        // anything done to one is done to all of them (`Editor::fanned`).
        let peers: Vec<ItemId> = self.editor.linked.iter().copied().filter(|l| *l != item).collect();
        let mut cards: Vec<(ItemId, oa_doc::EffectInstance)> = effects.iter().map(|(_, e)| (item, e.clone())).collect();
        if !reversing {
            for &other in &peers {
                let Some(o) = self.editor.item(other) else { continue };
                for fx in o.effects.iter().filter(|e| in_list(e)) {
                    let key = self.editor.effect_key(other, fx.id);
                    if !cards.iter().any(|(owner, e)| self.editor.effect_key(*owner, e.id) == key) {
                        cards.push((other, fx.clone()));
                    }
                }
            }
        }
        let total = peers.len() + 1;
        let seq = self.editor.seq;
        // The other selected clips (the background is never part of a selection).
        let others: Vec<ItemId> = if item == oa_doc::BACKGROUND { Vec::new() } else { self.selected_clips().into_iter().filter(|i| *i != item).collect() };
        let mut ops: Option<(&str, Vec<Op>)> = None;
        let mut apply_all: Option<oa_doc::EffectInstance> = None;
        if list == List::Outro {
            // "Reverse": the outro is the intro played backwards; own outros are off then.
            ui.horizontal(|ui| {
                let tip = "Play the intro backwards as the outro (turns off the outro's own effects)";
                if ui.selectable_label(reversing, "Reverse").on_hover_text(tip).clicked() {
                    ops = Some(("Reverse intro", vec![Op::SetReverseIntro { seq, item, on: !reversing }]));
                }
                if reversing {
                    ui.label(egui::RichText::new("the intro, played backwards").weak());
                }
            });
        }
        if cards.is_empty() && !reversing {
            let none = match list {
                List::Effects => "No effects yet.",
                List::Sound => "No sound effects yet.",
                List::Intro => "No intro yet.",
                List::Outro => "No outro yet.",
            };
            ui.label(egui::RichText::new(none).weak());
        } else if !cards.is_empty() {
            let order = if in_out { "They play together. Drag ⋮⋮ to reorder, or onto another clip to copy." } else { "Applied top to bottom. Drag ⋮⋮ to reorder, or onto another clip to copy." };
            let hint = if cards.len() > 1 { order } else { "Drag ⋮⋮ onto another clip to copy it there." };
            ui.label(egui::RichText::new(hint).small().weak());
        }
        // Dragging: the effect being dragged, and where each card sits.
        let drag_key = egui::Id::new(("effect-drag", item.0, list));
        let dragging: Option<u64> = ui.data(|d| d.get_temp(drag_key));
        let mut rects: Vec<egui::Rect> = Vec::new();
        let primary = item;
        for (owner, fx) in cards.iter() {
            // A card is its owner's effect (this clip's, or another selected clip's).
            let item = *owner;
            let have = 1 + self.editor.effect_peers(item, fx.id).len();
            let d = self.registry.effect(&fx.type_id).cloned();
            let sound_info = if sound { oa_audio::fx::info(&fx.type_id) } else { None };
            let fits: Vec<ItemId> = others.iter().copied().filter(|o| self.effect_fits(*o, &fx.type_id)).collect();
            let resp = ui.push_id(("effect", fx.id.0), |ui| {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        // The grip: two columns of dots.
                        let (grip, gr) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::drag());
                        let dot = if gr.hovered() || dragging == Some(fx.id.0) { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                        for y in [-5.0, 0.0, 5.0] {
                            for dx in [-2.5, 2.5] {
                                ui.painter().circle_filled(grip.center() + egui::vec2(dx, y), 1.4, dot);
                            }
                        }
                        let gr = gr.on_hover_text("Drag to reorder, or onto another clip to copy it there").on_hover_cursor(egui::CursorIcon::Grab);
                        if gr.drag_started() {
                            ui.data_mut(|d| d.insert_temp(drag_key, fx.id.0));
                            self.effect_drag = Some(EffectDrag { from: item, effect: fx.clone() });
                        }
                        let mut on = fx.enabled;
                        if ui.checkbox(&mut on, "").on_hover_text(if on { "Turn off" } else { "Turn on" }).changed() {
                            ops = Some(("Toggle effect", vec![Op::SetEffectEnabled { seq, item, effect: fx.id, enabled: on }]));
                        }
                        let name = d.as_ref().map_or(fx.type_id.as_str(), |d| d.name.as_str());
                        let color = if fx.enabled { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                        let r = ui.add(egui::Label::new(egui::RichText::new(name).strong().color(color)).sense(egui::Sense::click()));
                        if have < total {
                            ui.label(egui::RichText::new(format!("on {have} of {total}")).small().weak()).on_hover_text("Only some of the selected clips have it; changes go to those");
                        }
                        let about = sound_info.as_ref().map(|i| i.description.clone()).or_else(|| d.as_ref().map(|d| d.description.clone())).unwrap_or_default();
                        let tip = if about.is_empty() { String::new() } else { format!("{about}\n") };
                        let more = if is_text && self.can_bound(item, &fx.type_id) { ", or to bound it to some of the letters" } else { "" };
                        let r = r.on_hover_text(format!("{tip}Right-click to copy or paste{more}"));
                        crate::widgets::context_menu(&r, |ui| self.effect_menu(ui, item, fx));
                        if in_out {
                            // How long it plays, over the clip's start or end.
                            let mut seconds = match fx.role {
                                EffectRole::In { duration } | EffectRole::Out { duration } => duration.as_seconds_f64(),
                                _ => 0.5,
                            };
                            ui.label("over");
                            let r = ui.add(egui::DragValue::new(&mut seconds).speed(0.02).range(0.05..=length.as_seconds_f64().max(0.05)).suffix(" s"));
                            if r.changed() {
                                let duration = Time::from_seconds_f64(seconds);
                                let role = if list == List::Intro { EffectRole::In { duration } } else { EffectRole::Out { duration } };
                                if let Err(e) = self.editor.apply_drag("Effect duration", "in-out-duration", self.editor.fanned(vec![Op::SetEffectRole { seq, item, effect: fx.id, role }])) {
                                    self.error = Some(e.to_string());
                                }
                            }
                            self.seal_on_release(&r);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✕").on_hover_text("Remove this effect").clicked() {
                                ops = Some(("Remove effect", vec![Op::RemoveEffect { seq, item, effect: fx.id }]));
                            }
                            if !fits.is_empty() {
                                let n = fits.len();
                                let tip = format!("Put this effect, with these settings, on the other {n} selected clip{}", if n == 1 { "" } else { "s" });
                                if ui.small_button("Apply to All Selected").on_hover_text(tip).clicked() {
                                    apply_all = Some(fx.clone());
                                }
                            }
                        });
                    });
                    if sound {
                        // When it plays: over the whole clip, or as its intro or outro —
                        // the same roles picture effects have.
                        let (mut kind, mut seconds) = match fx.role {
                            EffectRole::Passive => (0, 1.0),
                            EffectRole::In { duration } => (1, duration.as_seconds_f64()),
                            EffectRole::Out { duration } | EffectRole::Reversed { duration } => (2, duration.as_seconds_f64()),
                        };
                        let was = kind;
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            for (k, label, tip) in [(0, "Whole clip", "Plays for the whole clip"), (1, "Intro", "Plays over the clip's first seconds"), (2, "Outro", "Plays over the clip's last seconds")] {
                                if ui.selectable_label(kind == k, label).on_hover_text(tip).clicked() {
                                    kind = k;
                                }
                            }
                            if kind != 0 {
                                let r = ui.add(egui::DragValue::new(&mut seconds).speed(0.02).range(0.05..=length.as_seconds_f64().max(0.05)).suffix(" s"));
                                if r.changed() {
                                    let duration = Time::from_seconds_f64(seconds);
                                    let role = if kind == 1 { EffectRole::In { duration } } else { EffectRole::Out { duration } };
                                    if let Err(e) = self.editor.apply_drag("Effect duration", "sound-fx-duration", self.editor.fanned(vec![Op::SetEffectRole { seq, item, effect: fx.id, role }])) {
                                        self.error = Some(e.to_string());
                                    }
                                }
                                self.seal_on_release(&r);
                            }
                        });
                        if kind != was {
                            let duration = Time::from_seconds_f64(seconds.min(length.as_seconds_f64()).max(0.05));
                            let role = match kind {
                                0 => EffectRole::Passive,
                                1 => EffectRole::In { duration },
                                _ => EffectRole::Out { duration },
                            };
                            ops = Some(("Effect timing", vec![Op::SetEffectRole { seq, item, effect: fx.id, role }]));
                        }
                    }
                    match &d {
                        Some(_) if sound && sound_info.is_none() => {
                            ui.label(egui::RichText::new("Its sound shader didn't compile; it's skipped (see Home → Plugins).").small().weak());
                        }
                        Some(d) if !sound && !d.renders() => {
                            ui.label(egui::RichText::new("Can't be rendered by this version yet; it's skipped.").small().weak());
                        }
                        Some(d) if d.editor.as_deref() == Some(oa_graph::registry::EDITOR_SURFACE) => self.surface_settings(ui, item, fx, d, t),
                        Some(d) if d.editor.as_deref() == Some(oa_graph::registry::EDITOR_EQUALIZER) => {
                            self.eq_settings(ui, item, fx, d, t);
                        }
                        Some(d) => {
                            let target = ParamTarget::Effect(fx.id);
                            for schema in &d.params {
                                self.param_widget(ui, item, &target, schema, t, &fx.id.0.to_string());
                            }
                            if sound {
                                self.sound_meter(ui, fx);
                            }
                            if is_text && self.can_bound(item, &fx.type_id) {
                                self.bounds_settings(ui, item, fx, t);
                            }
                        }
                        None => {
                            ui.label(egui::RichText::new("This effect's plugin is off or isn't installed; its settings are kept.").small().weak());
                        }
                    }
                });
            });
            // Only this clip's own cards are reordered here.
            if item == primary {
                rects.push(resp.response.rect);
            }
            // The card being dragged is dimmed where it was.
            if dragging == Some(fx.id.0) {
                ui.painter().rect_filled(resp.response.rect, 4.0, ui.visuals().panel_fill.gamma_multiply(0.6));
            }
        }
        // While dragging: inside the list, a line where it would land (let go: it moves
        // there); outside it, a label that follows the pointer (let go over a clip: it's
        // copied there — the timeline and viewer take the drop).
        if let Some(moving) = dragging {
            let from = effects.iter().position(|(_, e)| e.id.0 == moving);
            let pointer = ui.input(|i| i.pointer.latest_pos());
            let area = rects.iter().copied().reduce(|a, b| a.union(b)).map(|r| r.expand2(egui::vec2(40.0, 24.0)));
            let inside = matches!((area, pointer), (Some(a), Some(p)) if a.contains(p));
            // The slot: how many cards' middles are above the pointer.
            let slot = pointer.map_or(0, |p| rects.iter().filter(|r| r.center().y < p.y).count());
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if inside
                && let (Some(from), Some(first), Some(last)) = (from, rects.first(), rects.last())
                && slot != from
                && slot != from + 1
            {
                let y = if slot == 0 { first.top() - 2.0 } else if slot >= rects.len() { last.bottom() + 2.0 } else { (rects[slot - 1].bottom() + rects[slot].top()) / 2.0 };
                let (x0, x1) = (first.left(), first.right());
                ui.painter().line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], egui::Stroke::new(3.0, crate::style::ACCENT));
                ui.painter().circle_filled(egui::pos2(x0, y), 4.0, crate::style::ACCENT);
            }
            if !inside
                && let (Some(p), Some(from)) = (pointer, from)
            {
                let name = self.effect_name(&effects[from].1.type_id);
                let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, drag_key));
                let galley = painter.layout_no_wrap(format!("⧉ {name} — drop on a clip to copy"), egui::FontId::proportional(12.0), egui::Color32::WHITE);
                let at = p + egui::vec2(14.0, 10.0);
                let bg = egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(8.0, 4.0));
                painter.rect_filled(bg, 4.0, crate::style::ACCENT.gamma_multiply(0.9));
                painter.galley(at, galley, egui::Color32::WHITE);
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                ui.data_mut(|d| d.remove::<u64>(drag_key));
                // Let go outside the list: onto the clip under the pointer, if any.
                if let Some(drag) = self.effect_drag.take()
                    && !inside
                    && let Some(p) = pointer
                {
                    let lane = self.fx_lanes.iter().find(|l| l.0.contains(p)).copied();
                    match self.clip_on_screen(p) {
                        Some(onto) => self.drop_effect(&drag, onto),
                        // On an empty stretch of an effect track: a container starts there,
                        // holding the effect.
                        None if lane.is_some() => {
                            let (rect, track, from, per_point) = lane.expect("checked");
                            let at = Time::from_seconds_f64((from + (p.x - rect.left()) as f64 * per_point).max(0.0));
                            self.drop_effect_on_track(&drag, track, at);
                        }
                        None if !ui.max_rect().contains(p) => {
                            let name = self.effect_name(&drag.effect.type_id);
                            self.notify(format!("Drop {name} on a clip in the timeline or the viewer to copy it there."));
                        }
                        None => {}
                    }
                }
                if inside
                    && let Some(from) = from
                    && slot != from
                    && slot != from + 1
                {
                    // Where it goes among all the clip's effects (the other lists sit in
                    // the same one): before the card at `slot`, or just after the last one
                    // — counted after taking it out.
                    let positions: Vec<usize> = effects.iter().map(|(i, _)| *i).collect();
                    if let Some(at) = move_target(&positions, from, slot) {
                        let fx = effects[from].1.clone();
                        ops = Some(("Reorder effects", vec![Op::RemoveEffect { seq, item, effect: fx.id }, Op::InsertEffect { seq, item, index: at, effect: fx }]));
                    }
                }
            }
        }

        // Add: to this clip and every other selected clip it fits.
        let targets: Vec<ItemId> = std::iter::once(item).chain(others.iter().copied()).collect();
        let plural = |what: &str| if others.is_empty() { format!("+ Add {what}") } else { format!("+ Add {what} to {} clips", targets.len()) };
        let mut add: Option<String> = None;
        match list {
            List::Sound => {
                let offered: Vec<(String, String, String)> = self
                    .plugins
                    .sounds(&self.registry)
                    .iter()
                    .map(|d| {
                        let about = oa_audio::fx::info(&d.type_id).map(|i| i.description.clone()).unwrap_or_default();
                        (d.type_id.to_string(), d.name.clone(), about)
                    })
                    .collect();
                if offered.is_empty() {
                    ui.label(egui::RichText::new("No sound effects: Atelier Core is off (Home → Plugins).").small().weak());
                } else {
                    crate::widgets::sticky_menu(ui, &plural("sound effect"), |ui| {
                        for (type_id, name, about) in &offered {
                            if ui.button(name).on_hover_text(about).clicked() {
                                add = Some(type_id.clone());
                                ui.close();
                            }
                        }
                    });
                }
            }
            _ => {
                let taken: Vec<String> = effects.iter().map(|(_, e)| e.type_id.to_string()).collect();
                let (label, kind, usage) = match list {
                    List::Intro => (plural("intro"), ThumbKind::Intro, oa_graph::registry::EffectUsage::InOut),
                    List::Outro => (plural("outro"), ThumbKind::Outro, oa_graph::registry::EffectUsage::InOut),
                    _ => (plural("effect"), ThumbKind::Passive, oa_graph::registry::EffectUsage::Passive),
                };
                ui.add_enabled_ui(!reversing, |ui| {
                    crate::widgets::sticky_menu(ui, &label, |ui| {
                        let used = |id: &str| taken.iter().any(|t| t == id);
                        if let Some(id) = self.effect_picker(ui, item, kind, usage, is_text, &used) {
                            add = Some(id);
                            ui.close();
                        }
                    })
                });
            }
        }
        if let Some(type_id) = add {
            // How long a new intro/outro plays: as long as the last one, else half a
            // second; sound fades and the like start out as an intro of a second.
            let last = effects.last().and_then(|(_, c)| match c.role {
                EffectRole::In { duration } | EffectRole::Out { duration } => Some(duration),
                _ => None,
            });
            let sound_in_out = sound && self.registry.effect(&type_id).is_some_and(|d| d.usage == oa_graph::registry::EffectUsage::InOut);
            let fitting: Vec<ItemId> = targets.iter().copied().filter(|t| self.effect_fits(*t, &type_id)).collect();
            let n = fitting.len();
            let mut changes = Vec::new();
            for target in fitting {
                let Some(it) = self.editor.item(target).cloned() else { continue };
                let mut effect = oa_doc::EffectInstance::new(oa_doc::EffectId(self.editor.doc.alloc_id()), &type_id);
                effect.role = match list {
                    List::Intro => EffectRole::In { duration: last.unwrap_or(Time::from_seconds_f64(0.5)).min(it.range.duration) },
                    List::Outro => EffectRole::Out { duration: last.unwrap_or(Time::from_seconds_f64(0.5)).min(it.range.duration) },
                    List::Sound if sound_in_out => EffectRole::In { duration: Time::from_seconds_f64(1.0).min(it.range.duration) },
                    _ => EffectRole::Passive,
                };
                // Intros/outros go after the clip's others, before its passive effects (so
                // those see the clip as it arrives); everything else at the end.
                let index = if effect.role == EffectRole::Passive { it.effects.len() } else { it.effects.iter().take_while(|e| e.role != EffectRole::Passive).count() };
                changes.push(Op::InsertEffect { seq, item: target, index, effect });
            }
            if n > 1 {
                let name = self.effect_name(&type_id);
                self.notify(format!("Added {name} to {n} clips."));
            }
            ops = Some(("Add effect", changes));
        }
        if let Some(fx) = apply_all {
            let name = self.effect_name(&fx.type_id);
            let n = self.apply_effect_to(&fx, &others, "Apply to all selected");
            self.notify(if n == 0 { format!("The selected clips already have {name} like this.") } else { format!("Applied {name} to {n} more clip{}.", if n == 1 { "" } else { "s" }) });
        }
        if let Some((label, ops)) = ops
            && let Err(e) = self.editor.apply(label, self.editor.fanned(ops))
        {
            self.error = Some(e.to_string());
        }
    }

    /// Playback speed: the clip plays the same part of its file faster or slower, so its
    /// length on the timeline changes. Pitch is kept unless "keep pitch" is off.
    fn speed_row(&mut self, ui: &mut egui::Ui, item: ItemId) {
        let Some(it) = self.editor.item(item) else { return };
        let old = it.time_map.speed.num() as f64 / it.time_map.speed.den() as f64;
        let keep = !matches!(it.params.get(schema::AUDIO_KEEP_PITCH).map(|s| s.eval(&it.eval_context(it.range.start))), Some(Value::Bool(false)));
        let mut speed = old;
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            let r = ui
                .add(egui::DragValue::new(&mut speed).speed(0.01).range(0.05..=20.0).max_decimals(3).suffix("×"))
                .on_hover_text("Playback speed, picture and sound together (0.05× to 20×). The clip gets shorter or longer on the timeline.");
            ui.label("speed");
            for preset in [0.25, 0.5, 1.0, 2.0, 4.0] {
                if ui.small_button(format!("{preset}×")).clicked() {
                    speed = preset;
                }
            }
            if r.drag_stopped() || r.lost_focus() {
                self.editor.doc.seal();
            }
            let mut k = keep;
            if ui.checkbox(&mut k, "keep pitch").on_hover_text("Off: the pitch rises and falls with the speed, like tape").changed() {
                self.editor.set_param(item, ParamTarget::Item, schema::AUDIO_KEEP_PITCH, ParamSource::Static(Value::Bool(k)), "keep-pitch");
                self.editor.doc.seal();
            }
        });
        if (speed - old).abs() < 1e-9 || speed <= 0.0 || old <= 0.0 {
            return;
        }
        // Each selected clip: the same part of its file, played at the new speed.
        let mut ops = Vec::new();
        for id in std::iter::once(item).chain(self.editor.linked.iter().copied().filter(|l| *l != item)) {
            let Some(it) = self.editor.item(id) else { continue };
            let was = it.time_map.speed.num() as f64 / it.time_map.speed.den().max(1) as f64;
            if was <= 0.0 {
                continue;
            }
            let span = it.range.duration.as_seconds_f64() * was;
            let duration = Time::from_seconds_f64(span / speed).max(Time(1));
            let time_map = oa_doc::TimeMap { source_in: it.time_map.source_in, speed: oa_time::Rational::new((speed * 1000.0).round() as i64, 1000) };
            ops.push(oa_doc::Op::SetItemTiming { seq: self.editor.seq, item: id, range: oa_time::TimeRange::new(it.range.start, duration), time_map });
        }
        if let Err(e) = self.editor.apply_drag("Clip speed", "clip-speed", ops) {
            self.error = Some(format!("can't change the speed: {e} (make room after the clip first)"));
        }
    }

    /// The clip's sound effects, in the same cards as its picture effects.
    fn sound_effects_section(&mut self, ui: &mut egui::Ui, item: ItemId, t: Time) {
        self.effect_list(ui, item, List::Sound, t);
    }

    pub(crate) fn canvas_aspect(&self) -> f32 {
        let s = self.editor.sequence();
        let c = s.variants[self.variant.min(s.variants.len() - 1)].size;
        c.width as f32 / c.height.max(1) as f32
    }

    pub(crate) fn is_visual(&self, item: ItemId) -> bool {
        match self.editor.item(item).map(|i| &i.kind) {
            Some(ItemKind::Media { media }) => self.editor.pool_item(*media).is_none_or(|p| p.probe.video.is_some()),
            Some(_) => true,
            None => false,
        }
    }

    pub(crate) fn is_audible(&self, item: ItemId) -> bool {
        match self.editor.item(item).map(|i| &i.kind) {
            Some(ItemKind::Media { media }) => {
                self.editor.pool_item(*media).is_some_and(|p| p.probe.has_audio()) && self.editor.item(item).is_some_and(|i| i.audio_enabled())
            }
            _ => false,
        }
    }

    fn apply_or_report(&mut self, label: &str, ops: Vec<oa_doc::Op>) {
        if let Err(e) = self.editor.apply(label, ops) {
            self.error = Some(e.to_string());
        }
    }

    fn seal_on_release(&mut self, r: &egui::Response) {
        if r.drag_stopped() || r.lost_focus() {
            self.editor.doc.seal();
        }
    }

    /// Writes a transform value the same way a viewer drag does (keyframe- and
    /// format-aware).
    fn write_transform(&mut self, item: ItemId, t: Time, param: &str, value: Value) {
        let scope = self.transform_scope();
        let (project, seq, variant) = (self.editor.doc.snapshot(), self.editor.seq, self.variant_id());
        // The rest of a multiple selection follows: the same value, except the position,
        // which moves them all by the same amount (the same value would pile them up).
        let position_now = |id: ItemId| {
            transform::placement_of(&project, seq, variant, id, t)
                .ok()
                .map(|p| p.values.vec2(schema::POSITION))
                .or_else(|| self.editor.param_value(id, &ParamTarget::Item, schema::POSITION, t).and_then(|v| v.as_vec2()))
                .unwrap_or([0.0; 2])
        };
        let from = position_now(item);
        let mut ops = Vec::new();
        for id in std::iter::once(item).chain(self.editor.linked.iter().copied().filter(|l| *l != item && self.is_visual(*l))) {
            let v = match value.as_vec2() {
                Some(new) if param == schema::POSITION && id != item => {
                    let there = position_now(id);
                    Value::Vec2([there[0] + new[0] - from[0], there[1] + new[1] - from[1]])
                }
                _ => value.clone(),
            };
            match transform::write_param(&project, seq, id, variant, scope, t, param, v) {
                Ok(op) => ops.push(op),
                Err(e) if id == item => {
                    self.error = Some(e.to_string());
                    return;
                }
                Err(_) => {}
            }
        }
        if let Err(e) = self.editor.apply_drag("Transform", param, ops) {
            self.error = Some(e.to_string());
        }
    }

    /// A diamond: hollow gray (not keyframed), filled (a key at the playhead), hollow
    /// gold with a dot (keyframed, no key here).
    fn key_toggle(&mut self, ui: &mut egui::Ui, item: ItemId, param: &str, default: Value, t: Time) {
        self.key_toggle_for(ui, item, ParamTarget::Item, param, default, t);
    }

    fn key_toggle_for(&mut self, ui: &mut egui::Ui, item: ItemId, target: ParamTarget, param: &str, default: Value, t: Time) {
        let Some(it) = self.editor.item(item) else { return };
        let keys = self.editor.keyframe_times(item, &target, param);
        let local = t - it.range.start;
        let (state, tip) = match keys.len() {
            0 => (0, "Keyframe this property (adds a key at the playhead)"),
            _ if keys.contains(&local) => (2, "Stop keyframing (keeps the current value)"),
            _ => (1, "Keyframed: change the value to add a key here; click to stop keyframing"),
        };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
        let gold = crate::style::GOLD;
        let color = if state == 0 { ui.visuals().weak_text_color() } else { gold };
        let c = rect.center();
        let d = if response.hovered() { 5.5 } else { 4.5 };
        let diamond = vec![c + egui::vec2(0.0, -d), c + egui::vec2(d, 0.0), c + egui::vec2(0.0, d), c + egui::vec2(-d, 0.0)];
        let fill = if state == 2 { gold } else { egui::Color32::TRANSPARENT };
        ui.painter().add(egui::Shape::convex_polygon(diamond, fill, egui::Stroke::new(1.2, color)));
        if state == 1 {
            ui.painter().circle_filled(c, 1.5, gold);
        }
        // A little curve in the corner: some key eases differently from the default
        // curve (it was shaped in the curve editor).
        let custom = self
            .editor
            .param_source(item, &target, param)
            .and_then(|s| s.curve().cloned())
            .is_some_and(|c| {
                // Linear (older keys) and the default curve are plain; anything shaped is custom.
                let plain = |i: oa_params::Interp| i == oa_params::Interp::Linear || i == oa_params::default_interp();
                c.keys.len() > 1 && c.keys[..c.keys.len() - 1].iter().any(|k| !plain(k.interp))
            });
        let tip = if custom { format!("{tip}\nCustom easing (right-click the value → Edit curve…)") } else { tip.to_string() };
        if custom {
            let o = rect.right_bottom() + egui::vec2(-1.0, -1.0);
            let pts: Vec<egui::Pos2> = (0..=6)
                .map(|i| {
                    let u = i as f32 / 6.0;
                    let y = u * u * (3.0 - 2.0 * u);
                    o + egui::vec2(-6.0 + 6.0 * u, -6.0 * y)
                })
                .collect();
            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.3, crate::style::ACCENT)));
        }
        // A little wave in the other corner: it swings on its own (Edit wave…).
        let waving = self.editor.param_source(item, &target, param).is_some_and(|s| s.find_lfo().is_some());
        let tip = if waving { format!("{tip}\nHas a wave (right-click the value → Edit wave…)") } else { tip };
        if waving {
            let o = rect.left_bottom() + egui::vec2(1.0, -3.0);
            let pts: Vec<egui::Pos2> = (0..=8).map(|i| o + egui::vec2(i as f32 * 0.8, -2.0 * (i as f32 * std::f32::consts::TAU / 8.0).sin())).collect();
            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.2, crate::style::ACCENT)));
        }
        // A crosshair in the top corner: it uses a track — gold following it, green
        // stabilized by it (Edit track…).
        let source = self.editor.param_source(item, &target, param);
        let tracked = source.as_ref().and_then(|s| Some((s.find_track()?.0.name.clone(), matches!(s.track_use(), Some(oa_params::TrackUse::Stabilize(_))))));
        let tip = match &tracked {
            Some((name, true)) => format!("{tip}\nStabilized with the track “{name}” (right-click the value → Animate → Edit track…)"),
            Some((name, false)) => format!("{tip}\nFollows the track “{name}” (right-click the value → Animate → Edit track…)"),
            None => tip,
        };
        if let Some((_, stabilized)) = &tracked {
            let color = if *stabilized { egui::Color32::from_rgb(80, 220, 140) } else { gold };
            let o = rect.right_top() + egui::vec2(-3.5, 3.5);
            let stroke = egui::Stroke::new(1.2, color);
            ui.painter().circle_stroke(o, 2.2, stroke);
            for d in [egui::vec2(3.5, 0.0), egui::vec2(0.0, 3.5)] {
                ui.painter().line_segment([o - d, o - d * 0.55], stroke);
                ui.painter().line_segment([o + d * 0.55, o + d], stroke);
            }
        }
        // Little sound bars in the other top corner: it follows the sound (Edit connection offset…).
        let listening = source.as_ref().is_some_and(|s| s.follows_sound());
        let tip = if listening { format!("{tip}\nConnected to the sound (right-click the value → Animate → Edit connection offset…)") } else { tip };
        if listening {
            let o = rect.left_top() + egui::vec2(1.5, 6.5);
            for (i, h) in [2.5f32, 5.0, 3.5].into_iter().enumerate() {
                let x = o.x + i as f32 * 2.0;
                ui.painter().line_segment([egui::pos2(x, o.y), egui::pos2(x, o.y - h)], egui::Stroke::new(1.2, crate::style::ACCENT));
            }
        }
        if response.on_hover_text(tip).clicked() {
            let anchor = if param == schema::FOCUS { KeyframeAnchor::SourceMedia } else { KeyframeAnchor::ClipStart };
            if let Err(e) = self.editor.toggle_keyframing(item, target, param, default, anchor, t) {
                self.error = Some(e.to_string());
            }
        }
    }

    /// What's behind the clips: a color or gradient, the picture blurred to cover the
    /// canvas, or a tiled texture. Sequence-wide (every format).
    pub(crate) fn background_section(&mut self, ui: &mut egui::Ui) {
        let t = self.playhead;
        let bg = self.editor.sequence_values(t);
        let mode = match bg.get(schema::BG_MODE) {
            Some(Value::Enum(m)) => m.clone(),
            _ => "solid".into(),
        };
        let mut chosen = mode.clone();
        let label = |m: &str| match m {
            "blur" => "Blurred content",
            "texture" => "Texture",
            _ => "Solid color",
        };
        egui::ComboBox::from_id_salt("background-mode").selected_text(label(&mode)).show_ui(ui, |ui| {
            for m in ["solid", "blur", "texture"] {
                ui.selectable_value(&mut chosen, m.to_string(), label(m));
            }
        });
        if chosen != mode {
            self.editor.set_sequence_value(schema::BG_MODE, Value::Enum(chosen.clone()), t, "background-mode");
            self.editor.doc.seal();
        }
        let slider = |app: &mut Self, ui: &mut egui::Ui, param: &str, text: &str, range: std::ops::RangeInclusive<f64>, factor: f64, suffix: &str| {
            let mut v = bg.float(param) * factor;
            let r = ui.add(egui::Slider::new(&mut v, range).text(text).suffix(suffix).clamping(egui::SliderClamping::Edits));
            if r.changed() {
                app.editor.set_sequence_value(param, Value::Float(v / factor), t, param);
            }
            if r.drag_stopped() || r.lost_focus() {
                app.editor.doc.seal();
            }
        };
        match chosen.as_str() {
            "blur" => {
                ui.label(egui::RichText::new("The clips on screen, enlarged to fill the frame and blurred.").small().weak());
                slider(self, ui, schema::BG_BLUR, "blur", 0.0..=300.0, 1.0, " px");
                slider(self, ui, schema::BG_DIM, "darken", 0.0..=100.0, 100.0, " %");
            }
            "texture" => {
                let options: Vec<(u64, String)> = self
                    .editor
                    .pool
                    .iter()
                    .filter(|m| !m.missing && m.probe.video.is_some())
                    .map(|m| (m.id.0, m.name.clone()))
                    .chain(self.editor.compounds().into_iter().map(|s| (s.id.0, format!("▣ {}", s.name))))
                    .collect();
                let current = bg.get(schema::BG_TEXTURE).and_then(|v| v.as_media());
                let mut picked = current;
                let name = |m: Option<u64>| m.and_then(|m| options.iter().find(|o| o.0 == m)).map_or("none — pick a picture".to_string(), |o| o.1.clone());
                egui::ComboBox::from_id_salt("background-texture").selected_text(name(current)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut picked, None, "none");
                    for (m, n) in &options {
                        ui.selectable_value(&mut picked, Some(*m), n);
                    }
                });
                if picked != current {
                    self.editor.set_sequence_value(schema::BG_TEXTURE, Value::Media(picked), t, "background-texture");
                    self.editor.doc.seal();
                }
                slider(self, ui, schema::BG_TILE, "tile size", 2.0..=200.0, 100.0, " %");
            }
            _ => {
                // One color by default; right-click for a directional gradient.
                let mut g = bg.get(schema::BG_COLOR).and_then(|v| v.as_gradient()).cloned().unwrap_or_else(|| oa_params::Gradient::solid([0.0, 0.0, 0.0, 1.0]));
                let key = ui.id().with("background-gradient-advanced");
                let mut advanced = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false) || g.stops.len() > 1;
                let mut write: Option<(oa_params::Gradient, bool)> = None;
                ui.horizontal(|ui| {
                    let r = if advanced {
                        ui.add(egui::Label::new("color").sense(egui::Sense::click())).on_hover_text("Right-click for a single color")
                    } else {
                        let mut c = g.sorted_stops()[0].color;
                        let (r, changed) = crate::widgets::color_swatch(ui, &mut c);
                        if changed {
                            write = Some((oa_params::Gradient { angle: g.angle, stops: vec![oa_params::GradientStop { pos: 0.0, color: c }] }, false));
                        }
                        ui.label("color");
                        r.on_hover_text("Click to pick · right-click for a gradient")
                    };
                    crate::widgets::context_menu(&r, |ui| {
                        if !advanced && ui.button("Advanced: directional gradient").clicked() {
                            advanced = true;
                            ui.close();
                        }
                        if advanced && ui.button("Simple: one color (keeps the first)").clicked() {
                            advanced = false;
                            let first = g.sorted_stops()[0].clone();
                            write = Some((oa_params::Gradient { angle: g.angle, stops: vec![oa_params::GradientStop { pos: 0.0, ..first }] }, true));
                            ui.close();
                        }
                    });
                });
                if advanced && write.is_none() {
                    let (changed, finished) = crate::widgets::gradient(ui, ui.id().with("background-gradient"), &mut g);
                    if changed {
                        write = Some((g.clone(), finished));
                    } else if finished {
                        self.editor.doc.seal();
                    }
                }
                ui.data_mut(|d| d.insert_temp(key, advanced));
                if let Some((g, seal)) = write {
                    self.editor.set_sequence_value(schema::BG_COLOR, Value::Gradient(g), t, "background-color");
                    if seal {
                        self.editor.doc.seal();
                    }
                }
            }
        }
    }

    fn slider(&mut self, ui: &mut egui::Ui, item: ItemId, prop: Prop<'_>, range: std::ops::RangeInclusive<f64>, t: Time) {
        let default = prop.default.as_float().unwrap_or(0.0);
        let (lo, hi) = (*range.start(), *range.end());
        ui.horizontal(|ui| {
            self.key_toggle_for(ui, item, prop.target.clone(), prop.param, prop.default.clone(), t);
            let mut value = self.editor.param_value(item, &prop.target, prop.param, t).and_then(|v| v.as_float()).unwrap_or(default);
            // Dragging stays in the comfortable range; typing a value goes past it.
            let r = ui.add(egui::Slider::new(&mut value, range).text(prop.label).clamping(egui::SliderClamping::Edits));
            if r.changed() {
                self.editor.set_value_at(item, prop.target.clone(), prop.param, Value::Float(value), t, prop.param);
            }
            if r.drag_stopped() {
                self.editor.doc.seal();
            }
            self.property_menu(&r, item, &prop.target, prop.param, &prop.default, Some((lo, hi)), t);
        });
    }

    /// Right-click on a property: copy / paste its value, reset it, and — for numbers —
    /// make it the one the clip's keyframe line shows on the timeline.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn property_menu(&mut self, r: &egui::Response, item: ItemId, target: &ParamTarget, param: &str, default: &Value, band: Option<(f64, f64)>, t: Time) {
        crate::widgets::context_menu(r, |ui| self.property_menu_items(ui, item, target, param, default, band, t));
    }

    /// The entries of [`App::property_menu`], for menus that add their own.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn property_menu_items(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, param: &str, default: &Value, band: Option<(f64, f64)>, t: Time) {
        {
            let current = self.editor.param_value(item, target, param, t).unwrap_or_else(|| default.clone());
            if ui.button("Copy value").clicked() {
                self.value_clipboard = Some(current.clone());
                ui.close();
            }
            let pastable = self.value_clipboard.clone().and_then(|v| v.coerce(default.ty()));
            if ui.add_enabled(pastable.is_some(), egui::Button::new("Paste value")).clicked()
                && let Some(v) = pastable
            {
                self.editor.set_value_at(item, target.clone(), param, v, t, param);
                self.editor.doc.seal();
                ui.close();
            }
            if ui.button("Reset to default").clicked() {
                self.editor.set_value_at(item, target.clone(), param, default.clone(), t, param);
                self.editor.doc.seal();
                ui.close();
            }
            if let Some((lo, hi)) = band {
                ui.separator();
                let band = crate::band::Band { target: target.clone(), param: param.to_string(), lo, hi };
                ui.menu_button("Animate", |ui| {
                    if ui.button("Edit curve…").on_hover_text("Its animation as a graph: drag keys and easing handles").clicked() {
                        self.edit_curve(item, band.clone());
                        ui.close();
                    }
                    if ui.button("Edit wave…").on_hover_text("Make it swing back and forth on its own: shape, frequency, min/max, decay").clicked() {
                        self.edit_wave(item, band.clone());
                        ui.close();
                    }
                    if ui.button("Edit connection offset…").on_hover_text("Connect it to the sound: offset it by how loud the mix or a chosen clip is (or its bass, mids or treble)").clicked() {
                        self.edit_connection(item, band.clone());
                        ui.close();
                    }
                });
                let on = self.clip_band(item).is_some_and(|b| b == band);
                if ui.selectable_label(on, "Default keyframe property").on_hover_text("Show this on the clip in the timeline, where its keyframes can be dragged").clicked() {
                    if on {
                        self.bands.remove(&item);
                    } else {
                        self.bands.insert(item, band);
                    }
                    ui.close();
                }
            }
            if self.track_space(item, target, param).is_some() {
                ui.separator();
                ui.menu_button("Animate", |ui| {
                    if ui
                        .button("Edit track…")
                        .on_hover_text("Make it follow something in the picture: have the AI follow a point, click it in by hand, or record it with the mouse")
                        .clicked()
                    {
                        self.edit_track(item, target.clone(), param, crate::tracks::Purpose::Track);
                        ui.close();
                    }
                    if self.can_stabilize(item, target, param)
                        && ui
                            .button("Edit stabilization…")
                            .on_hover_text("Steady this clip: track something in it that should hold still, then choose how firmly — smoothed, or locked in place")
                            .clicked()
                    {
                        self.edit_track(item, target.clone(), param, crate::tracks::Purpose::Stabilize);
                        ui.close();
                    }
                });
            }
            self.spoken_entry(ui, item, target, param, default, t);
        }
    }

    /// Whether `param` can take its own value on the word being spoken: a title's color
    /// and outline, and any text effect's parameters.
    pub(crate) fn can_highlight_spoken(&self, item: ItemId, target: &ParamTarget, param: &str) -> bool {
        if param.ends_with(schema::SPOKEN_SUFFIX) {
            return false;
        }
        let Some(it) = self.editor.item(item).filter(|i| i.kind == ItemKind::Text) else { return false };
        match target {
            ParamTarget::Item => schema::SPOKEN_TEXT_PARAMS.contains(&param),
            ParamTarget::Effect(id) => it.effects.iter().find(|e| e.id == *id).and_then(|e| self.registry.effect(&e.type_id)).is_some_and(|d| d.kind.text_only()),
            _ => false,
        }
    }

    /// The "Highlight when spoken" entry of a property's menu: adds a second value (shown
    /// under the property) used on the word being spoken, or takes it away.
    fn spoken_entry(&mut self, ui: &mut egui::Ui, item: ItemId, target: &ParamTarget, param: &str, default: &Value, t: Time) {
        // The "… when spoken" value itself: its own menu takes it away.
        if let Some(base) = param.strip_suffix(schema::SPOKEN_SUFFIX) {
            if self.can_highlight_spoken(item, target, base) {
                ui.separator();
                if ui.button("Remove highlight when spoken").on_hover_text("The spoken word goes back to the same look as the rest").clicked() {
                    self.remove_spoken(item, target, param);
                    ui.close();
                }
            }
            return;
        }
        if !self.can_highlight_spoken(item, target, param) {
            return;
        }
        ui.separator();
        let key = schema::spoken(param);
        if self.editor.param_source(item, target, &key).is_none() {
            let tip = "Give the word being spoken its own value, set just below. Captions know when each word is said; other titles spread their words over the clip.";
            if ui.button("Highlight when spoken").on_hover_text(tip).clicked() {
                let now = self.editor.param_value(item, target, param, t).unwrap_or_else(|| default.clone());
                self.editor.set_param(item, target.clone(), &key, ParamSource::Static(now), &key);
                self.editor.doc.seal();
                ui.close();
            }
        } else if ui.button("Remove highlight when spoken").clicked() {
            self.remove_spoken(item, target, &key);
            ui.close();
        }
    }

    /// Takes a "when spoken" value away — from every selected clip that has it, when
    /// several are being edited together.
    fn remove_spoken(&mut self, item: ItemId, target: &ParamTarget, key: &str) {
        let seq = self.editor.seq;
        let mut clips = vec![(item, target.clone())];
        for other in self.editor.linked.clone().into_iter().filter(|o| *o != item) {
            // The same property there: a title's own, or its matching text effect.
            let there = match target {
                ParamTarget::Effect(id) => {
                    let type_id = self.editor.item(item).and_then(|it| it.effects.iter().find(|e| e.id == *id)).map(|e| e.type_id.clone());
                    self.editor.item(other).and_then(|it| it.effects.iter().find(|e| Some(&e.type_id) == type_id.as_ref())).map(|e| ParamTarget::Effect(e.id))
                }
                t => Some(t.clone()),
            };
            clips.extend(there.map(|t| (other, t)));
        }
        let ops = clips
            .into_iter()
            .filter(|(it, t)| self.editor.param_source(*it, t, key).is_some())
            .map(|(item, target)| oa_doc::Op::SetParam { seq, item, target, param: oa_params::ParamId::new(key), source: None })
            .collect();
        self.apply_or_report("Remove highlight when spoken", ops);
    }
}
