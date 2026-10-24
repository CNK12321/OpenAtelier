//! Multi-cam editing (Settings → Features): a compound clip shown one cam at a time.
//!
//! Each picture track inside the compound is a cam (one layer each; a nested compound
//! clip on a track makes a cam of several). Which cam shows is the clip's
//! `schema::CAM`, keyed with hold keys: switching at the playhead (1–9, or right-click →
//! Show cam) puts a cut there, drawn on the clip as a white line. Its audio tracks
//! always play; a cam's own sound plays while it shows. **Extract cam** takes one cam
//! out for the stretch between two cuts as ordinary clips on a layer above — a second
//! view (picture-in-picture, a split screen) over the same moment.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{schema, ItemId, ItemKind, Op, ParamTarget};
use oa_params::{Curve, Keyframe, KeyframeAnchor, ParamId, ParamSource, Value};
use oa_time::Time;

impl App {
    /// The multicam clip `id`, if it is one.
    pub(crate) fn multicam_item(&self, id: ItemId) -> Option<&oa_doc::Item> {
        self.editor.item(id).filter(|i| i.multicam && matches!(i.kind, ItemKind::Nested { .. }))
    }

    /// The selected clip, if it's a multicam clip (and multi-cam is switched on).
    pub(crate) fn selected_multicam(&self) -> Option<ItemId> {
        self.selection.filter(|id| self.settings.multicam && self.multicam_item(*id).is_some())
    }

    /// Its cams' names: "Cam 1 · V1"…
    pub(crate) fn cam_names(&self, id: ItemId) -> Vec<String> {
        let Some(ItemKind::Nested { sequence }) = self.editor.item(id).map(|i| i.kind.clone()) else { return Vec::new() };
        let Some(s) = self.editor.doc.project().sequence(sequence) else { return Vec::new() };
        s.cams().iter().enumerate().map(|(i, t)| trf("Cam {n} · {track}", &[("n", &(i + 1).to_string()), ("track", &t.name)])).collect()
    }

    /// Turns compound clip `id` into a multicam clip, showing its first cam.
    pub(crate) fn convert_to_multicam(&mut self, id: ItemId) {
        let Some(it) = self.editor.item(id).filter(|i| matches!(i.kind, ItemKind::Nested { .. }) && !i.multicam) else { return };
        let cams = self.cam_names(id).len();
        if cams == 0 {
            self.notify(tr("This compound clip has no picture tracks to switch between."));
            return;
        }
        let seq = self.editor.seq;
        let mut ops = vec![Op::SetMulticam { seq, item: id, on: true }];
        if it.params.get(schema::CAM).is_none() {
            ops.push(Op::SetParam { seq, item: id, target: ParamTarget::Item, param: ParamId::new(schema::CAM), source: Some(ParamSource::Static(Value::Float(0.0))) });
        }
        self.apply_or_report(tr("Make multicam clip"), ops);
        self.notify(trf("A multicam clip of {n} cams: switch at the playhead with 1–9, or right-click it.", &[("n", &cams.to_string())]));
    }

    /// Turns multicam clip `id` back into a plain compound clip (its cuts dropped).
    pub(crate) fn convert_to_compound(&mut self, id: ItemId) {
        if self.multicam_item(id).is_none() {
            return;
        }
        let seq = self.editor.seq;
        let ops = vec![Op::SetMulticam { seq, item: id, on: false }, Op::SetParam { seq, item: id, target: ParamTarget::Item, param: ParamId::new(schema::CAM), source: None }];
        self.apply_or_report(tr("Make compound clip"), ops);
    }

    /// The selected clips as one multicam clip: nested into a compound clip (each track
    /// a cam), then made multicam.
    pub(crate) fn make_multicam(&mut self) {
        let chosen = self.selected_clips();
        match self.editor.compound(&chosen, true, self.variant_id()) {
            Ok((_, Some(clip))) => {
                self.select_clip(clip, false);
                self.convert_to_multicam(clip);
            }
            Ok(_) => {}
            Err(e) => self.report_error(e.to_string()),
        }
    }

    /// Shows cam `cam` from `at` on: a cut there (a hold key), the one before it kept.
    pub(crate) fn switch_cam(&mut self, id: ItemId, cam: usize, at: Time) {
        let Some(it) = self.multicam_item(id).cloned() else { return };
        if cam >= self.cam_names(id).len() || !it.range.contains(at) {
            return;
        }
        let rate = self.editor.sequence().rate;
        let local = rate.frame_start(rate.frame_at(at)) - it.range.start;
        let local = local.max(Time::ZERO);
        let before = schema::cam_at(&it, at);
        let mut keys: Vec<Keyframe> = match it.params.get(schema::CAM) {
            Some(ParamSource::Animated(c)) if c.anchor == KeyframeAnchor::ClipStart => c.keys.iter().map(|k| Keyframe::hold(k.t, k.value.clone())).collect(),
            // Steady so far: what it shows from the start, held.
            _ => vec![Keyframe::hold(Time::ZERO, Value::Float(before as f64))],
        };
        keys.retain(|k| k.t != local);
        keys.push(Keyframe::hold(local, Value::Float(cam as f64)));
        self.set_cam_keys(id, keys, tr("Switch cam"));
    }

    /// Writes a multicam clip's cuts (hold keys on its own clock), tidied: in order, a
    /// cut to what was already showing dropped, and none at all a steady cam.
    fn set_cam_keys(&mut self, id: ItemId, keys: Vec<Keyframe>, label: &str) {
        let keys = tidy_cuts(keys);
        let source = if keys.len() == 1 && keys[0].t == Time::ZERO { ParamSource::Static(keys[0].value.clone()) } else { ParamSource::Animated(Curve::new(KeyframeAnchor::ClipStart, keys)) };
        let op = Op::SetParam { seq: self.editor.seq, item: id, target: ParamTarget::Item, param: ParamId::new(schema::CAM), source: Some(source) };
        self.apply_or_report(label, vec![op]);
    }

    /// Takes cam `cam` out for the stretch of multicam clip `id` around `at` (between
    /// the cuts either side), onto a layer above; selects what came out.
    pub(crate) fn extract_cam(&mut self, id: ItemId, cam: usize, at: Time) {
        let Some(it) = self.multicam_item(id).cloned() else { return };
        let Some((stretch, _)) = schema::cam_stretches(&it).into_iter().find(|(r, _)| r.contains(at) || r.end() == at) else { return };
        let snapshot = self.editor.doc.snapshot();
        let seq = self.editor.seq;
        let editor = &mut self.editor;
        match oa_edit::compound::extract_cam(&snapshot, seq, id, cam, stretch, || editor.doc.alloc_id()) {
            Ok((ops, clips)) => {
                self.apply_or_report(tr("Extract cam"), ops);
                if let Some(first) = clips.first() {
                    self.select_clip(*first, false);
                }
            }
            Err(oa_doc::EditError::InvalidRange) => self.notify(tr("Extracting a cam works on multicam clips playing at normal speed.")),
            Err(_) => self.notify(tr("That cam has nothing in this stretch.")),
        }
    }

    /// The multicam entries of a clip's right-click menu (`at`: where on the timeline).
    pub(crate) fn multicam_menu(&mut self, ui: &mut egui::Ui, id: ItemId, at: Time) {
        if !self.settings.multicam {
            return;
        }
        let Some(it) = self.editor.item(id).cloned() else { return };
        if !matches!(it.kind, ItemKind::Nested { .. }) {
            return;
        }
        if !it.multicam {
            if ui.button(tr("Make multicam clip")).on_hover_text(tr("Show one of its picture tracks at a time, switching between them")).clicked() {
                self.convert_to_multicam(id);
                ui.close();
            }
            return;
        }
        let names = self.cam_names(id);
        let showing = schema::cam_at(&it, at);
        ui.menu_button(tr("Show cam"), |ui| {
            for (i, name) in names.iter().enumerate() {
                let key = if i < 9 { format!("{}", i + 1) } else { String::new() };
                if ui.add(egui::Button::selectable(i == showing, name.as_str()).shortcut_text(key)).on_hover_text(tr("From the playhead on (a cut there)")).clicked() {
                    self.switch_cam(id, i, self.playhead);
                    ui.close();
                }
            }
        });
        ui.menu_button(tr("Extract cam"), |ui| {
            ui.label(egui::RichText::new(tr("Its clips for this stretch, between the cuts either side, on a layer above")).small().weak());
            for (i, name) in names.iter().enumerate() {
                if ui.button(name).clicked() {
                    self.extract_cam(id, i, at);
                    ui.close();
                }
            }
        });
        if ui.button(tr("Make compound clip")).on_hover_text(tr("Back to a compound clip showing all its tracks (its cuts go)")).clicked() {
            self.convert_to_compound(id);
            ui.close();
        }
    }

    /// The multi-cam button beside Captions: a compound clip selected, make it multicam
    /// (or back); several clips, make them one multicam clip.
    pub(crate) fn multicam_button(&mut self, ui: &mut egui::Ui) {
        if !self.settings.multicam {
            return;
        }
        let picked = self.selected_clips();
        let compound = self.selection.filter(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, ItemKind::Nested { .. })) && picked.len() <= 1);
        let (enabled, tip) = match compound {
            Some(id) if self.multicam_item(id).is_some() => (true, tr("Multicam clip: switch cams with 1–9 at the playhead, or right-click it · click to make it a plain compound clip again")),
            Some(_) => (true, tr("Make this compound clip multicam: its picture tracks become cams, shown one at a time")),
            None if picked.len() > 1 => (true, tr("Make a multicam clip of the selected clips: each track a cam (line them up in time first)")),
            None => (false, tr("Multi-cam: select a compound clip, or the clips from each camera, lined up")),
        };
        let on = compound.is_some_and(|id| self.multicam_item(id).is_some());
        let r = crate::icons::button(ui, crate::icons::MULTICAM, tip, "", enabled);
        if on {
            ui.painter().rect_stroke(r.rect, 4.0, egui::Stroke::new(1.5, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        if r.clicked() {
            match compound {
                Some(id) if on => self.convert_to_compound(id),
                Some(id) => self.convert_to_multicam(id),
                None => self.make_multicam(),
            }
        }
    }

    /// Down the right of the viewer while a multicam clip is selected: each cam as it is
    /// at the playhead (all of them, smaller when many), a small gray number in its
    /// corner (its key), the one showing outlined — click one to cut to it there. Under
    /// them: the previous cut, delete the cut, the next cut. It folds away to a tab.
    pub(crate) fn cam_switcher(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let Some(id) = self.selected_multicam() else { return };
        let Some(it) = self.editor.item(id).cloned() else { return };
        let ItemKind::Nested { sequence } = it.kind else { return };
        let names = self.cam_names(id);
        if names.is_empty() {
            return;
        }
        const PAD: f32 = 5.0;
        const GAP: f32 = 4.0;
        const HEAD: f32 = 22.0;
        const BUTTON: f32 = 28.0;
        let v = ui.visuals().clone();
        let panel = |ui: &egui::Ui, rect: egui::Rect| {
            ui.painter().add(egui::epaint::Shadow { offset: [0, 2], blur: 10, spread: 0, color: egui::Color32::from_black_alpha(90) }.as_shape(rect, 8.0));
            ui.painter().rect(rect, 8.0, v.window_fill.gamma_multiply(0.96), egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
        };
        // An icon button drawn on the panel.
        let icon_button = |ui: &egui::Ui, rect: egui::Rect, key: &str, icon: crate::icons::Icon, tip: &str, enabled: bool| -> bool {
            let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
            let r = ui.interact(rect, ui.id().with(("cam-switcher", key)), sense);
            if enabled && r.hovered() {
                ui.painter().rect_filled(rect, 5.0, v.widgets.hovered.weak_bg_fill);
            }
            let color = if !enabled { v.weak_text_color().gamma_multiply(0.6) } else if r.hovered() { v.strong_text_color() } else { v.text_color() };
            crate::icons::paint(ui.painter(), rect.shrink(5.0), icon, color);
            let r = r.on_hover_text(tip);
            enabled && r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
        };

        // Folded: a tab at the edge to open it again.
        if self.cam_switcher_folded {
            let tab = egui::Rect::from_min_size(egui::pos2(area.right() - 8.0 - BUTTON - 2.0 * PAD, area.center().y - (BUTTON + 2.0 * PAD) / 2.0), egui::vec2(BUTTON + 2.0 * PAD, BUTTON + 2.0 * PAD));
            panel(ui, tab);
            if icon_button(ui, tab.shrink(PAD), "open", crate::icons::MULTICAM, tr("Show the cams"), true) {
                self.cam_switcher_folded = false;
            }
            return;
        }

        let ctx = ui.ctx().clone();
        let at = self.playhead.max(it.range.start).min(it.range.end() - Time(1));
        // The compound's own time there (what its filmstrips are of).
        let inside = it.eval_context(at).source_time.as_seconds_f64();
        let showing = schema::cam_at(&it, at);
        let strips: Vec<Option<crate::previews::Strip>> = (0..names.len()).map(|k| self.cam_strip(&ctx, sequence, Some(k))).collect();
        let aspect = strips.iter().flatten().next().map_or(16.0 / 9.0, |s| s.aspect).max(0.2);
        // Every cam, at most 96 px wide, smaller to fit them all down the viewer.
        let n = names.len();
        let room = area.height() - 16.0 - 2.0 * PAD - HEAD - BUTTON - 2.0 * GAP;
        let each = ((room - (n - 1) as f32 * GAP) / n as f32).max(12.0);
        let w = (each * aspect).clamp(24.0, 96.0);
        let h = w / aspect;
        let footer_w = 3.0 * BUTTON + 2.0 * GAP;
        let inner_w = w.max(footer_w);
        let height = HEAD + n as f32 * h + (n - 1) as f32 * GAP + GAP + BUTTON + 2.0 * PAD;
        let top = (area.center().y - height / 2.0).max(area.top() + 8.0);
        let strip = egui::Rect::from_min_size(egui::pos2(area.right() - 8.0 - inner_w - 2.0 * PAD, top), egui::vec2(inner_w + 2.0 * PAD, height));
        panel(ui, strip);
        // It takes the pointer: a click between cams doesn't reach the clip.
        ui.interact(strip, ui.id().with("cam-switcher"), egui::Sense::click_and_drag());

        // Its head: what it is, and the fold.
        let head = egui::Rect::from_min_size(strip.min + egui::vec2(PAD, PAD), egui::vec2(inner_w, HEAD - 4.0));
        ui.painter().text(head.left_center() + egui::vec2(2.0, 0.0), egui::Align2::LEFT_CENTER, tr("Cams"), egui::FontId::proportional(11.0), v.weak_text_color());
        let fold = egui::Rect::from_min_size(egui::pos2(head.right() - (HEAD - 4.0), head.top()), egui::vec2(HEAD - 4.0, HEAD - 4.0));
        if icon_button(ui, fold, "fold", crate::icons::STEP_ON, tr("Fold the cams away"), true) {
            self.cam_switcher_folded = true;
        }

        let left = strip.left() + PAD + (inner_w - w) / 2.0;
        for k in 0..n {
            let rect = egui::Rect::from_min_size(egui::pos2(left, strip.top() + PAD + HEAD + k as f32 * (h + GAP)), egui::vec2(w, h));
            let r = ui.interact(rect, ui.id().with(("cam", k)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let painter = ui.painter();
            painter.rect_filled(rect, 4.0, v.extreme_bg_color);
            match &strips[k] {
                Some(s) => {
                    painter.image(s.texture.id(), rect, s.uv(inside), if r.hovered() || k == showing { egui::Color32::WHITE } else { egui::Color32::from_gray(200) });
                }
                // Still being made.
                None => crate::widgets::skeleton(ui, painter, rect),
            }
            let on = k == showing;
            if on {
                painter.rect_stroke(rect, 4.0, egui::Stroke::new(2.0, crate::style::ACCENT), egui::StrokeKind::Inside);
            } else if r.hovered() {
                painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, v.strong_text_color()), egui::StrokeKind::Inside);
            }
            // Its number, small and gray in the corner (white on the one showing).
            let number = painter.layout_no_wrap((k + 1).to_string(), egui::FontId::proportional(11.0), if on { egui::Color32::WHITE } else { egui::Color32::from_gray(170) });
            let corner = rect.right_bottom() - number.size() - egui::vec2(5.0, 3.0);
            painter.rect_filled(egui::Rect::from_min_size(corner, number.size()).expand2(egui::vec2(3.0, 1.0)), 3.0, egui::Color32::from_black_alpha(150));
            painter.galley(corner, number, egui::Color32::WHITE);
            let tip = if k < 9 { trf("{name} — click to cut to it at the playhead (or press {n})", &[("name", &names[k]), ("n", &(k + 1).to_string())]) } else { trf("{name} — click to cut to it at the playhead", &[("name", &names[k])]) };
            if r.on_hover_text(tip).clicked() {
                self.switch_cam(id, k, self.playhead);
            }
        }

        // Its foot: the cut before the playhead, delete the cut, the cut after it.
        let cuts = schema::cam_cut_times(&it);
        let (before, after, here) = cut_neighbors(&cuts, self.playhead);
        let foot_top = strip.bottom() - PAD - BUTTON;
        let foot_left = strip.left() + PAD + (inner_w - footer_w) / 2.0;
        let slot = |i: usize| egui::Rect::from_min_size(egui::pos2(foot_left + i as f32 * (BUTTON + GAP), foot_top), egui::vec2(BUTTON, BUTTON));
        if icon_button(ui, slot(0), "previous", crate::icons::SKIP_START, tr("Previous cut"), before.is_some()) {
            self.set_playing(false);
            self.set_playhead(before.expect("enabled"));
        }
        let delete_tip = if here.is_some() { tr("Delete this cut: the cam before it carries on") } else { tr("Delete the cut before the playhead") };
        if icon_button(ui, slot(1), "delete", crate::icons::DELETE, delete_tip, here.or(before).is_some()) {
            self.delete_cut(id, here.or(before).expect("enabled"));
        }
        if icon_button(ui, slot(2), "next", crate::icons::SKIP_END, tr("Next cut"), after.is_some()) {
            self.set_playing(false);
            self.set_playhead(after.expect("enabled"));
        }
    }

    /// Takes away the cut at `cut` (timeline time): the cam before it carries on.
    pub(crate) fn delete_cut(&mut self, id: ItemId, cut: Time) {
        let Some(it) = self.multicam_item(id).cloned() else { return };
        let Some(ParamSource::Animated(c)) = it.params.get(schema::CAM).cloned() else { return };
        let local = cut - it.range.start;
        let mut keys: Vec<Keyframe> = c.keys.iter().filter(|k| k.t != local).map(|k| Keyframe::hold(k.t, k.value.clone())).collect();
        if keys.len() == c.keys.len() {
            return;
        }
        // What showed from the start stays first.
        if keys.first().is_none_or(|k| k.t > Time::ZERO) {
            keys.insert(0, Keyframe::hold(Time::ZERO, Value::Float(schema::cam_at(&it, it.range.start) as f64)));
        }
        self.set_cam_keys(id, keys, tr("Delete cut"));
    }

    /// Keys 1–9 with a multicam clip selected: that cam from the playhead on. Whether a
    /// key was taken.
    pub(crate) fn multicam_keys(&mut self, ctx: &egui::Context) -> bool {
        let Some(id) = self.selected_multicam() else { return false };
        use egui::Key;
        let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (i, key) in keys.into_iter().enumerate() {
            if ctx.input_mut(|inp| inp.consume_key(egui::Modifiers::NONE, key)) {
                self.switch_cam(id, i, self.playhead);
                return true;
            }
        }
        false
    }
}

/// Cuts in order, each a hold key, a cut to the cam already showing taken out.
fn tidy_cuts(mut keys: Vec<Keyframe>) -> Vec<Keyframe> {
    keys.sort_by_key(|k| k.t);
    let mut last: Option<f64> = None;
    keys.retain(|k| {
        let v = k.value.as_float();
        let keep = v != last;
        last = v;
        keep
    });
    keys.into_iter().map(|k| Keyframe::hold(k.t, k.value)).collect()
}

/// Around the playhead `at`, of a multicam clip's cuts (`cuts`, in order): the last one
/// before it, the first one after it, and the one it's on (within a millisecond).
fn cut_neighbors(cuts: &[Time], at: Time) -> (Option<Time>, Option<Time>, Option<Time>) {
    let near = Time::from_flicks(oa_time::FLICKS_PER_SECOND / 1000);
    let here = cuts.iter().copied().find(|c| (*c - at).0.abs() < near.0);
    let before = cuts.iter().copied().rev().find(|c| *c < at && Some(*c) != here);
    let after = cuts.iter().copied().find(|c| *c > at && Some(*c) != here);
    (before, after, here)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: i64) -> Time {
        Time::from_seconds(s)
    }

    #[test]
    fn the_cuts_around_the_playhead() {
        let cuts = [secs(2), secs(5), secs(9)];
        assert_eq!(cut_neighbors(&cuts, secs(6)), (Some(secs(5)), Some(secs(9)), None));
        assert_eq!(cut_neighbors(&cuts, secs(5)), (Some(secs(2)), Some(secs(9)), Some(secs(5))), "on a cut: it's the one deleted, the steps go past it");
        assert_eq!(cut_neighbors(&cuts, secs(1)), (None, Some(secs(2)), None));
        assert_eq!(cut_neighbors(&cuts, secs(10)), (Some(secs(9)), None, None));
    }

    #[test]
    fn cuts_to_what_is_showing_go() {
        let k = |t: i64, cam: f64| Keyframe::linear(secs(t), Value::Float(cam));
        let tidy = tidy_cuts(vec![k(4, 1.0), k(0, 0.0), k(6, 1.0), k(8, 0.0)]);
        let got: Vec<(i64, f64)> = tidy.iter().map(|k| (k.t.as_seconds_f64() as i64, k.value.as_float().unwrap())).collect();
        assert_eq!(got, [(0, 0.0), (4, 1.0), (8, 0.0)], "the cut at 6 s to cam 2, already showing, goes");
        assert!(tidy.iter().all(|k| k.interp == oa_params::Interp::Hold), "hard cuts");
    }
}
