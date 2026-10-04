//! The Transitions tab's two ends: where another clip touches the selected one (on its
//! track, end to start), each end toggles between the clip's own **intro/outro** and a
//! **transition** with that neighbor — mixing one picture into the other over the cut
//! (`Item::transition_in` of the clip after it: DESIGN.md §16), the sound crossfading
//! with it. A transition on the left edge belongs to this clip; one on the right edge to
//! the next clip, so both clips show the same transition.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{ClipEnd, ItemId, Op, ParamTarget};
use oa_time::Time;

/// What a new transition is, and how long.
const DEFAULT_TRANSITION: &str = "oa.transition.crossfade";
const DEFAULT_SECONDS: f64 = 0.5;

impl App {
    /// The clip touching `item` at `end` on its track: the one ending exactly where it
    /// starts (Head), or starting exactly where it ends (Tail).
    fn touching(&self, item: ItemId, end: ClipEnd) -> Option<ItemId> {
        let s = self.editor.sequence();
        let (t, i) = s.find_item(item)?;
        let track = &s.tracks[t];
        let it = &track.items[i];
        match end {
            ClipEnd::Head => i.checked_sub(1).map(|k| &track.items[k]).filter(|p| p.range.end() == it.range.start).map(|p| p.id),
            ClipEnd::Tail => track.items.get(i + 1).filter(|n| n.range.start == it.range.end()).map(|n| n.id),
        }
    }

    /// One end of the Transitions tab: the intro (or outro) list — or, where a clip
    /// touches that end, a choice between it and a transition with that clip.
    pub(crate) fn transition_end(&mut self, ui: &mut egui::Ui, item: ItemId, intro: bool, t: Time) {
        let end = if intro { ClipEnd::Head } else { ClipEnd::Tail };
        let Some(neighbor) = self.touching(item, end) else {
            self.in_out_section(ui, item, intro, t);
            return;
        };
        // The cut's transition sits on the head of the clip after it.
        let owner = if intro { item } else { neighbor };
        let existing = self.editor.item(owner).and_then(|i| i.transition_in.clone());
        let name = self.editor.item(neighbor).map_or_else(String::new, |i| i.name.clone());
        let (own, cut) = if intro { ("Intro", trf("Transition from “{name}”", &[("name", &(name).to_string())])) } else { ("Outro", trf("Transition into “{name}”", &[("name", &(name).to_string())])) };
        let mut transition = existing.is_some();
        ui.horizontal(|ui| {
            let tip_own = if intro { tr("The clip arrives on its own: intro effects over its first seconds") } else { tr("The clip leaves on its own: outro effects over its last seconds") };
            if ui.selectable_label(!transition, own).on_hover_text(tip_own).clicked() {
                transition = false;
            }
            let tip_cut = tr("Mix one clip into the other over the cut — the picture and the sound — using footage past both edit points");
            if ui.selectable_label(transition, cut).on_hover_text(tip_cut).clicked() {
                transition = true;
            }
        });
        let seq = self.editor.seq;
        match (existing.is_some(), transition) {
            (false, true) => {
                let made = oa_edit::timeline::set_transition(self.editor.doc.project(), seq, owner, ClipEnd::Head, DEFAULT_TRANSITION, Time::from_seconds_f64(DEFAULT_SECONDS));
                match made {
                    Ok(ops) => self.apply_or_report("Add transition", ops),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            (true, false) => self.apply_or_report("Remove transition", vec![Op::SetTransition { seq, item: owner, end: ClipEnd::Head, transition: None }]),
            _ => {}
        }
        if !transition {
            self.in_out_section(ui, item, intro, t);
            return;
        }
        let Some(cut) = self.editor.item(owner).and_then(|i| i.transition_in.clone()) else { return };
        // Which transition.
        let kinds: Vec<(String, String)> = self.registry.transitions().map(|d| (d.type_id.to_string(), d.name.clone())).collect();
        let current = kinds.iter().find(|k| k.0 == cut.type_id).map_or_else(|| cut.type_id.clone(), |k| k.1.clone());
        let mut chosen = cut.type_id.clone();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("cut-transition", owner.0)).selected_text(&current).show_ui(ui, |ui| {
                for (id, name) in &kinds {
                    ui.selectable_value(&mut chosen, id.clone(), name);
                }
            });
            // How long: centered on the cut (half on each side).
            let mut seconds = cut.duration.as_seconds_f64();
            let r = ui.add(egui::DragValue::new(&mut seconds).speed(0.02).range(0.05..=30.0).suffix(" s")).on_hover_text(tr("How long it lasts, centered on the cut"));
            if r.changed() {
                match oa_edit::timeline::set_transition(self.editor.doc.project(), seq, owner, ClipEnd::Head, &cut.type_id, Time::from_seconds_f64(seconds)) {
                    Ok(ops) => {
                        if let Err(e) = self.editor.apply_drag("Transition length", "cut-transition-length", ops) {
                            self.error = Some(e.to_string());
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            if r.drag_stopped() || r.lost_focus() {
                self.editor.doc.seal();
            }
        });
        if chosen != cut.type_id {
            match oa_edit::timeline::set_transition(self.editor.doc.project(), seq, owner, ClipEnd::Head, &chosen, cut.duration) {
                Ok(ops) => self.apply_or_report("Change transition", ops),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        // Its settings (angle, color, softness…), keyframable.
        if let Some(d) = self.registry.effect(&cut.type_id).cloned() {
            for s in d.params.iter() {
                self.param_widget(ui, owner, &ParamTarget::Transition(ClipEnd::Head), s, t, "cut-transition");
            }
        }
        // It borrows footage from past both edit points; a clip without any is held on
        // its last (or first) frame there.
        let short = [(neighbor, !intro), (item, intro)].into_iter().any(|(id, head)| self.lacks_handle(id, head, cut.duration));
        if short {
            ui.label(egui::RichText::new(tr("One of the clips has no footage past its edit point for this: it holds on its end frame there. Trim it a little to give it some.")).small().color(crate::style::GOLD));
        }
        let own_effects = self.editor.item(item).map_or(0, |i| {
            i.effects.iter().filter(|e| if intro { matches!(e.role, oa_doc::EffectRole::In { .. }) } else { matches!(e.role, oa_doc::EffectRole::Out { .. }) }).count()
        });
        if own_effects > 0 {
            let what = if intro { "intro" } else { "outro" };
            ui.label(egui::RichText::new(trf("The clip's {own_effects} {what} effect{0} still play too (switch back to see them).", &[("own_effects", &(own_effects).to_string()), ("what", (what)), ("0", (if own_effects == 1 { "" } else { "s" }))])).small().weak());
        }
    }

    /// Whether clip `item` lacks the footage a cut transition of `duration` borrows past
    /// its edit point — before its start (`head`) or after its end. Stills, titles,
    /// solids and compounds have as much as they need.
    fn lacks_handle(&self, item: ItemId, head: bool, duration: Time) -> bool {
        let Some(it) = self.editor.item(item) else { return false };
        let oa_doc::ItemKind::Media { media } = it.kind else { return false };
        let Some(pool) = self.editor.pool_item(media) else { return false };
        if pool.kind == oa_media::MediaKind::Still {
            return false;
        }
        let speed = it.time_map.speed.num() as f64 / it.time_map.speed.den() as f64;
        if speed <= 0.0 {
            return false;
        }
        let half = duration.as_seconds_f64() / 2.0 * speed;
        let (from, to) = (it.time_map.source_in.as_seconds_f64(), it.time_map.source_time(it.range.duration).as_seconds_f64());
        if head { from < half } else { pool.probe.duration.as_seconds_f64() - to < half }
    }
}
