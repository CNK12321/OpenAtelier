//! Effect groups, the inspector's side (the model is `oa_doc::EffectGroup`).
//!
//! A group is a card heading some of a clip's effects, which sit together in its list:
//! its grip (drag it up or down the list to move the whole group, onto a clip to copy
//! it there, onto the Assets tab to keep it as a preset), a chevron to collapse it, its
//! switch, its name and its **mix** — keyframable, the group's result over the picture
//! or sound that went in. Each of its effects' properties has a circle beside it:
//! ticked, it's one of the few the collapsed group still shows. Right-click the head to
//! rename, lock, ungroup or save the group. A locked group stays collapsed and its
//! effects can't be changed — only its mix, its switch and the ticked properties.
//!
//! Effects join a group from their right-click menu, or by being dragged between two of
//! its effects or onto its head.

use crate::i18n::{tr, trf};
use crate::App;
use eframe::egui;
use oa_doc::{EffectGroup, EffectId, EffectInstance, ItemId, Op, ParamTarget};
use oa_time::Time;
use std::collections::HashMap;

/// The effect lists the inspector's tabs show.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Listed {
    /// Passive picture effects (the Effects tab).
    Effects,
    /// Sound effects (the Sound tab).
    Sound,
    /// Intros and outros (the Transitions tab).
    Transitions,
}

/// Where moved effects go in a clip's list.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Anchor {
    Before(EffectId),
    After(EffectId),
    End,
}

/// What a group's head did this frame.
pub(crate) struct GroupHead {
    /// The head, as drawn (a drop target: effects let go on it join the group).
    pub rect: egui::Rect,
    /// Its grip started a drag.
    pub drag_started: bool,
}

impl App {
    /// Ops putting copies of `effects` on `item`, starting at `index` of its list: fresh
    /// ids (a Duplicate's own effects stay tied to it), and the groups they're in
    /// (`groups`) added with ids its own groups don't use.
    pub(crate) fn insert_effects_ops(&mut self, item: ItemId, effects: &[EffectInstance], groups: &[EffectGroup], index: usize) -> Vec<Op> {
        let Some(it) = self.editor.item(item).cloned() else { return Vec::new() };
        let seq = self.editor.seq;
        let mut kept = it.effect_groups.clone();
        let mut renamed = HashMap::new();
        for (next, g) in (it.free_group_id()..).zip(groups.iter().filter(|g| effects.iter().any(|e| e.group == Some(g.id)))) {
            renamed.insert(g.id, next);
            kept.push(EffectGroup { id: next, ..g.clone() });
        }
        let mut ops = Vec::new();
        if kept.len() != it.effect_groups.len() {
            ops.push(Op::SetEffectGroups { seq, item, groups: kept });
        }
        let ids: HashMap<EffectId, EffectId> = effects.iter().map(|fx| (fx.id, EffectId(self.editor.doc.alloc_id()))).collect();
        for (k, fx) in effects.iter().enumerate() {
            let effect = EffectInstance { id: ids[&fx.id], on_duplicate: fx.on_duplicate.and_then(|d| ids.get(&d).copied()), group: fx.group.and_then(|g| renamed.get(&g).copied()), ..fx.clone() };
            ops.push(Op::InsertEffect { seq, item, index: index + k, effect });
        }
        ops
    }

    /// Ops moving `moved` (some of `item`'s effects, as they should be once moved —
    /// their group, say) together to `at`, in the order given.
    pub(crate) fn place_ops(&self, item: ItemId, moved: &[EffectInstance], at: Anchor) -> Vec<Op> {
        self.editor.item(item).map(|it| place_ops(self.editor.seq, it, moved, at)).unwrap_or_default()
    }

    /// One of `item`'s groups, changed by `change` (one undo step, `label`).
    pub(crate) fn change_group(&mut self, item: ItemId, group: u64, label: &str, change: impl FnOnce(&mut EffectGroup)) {
        let Some(it) = self.editor.item(item) else { return };
        let mut groups = it.effect_groups.clone();
        let Some(g) = groups.iter_mut().find(|g| g.id == group) else { return };
        change(g);
        let op = Op::SetEffectGroups { seq: self.editor.seq, item, groups };
        self.apply_or_report(label, vec![op]);
    }

    /// A new group on `item` holding `effect`, named "Group 1", "Group 2"…
    pub(crate) fn new_group(&mut self, item: ItemId, effect: EffectId) {
        let Some(it) = self.editor.item(item) else { return };
        let id = it.free_group_id();
        let n = (1..).find(|n| !it.effect_groups.iter().any(|g| g.name == trf("Group {n}", &[("n", &n.to_string())]))).unwrap_or(1);
        let mut groups = it.effect_groups.clone();
        groups.push(EffectGroup::new(id, &trf("Group {n}", &[("n", &n.to_string())])));
        let seq = self.editor.seq;
        self.apply_or_report(tr("Group effect"), vec![Op::SetEffectGroups { seq, item, groups }, Op::SetEffectGroup { seq, item, effect, group: Some(id) }]);
    }

    /// Moves `effect` into `group`, after its last effect.
    pub(crate) fn add_to_group(&mut self, item: ItemId, effect: EffectId, group: u64) {
        let Some(it) = self.editor.item(item) else { return };
        let Some(fx) = it.effects.iter().find(|e| e.id == effect).cloned() else { return };
        let last = it.effects.iter().rev().find(|e| e.group == Some(group) && e.id != effect).map(|e| e.id);
        let moved = EffectInstance { group: Some(group), ..fx };
        let ops = self.place_ops(item, &[moved], last.map_or(Anchor::End, Anchor::After));
        self.apply_or_report(tr("Add to group"), ops);
    }

    /// Takes `effect` out of its group, to just after the group.
    pub(crate) fn take_out_of_group(&mut self, item: ItemId, effect: EffectId) {
        let Some(it) = self.editor.item(item) else { return };
        let Some(fx) = it.effects.iter().find(|e| e.id == effect).cloned() else { return };
        let Some(group) = fx.group else { return };
        let last = it.effects.iter().rev().find(|e| e.group == Some(group) && e.id != effect).map(|e| e.id);
        let moved = EffectInstance { group: None, ..fx };
        let ops = self.place_ops(item, &[moved], last.map_or(Anchor::End, Anchor::After));
        self.apply_or_report(tr("Take out of group"), ops);
    }

    /// Ungroups `group`: its effects stay where they are, on their own.
    pub(crate) fn ungroup(&mut self, item: ItemId, group: u64) {
        let Some(it) = self.editor.item(item) else { return };
        let seq = self.editor.seq;
        let mut ops: Vec<Op> = it.effects.iter().filter(|e| e.group == Some(group)).map(|e| Op::SetEffectGroup { seq, item, effect: e.id, group: None }).collect();
        ops.push(Op::SetEffectGroups { seq, item, groups: it.effect_groups.iter().filter(|g| g.id != group).cloned().collect() });
        self.apply_or_report(tr("Ungroup effects"), ops);
    }

    /// Removes `group` and its effects (a Duplicate's own effects with it).
    pub(crate) fn remove_group(&mut self, item: ItemId, group: u64) {
        let Some(it) = self.editor.item(item) else { return };
        let seq = self.editor.seq;
        let members: Vec<EffectId> = it.effects.iter().filter(|e| e.group == Some(group)).map(|e| e.id).collect();
        let mut ops: Vec<Op> = it.effects.iter().filter(|e| members.contains(&e.id) || e.on_duplicate.is_some_and(|d| members.contains(&d))).map(|e| Op::RemoveEffect { seq, item, effect: e.id }).collect();
        ops.push(Op::SetEffectGroups { seq, item, groups: it.effect_groups.iter().filter(|g| g.id != group).cloned().collect() });
        self.apply_or_report(tr("Remove group"), ops);
    }

    /// The effects picked on `item` that it still has, in its list's order.
    pub(crate) fn picks(&self, item: ItemId) -> Vec<EffectId> {
        let Some((owner, picked)) = &self.effect_picks else { return Vec::new() };
        let Some(it) = self.editor.item(item).filter(|_| *owner == item) else { return Vec::new() };
        it.effects.iter().map(|e| e.id).filter(|id| picked.contains(id)).collect()
    }

    /// A click on effect `id` of `item` (`shown`: the list's effects in order): alone it
    /// picks just that one (or, if it's all that's picked, nothing); Ctrl adds or drops
    /// it; Shift picks the run from the last one picked.
    pub(crate) fn pick_effect(&mut self, item: ItemId, shown: &[EffectId], id: EffectId, mods: egui::Modifiers) {
        let picked = next_picks(self.picks(item), shown, id, mods);
        self.effect_picks = (!picked.is_empty()).then_some((item, picked));
    }

    /// Picks `ids` on `item` (Ctrl: added to what's picked).
    pub(crate) fn pick_all(&mut self, item: ItemId, ids: &[EffectId], add: bool) {
        let mut picked = if add { self.picks(item) } else { Vec::new() };
        picked.extend(ids.iter().filter(|id| !picked.contains(id)).copied().collect::<Vec<_>>());
        self.effect_picks = (!picked.is_empty()).then_some((item, picked));
    }

    /// A click on a group's head picks the group: its effects (`ids`), like a card's click
    /// picks the card's (see [`group_click`]).
    pub(crate) fn pick_group(&mut self, item: ItemId, ids: &[EffectId], mods: egui::Modifiers) {
        let picked = group_click(self.picks(item), ids, mods);
        self.effect_picks = (!picked.is_empty()).then_some((item, picked));
    }

    /// The picked effects that can be changed (not in a locked group).
    fn free_picks(&self, item: ItemId) -> Vec<EffectInstance> {
        let picks = self.picks(item);
        let Some(it) = self.editor.item(item) else { return Vec::new() };
        it.effects.iter().filter(|e| picks.contains(&e.id) && it.group_of(e).is_none_or(|g| !g.locked)).cloned().collect()
    }

    /// A new group holding the picked effects, where the first of them was.
    pub(crate) fn group_picks(&mut self, item: ItemId) {
        let picked = self.free_picks(item);
        let Some(it) = self.editor.item(item).cloned() else { return };
        if picked.is_empty() {
            return;
        }
        // A group runs as one: intros with intros, outros with outros.
        if picked.iter().any(|e| std::mem::discriminant(&e.role) != std::mem::discriminant(&picked[0].role)) {
            self.notify(tr("Intros and outros can't share a group: pick only intros, or only outros."));
            return;
        }
        // And the clip's effects with the clip's, a copy's with that copy's.
        if picked.iter().any(|e| e.on_duplicate != picked[0].on_duplicate) {
            self.notify(tr("The clip's effects and a duplicate's can't share a group: pick only one list's."));
            return;
        }
        let id = it.free_group_id();
        let name = (1..).map(|n| trf("Group {n}", &[("n", &n.to_string())])).find(|n| !it.effect_groups.iter().any(|g| g.name == *n)).unwrap_or_default();
        let mut groups = it.effect_groups.clone();
        groups.push(EffectGroup::new(id, &name));
        // Where the first picked one is: after the last effect before it that isn't picked.
        let first = it.effects.iter().position(|e| e.id == picked[0].id).unwrap_or(0);
        let anchor = match it.effects[..first].iter().rev().find(|e| !picked.iter().any(|p| p.id == e.id)) {
            Some(e) => Anchor::After(e.id),
            None => it.effects.iter().find(|e| !picked.iter().any(|p| p.id == e.id)).map_or(Anchor::End, |e| Anchor::Before(e.id)),
        };
        let moved: Vec<EffectInstance> = picked.into_iter().map(|e| EffectInstance { group: Some(id), ..e }).collect();
        let seq = self.editor.seq;
        let mut ops = vec![Op::SetEffectGroups { seq, item, groups }];
        ops.extend(place_ops(seq, &it, &moved, anchor));
        self.apply_or_report(tr("Group effects"), ops);
    }

    /// Takes the picked effects out of their groups, where they are.
    pub(crate) fn ungroup_picks(&mut self, item: ItemId) {
        let seq = self.editor.seq;
        let ops: Vec<Op> = self.free_picks(item).into_iter().filter(|e| e.group.is_some()).map(|e| Op::SetEffectGroup { seq, item, effect: e.id, group: None }).collect();
        self.apply_or_report(tr("Take out of group"), ops);
    }

    /// Turns the picked effects on or off.
    pub(crate) fn switch_picks(&mut self, item: ItemId, on: bool) {
        let seq = self.editor.seq;
        let ops: Vec<Op> = self.free_picks(item).into_iter().filter(|e| e.enabled != on).map(|e| Op::SetEffectEnabled { seq, item, effect: e.id, enabled: on }).collect();
        self.apply_or_report(tr("Toggle effects"), ops);
    }

    /// Removes the picked effects (a Duplicate's own effects with it), and groups left empty.
    /// A locked group's go too: locking keeps them from being changed, not from going.
    pub(crate) fn remove_picks(&mut self, item: ItemId) {
        let picked: Vec<EffectId> = self.picks(item);
        let Some(it) = self.editor.item(item) else { return };
        let seq = self.editor.seq;
        let mut ops: Vec<Op> = it.effects.iter().filter(|e| picked.contains(&e.id) || e.on_duplicate.is_some_and(|d| picked.contains(&d))).map(|e| Op::RemoveEffect { seq, item, effect: e.id }).collect();
        let left: Vec<EffectGroup> = it.effect_groups.iter().filter(|g| it.effects.iter().any(|e| e.group == Some(g.id) && !picked.contains(&e.id))).cloned().collect();
        if left.len() != it.effect_groups.len() {
            ops.push(Op::SetEffectGroups { seq, item, groups: left });
        }
        self.effect_picks = None;
        self.apply_or_report(tr("Remove effects"), ops);
    }

    /// Copies the picked effects (with the groups they fill wholly).
    pub(crate) fn copy_picks(&mut self, item: ItemId) {
        let picks = self.picks(item);
        let Some(it) = self.editor.item(item) else { return };
        let whole = |g: u64| it.effects.iter().filter(|e| e.group == Some(g)).all(|e| picks.contains(&e.id));
        let groups: Vec<EffectGroup> = it.effect_groups.iter().filter(|g| whole(g.id)).cloned().collect();
        self.effect_clipboard = it.effects.iter().filter(|e| picks.contains(&e.id)).map(|e| EffectInstance { group: e.group.filter(|g| groups.iter().any(|x| x.id == *g)), ..e.clone() }).collect();
        self.effect_clipboard_groups = groups;
        self.effect_clipboard_reverse = None;
        let n = self.effect_clipboard.len();
        self.notify(trf("Copied {n} effects.", &[("n", &n.to_string())]));
    }

    /// Which effect list the open tab shows (for the tools beside it and its keys).
    pub(crate) fn tab_list(&self) -> Option<Listed> {
        match self.inspector_tab {
            crate::inspector::Tab::Effects => Some(Listed::Effects),
            crate::inspector::Tab::Sound => Some(Listed::Sound),
            crate::inspector::Tab::Transitions => Some(Listed::Transitions),
            _ => None,
        }
    }

    /// The clip whose effect list the pointer is over, and which list, as last drawn.
    pub(crate) fn effects_under_pointer(&self, ctx: &egui::Context) -> Option<(ItemId, Listed)> {
        let item = self.selection.filter(|i| self.editor.item(*i).is_some())?;
        let list = self.tab_list()?;
        let p = ctx.input(|i| i.pointer.latest_pos())?;
        self.effect_list_rect.filter(|r| r.contains(p)).map(|_| (item, list))
    }

    /// The effects a tab lists on `item`, in order: its passive picture effects, its
    /// sound effects, or its intros and outros.
    pub(crate) fn listed_effects(&self, item: ItemId, list: Listed) -> Vec<EffectId> {
        let Some(it) = self.editor.item(item) else { return Vec::new() };
        it.effects
            .iter()
            .filter(|e| {
                let sound = self.registry.is_sound(&e.type_id);
                e.on_duplicate.is_none()
                    && match list {
                        Listed::Effects => !sound && e.role == oa_doc::EffectRole::Passive,
                        Listed::Sound => sound,
                        Listed::Transitions => !sound && matches!(e.role, oa_doc::EffectRole::In { .. } | oa_doc::EffectRole::Out { .. }),
                    }
            })
            .map(|e| e.id)
            .collect()
    }

    /// Moves the picked effects one place up or down their list, together. An effect
    /// stays in its group while it moves among that group's effects; a group they aren't
    /// in is stepped over whole, never split.
    pub(crate) fn move_picks(&mut self, item: ItemId, up: bool) {
        let picked = self.free_picks(item);
        let Some(it) = self.editor.item(item).cloned() else { return };
        let class = |e: &EffectInstance| (self.registry.is_sound(&e.type_id), std::mem::discriminant(&e.role), e.on_duplicate);
        let Some((moved, anchor)) = step_run(&it, &picked, up, &|a, b| class(a) == class(b)) else { return };
        let ops = place_ops(self.editor.seq, &it, &moved, anchor);
        self.apply_or_report(if up { tr("Move effects up") } else { tr("Move effects down") }, ops);
    }

    /// Copies of the picked effects (with the groups they fill wholly) on the other
    /// selected clips they fit.
    pub(crate) fn apply_picks_to_selected(&mut self, item: ItemId) {
        let picks = self.picks(item);
        let Some(it) = self.editor.item(item).cloned() else { return };
        let whole = |g: u64| it.effects.iter().filter(|e| e.group == Some(g)).all(|e| picks.contains(&e.id));
        let groups: Vec<EffectGroup> = it.effect_groups.iter().filter(|g| whole(g.id)).cloned().collect();
        let effects: Vec<EffectInstance> = it.effects.iter().filter(|e| picks.contains(&e.id)).map(|e| EffectInstance { group: e.group.filter(|g| groups.iter().any(|x| x.id == *g)), ..e.clone() }).collect();
        let mut ops = Vec::new();
        let mut n = 0;
        for target in self.selected_clips().into_iter().filter(|t| *t != item) {
            let fitting: Vec<EffectInstance> = effects.iter().filter(|e| self.effect_fits(target, &e.type_id)).cloned().collect();
            if fitting.is_empty() {
                continue;
            }
            let at = self.editor.item(target).map_or(0, |t| t.effects.len());
            ops.extend(self.insert_effects_ops(target, &fitting, &groups, at));
            n += 1;
        }
        if n == 0 {
            self.notify(tr("None of the picked effects fit the other selected clips."));
            return;
        }
        self.apply_or_report(tr("Apply effects to selected clips"), ops);
        self.notify(trf("Put the picked effects on {n} more clip{0}.", &[("n", &n.to_string()), ("0", if n == 1 { "" } else { "s" })]));
    }

    /// Folds every card of the list (or, `false`, opens them all).
    pub(crate) fn fold_all(&mut self, item: ItemId, list: Listed, fold: bool) {
        let ids = self.listed_effects(item, list);
        if fold {
            self.folded_effects.extend(ids);
        } else {
            self.folded_effects.retain(|id| !ids.contains(id));
        }
    }

    /// The right-click menu of a picked card while several are picked: for all of them.
    pub(crate) fn picks_menu(&mut self, ui: &mut egui::Ui, item: ItemId) {
        let picks = self.picks(item);
        let n = picks.len().to_string();
        let Some(it) = self.editor.item(item) else { return };
        let all_on = it.effects.iter().filter(|e| picks.contains(&e.id)).all(|e| e.enabled);
        let grouped = it.effects.iter().any(|e| picks.contains(&e.id) && e.group.is_some());
        let others = self.selected_clips().iter().any(|c| *c != item);
        ui.label(egui::RichText::new(trf("{n} effects picked", &[("n", &n)])).strong());
        ui.separator();
        if crate::icons::menu_item(ui, Some(crate::icons::GROUP), tr("Group them"), "Ctrl+G", true).clicked() {
            self.group_picks(item);
            ui.close();
        }
        if crate::icons::menu_item(ui, Some(crate::icons::UNGROUP), tr("Take them out of their groups"), "Ctrl+Shift+G", grouped).clicked() {
            self.ungroup_picks(item);
            ui.close();
        }
        let (icon, label) = if all_on { (crate::icons::HIDDEN, tr("Turn them off")) } else { (crate::icons::VISIBLE, tr("Turn them on")) };
        if crate::icons::menu_item(ui, Some(icon), label, "", true).clicked() {
            self.switch_picks(item, !all_on);
            ui.close();
        }
        if crate::icons::menu_item(ui, Some(crate::icons::ARROW_UP), tr("Move up"), "Alt+↑", true).clicked() {
            self.move_picks(item, true);
            ui.close();
        }
        if crate::icons::menu_item(ui, Some(crate::icons::ARROW_DOWN), tr("Move down"), "Alt+↓", true).clicked() {
            self.move_picks(item, false);
            ui.close();
        }
        ui.separator();
        if crate::icons::menu_item(ui, Some(crate::icons::DUPLICATE), tr("Copy them"), "Ctrl+C", true).clicked() {
            self.copy_picks(item);
            ui.close();
        }
        if crate::icons::menu_item(ui, Some(crate::icons::APPLY_TO_SELECTED), tr("Put them on the other selected clips"), "", others).clicked() {
            self.apply_picks_to_selected(item);
            ui.close();
        }
        if crate::icons::menu_item(ui, Some(crate::icons::ASSETS), tr("Save to assets…"), "", true).clicked() {
            self.save_preset_from(item, None, picks.clone());
            ui.close();
        }
        ui.separator();
        if crate::icons::menu_item(ui, Some(crate::icons::DELETE), &trf("Remove {n} effects", &[("n", &n)]), "Del", true).clicked() {
            self.remove_picks(item);
            ui.close();
        }
    }

    /// The effect tools, standing down the side of the effect list (and staying put while
    /// it scrolls), like the mask tools beside the viewer: how many are picked (click to
    /// pick none), then group · switch and order · copy, paste, put on the other selected
    /// clips, save · fold and filter · remove. What needs a pick is dimmed without one.
    pub(crate) fn effect_strip(&mut self, ui: &mut egui::Ui, area: egui::Rect, item: ItemId, list: Listed) {
        #[derive(Copy, Clone, PartialEq)]
        enum Act {
            Clear,
            Group,
            Ungroup,
            Switch,
            Up,
            Down,
            Copy,
            Paste,
            Apply,
            Save,
            Fold,
            OnlyPicked,
            Remove,
        }
        /// What it does, its icon, its tip and keys, whether it can now, whether it's lit.
        type Button<'a> = (Act, crate::icons::Icon, &'a str, &'a str, bool, bool);
        const BUTTON: f32 = 28.0;
        const PAD: f32 = 3.0;
        const BREAK: f32 = 9.0;
        let picks = self.picks(item);
        let Some(it) = self.editor.item(item) else { return };
        let picked: Vec<&EffectInstance> = it.effects.iter().filter(|e| picks.contains(&e.id)).collect();
        let some = !picked.is_empty();
        let all_on = picked.iter().all(|e| e.enabled);
        let grouped = picked.iter().any(|e| e.group.is_some());
        let others = self.selected_clips().iter().any(|c| *c != item);
        let listed = self.listed_effects(item, list);
        let all_folded = !listed.is_empty() && listed.iter().all(|id| self.folded_effects.contains(id));
        let n = picks.len();
        let (switch_icon, switch_tip) = if some && all_on { (crate::icons::HIDDEN, tr("Turn the picked effects off")) } else { (crate::icons::VISIBLE, tr("Turn the picked effects on")) };
        let (fold_icon, fold_tip) = if all_folded { (crate::icons::EXPAND_ALL, tr("Open every card")) } else { (crate::icons::COLLAPSE_ALL, tr("Fold every card to its header")) };
        let groups: [&[Button]; 5] = [
            &[
                (Act::Group, crate::icons::GROUP, tr("Group the picked effects"), "Ctrl+G", some, false),
                (Act::Ungroup, crate::icons::UNGROUP, tr("Take the picked effects out of their groups"), "Ctrl+Shift+G", grouped, false),
            ],
            &[
                (Act::Switch, switch_icon, switch_tip, "", some, false),
                (Act::Up, crate::icons::ARROW_UP, tr("Move the picked effects up"), "Alt+↑", some, false),
                (Act::Down, crate::icons::ARROW_DOWN, tr("Move the picked effects down"), "Alt+↓", some, false),
            ],
            &[
                (Act::Copy, crate::icons::DUPLICATE, tr("Copy the picked effects"), "Ctrl+C", some, false),
                (Act::Paste, crate::icons::PASTE, tr("Paste copied effects onto this clip"), "Ctrl+V", !self.effect_clipboard.is_empty(), false),
                (Act::Apply, crate::icons::APPLY_TO_SELECTED, tr("Put the picked effects on the other selected clips"), "", some && others, false),
                (Act::Save, crate::icons::ASSETS, tr("Save the picked effects to assets, as one preset"), "", some, false),
            ],
            &[
                (Act::Fold, fold_icon, fold_tip, "", !listed.is_empty(), false),
                (Act::OnlyPicked, crate::icons::ONLY_PICKED, tr("Show only the picked effects"), "", some || self.only_picked, self.only_picked),
            ],
            &[(Act::Remove, crate::icons::DELETE, tr("Remove the picked effects"), "Del", some, false)],
        ];
        let count: usize = groups.iter().map(|g| g.len()).sum();
        let height = (count + 1) as f32 * BUTTON + groups.len() as f32 * BREAK + 2.0 * PAD;
        let strip = egui::Rect::from_min_size(area.min, egui::vec2(BUTTON + 2.0 * PAD, height.min(area.height())));
        let v = ui.visuals().clone();
        ui.painter().rect(strip, 8.0, v.window_fill, egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
        ui.interact(strip, ui.id().with("effect-strip"), egui::Sense::click_and_drag());
        let mut act = None;
        // How many are picked: a badge (click it to pick none).
        let badge = egui::Rect::from_min_size(egui::pos2(strip.left() + PAD, strip.top() + PAD), egui::vec2(BUTTON, BUTTON));
        let r = ui.interact(badge, ui.id().with("effect-strip-count"), egui::Sense::click());
        if some {
            ui.painter().circle_filled(badge.center(), 10.0, if r.hovered() { crate::style::ACCENT.gamma_multiply(0.8) } else { crate::style::ACCENT });
            let text = if r.hovered() { "✕".to_string() } else { n.to_string() };
            centered_text(ui.painter(), badge.center(), &text, egui::Color32::WHITE);
            if r.on_hover_text(tr("Picked effects: click to pick none (Esc)")).clicked() {
                act = Some(Act::Clear);
            }
        } else {
            ui.painter().circle_stroke(badge.center(), 9.0, egui::Stroke::new(1.0, v.weak_text_color()));
            centered_text(ui.painter(), badge.center(), "0", v.weak_text_color());
            r.on_hover_text(tr("Click an effect to pick it (Ctrl+click for more, Shift+click for a run); these tools act on the picked ones"));
        }
        let mut y = badge.bottom();
        for group in groups {
            let line_y = y + BREAK / 2.0 - 0.5;
            ui.painter().line_segment([egui::pos2(strip.left() + 6.0, line_y), egui::pos2(strip.right() - 6.0, line_y)], egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color));
            y += BREAK;
            for &(a, icon, tip, keys, enabled, on) in group {
                let rect = egui::Rect::from_min_size(egui::pos2(strip.left() + PAD, y), egui::vec2(BUTTON, BUTTON));
                y += BUTTON;
                if rect.bottom() > strip.bottom() {
                    continue;
                }
                let r = ui.interact(rect, ui.id().with(("effect-strip", a as u8)), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
                if on {
                    ui.painter().rect_filled(rect, 6.0, crate::style::ACCENT);
                } else if enabled && r.hovered() {
                    ui.painter().rect_filled(rect, 6.0, v.widgets.hovered.weak_bg_fill);
                }
                let color = if on {
                    egui::Color32::WHITE
                } else if !enabled {
                    v.weak_text_color().gamma_multiply(0.6)
                } else if r.hovered() {
                    v.strong_text_color()
                } else {
                    v.text_color()
                };
                crate::icons::paint(ui.painter(), rect.shrink(6.0), icon, color);
                let tip = if keys.is_empty() { tip.to_string() } else { format!("{tip}  ·  {keys}") };
                if r.on_hover_text(tip).clicked() {
                    act = Some(a);
                }
            }
        }
        match act {
            Some(Act::Clear) => self.effect_picks = None,
            Some(Act::Group) => self.group_picks(item),
            Some(Act::Ungroup) => self.ungroup_picks(item),
            Some(Act::Switch) => self.switch_picks(item, !all_on),
            Some(Act::Up) => self.move_picks(item, true),
            Some(Act::Down) => self.move_picks(item, false),
            Some(Act::Copy) => self.copy_picks(item),
            Some(Act::Paste) => self.paste_effects(&[item]),
            Some(Act::Apply) => self.apply_picks_to_selected(item),
            Some(Act::Save) => self.save_preset_from(item, None, picks),
            Some(Act::Fold) => self.fold_all(item, list, !all_folded),
            Some(Act::OnlyPicked) => self.only_picked = !self.only_picked,
            Some(Act::Remove) => self.remove_picks(item),
            None => {}
        }
    }

    /// Keys while the pointer is over the effect list (with `item` selected; `sound`: the
    /// sound effects): Ctrl+A picks them all; with some picked, Del removes them, Ctrl+G
    /// groups them (Ctrl+Shift+G takes them out), Alt+↑/↓ moves them and Esc picks none.
    /// (Ctrl+C and Ctrl+V come as events: [`App::effect_clipboard_event`].) Returns whether
    /// a key was taken.
    pub(crate) fn effect_keys(&mut self, ctx: &egui::Context, item: ItemId, list: Listed) -> bool {
        use egui::{Key, Modifiers as M};
        let pressed = |m: M, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(M::COMMAND, Key::A) {
            let ids = self.listed_effects(item, list);
            self.pick_all(item, &ids, false);
            return true;
        }
        if self.picks(item).is_empty() {
            return false;
        }
        if pressed(M::NONE, Key::Delete) || pressed(M::NONE, Key::Backspace) {
            self.remove_picks(item);
        } else if pressed(M::COMMAND | M::SHIFT, Key::G) {
            self.ungroup_picks(item);
        } else if pressed(M::COMMAND, Key::G) {
            self.group_picks(item);
        } else if pressed(M::ALT, Key::ArrowUp) {
            self.move_picks(item, true);
        } else if pressed(M::ALT, Key::ArrowDown) {
            self.move_picks(item, false);
        } else if pressed(M::NONE, Key::Escape) {
            self.effect_picks = None;
        } else {
            return false;
        }
        true
    }

    /// `group`'s effects on `item`, in order.
    pub(crate) fn group_members(&self, item: ItemId, group: u64) -> Vec<EffectInstance> {
        self.editor.item(item).map(|it| it.effects.iter().filter(|e| e.group == Some(group)).cloned().collect()).unwrap_or_default()
    }

    /// The head of `group`'s panel (the effect list draws the panel around it and its
    /// effects): grip, chevron (or the lock), its name — click to pick its effects,
    /// double-click to rename — and how many effects; its ⋮ menu and switch on the right;
    /// then its mix. Collapsed, its effects' names and the properties they have ticked.
    pub(crate) fn group_head(&mut self, ui: &mut egui::Ui, item: ItemId, group: &EffectGroup, dragging: bool, t: Time) -> GroupHead {
        let members = self.group_members(item, group.id);
        let folded = group.collapsed || group.locked;
        let rename_key = egui::Id::new(("group-rename", item.0, group.id));
        let mut drag_started = false;
        let mut name_clicked = false;
        let inner = ui.push_id(("effect-group", group.id), |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 6, bottom: 2 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let head = ui.horizontal(|ui| {
                    let (grip, gr) = ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::drag());
                    let dot = if gr.hovered() || dragging { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                    for y in [-5.0, 0.0, 5.0] {
                        for dx in [-2.5, 2.5] {
                            ui.painter().circle_filled(grip.center() + egui::vec2(dx, y), 1.4, dot);
                        }
                    }
                    let gr = gr.on_hover_text(tr("Drag to move the group, onto another clip to copy it there, or onto Assets to keep it")).on_hover_cursor(egui::CursorIcon::Grab);
                    drag_started = gr.drag_started();
                    if group.locked {
                        crate::icons::button(ui, crate::icons::LOCK, tr("Locked: right-click to unlock"), "", true);
                    } else {
                        let tip = if folded { tr("Show its effects") } else { tr("Collapse to its mix and ticked properties") };
                        let icon = if folded { crate::icons::STEP_ON } else { crate::icons::EXPAND };
                        if crate::icons::button(ui, icon, tip, "", true).clicked() {
                            let label = if folded { tr("Expand group") } else { tr("Collapse group") };
                            self.change_group(item, group.id, label, |g| g.collapsed = !folded);
                        }
                    }
                    let color = if group.enabled { crate::style::ACCENT } else { ui.visuals().weak_text_color() };
                    let (icon, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    crate::icons::paint(ui.painter(), icon, crate::icons::LAYERS, color);
                    // Its switch and menu, on the right; the name takes the room left.
                    let mut on = group.enabled;
                    let mut menu = None;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.checkbox(&mut on, "").on_hover_text(if on { tr("Turn the group off (its mix is kept)") } else { tr("Turn the group on") }).changed() {
                            self.change_group(item, group.id, tr("Toggle group"), |g| g.enabled = on);
                        }
                        menu = Some(crate::icons::button(ui, crate::icons::MORE, tr("Rename, lock, save, ungroup…"), "", true));
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let editing: Option<String> = ui.data(|d| d.get_temp(rename_key));
                            match editing {
                                Some(mut name) => {
                                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(ui.available_width().min(180.0)));
                                    if !r.has_focus() && !r.lost_focus() {
                                        r.request_focus();
                                    }
                                    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                    if r.lost_focus() || cancel {
                                        ui.data_mut(|d| d.remove::<String>(rename_key));
                                        let name = name.trim().to_string();
                                        if !cancel && !name.is_empty() && name != group.name {
                                            self.change_group(item, group.id, tr("Rename group"), |g| g.name = name);
                                        }
                                    } else {
                                        ui.data_mut(|d| d.insert_temp(rename_key, name));
                                    }
                                }
                                None => {
                                    let text = if group.enabled { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                                    let r = ui.add(egui::Label::new(egui::RichText::new(&group.name).strong().color(text)).truncate().sense(egui::Sense::click()));
                                    let r = r.on_hover_text(if group.locked { tr("Click to pick the group") } else { tr("Click to pick the group · double-click to rename") });
                                    if r.double_clicked() && !group.locked {
                                        ui.data_mut(|d| d.insert_temp(rename_key, group.name.clone()));
                                    } else if r.clicked() {
                                        name_clicked = true;
                                    }
                                    let n = members.len();
                                    let count = if group.enabled { trf("{n} effect{0}", &[("n", &n.to_string()), ("0", if n == 1 { "" } else { "s" })]) } else { tr("off").to_string() };
                                    ui.label(egui::RichText::new(count).small().weak());
                                }
                            }
                        });
                    });
                    if let Some(r) = menu {
                        egui::Popup::menu(&r).show(|ui| self.group_menu(ui, item, group, rename_key));
                    }
                });
                let head = head.response.interact(egui::Sense::click());
                // A click on its name or anywhere on its head picks the group (Ctrl or
                // Shift: with what's picked) — locked ones too, to remove or copy them.
                if name_clicked || head.clicked() {
                    let ids: Vec<EffectId> = members.iter().map(|e| e.id).collect();
                    let mods = ui.input(|i| i.modifiers);
                    self.pick_group(item, &ids, mods);
                }
                crate::widgets::context_menu(&head, |ui| self.group_menu(ui, item, group, rename_key));
                // The mix: keyframable, like any property.
                if let Some(schema) = oa_doc::schema::group().first() {
                    self.param_widget(ui, item, &ParamTarget::Group(group.id), schema, t, &format!("group-{}", group.id));
                }
                if folded {
                    // What's in it, then the properties its effects have ticked.
                    let names: Vec<String> = members.iter().map(|e| self.effect_name(&e.type_id)).collect();
                    ui.add(egui::Label::new(egui::RichText::new(names.join(" · ")).small().weak()).truncate());
                    self.group_shown(ui, item, &members, t);
                }
            })
            .response
            .rect
        });
        // Picked (all its effects): outlined, like a picked card.
        let picks = self.picks(item);
        if !members.is_empty() && members.iter().all(|e| picks.contains(&e.id)) {
            ui.painter().rect_stroke(inner.inner, 4.0, egui::Stroke::new(2.0, crate::style::ACCENT), egui::StrokeKind::Inside);
        }
        GroupHead { rect: inner.inner, drag_started }
    }

    /// A collapsed group's ticked properties, under each effect's name.
    fn group_shown(&mut self, ui: &mut egui::Ui, item: ItemId, members: &[EffectInstance], t: Time) {
        let mut any = false;
        for fx in members.iter().filter(|e| !e.shown.is_empty()) {
            let Some(d) = self.registry.effect(&fx.type_id).cloned() else { continue };
            let shown: Vec<&oa_params::ParamSchema> = d.params.iter().filter(|s| fx.shown.iter().any(|id| id == s.id.as_str())).collect();
            if shown.is_empty() {
                continue;
            }
            any = true;
            ui.label(egui::RichText::new(&d.name).small().weak());
            let target = ParamTarget::Effect(fx.id);
            for schema in shown {
                self.param_widget(ui, item, &target, schema, t, &fx.id.0.to_string());
            }
        }
        if !any && !members.is_empty() {
            ui.label(egui::RichText::new(tr("Tick the circle beside any of its effects' properties to show it here.")).small().weak());
        }
    }

    /// The circle beside a grouped effect's property: ticked, the collapsed group shows it.
    pub(crate) fn shown_circle(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &EffectInstance, param: &str) {
        let ticked = fx.shown.iter().any(|p| p == param);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::click());
        let c = rect.center();
        let stroke = if r.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
        if ticked {
            ui.painter().circle_filled(c, 5.5, crate::style::ACCENT);
            ui.painter().circle_filled(c, 2.2, egui::Color32::WHITE);
        } else {
            ui.painter().circle_stroke(c, 5.0, egui::Stroke::new(1.3, stroke));
        }
        let r = r.on_hover_text(if ticked { tr("Shown on the collapsed group: click to hide it there") } else { tr("Show this on the collapsed group") });
        if r.clicked() {
            let mut shown = fx.shown.clone();
            if ticked {
                shown.retain(|p| p != param);
            } else {
                shown.push(param.to_string());
            }
            let op = Op::SetEffectShown { seq: self.editor.seq, item, effect: fx.id, shown };
            self.apply_or_report(tr("Show on group"), vec![op]);
        }
    }

    /// A group head's right-click menu.
    fn group_menu(&mut self, ui: &mut egui::Ui, item: ItemId, group: &EffectGroup, rename_key: egui::Id) {
        if !group.locked && ui.button(tr("Rename")).clicked() {
            ui.data_mut(|d| d.insert_temp(rename_key, group.name.clone()));
            ui.close();
        }
        let lock = if group.locked { tr("Unlock") } else { tr("Lock") };
        let tip = tr("Locked, it stays collapsed and its effects can't be changed: only its mix, its switch and the ticked properties");
        if ui.button(lock).on_hover_text(tip).clicked() {
            let label = if group.locked { tr("Unlock group") } else { tr("Lock group") };
            let locked = !group.locked;
            self.change_group(item, group.id, label, |g| g.locked = locked);
            ui.close();
        }
        if ui.button(tr("Save to assets…")).on_hover_text(tr("Keeps it as a preset, for any clip in any project")).clicked() {
            self.save_preset_from(item, Some(group.id), Vec::new());
            ui.close();
        }
        if ui.button(tr("Copy group")).clicked() {
            self.effect_clipboard = self.group_members(item, group.id);
            self.effect_clipboard_groups = vec![group.clone()];
            self.effect_clipboard_reverse = None;
            ui.close();
        }
        ui.separator();
        if !group.locked && ui.button(tr("Ungroup")).on_hover_text(tr("Its effects stay, on their own")).clicked() {
            self.ungroup(item, group.id);
            ui.close();
        }
        // Locked or not: locking stops changes, not removing it.
        if ui.button(tr("Remove group and its effects")).clicked() {
            self.remove_group(item, group.id);
            ui.close();
        }
    }

    /// The group entries of an effect's right-click menu (its own clip's effects only).
    pub(crate) fn effect_group_menu(&mut self, ui: &mut egui::Ui, item: ItemId, fx: &EffectInstance) {
        let Some(it) = self.editor.item(item) else { return };
        if it.effects.iter().all(|e| e.id != fx.id) {
            return;
        }
        let groups = it.effect_groups.clone();
        ui.separator();
        match fx.group.and_then(|g| groups.iter().find(|x| x.id == g)) {
            Some(g) => {
                if ui.button(trf("Take out of {name}", &[("name", &g.name)])).clicked() {
                    self.take_out_of_group(item, fx.id);
                    ui.close();
                }
            }
            None => {
                if ui.button(tr("Group")).on_hover_text(tr("A new group holding it: add more effects, collapse it, mix it")).clicked() {
                    self.new_group(item, fx.id);
                    ui.close();
                }
                for g in groups.iter().filter(|g| !g.locked) {
                    if ui.button(trf("Add to {name}", &[("name", &g.name)])).clicked() {
                        self.add_to_group(item, fx.id, g.id);
                        ui.close();
                    }
                }
            }
        }
        if ui.button(tr("Save to assets…")).on_hover_text(tr("Keeps it as a preset, for any clip in any project")).clicked() {
            let picks = self.picks(item);
            let effects = if picks.len() > 1 && picks.contains(&fx.id) { picks } else { vec![fx.id] };
            self.save_preset_from(item, None, effects);
            ui.close();
        }
    }
}

/// The picked effects of clip `it` (`picked`, in list order) moved one place up or down
/// past the nearest effect of their list (`same_list`): as they'll be, and where. An
/// effect keeps its group while it moves among that group's effects; a group they
/// aren't in is stepped over whole. `None` at the end of the list.
fn step_run(it: &oa_doc::Item, picked: &[EffectInstance], up: bool, same_list: &dyn Fn(&EffectInstance, &EffectInstance) -> bool) -> Option<(Vec<EffectInstance>, Anchor)> {
    let (first, last) = (picked.first()?, picked.last()?);
    let is_picked = |e: &EffectInstance| picked.iter().any(|p| p.id == e.id);
    let at = |id: EffectId| it.effects.iter().position(|e| e.id == id).unwrap_or(0);
    let n = if up {
        it.effects[..at(first.id)].iter().rev().find(|e| !is_picked(e) && same_list(e, first))
    } else {
        it.effects[at(last.id) + 1..].iter().find(|e| !is_picked(e) && same_list(e, first))
    }?;
    let shared = first.group.filter(|g| picked.iter().all(|p| p.group == Some(*g)));
    let anchor = match n.group.filter(|g| Some(*g) != shared && it.effect_group(*g).is_some()) {
        // Over that whole group.
        Some(g) => {
            let members: Vec<&EffectInstance> = it.effects.iter().filter(|e| e.group == Some(g)).collect();
            if up { Anchor::Before(members[0].id) } else { Anchor::After(members[members.len() - 1].id) }
        }
        None if up => Anchor::Before(n.id),
        None => Anchor::After(n.id),
    };
    let moved = picked.iter().map(|p| EffectInstance { group: p.group.filter(|g| n.group == Some(*g)), ..p.clone() }).collect();
    Some((moved, anchor))
}

/// `text` drawn with its ink centered on `at` (a font's line box sits lower or higher
/// than the digits themselves, which is what the eye centers).
fn centered_text(painter: &egui::Painter, at: egui::Pos2, text: &str, color: egui::Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(crate::style::TEXT_S + 1.0), color);
    let ink = galley.mesh_bounds;
    painter.galley(at - ink.center().to_vec2(), galley, color);
}

/// What's picked after a click on `id` (see [`App::pick_effect`]); the last one picked
/// is last.
fn next_picks(mut picked: Vec<EffectId>, shown: &[EffectId], id: EffectId, mods: egui::Modifiers) -> Vec<EffectId> {
    if mods.shift
        && let Some(&anchor) = picked.last()
        && let (Some(a), Some(b)) = (shown.iter().position(|e| *e == anchor), shown.iter().position(|e| *e == id))
    {
        let run: Vec<EffectId> = if a <= b { shown[a..=b].to_vec() } else { shown[b..=a].iter().rev().copied().collect() };
        picked.retain(|e| !run.contains(e));
        picked.extend(run);
    } else if mods.command {
        match picked.iter().position(|e| *e == id) {
            Some(i) => {
                picked.remove(i);
            }
            None => picked.push(id),
        }
    } else if picked == [id] {
        picked.clear();
    } else {
        picked = vec![id];
    }
    picked
}

/// What's picked after a click on a group whose effects are `ids`: alone, just the group
/// (or nothing, if it's all that's picked); with Ctrl or Shift, the group added — or, if
/// it was all picked already, dropped.
fn group_click(mut picked: Vec<EffectId>, ids: &[EffectId], mods: egui::Modifiers) -> Vec<EffectId> {
    let all = !ids.is_empty() && ids.iter().all(|id| picked.contains(id));
    if mods.command || mods.shift {
        if all {
            picked.retain(|id| !ids.contains(id));
        } else {
            picked.extend(ids.iter().filter(|id| !picked.contains(id)).copied().collect::<Vec<_>>());
        }
        picked
    } else if all && picked.len() == ids.len() {
        Vec::new()
    } else {
        ids.to_vec()
    }
}

/// Ops moving `moved` (some of clip `it`'s effects, as they should be once moved) together
/// to `at` in its list, in the order given: taken out, then put back in a run.
fn place_ops(seq: oa_doc::SeqId, it: &oa_doc::Item, moved: &[EffectInstance], at: Anchor) -> Vec<Op> {
    let item = it.id;
    let rest: Vec<EffectId> = it.effects.iter().map(|e| e.id).filter(|id| !moved.iter().any(|m| m.id == *id)).collect();
    let index = match at {
        Anchor::Before(id) => rest.iter().position(|e| *e == id).unwrap_or(rest.len()),
        Anchor::After(id) => rest.iter().position(|e| *e == id).map_or(rest.len(), |i| i + 1),
        Anchor::End => rest.len(),
    };
    let mut ops: Vec<Op> = moved.iter().map(|m| Op::RemoveEffect { seq, item, effect: m.id }).collect();
    ops.extend(moved.iter().enumerate().map(|(k, m)| Op::InsertEffect { seq, item, index: index + k, effect: m.clone() }));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_doc::{ItemKind, Project, Sequence, Track, TrackKind};
    use oa_time::{FrameRate, TimeRange};

    /// A clip with effects shaped [intro, A, B, sound, C] (A and B in group 1), on a
    /// timeline: the order after `ops`.
    fn after(ops: impl Fn(&oa_doc::Item) -> Vec<Op>) -> Vec<(String, Option<u64>)> {
        let mut p = Project::new("t");
        let seq = oa_doc::SeqId(1);
        let mut s = Sequence::new(seq, "Main", FrameRate::FPS_30, oa_doc::FormatVariant { id: oa_doc::VariantId(2), name: "16:9".into(), size: oa_doc::CanvasSize::new(1920, 1080), overrides: Default::default() });
        let mut t = Track::new(oa_doc::TrackId(3), "V1", TrackKind::Video);
        let mut it = oa_doc::Item::new(ItemId(4), "clip", ItemKind::Solid, TimeRange::new(Time::ZERO, Time::from_seconds(4)));
        it.effect_groups.push(EffectGroup::new(1, "Look"));
        for (n, name) in ["intro", "A", "B", "sound", "C"].into_iter().enumerate() {
            let mut fx = EffectInstance::new(EffectId(10 + n as u64), name);
            fx.group = matches!(name, "A" | "B").then_some(1);
            it.effects.push(fx);
        }
        let ops = ops(&it);
        t.items.push(it);
        s.tracks.push(std::sync::Arc::new(t));
        p.sequences.insert(seq, std::sync::Arc::new(s));
        let mut doc = oa_doc::Document::new(p);
        doc.edit("move", ops).unwrap();
        let s = doc.project().sequence(seq).unwrap().clone();
        s.tracks[0].items[0].effects.iter().map(|e| (e.type_id.clone(), e.group)).collect()
    }

    /// Alt+↑/↓: past the nearest effect of the same list (the sound effect between is
    /// another list's), over a whole group it isn't in, and out of its own group at
    /// the group's edge.
    #[test]
    fn picked_effects_step_up_and_down() {
        let seq = oa_doc::SeqId(1);
        let kind = |e: &EffectInstance| matches!(e.type_id.as_str(), "intro" | "sound").then(|| e.type_id.clone());
        let same = |a: &EffectInstance, b: &EffectInstance| kind(a) == kind(b);
        let step = |name: &'static str, up: bool| {
            move |it: &oa_doc::Item| {
                let fx = it.effects.iter().find(|e| e.type_id == name).cloned().unwrap();
                let (moved, at) = step_run(it, &[fx], up, &same).expect("it can move");
                place_ops(seq, it, &moved, at)
            }
        };
        assert_eq!(names(&after(step("C", true))), ["intro", "C", "A", "B", "sound"], "over the whole group");
        let list = after(step("A", false));
        assert_eq!(names(&list), ["intro", "B", "A", "sound", "C"]);
        assert_eq!(list[2].1, Some(1), "still in its group");
        let list = after(step("B", false));
        assert_eq!(names(&list), ["intro", "A", "sound", "C", "B"]);
        assert_eq!(list[4].1, None, "out of the group past its edge");
        after(|it| {
            let intro = it.effects[0].clone();
            assert!(step_run(it, &[intro], true, &same).is_none(), "nothing above it");
            Vec::new()
        });
    }

    /// A click picks one (again: none); Ctrl+click adds or drops; Shift+click picks the
    /// run from the last one picked.
    #[test]
    fn effects_are_picked_like_files() {
        let ids: Vec<EffectId> = (1..=5).map(EffectId).collect();
        let (plain, ctrl, shift) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND, egui::Modifiers::SHIFT);
        let p = next_picks(Vec::new(), &ids, ids[1], plain);
        assert_eq!(p, [ids[1]]);
        assert!(next_picks(p.clone(), &ids, ids[1], plain).is_empty(), "a second click picks none");
        let p = next_picks(p, &ids, ids[3], ctrl);
        assert_eq!(p, [ids[1], ids[3]]);
        assert_eq!(next_picks(p.clone(), &ids, ids[1], ctrl), [ids[3]], "Ctrl+click drops one");
        let p = next_picks(p, &ids, ids[0], shift);
        assert_eq!(p.len(), 4, "the run from 4 back to 1 (2 already in it): {p:?}");
        assert_eq!(next_picks(p, &ids, ids[4], plain), [ids[4]]);
    }

    #[test]
    fn groups_are_picked_whole() {
        let group: Vec<EffectId> = [2, 3].map(EffectId).to_vec();
        let (plain, ctrl) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND);
        let p = group_click(vec![EffectId(1)], &group, plain);
        assert_eq!(p, group, "a click picks just the group");
        assert!(group_click(p, &group, plain).is_empty(), "a second click picks none");
        let p = group_click(vec![EffectId(1)], &group, ctrl);
        assert_eq!(p, [EffectId(1), EffectId(2), EffectId(3)], "Ctrl+click adds it");
        assert_eq!(group_click(p.clone(), &group, ctrl), [EffectId(1)], "and drops it again");
        assert_eq!(group_click(p, &group, plain), group, "picked with others: a click leaves only it");
    }

    fn names(list: &[(String, Option<u64>)]) -> Vec<&str> {
        list.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Effects move together to just before or after another, or to the end; a group
    /// moves as a run; an effect can change group as it moves.
    #[test]
    fn effects_and_groups_move_in_a_run() {
        let seq = oa_doc::SeqId(1);
        let fx = |it: &oa_doc::Item, name: &str| it.effects.iter().find(|e| e.type_id == name).cloned().unwrap();
        // C joins the group, between A and B.
        let list = after(|it| place_ops(seq, it, &[EffectInstance { group: Some(1), ..fx(it, "C") }], Anchor::Before(fx(it, "B").id)));
        assert_eq!(names(&list), ["intro", "A", "C", "B", "sound"]);
        assert_eq!(list[2].1, Some(1));
        // The group, after C (past the sound effect in between).
        let list = after(|it| place_ops(seq, it, &[fx(it, "A"), fx(it, "B")], Anchor::After(fx(it, "C").id)));
        assert_eq!(names(&list), ["intro", "sound", "C", "A", "B"]);
        // A leaves the group, to the end.
        let list = after(|it| place_ops(seq, it, &[EffectInstance { group: None, ..fx(it, "A") }], Anchor::End));
        assert_eq!(names(&list), ["intro", "B", "sound", "C", "A"]);
        assert_eq!(list[4].1, None);
    }
}
