//! Effect presets: effects — one, or a group with its mix, collapsed view and lock —
//! kept as a small file (`.oafx`, JSON) in the asset library, for any clip in any
//! project.
//!
//! Drag an effect or a group's grip onto the media bin (it turns to Assets) to save one,
//! or use **Save to assets…** in its right-click menu: a name, whether to keep its
//! keyframes (they stretch from the clip it came from to the one it goes on), and
//! whether to leave out what only fits that clip (its masks). Pictures from the
//! project an effect uses (a media setting) never come along: another project doesn't
//! have them.
//!
//! The Assets tab lists presets as cards, a picture effect's previewed on the sample
//! picture. Drag one onto a clip in the timeline or the viewer, or onto the effect
//! list, or double-click it, to put copies of its effects on the selected clips. An
//! effect whose plugin isn't installed comes along anyway (its settings kept, as in a
//! project) and is named, rather than silently left out.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{EffectGroup, EffectId, EffectInstance, Item, ItemId};
use oa_params::{ParamSource, Value};
use oa_time::Time;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A preset file's extension.
pub const EXTENSION: &str = "oafx";
/// What a preset file says it is.
const FORMAT: &str = "openatelier.effects";
const VERSION: u32 = 1;

/// One preset, as kept on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub format: String,
    pub version: u32,
    pub name: String,
    /// Sound effects (else picture effects).
    #[serde(default)]
    pub sound: bool,
    /// How long the clip they came from was, in seconds: kept keys stretch from it to
    /// the length of the clip they go on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// The groups its effects are in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<EffectGroup>,
    pub effects: Vec<EffectInstance>,
}

/// What saving a preset keeps.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Keep {
    /// Keyframes (else each animated value as it is at `at`, the clip's own time).
    pub keys: bool,
    /// Its masks (what only fits the clip it came from).
    pub masks: bool,
}

impl Preset {
    /// `item`'s effects in `group`, or its effects `picked` (a group comes along when all
    /// of its effects are picked), as a preset named `name`:
    /// `at` is when on the clip (its own clock) animated values are taken, when keys
    /// aren't kept.
    pub fn take(item: &Item, group: Option<u64>, picked: &[EffectId], name: &str, sound: bool, keep: Keep, at: Time) -> Preset {
        let picked: Vec<EffectInstance> = item.effects.iter().filter(|e| match group {
            Some(g) => e.group == Some(g),
            None => picked.contains(&e.id),
        }).cloned().collect();
        // The groups that come along: the one asked for, or those whose effects are all picked.
        let whole = |g: u64| item.effects.iter().filter(|e| e.group == Some(g)).all(|e| picked.iter().any(|p| p.id == e.id));
        let kept: Vec<u64> = item.effect_groups.iter().map(|g| g.id).filter(|g| group == Some(*g) || (group.is_none() && whole(*g) && picked.iter().any(|p| p.group == Some(*g)))).collect();
        // A Duplicate's own effects go with it.
        let mut effects: Vec<EffectInstance> = item.effects.iter().filter(|e| picked.iter().any(|p| p.id == e.id) || e.on_duplicate.is_some_and(|d| picked.iter().any(|p| p.id == d))).cloned().collect();
        let ctx = oa_params::EvalContext::at(at, at);
        for fx in &mut effects {
            if fx.group.is_some_and(|g| !kept.contains(&g)) {
                fx.group = None;
            }
            fx.params.0.retain(|id, src| {
                // Pictures from this project: another project doesn't have them.
                if matches!(src, ParamSource::Static(Value::Media(_))) {
                    return false;
                }
                keep.masks || !(id.as_str().starts_with("mask.") || id.as_str().ends_with(oa_doc::mask::ON_MASK_SUFFIX))
            });
            if !keep.keys {
                for src in fx.params.0.values_mut() {
                    if matches!(src, ParamSource::Animated(_)) {
                        *src = ParamSource::Static(src.eval(&ctx));
                    }
                }
            }
        }
        let mut groups: Vec<EffectGroup> = item.effect_groups.iter().filter(|g| kept.contains(&g.id)).cloned().collect();
        if !keep.keys {
            for g in &mut groups {
                for src in g.params.0.values_mut() {
                    if matches!(src, ParamSource::Animated(_)) {
                        *src = ParamSource::Static(src.eval(&ctx));
                    }
                }
            }
        }
        Preset { format: FORMAT.into(), version: VERSION, name: name.into(), sound, length: Some(item.range.duration.as_seconds_f64()), groups, effects }
    }

    /// Its effects and groups ready for a clip `length` long: kept keys stretched from
    /// the clip they came from.
    pub fn fitted(&self, length: Time) -> (Vec<EffectInstance>, Vec<EffectGroup>) {
        let (mut effects, mut groups) = (self.effects.clone(), self.groups.clone());
        if let Some(from) = self.length.filter(|l| *l > 0.0) {
            let factor = length.as_seconds_f64() / from;
            if factor > 0.0 && (factor - 1.0).abs() > 1e-9 {
                for src in effects.iter_mut().flat_map(|e| e.params.0.values_mut()).chain(groups.iter_mut().flat_map(|g| g.params.0.values_mut())) {
                    src.scale_clip_clock(factor);
                }
            }
        }
        (effects, groups)
    }

    pub fn read(path: &Path) -> Result<Preset, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let preset: Preset = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if preset.format != FORMAT {
            return Err(trf("{0} isn't an effects preset", &[("0", &path.display().to_string())]));
        }
        if preset.version > VERSION {
            return Err(trf("{0} was saved by a newer version of OpenAtelier", &[("0", &path.display().to_string())]));
        }
        Ok(preset)
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::autosave::write_atomic(path, &json).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// A preset being saved: the save window's state.
pub struct PresetSave {
    item: ItemId,
    group: Option<u64>,
    effects: Vec<EffectId>,
    name: String,
    keys: bool,
    masks: bool,
    fresh: bool,
}

/// A preset card being dragged out of the Assets tab.
#[derive(Clone)]
pub struct PresetDrag {
    pub path: PathBuf,
    pub name: String,
}

impl App {
    /// Opens the save window for `item`'s group `group`, or its effects `effects`.
    pub(crate) fn save_preset_from(&mut self, item: ItemId, group: Option<u64>, effects: Vec<EffectId>) {
        let Some(it) = self.editor.item(item) else { return };
        let picked: Vec<&EffectInstance> = it.effects.iter().filter(|e| effects.contains(&e.id)).collect();
        let name = match (group.and_then(|g| it.effect_group(g)), picked.as_slice()) {
            (Some(g), _) => g.name.clone(),
            (None, [fx]) => self.effect_name(&fx.type_id),
            (None, [_, ..]) => picked.iter().map(|e| self.effect_name(&e.type_id)).collect::<Vec<_>>().join(" + ").chars().take(60).collect(),
            _ => return,
        };
        self.preset_save = Some(PresetSave { item, group, effects, name, keys: true, masks: false, fresh: true });
    }

    /// The save window, while a preset is being saved.
    pub(crate) fn preset_save_window(&mut self, ctx: &egui::Context) {
        let Some(mut save) = self.preset_save.take() else { return };
        let mut done: Option<bool> = None;
        egui::Modal::new(egui::Id::new("preset-save")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading(tr("Save to assets"));
            ui.label(egui::RichText::new(tr("A preset in the asset library: drag it onto any clip, in any project.")).small().weak());
            ui.add_space(6.0);
            let r = ui.add(egui::TextEdit::singleline(&mut save.name).hint_text(tr("Name")).desired_width(f32::INFINITY));
            if save.fresh {
                r.request_focus();
                save.fresh = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = Some(true);
            }
            ui.checkbox(&mut save.keys, tr("Keep its keyframes")).on_hover_text(tr("They stretch to the length of the clip it goes on. Off: each animated value as it is at the playhead."));
            ui.checkbox(&mut save.masks, tr("Keep its masks")).on_hover_text(tr("Masks are drawn for this clip, so they rarely fit another. Pictures from this project never come along."));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!save.name.trim().is_empty(), egui::Button::new(tr("Save"))).clicked() {
                    done = Some(true);
                }
                if ui.button(tr("Cancel")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => self.write_preset(&save),
            Some(false) => {}
            None => self.preset_save = Some(save),
        }
    }

    fn write_preset(&mut self, save: &PresetSave) {
        let Some(it) = self.editor.item(save.item).cloned() else { return };
        let sound = it.effects.iter().find(|e| save.group.is_some_and(|g| e.group == Some(g)) || save.effects.contains(&e.id)).is_some_and(|e| self.registry.is_sound(&e.type_id));
        let at = (self.playhead - it.range.start).max(Time::ZERO).min(it.range.duration);
        let preset = Preset::take(&it, save.group, &save.effects, save.name.trim(), sound, Keep { keys: save.keys, masks: save.masks }, at);
        if preset.effects.is_empty() {
            return;
        }
        let folder = self.bin.asset_folder.clone();
        match self.assets.add_preset(&preset, &folder) {
            Ok(_) => {
                self.bin.tab = crate::bin::BinTab::Assets;
                self.notify(trf("Saved {name} to assets.", &[("name", &preset.name)]));
            }
            Err(e) => self.report_error(trf("can't save the preset: {e}", &[("e", &e)])),
        }
    }

    /// Puts copies of the preset at `path` on each of `targets` its effects fit (after
    /// their own effects): one undo step. An effect whose plugin isn't here comes along
    /// and is named.
    pub(crate) fn apply_preset(&mut self, path: &Path, targets: &[ItemId]) {
        let preset = match Preset::read(path) {
            Ok(p) => p,
            Err(e) => {
                self.report_error(e);
                return;
            }
        };
        let known = |app: &App, type_id: &str| app.registry.effect(type_id).is_some() || oa_audio::fx::info(type_id).is_some();
        let mut missing: Vec<String> = preset.effects.iter().filter(|e| !known(self, &e.type_id)).map(|e| e.type_id.clone()).collect();
        missing.dedup();
        let mut ops = Vec::new();
        let mut changed = 0;
        for &item in targets {
            let Some(it) = self.editor.item(item).cloned() else { continue };
            let (effects, groups) = preset.fitted(it.range.duration);
            // The ones that belong on this clip (a missing plugin's come along).
            let fitting: Vec<EffectInstance> = effects.into_iter().filter(|e| !known(self, &e.type_id) || self.effect_fits(item, &e.type_id)).collect();
            if fitting.is_empty() || fitting.iter().all(|e| !known(self, &e.type_id)) {
                continue;
            }
            ops.extend(self.insert_effects_ops(item, &fitting, &groups, it.effects.len()));
            changed += 1;
        }
        if changed == 0 {
            let what = if preset.sound { tr("they're sound effects and none of those clips has sound") } else { tr("none of its effects fit those clips") };
            self.report_error(trf("{name} can't go there: {what}", &[("name", &preset.name), ("what", what)]));
            return;
        }
        if let Err(e) = self.editor.apply(tr("Apply preset"), ops) {
            self.report_error(e.to_string());
            return;
        }
        let mut note = if changed == 1 { trf("Applied {name}.", &[("name", &preset.name)]) } else { trf("Applied {name} to {n} clips.", &[("name", &preset.name), ("n", &changed.to_string())]) };
        if !missing.is_empty() {
            note.push(' ');
            note.push_str(&trf("Not installed (its settings are kept, but it doesn't run): {list}.", &[("list", &missing.join(", "))]));
        }
        self.notify(note);
    }

    /// Where a dragged preset lands, as the pointer is let go: on the clip under it (and
    /// the rest of the selection, when that clip is selected), or on the effect list
    /// (the selected clips).
    pub(crate) fn drop_preset(&mut self, ctx: &egui::Context) {
        let Some(drag) = self.preset_drag.clone() else { return };
        let pointer = ctx.input(|i| i.pointer.latest_pos());
        if let Some(p) = pointer {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("preset-drag")));
            let galley = painter.layout_no_wrap(trf("⧉ {name} — drop on a clip", &[("name", &drag.name)]), egui::FontId::proportional(12.0), egui::Color32::WHITE);
            let at = p + egui::vec2(14.0, 10.0);
            painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(8.0, 4.0)), 4.0, crate::style::ACCENT.gamma_multiply(0.9));
            painter.galley(at, galley, egui::Color32::WHITE);
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if ctx.input(|i| i.pointer.primary_down()) {
            return;
        }
        self.preset_drag = None;
        let Some(p) = pointer else { return };
        let targets: Vec<ItemId> = match self.clip_on_screen(p) {
            Some(onto) => {
                let selected = self.selected_clips();
                if selected.contains(&onto) { selected } else { vec![onto] }
            }
            None if self.effect_list_rect.is_some_and(|r| r.contains(p)) && matches!(self.inspector_tab, crate::inspector::Tab::Effects | crate::inspector::Tab::Sound) => match self.selection {
                Some(item) => {
                    let mut all = vec![item];
                    all.extend(self.selected_clips().into_iter().filter(|i| *i != item));
                    all
                }
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        if !targets.is_empty() {
            self.apply_preset(&drag.path, &targets);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::ItemKind;
    use oa_params::ParamSet;
    use oa_params::{Curve, Keyframe, KeyframeAnchor};
    use oa_time::TimeRange;

    fn clip() -> Item {
        let mut it = Item::new(ItemId(1), "clip", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(4)));
        let mut group = EffectGroup::new(3, "Look");
        group.locked = true;
        group.params.set(oa_doc::schema::GROUP_MIX, ParamSource::Static(Value::Float(60.0)));
        it.effect_groups.push(group);
        let mut blur = EffectInstance::new(EffectId(10), "oa.blur.gaussian");
        blur.group = Some(3);
        blur.shown = vec!["radius".into()];
        let ramp = Curve::new(KeyframeAnchor::ClipStart, vec![Keyframe::linear(Time::ZERO, Value::Float(0.0)), Keyframe::linear(Time::from_seconds(4), Value::Float(40.0))]);
        blur.params = ParamSet::default();
        blur.params.set("radius", ParamSource::Animated(ramp));
        blur.params.set(oa_doc::mask::EFFECT_USE, ParamSource::Static(Value::Float(1.0)));
        blur.params.set("picture", ParamSource::Static(Value::Media(Some(5))));
        it.effects.push(blur);
        let mut invert = EffectInstance::new(EffectId(11), "oa.color.invert");
        invert.group = Some(3);
        it.effects.push(invert);
        it.effects.push(EffectInstance::new(EffectId(12), "oa.color.levels"));
        it
    }

    /// A group saved as a preset keeps its effects, mix, ticked properties and lock,
    /// leaves out this project's pictures (and, unless asked, masks), and comes back
    /// from disk as it went: its keys stretched to the clip it goes on.
    #[test]
    fn groups_round_trip_as_presets() {
        let it = clip();
        let p = Preset::take(&it, Some(3), &[], "Dream", false, Keep { keys: true, masks: false }, Time::ZERO);
        assert_eq!(p.effects.len(), 2, "only the group's effects");
        assert_eq!(p.groups.len(), 1);
        assert!(p.groups[0].locked && p.effects[0].shown == ["radius"]);
        let params = &p.effects[0].params;
        assert!(params.get(oa_doc::mask::EFFECT_USE).is_none() && params.get("picture").is_none(), "no masks or project pictures");

        let path = std::env::temp_dir().join(format!("oa-preset-{}.oafx", std::process::id()));
        p.write(&path).unwrap();
        let back = Preset::read(&path).unwrap();
        assert_eq!(back, p);
        let _ = std::fs::remove_file(&path);

        // On a clip twice as long, the ramp takes twice as long.
        let (effects, _) = back.fitted(Time::from_seconds(8));
        let at = |t: i64| effects[0].params.get("radius").unwrap().eval(&oa_params::EvalContext::at(Time::from_seconds(t), Time::from_seconds(t)));
        assert_eq!(at(4), Value::Float(20.0));

        // Without keys: the value at the moment asked for. Part of a group: no group.
        let still = Preset::take(&it, None, &[EffectId(10)], "Blur", false, Keep { keys: false, masks: true }, Time::from_seconds(1));
        assert_eq!(still.effects[0].params.get("radius"), Some(&ParamSource::Static(Value::Float(10.0))));
        assert!(still.groups.is_empty() && still.effects[0].group.is_none());
        assert!(still.effects[0].params.get(oa_doc::mask::EFFECT_USE).is_some(), "masks kept when asked");
    }
}
