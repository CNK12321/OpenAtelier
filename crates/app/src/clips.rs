//! Clip and track commands behind the timeline's menus and shortcuts: multi-selection
//! (with groups), cut/copy/paste/duplicate, delete, group/ungroup, enable/disable, and
//! track rename/reorder/delete. Each is one undo step.

use crate::App;
use oa_doc::{ItemId, Op, TrackId, TrackKind};
use oa_time::Time;
use std::collections::BTreeSet;

/// Copied clips, each with the track it came from.
pub type Clipboard = Vec<(TrackId, oa_doc::Item)>;

type Params = Vec<(String, Option<oa_params::ParamSource>)>;

/// Something done to one section of the timeline (see `oa_edit::sections`).
#[derive(Clone, Debug)]
pub enum SectionCommand {
    /// Select its clips.
    Select(usize),
    /// Remove its clips, keeping the space.
    Clear(usize),
    /// Remove it and close the gap.
    Delete(usize),
    Move { from: usize, to: usize },
    /// Change (move, recolor, rename) or, with `None`, remove a divider.
    Edit { divider: u64, change: Option<oa_doc::Divider> },
}

/// A copied transform: each transform parameter as the clip stored it (`None`: default),
/// and the same for each format's override.
#[derive(Clone)]
pub struct TransformCopy {
    base: Params,
    overrides: Vec<(oa_doc::VariantId, Params)>,
}

impl App {
    /// Keeps the multi-selection consistent with the primary `selection`, which other
    /// code sets directly: a new primary outside the set starts a new selection; deleted
    /// clips drop out.
    pub(crate) fn sync_selection(&mut self) {
        match self.selection {
            None => self.selected.clear(),
            Some(p) if !self.selected.contains(&p) => {
                self.selected = self.with_groups([p]);
            }
            _ => {}
        }
        let editor = &self.editor;
        self.selected.retain(|id| editor.item(*id).is_some());
        if self.selection.is_some_and(|p| !self.selected.contains(&p)) {
            self.selection = self.selected.iter().next_back().copied();
        }
    }

    /// `items` plus every clip grouped with any of them.
    pub(crate) fn with_groups(&self, items: impl IntoIterator<Item = ItemId>) -> BTreeSet<ItemId> {
        let mut out: BTreeSet<ItemId> = items.into_iter().collect();
        let groups: BTreeSet<u64> = out.iter().filter_map(|id| self.editor.item(*id).and_then(|i| i.group)).collect();
        if !groups.is_empty() {
            for t in &self.editor.sequence().tracks {
                out.extend(t.items.iter().filter(|i| i.group.is_some_and(|g| groups.contains(&g))).map(|i| i.id));
            }
        }
        out
    }

    /// Selects a clip (and its group). `add`: toggle it in the current selection.
    pub(crate) fn select_clip(&mut self, id: ItemId, add: bool) {
        let members = self.with_groups([id]);
        if add {
            if self.selected.contains(&id) {
                for m in &members {
                    self.selected.remove(m);
                }
                self.selection = self.selected.iter().next_back().copied();
                return;
            }
            self.selected.extend(members);
        } else {
            self.selected = members;
        }
        self.selection = Some(id);
    }

    pub(crate) fn select_all(&mut self) {
        self.selected = self.editor.sequence().tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
        self.selection = self.selected.iter().next_back().copied();
    }

    /// The selected clips, in timeline order.
    pub(crate) fn selected_clips(&self) -> Vec<ItemId> {
        let s = self.editor.sequence();
        let mut v: Vec<(Time, ItemId)> =
            self.selected.iter().filter_map(|id| s.item(*id).map(|i| (i.range.start, *id))).collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    pub(crate) fn copy_selection(&mut self) {
        let s = self.editor.sequence();
        let clips: Clipboard = self
            .selected_clips()
            .into_iter()
            .filter_map(|id| s.find_item(id).map(|(t, i)| (s.tracks[t].id, s.tracks[t].items[i].clone())))
            .collect();
        if !clips.is_empty() {
            self.clipboard = clips;
        }
    }

    pub(crate) fn cut_selection(&mut self) {
        self.copy_selection();
        self.delete_selection(false);
    }

    /// Pastes the clipboard with its first clip at `at`; the pasted clips become the
    /// selection.
    pub(crate) fn paste_at(&mut self, at: Time) {
        if self.clipboard.is_empty() {
            return;
        }
        let project = self.editor.doc.snapshot();
        let doc = &mut self.editor.doc;
        let mut alloc = || doc.alloc_id();
        match oa_edit::timeline::paste(&project, self.editor.seq, &self.clipboard, at, &mut alloc) {
            Ok((ops, ids)) => {
                match self.editor.apply("Paste", ops) {
                    Ok(()) => {
                        self.selected = ids.iter().copied().collect();
                        self.selection = ids.last().copied();
                    }
                    Err(e) => self.error = Some(format!("can't paste: {e}")),
                }
            }
            Err(e) => self.error = Some(format!("can't paste: {e}")),
        }
    }

    /// Copies of the selection, right after it.
    pub(crate) fn duplicate_selection(&mut self) {
        let clips = self.selected_clips();
        let Some(end) = clips.iter().filter_map(|id| self.editor.item(*id)).map(|i| i.range.end()).max() else { return };
        let saved = std::mem::take(&mut self.clipboard);
        self.copy_selection();
        self.paste_at(end);
        self.clipboard = saved;
    }

    /// Deletes the selection. `ripple`: later clips on each track close the gaps.
    pub(crate) fn delete_selection(&mut self, ripple: bool) {
        let clips = self.selected_clips();
        if clips.is_empty() {
            return;
        }
        let seq = self.editor.seq;
        let result = if ripple {
            // Last first, so earlier deletions don't shift what the later ones remove;
            // coalesced into one undo step.
            let mut r = Ok(());
            for id in clips.iter().rev() {
                r = oa_edit::timeline::ripple_delete(self.editor.doc.project(), seq, *id)
                    .and_then(|ops| self.editor.apply_drag("Ripple delete", "ripple-delete-selection", ops));
                if r.is_err() {
                    break;
                }
            }
            self.editor.doc.seal();
            r
        } else {
            self.editor.apply("Delete", clips.iter().map(|&item| Op::RemoveItem { seq, item }).collect())
        };
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
        self.selected.clear();
        self.selection = None;
    }

    pub(crate) fn group_selection(&mut self) {
        let clips = self.selected_clips();
        if clips.len() < 2 {
            return;
        }
        let group = self.editor.doc.alloc_id();
        let seq = self.editor.seq;
        let ops = clips.iter().map(|&item| Op::SetItemGroup { seq, item, group: Some(group) }).collect();
        if let Err(e) = self.editor.apply("Group", ops) {
            self.error = Some(e.to_string());
        }
    }

    pub(crate) fn ungroup_selection(&mut self) {
        let seq = self.editor.seq;
        let ops: Vec<Op> = self
            .selected_clips()
            .into_iter()
            .filter(|id| self.editor.item(*id).is_some_and(|i| i.group.is_some()))
            .map(|item| Op::SetItemGroup { seq, item, group: None })
            .collect();
        if let Err(e) = self.editor.apply("Ungroup", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// The selected clips (a group, usually) as media: a compound clip in the bin, usable
    /// as a clip or as an effect's picture (a mask…). `nest`: the clips are also replaced
    /// by one clip playing it, which is then selected.
    pub(crate) fn compound_selection(&mut self, nest: bool) {
        let clips = self.selected_clips();
        match self.editor.compound(&clips, nest) {
            Ok((_, Some(clip))) => {
                self.selected.clear();
                self.selection = Some(clip);
                self.sync_selection();
            }
            Ok(_) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Whether any selected clip is a compound clip (one that can be broken apart).
    pub(crate) fn compound_selected(&self) -> bool {
        self.selected.iter().any(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, oa_doc::ItemKind::Nested { .. })))
    }

    /// Breaks the selected compound clips apart: the clips inside come back onto the
    /// timeline where they play (`oa_edit::compound`), and are selected. One undo step.
    pub(crate) fn break_compounds(&mut self) {
        let compounds: Vec<ItemId> = self.selected_clips().into_iter().filter(|id| self.editor.item(*id).is_some_and(|i| matches!(i.kind, oa_doc::ItemKind::Nested { .. }))).collect();
        let (mut placed, mut lost, mut retimed) = (Vec::new(), false, 0);
        // One at a time, each on the timeline the one before left: pieces of the second
        // need to see where the first's went.
        let before = self.editor.doc.undo_depth();
        for id in compounds {
            let editor = &mut self.editor;
            let snapshot = editor.doc.snapshot();
            let broken = oa_edit::compound::break_apart(&snapshot, editor.seq, id, || editor.doc.alloc_id());
            match broken {
                Ok(b) => {
                    lost |= b.dropped_own_changes;
                    match self.editor.doc.edit_coalesced("Break apart", Some("break-apart"), b.ops) {
                        Ok(()) => placed.extend(b.clips),
                        Err(e) => self.error = Some(format!("can't break it apart: {e}")),
                    }
                }
                Err(oa_doc::EditError::InvalidRange) => retimed += 1,
                Err(e) => self.error = Some(format!("can't break it apart: {e}")),
            }
        }
        if self.editor.doc.undo_depth() > before {
            self.editor.doc.seal();
        }
        if !placed.is_empty() {
            self.selected = placed.iter().copied().collect();
            self.selection = placed.first().copied();
            self.sync_selection();
            if lost {
                self.notify("Broken apart. The compound clip's own transform, effects and transitions applied to it as a whole, so the pieces don't have them.");
            }
        }
        if retimed > 0 {
            self.notify("A compound clip playing at another speed can't be broken apart yet — set it back to 1× first.");
        }
    }

    /// Copies `item`'s transform (position, scale, rotation, squash, anchor, opacity,
    /// crop, reframing — keyframes included) and its per-format overrides of them.
    pub(crate) fn copy_transform(&mut self, item: ItemId) {
        let Some(it) = self.editor.item(item) else { return };
        let ids = oa_doc::schema::visual().iter().map(|p| p.id.as_str());
        let base = ids.clone().map(|id| (id.to_string(), it.params.get(id).cloned())).collect();
        let overrides = self
            .editor
            .sequence()
            .variants
            .iter()
            .map(|v| {
                let o = v.overrides.get(&item);
                (v.id, ids.clone().map(|id| (id.to_string(), o.and_then(|o| o.get(id)).cloned())).collect())
            })
            .collect();
        self.transform_clipboard = Some(TransformCopy { base, overrides });
    }

    /// Gives every clip in `items` the copied transform (one undo step).
    pub(crate) fn paste_transform(&mut self, items: &[ItemId]) {
        let Some(copy) = self.transform_clipboard.clone() else { return };
        let seq = self.editor.seq;
        let s = self.editor.sequence();
        let mut ops = Vec::new();
        for &item in items {
            let Some(it) = s.item(item) else { continue };
            if s.find_item(item).is_some_and(|(t, _)| s.tracks[t].kind == TrackKind::Audio) {
                continue; // sound has no picture to place
            }
            // Only what changes, so undo history stays small.
            let mut set = |target: oa_doc::ParamTarget, id: &str, now: Option<&oa_params::ParamSource>, new: &Option<oa_params::ParamSource>| {
                if now != new.as_ref() {
                    ops.push(Op::SetParam { seq, item, target, param: oa_params::ParamId::new(id), source: new.clone() });
                }
            };
            for (id, new) in &copy.base {
                set(oa_doc::ParamTarget::Item, id, it.params.get(id), new);
            }
            for v in &s.variants {
                let o = v.overrides.get(&item);
                let copied = copy.overrides.iter().find(|(vid, _)| *vid == v.id);
                for (id, _) in &copy.base {
                    let new = copied.and_then(|(_, list)| list.iter().find(|(k, _)| k == id)).and_then(|(_, src)| src.clone());
                    set(oa_doc::ParamTarget::VariantOverride(v.id), id, o.and_then(|o| o.get(id)), &new);
                }
            }
        }
        if let Err(e) = self.editor.apply("Paste transform", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// Menu entries choosing how `media`'s pixels are scaled.
    pub(crate) fn scaling_menu(&mut self, ui: &mut eframe::egui::Ui, media: oa_doc::MediaId) {
        use oa_doc::MediaScaling;
        let Some(m) = self.editor.doc.project().media(media) else { return };
        let (current, pixel_now) = (m.scaling, m.pixelated());
        ui.menu_button("Scale mode", |ui| {
            let auto = format!("Automatic ({})", if pixel_now && current.is_auto() { "pixel" } else { "smooth" });
            for (mode, label, hint) in [
                (MediaScaling::Pixel, "Pixel scale (nearest neighbor)".to_string(), "Every pixel stays a crisp square — for pixel art"),
                (MediaScaling::Smooth, "Smooth scale".to_string(), "Filtered — for photos and video"),
                (MediaScaling::Auto, auto, "Pixel for pictures up to 32×32, smooth otherwise"),
            ] {
                if ui.radio(current == mode, label).on_hover_text(hint).clicked() {
                    if let Err(e) = self.editor.apply("Scale mode", vec![Op::SetMediaScaling { media, scaling: mode }]) {
                        self.error = Some(e.to_string());
                    }
                    ui.close();
                }
            }
        });
    }

    pub(crate) fn selection_grouped(&self) -> bool {
        self.selected.iter().any(|id| self.editor.item(*id).is_some_and(|i| i.group.is_some()))
    }

    /// Turns the selected clips off (or on, if they're all off).
    pub(crate) fn toggle_selection_enabled(&mut self) {
        let clips = self.selected_clips();
        let all_off = clips.iter().all(|id| self.editor.item(*id).is_some_and(|i| !i.enabled));
        let seq = self.editor.seq;
        let ops = clips.iter().map(|&item| Op::SetItemEnabled { seq, item, enabled: all_off }).collect();
        if let Err(e) = self.editor.apply(if all_off { "Enable clips" } else { "Disable clips" }, ops) {
            self.error = Some(e.to_string());
        }
    }

    /// Selected picture clips whose file has sound that still plays from the clip.
    pub(crate) fn extractable(&self) -> Vec<ItemId> {
        let s = self.editor.sequence();
        self.selected_clips()
            .into_iter()
            .filter(|id| {
                let Some((t, i)) = s.find_item(*id) else { return false };
                let it = &s.tracks[t].items[i];
                let oa_doc::ItemKind::Media { media } = it.kind else { return false };
                s.tracks[t].kind == TrackKind::Video
                    && it.audio_enabled()
                    && self.editor.pool_item(media).is_some_and(|p| p.probe.has_audio())
            })
            .collect()
    }

    /// Moves the sound of the selected picture clips onto audio tracks: a new audio clip
    /// each (same file, timing, speed and gain), grouped with its picture so they move
    /// together, and the picture clip's own sound turned off. One undo step.
    pub(crate) fn extract_audio(&mut self) {
        let clips = self.extractable();
        if clips.is_empty() {
            return;
        }
        let seq = self.editor.seq;
        let s = self.editor.sequence().clone();
        let mut ops = Vec::new();
        let mut placed: Vec<(TrackId, oa_time::TimeRange)> = Vec::new();
        let mut new_tracks = 0;
        let mut new_items = Vec::new();
        for id in clips {
            let Some(it) = s.item(id).cloned() else { continue };
            // A free audio track, or a new one.
            let free = |t: &oa_doc::Track| {
                !t.items.iter().any(|i| i.range.overlaps(it.range)) && !placed.iter().any(|(tid, r)| *tid == t.id && r.overlaps(it.range))
            };
            let track = match s.tracks.iter().filter(|t| t.kind == TrackKind::Audio && !t.effects).find(|t| free(t)).map(|t| t.id) {
                Some(t) => t,
                None => {
                    let id = TrackId(self.editor.doc.alloc_id());
                    let count = s.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count() + new_tracks + 1;
                    new_tracks += 1;
                    let track = std::sync::Arc::new(oa_doc::Track::new(id, &format!("A{count}"), TrackKind::Audio));
                    ops.push(Op::InsertTrack { seq, index: s.tracks.len() + new_tracks, track });
                    id
                }
            };
            placed.push((track, it.range));
            let group = it.group.unwrap_or_else(|| self.editor.doc.alloc_id());
            let mut sound = oa_doc::Item::new(ItemId(self.editor.doc.alloc_id()), &format!("{} (audio)", it.name), it.kind.clone(), it.range);
            sound.time_map = it.time_map;
            sound.group = Some(group);
            for p in [oa_doc::schema::AUDIO_GAIN, oa_doc::schema::AUDIO_KEEP_PITCH] {
                if let Some(src) = it.params.get(p) {
                    sound.params.set(p, src.clone());
                }
            }
            // Sound effects go with the sound.
            sound.effects = it.effects.iter().filter(|e| self.registry.is_sound(&e.type_id)).cloned().collect();
            new_items.push(sound.id);
            ops.push(Op::SetParam {
                seq,
                item: id,
                target: oa_doc::ParamTarget::Item,
                param: oa_params::ParamId::new(oa_doc::schema::AUDIO_ENABLED),
                source: Some(oa_params::ParamSource::Static(oa_params::Value::Bool(false))),
            });
            if it.group.is_none() {
                ops.push(Op::SetItemGroup { seq, item: id, group: Some(group) });
            }
            ops.push(Op::InsertItem { seq, track, item: sound });
        }
        if let Err(e) = self.editor.apply("Extract audio", ops) {
            self.error = Some(format!("can't extract the audio: {e}"));
        }
    }

    // ---- effects ----

    /// Copies one of `item`'s effects, or (`None`) all of them.
    pub(crate) fn copy_effects(&mut self, item: ItemId, only: Option<oa_doc::EffectId>) {
        let Some(it) = self.editor.item(item) else { return };
        self.effect_clipboard = it.effects.iter().filter(|e| only.is_none_or(|id| e.id == id)).cloned().collect();
        // All of them: the "Reverse" outro comes too (it decides what the intros do at
        // the end of the clip).
        self.effect_clipboard_reverse = only.is_none().then_some(it.outro_reverses_intro);
    }

    /// Adds copies of the copied effects (fresh ids, same settings and roles) to every
    /// clip in `items`, after their own.
    pub(crate) fn paste_effects(&mut self, items: &[ItemId]) {
        if self.effect_clipboard.is_empty() {
            return;
        }
        let seq = self.editor.seq;
        let mut ops = Vec::new();
        for &item in items {
            let Some(it) = self.editor.item(item) else { continue };
            let n = it.effects.len();
            if let Some(on) = self.effect_clipboard_reverse
                && it.outro_reverses_intro != on
                && item != oa_doc::BACKGROUND
            {
                ops.push(Op::SetReverseIntro { seq, item, on });
            }
            for (k, fx) in self.effect_clipboard.clone().into_iter().enumerate() {
                let effect = oa_doc::EffectInstance { id: oa_doc::EffectId(self.editor.doc.alloc_id()), ..fx };
                ops.push(Op::InsertEffect { seq, item, index: n + k, effect });
            }
        }
        if let Err(e) = self.editor.apply("Paste effects", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// Replaces `effect`'s settings with the copied effect's (same type only).
    pub(crate) fn paste_effect_settings(&mut self, item: ItemId, effect: oa_doc::EffectId) {
        let Some(it) = self.editor.item(item) else { return };
        let Some(index) = it.effects.iter().position(|e| e.id == effect) else { return };
        let target = &it.effects[index];
        let Some(source) = self.effect_clipboard.iter().find(|e| e.type_id == target.type_id) else { return };
        let updated = oa_doc::EffectInstance { params: source.params.clone(), ..target.clone() };
        let seq = self.editor.seq;
        let ops = vec![Op::RemoveEffect { seq, item, effect }, Op::InsertEffect { seq, item, index, effect: updated }];
        if let Err(e) = self.editor.apply("Paste effect settings", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// Whether an effect of `type_id` belongs on `item`: sound effects on clips that play
    /// sound, picture effects on clips with a picture (text effects on titles only; the
    /// background takes neither motion nor text effects).
    /// For an effect container: the kind of effect track it's on (what it can hold).
    pub(crate) fn container_kind(&self, item: ItemId) -> Option<TrackKind> {
        let s = self.editor.sequence();
        let (t, i) = s.find_item(item)?;
        (s.tracks[t].effects && matches!(s.tracks[t].items[i].kind, oa_doc::ItemKind::Adjustment)).then_some(s.tracks[t].kind)
    }

    pub(crate) fn effect_fits(&self, item: ItemId, type_id: &str) -> bool {
        // A container holds what its effect track runs: sound effects on a sound one,
        // picture effects (not a clip's own motion or text effects) on a picture one.
        if let Some(kind) = self.container_kind(item) {
            let sound = self.registry.is_sound(type_id);
            return match kind {
                TrackKind::Audio => sound,
                TrackKind::Video => !sound && self.registry.effect(type_id).is_some_and(|d| !d.kind.text_only() && (d.kind != oa_graph::EffectKind::Motion || fades_only(d))),
            };
        }
        if self.registry.is_sound(type_id) {
            return self.is_audible(item);
        }
        let Some(d) = self.registry.effect(type_id) else { return false };
        let Some(it) = self.editor.item(item) else { return false };
        if item == oa_doc::BACKGROUND {
            return !d.kind.text_only() && d.kind != oa_graph::EffectKind::Motion && d.usage == oa_graph::registry::EffectUsage::Passive;
        }
        self.is_visual(item) && (!d.kind.text_only() || it.kind == oa_doc::ItemKind::Text)
    }

    /// Puts a copy of `effect` (its settings and role) on each of `targets` it fits: a
    /// clip that already has one of the same kind in the same role gets these settings
    /// instead of a second one. One undo step; returns how many clips changed.
    pub(crate) fn apply_effect_to(&mut self, effect: &oa_doc::EffectInstance, targets: &[ItemId], label: &str) -> usize {
        use oa_doc::EffectRole;
        let seq = self.editor.seq;
        let mut ops = Vec::new();
        let mut changed = 0;
        for &item in targets {
            if !self.effect_fits(item, &effect.type_id) {
                continue;
            }
            let Some(it) = self.editor.item(item).cloned() else { continue };
            let same_role = |r: &EffectRole| std::mem::discriminant(r) == std::mem::discriminant(&effect.role);
            match it.effects.iter().position(|e| e.type_id == effect.type_id && same_role(&e.role)) {
                Some(index) => {
                    let existing = &it.effects[index];
                    if existing.params == effect.params && existing.role == effect.role && existing.enabled == effect.enabled {
                        continue;
                    }
                    let updated = oa_doc::EffectInstance { params: effect.params.clone(), role: effect.role, enabled: effect.enabled, ..existing.clone() };
                    ops.push(Op::RemoveEffect { seq, item, effect: existing.id });
                    ops.push(Op::InsertEffect { seq, item, index, effect: updated });
                }
                None => {
                    // Intros/outros go with the clip's others (before its passive effects,
                    // which see the clip as it arrives); everything else at the end.
                    let index = match effect.role {
                        EffectRole::Passive => it.effects.len(),
                        _ => it.effects.iter().take_while(|e| e.role != EffectRole::Passive).count(),
                    };
                    let copy = oa_doc::EffectInstance { id: oa_doc::EffectId(self.editor.doc.alloc_id()), ..effect.clone() };
                    ops.push(Op::InsertEffect { seq, item, index, effect: copy });
                }
            }
            changed += 1;
        }
        if !ops.is_empty()
            && let Err(e) = self.editor.apply(label, ops)
        {
            self.error = Some(e.to_string());
            return 0;
        }
        changed
    }

    /// An effect dragged out of the inspector and let go over an empty stretch of effect
    /// track `track` at `at`: a new container starts there holding a copy of it — if
    /// it's the track's kind of effect (sound on a sound one, picture on a picture one).
    pub(crate) fn drop_effect_on_track(&mut self, drag: &crate::inspector::EffectDrag, track: TrackId, at: Time) {
        let name = self.effect_name(&drag.effect.type_id);
        let Some(kind) = self.editor.sequence().track(track).map(|t| t.kind) else { return };
        let sound = self.registry.is_sound(&drag.effect.type_id);
        if sound != (kind == TrackKind::Audio) {
            self.notify(format!("{name} is a {} effect: drop it on a {} effect track.", if sound { "sound" } else { "picture" }, if sound { "sound" } else { "picture" }));
            return;
        }
        let at = oa_edit::timeline::snap_to_frame(self.editor.sequence(), at);
        let before = self.editor.doc.undo_depth();
        let container = match self.editor.add_container(track, at) {
            Ok(id) => id,
            Err(e) => {
                self.error = Some(format!("can't start a container there: {e}"));
                return;
            }
        };
        let mut effect = drag.effect.clone();
        effect.role = oa_doc::EffectRole::Passive;
        if self.apply_effect_to(&effect, &[container], "Add effect container") == 0 && self.editor.doc.undo_depth() > before {
            self.undo();
            return;
        }
        self.selection = Some(container);
        self.sync_selection();
        self.notify(format!("{name} now runs over everything below that track, for as long as its container lasts."));
    }

    /// An effect dragged out of the inspector and let go over clip `onto`: copied there.
    pub(crate) fn drop_effect(&mut self, drag: &crate::inspector::EffectDrag, onto: ItemId) {
        if onto == drag.from {
            return;
        }
        let name = self.effect_name(&drag.effect.type_id);
        let target = self.editor.item(onto).map(|i| i.name.clone()).unwrap_or_default();
        if !self.effect_fits(onto, &drag.effect.type_id) {
            let why = if self.registry.is_sound(&drag.effect.type_id) { "it has no sound" } else { "it doesn't take that kind of effect" };
            self.report_error(format!("{name} can't go on {target}: {why}"));
            return;
        }
        if self.apply_effect_to(&drag.effect, &[onto], "Copy effect") > 0 {
            self.notify(format!("Copied {name} onto {target}."));
        } else if self.error.is_none() {
            self.notify(format!("{target} already has {name} with these settings."));
        }
    }

    /// The clip drawn at screen point `p` last frame: a box in the timeline, or a layer
    /// on the viewer's canvas at the playhead.
    pub(crate) fn clip_on_screen(&self, p: eframe::egui::Pos2) -> Option<ItemId> {
        if let Some((_, id)) = self.drop_targets.iter().rev().find(|(r, _)| r.contains(p)) {
            return Some(*id);
        }
        let (area, canvas) = self.viewer_canvas?;
        if !area.contains(p) {
            return None;
        }
        let s = self.editor.sequence();
        let v = s.variant(self.variant_id())?;
        let zoom = canvas.width() as f64 / v.size.width.max(1) as f64;
        let at = [(p.x - canvas.left()) as f64 / zoom, (p.y - canvas.top()) as f64 / zoom];
        oa_plan::scene::hit_test(self.editor.doc.project(), self.editor.seq, v, self.playhead, at)
    }

    /// An effect type's name as menus show it.
    pub(crate) fn effect_name(&self, type_id: &str) -> String {
        self.registry.effect(type_id).map_or_else(|| type_id.to_string(), |d| d.name.clone())
    }

    /// Right-click menu entries for one effect card.
    pub(crate) fn effect_menu(&mut self, ui: &mut eframe::egui::Ui, item: ItemId, effect: &oa_doc::EffectInstance) {
        if ui.button("Copy effect").clicked() {
            self.copy_effects(item, Some(effect.id));
            ui.close();
        }
        let same_type = self.effect_clipboard.iter().any(|e| e.type_id == effect.type_id);
        if ui.add_enabled(same_type, eframe::egui::Button::new("Paste settings")).on_hover_text("From the copied effect of the same kind").clicked() {
            self.paste_effect_settings(item, effect.id);
            ui.close();
        }
        ui.separator();
        if ui.button("Copy all effects").clicked() {
            self.copy_effects(item, None);
            ui.close();
        }
        if ui.add_enabled(!self.effect_clipboard.is_empty(), eframe::egui::Button::new("Paste effects")).clicked() {
            self.paste_effects(&[item]);
            ui.close();
        }
    }

    // ---- a selected track: pasting into its free space, and its sections ----

    /// Pastes the clipboard into `track`'s closest free space at or after the playhead.
    pub(crate) fn paste_into_track(&mut self, track: TrackId) {
        let clips = self.clipboard.clone();
        self.place_into_track(track, &clips, "Paste");
    }

    /// Copies of the selected clips into `track`'s closest free space after the playhead.
    pub(crate) fn duplicate_into_track(&mut self, track: TrackId) {
        let s = self.editor.sequence();
        let clips: Clipboard =
            self.selected_clips().into_iter().filter_map(|id| s.find_item(id).map(|(t, i)| (s.tracks[t].id, s.tracks[t].items[i].clone()))).collect();
        self.place_into_track(track, &clips, "Duplicate");
    }

    fn place_into_track(&mut self, track: TrackId, clips: &Clipboard, label: &str) {
        if clips.is_empty() {
            return;
        }
        let project = self.editor.doc.snapshot();
        let doc = &mut self.editor.doc;
        let mut alloc = || doc.alloc_id();
        match oa_edit::sections::paste_into_track(&project, self.editor.seq, track, clips, self.playhead, &mut alloc) {
            Ok((ops, _)) if ops.is_empty() => self.report_error("nothing to put there: those clips are the other kind (sound or picture) from this track"),
            Ok((ops, ids)) => match self.editor.apply(label, ops) {
                Ok(()) => {
                    self.selected = ids.iter().copied().collect();
                    self.selection = ids.last().copied();
                }
                Err(e) => self.report_error(format!("can't {}: {e}", label.to_lowercase())),
            },
            Err(e) => self.report_error(format!("can't {}: {e}", label.to_lowercase())),
        }
    }

    /// A divider across the timeline at `at` (splitting clips it lands inside).
    pub(crate) fn add_divider(&mut self, at: Time) {
        let project = self.editor.doc.snapshot();
        // Each new one takes the next color, so neighbors differ.
        let count = project.sequence(self.editor.seq).map_or(0, |s| s.dividers.len());
        let color = oa_edit::sections::COLORS[count % oa_edit::sections::COLORS.len()];
        let doc = &mut self.editor.doc;
        let mut alloc = || doc.alloc_id();
        let result = oa_edit::sections::add_divider(&project, self.editor.seq, at, color, &mut alloc).and_then(|ops| self.editor.apply("Add divider", ops));
        if let Err(e) = result {
            self.report_error(format!("can't add a divider: {e}"));
        }
    }

    /// One of the section commands, as one undo step.
    pub(crate) fn section_command(&mut self, command: SectionCommand) {
        use oa_edit::sections as sec;
        let project = self.editor.doc.snapshot();
        let seq = self.editor.seq;
        let Some(s) = project.sequence(seq) else { return };
        if let SectionCommand::Select(index) = command {
            let ids: Vec<ItemId> = sec::members(s, index).iter().map(|(_, i)| i.id).collect();
            self.selected = self.with_groups(ids.iter().copied());
            self.selection = ids.last().copied();
            return;
        }
        let doc = &mut self.editor.doc;
        let mut alloc = || doc.alloc_id();
        let (label, ops) = match command {
            SectionCommand::Clear(i) => ("Clear section", sec::clear_section(&project, seq, i)),
            SectionCommand::Delete(i) => ("Delete section", sec::delete_section(&project, seq, i)),
            SectionCommand::Move { from, to } => ("Move section", sec::move_section(&project, seq, from, to, &mut alloc)),
            SectionCommand::Edit { divider, change } => ("Edit divider", sec::edit_divider(&project, seq, divider, change)),
            SectionCommand::Select(_) => unreachable!("handled above"),
        };
        let result = ops.and_then(|ops| if ops.is_empty() { Ok(()) } else { self.editor.apply(label, ops) });
        if let Err(e) = result {
            self.report_error(format!("{label}: {e}"));
        }
        self.sync_selection();
    }

    // ---- tracks ----

    pub(crate) fn rename_track(&mut self, track: TrackId, name: String) {
        let op = Op::SetTrackName { seq: self.editor.seq, track, name };
        if let Err(e) = self.editor.apply("Rename track", vec![op]) {
            self.error = Some(e.to_string());
        }
    }

    /// Moves a track one place up (towards the top of the stack for video, down the list
    /// for audio: `up` means "draws over more" / "listed earlier").
    pub(crate) fn move_track(&mut self, track: TrackId, up: bool) {
        let s = self.editor.sequence();
        let Some(index) = s.tracks.iter().position(|t| t.id == track) else { return };
        let kind = s.tracks[index].kind;
        // Video is drawn top-down from the end of the list, audio from the start.
        let step: isize = match (kind, up) {
            (TrackKind::Video, true) | (TrackKind::Audio, false) => 1,
            _ => -1,
        };
        let to = index as isize + step;
        if to < 0 || to as usize >= s.tracks.len() || s.tracks[to as usize].kind != kind {
            return;
        }
        let arc = s.tracks[index].clone();
        let seq = self.editor.seq;
        let ops = vec![Op::RemoveTrack { seq, track }, Op::InsertTrack { seq, index: to as usize, track: arc }];
        if let Err(e) = self.editor.apply("Move track", ops) {
            self.error = Some(e.to_string());
        }
    }

    /// Deletes a track and its clips (undoable). The last track of a kind stays.
    pub(crate) fn delete_track(&mut self, track: TrackId) {
        let s = self.editor.sequence();
        let Some(t) = s.track(track) else { return };
        if s.tracks.iter().filter(|x| x.kind == t.kind).count() <= 1 {
            self.error = Some("the last track of its kind can't be deleted".into());
            return;
        }
        let op = Op::RemoveTrack { seq: self.editor.seq, track };
        if let Err(e) = self.editor.apply("Delete track", vec![op]) {
            self.error = Some(e.to_string());
        }
    }
}

/// A motion effect that only fades (no move, zoom or turn) — the one kind of motion an
/// effect container can use: it fades the container's result in or out. Probed at a few
/// moments of its window with its default settings.
fn fades_only(d: &oa_graph::registry::EffectDescriptor) -> bool {
    use oa_graph::registry::MotionInput;
    let Some(f) = d.motion else { return false };
    let values = oa_params::ParamSet::default().eval(&d.params, None, &oa_params::EvalContext::at(Time::ZERO, Time::ZERO));
    [0.0, 0.3, 0.7].into_iter().all(|v| {
        let input = MotionInput { visibility: v, progress: v, seconds: v, canvas: [1920.0, 1080.0], seed: 1, leaving: false };
        let m = f(&values, &input);
        m.offset == [0.0, 0.0] && m.scale == 1.0 && m.rotation == 0.0
    })
}
