//! Effect previews: small renders of the selected clip with an effect applied, shown in
//! the effect pickers so you can see what an effect does before adding it. Hovering a
//! preview plays it: intros and outros run through, animated effects move, and still
//! effects sweep from off to full strength.
//!
//! Each preview is a real render — the same planner and GPU path as the viewer — of a
//! scratch copy of the project with the effect added to the clip, made on the preview
//! thread (`preview_worker`) so the UI never waits on one. The clip's picture is frozen
//! on the playhead's frame in that copy, so every preview (and every frame of a hover
//! animation) reuses one decoded frame from that thread's render cache. Still previews
//! are cached until the document, playhead, format or clip changes (not while playing:
//! they stay on the moment playback started from); requests for a moment that has since
//! changed are skipped on the thread, and a preview being redone shows the one before
//! until it's ready, never a blank.

use crate::App;
use eframe::egui;
use oa_doc::{EffectId, EffectInstance, EffectRole, ItemId, Project};
use oa_params::{ParamSource, Value};
use oa_time::{Rational, Time};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Previews render ~200 px wide.
const THUMB_WIDTH: f64 = 200.0;
/// How long a previewed intro/outro lasts.
const IN_OUT_PREVIEW: Time = Time::from_seconds(1);
/// Pause after an intro/outro plays before it loops.
const HOLD: f64 = 0.6;

/// Which picker the previews are for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ThumbKind {
    Passive,
    Intro,
    Outro,
}

#[derive(Clone, PartialEq, Eq)]
struct Context {
    document: usize,
    playhead: Time,
    variant: usize,
    item: ItemId,
}

struct Hover {
    key: (ThumbKind, String),
    /// UI time the hover began (the animation's zero).
    since: f64,
    seen: bool,
}

#[derive(Default)]
pub struct Thumbs {
    context: Option<Context>,
    images: HashMap<(ThumbKind, String), (egui::TextureId, wgpu::Texture)>,
    /// Previews of the moment before, shown until each is rendered for this one.
    stale: HashMap<(ThumbKind, String), (egui::TextureId, wgpu::Texture)>,
    /// Which moment the previews are for, bumped when it changes: requests for an older
    /// one are skipped on the preview thread, and their results dropped here.
    generation: Arc<AtomicU64>,
    /// Still previews asked for in this generation (so each is asked for once).
    requested: HashSet<(ThumbKind, String)>,
    hover: Option<Hover>,
    /// The hover animation's latest frame: its egui id, texture, and which cell it's of.
    live: Option<(egui::TextureId, wgpu::Texture, u64)>,
    /// The part of the canvas previews show (0..1): the clip and room around it, so a
    /// small title fills the preview instead of sitting in a corner of it.
    framing: Option<egui::Rect>,
}

/// The region of the canvas (fractions) to show for `item`: its bounds at the playhead
/// plus room for effects to move it, at the cells' aspect.
fn framing(p: &oa_plan::scene::Placement, canvas: [f64; 2], aspect: f64) -> egui::Rect {
    let corners = p.corners();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for [x, y] in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let pad = 0.35 * (x1 - x0).max(y1 - y0);
    let (mut w, mut h) = ((x1 - x0) + 2.0 * pad, (y1 - y0) + 2.0 * pad);
    // Match the cell's aspect, then keep it on the canvas.
    if w / h < aspect {
        w = h * aspect;
    } else {
        h = w / aspect;
    }
    let (w, h) = (w.min(canvas[0]), h.min(canvas[1]));
    let cx = ((x0 + x1) / 2.0).clamp(w / 2.0, canvas[0] - w / 2.0);
    let cy = ((y0 + y1) / 2.0).clamp(h / 2.0, canvas[1] - h / 2.0);
    egui::Rect::from_min_max(
        egui::pos2(((cx - w / 2.0) / canvas[0]) as f32, ((cy - h / 2.0) / canvas[1]) as f32),
        egui::pos2(((cx + w / 2.0) / canvas[0]) as f32, ((cy + h / 2.0) / canvas[1]) as f32),
    )
}

impl App {
    /// Called once per UI frame: forgets previews made for another moment or clip, and
    /// the hover animation once nothing is hovered. While playing, the previews stay on
    /// the moment they were made for (the playhead moving every frame would otherwise
    /// throw them away as fast as they render); and when the moment does change, the
    /// old previews stay up until their replacements are ready.
    pub(crate) fn thumbs_frame_start(&mut self) {
        if let Some(h) = &mut self.thumbs.hover
            && !std::mem::replace(&mut h.seen, false)
        {
            self.thumbs.hover = None;
        }
        let kept = self.thumbs.context.as_ref().filter(|c| self.playing && Some(c.item) == self.selection).map(|c| c.playhead);
        let now = self.selection.map(|item| Context {
            document: Arc::as_ptr(&self.editor.doc.snapshot()) as usize,
            playhead: kept.unwrap_or(self.playhead),
            variant: self.variant,
            item,
        });
        if now != self.thumbs.context {
            let same_clip = now.as_ref().map(|c| c.item) == self.thumbs.context.as_ref().map(|c| c.item);
            self.thumbs.context = now;
            if same_clip {
                self.age_thumbs();
            } else {
                self.forget_thumbs();
            }
        }
    }

    /// The moment previews show: the playhead, or where it was when playback started.
    fn thumb_moment(&self) -> Time {
        self.thumbs.context.as_ref().map_or(self.playhead, |c| c.playhead)
    }

    /// The current previews become stand-ins, shown until each is rendered again. A
    /// stand-in is only replaced by a newer preview (the moment can change many times
    /// before anything re-renders — dragging a slider changes it every frame).
    fn age_thumbs(&mut self) {
        self.next_thumb_generation();
        let fresh = std::mem::take(&mut self.thumbs.images);
        let mut replaced = Vec::new();
        for (key, image) in fresh {
            replaced.extend(self.thumbs.stale.insert(key, image));
        }
        self.free_textures(replaced.into_iter());
    }

    /// The moment changed: previews asked for before are no longer wanted.
    fn next_thumb_generation(&mut self) {
        self.thumbs.framing = None;
        self.thumbs.generation.fetch_add(1, Ordering::AcqRel);
        self.thumbs.requested.clear();
    }

    fn free_textures(&self, textures: impl Iterator<Item = (egui::TextureId, wgpu::Texture)>) {
        if let Some(rs) = &self.render_state {
            let mut renderer = rs.renderer.write();
            for (id, _) in textures {
                renderer.free_texture(&id);
            }
        }
    }

    /// Drops every cached preview (the moment changed, or the effects did).
    pub(crate) fn forget_thumbs(&mut self) {
        self.next_thumb_generation();
        let images = std::mem::take(&mut self.thumbs.images);
        let stale = std::mem::take(&mut self.thumbs.stale);
        self.free_textures(images.into_values().chain(stale.into_values()));
    }

    /// A picker cell: the preview (animated while hovered, a placeholder while it
    /// renders) over the effect's name. Returns true when clicked.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn effect_cell(&mut self, ui: &mut egui::Ui, item: ItemId, kind: ThumbKind, type_id: &str, name: &str, aspect: f32, selected: bool) -> bool {
        let width = crate::picker::CELL;
        let size = egui::vec2(width, width / aspect.max(0.1));
        let (rect, response) = ui.allocate_exact_size(size + egui::vec2(0.0, 20.0), egui::Sense::click());
        let texture = if response.hovered() {
            let now = ui.input(|i| i.time);
            let key = (kind, type_id.to_string());
            let since = match &self.thumbs.hover {
                Some(h) if h.key == key => h.since,
                _ => now,
            };
            self.thumbs.hover = Some(Hover { key, since, seen: true });
            ui.ctx().request_repaint();
            self.animated_thumb(item, kind, type_id, now - since).or_else(|| self.effect_thumb(item, kind, type_id))
        } else {
            self.effect_thumb(item, kind, type_id)
        };
        let image_rect = egui::Rect::from_min_size(rect.min, size);
        let painter = ui.painter_at(rect);
        match texture {
            Some(id) => {
                painter.image(id, image_rect, self.thumb_framing(), egui::Color32::WHITE);
            }
            None => {
                crate::widgets::skeleton(ui, &painter, image_rect);
            }
        }
        let outline = if selected {
            egui::Stroke::new(2.0, ui.visuals().selection.stroke.color)
        } else if response.hovered() {
            egui::Stroke::new(1.5, ui.visuals().widgets.hovered.fg_stroke.color)
        } else {
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
        };
        painter.rect_stroke(image_rect, 3.0, outline, egui::StrokeKind::Inside);
        painter.text(
            egui::pos2(rect.center().x, image_rect.bottom() + 10.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(13.0),
            ui.visuals().text_color(),
        );
        response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    }

    /// The still preview of `type_id` for `item`: asked of the preview thread the first
    /// time, picked up when it's done. Until then the one from the moment before (or
    /// `None`: a placeholder).
    fn effect_thumb(&mut self, item: ItemId, kind: ThumbKind, type_id: &str) -> Option<egui::TextureId> {
        let key = (kind, type_id.to_string());
        if let Some((id, _)) = self.thumbs.images.get(&key) {
            return Some(*id);
        }
        let generation = self.thumbs.generation.load(Ordering::Acquire);
        let slot = thumb_slot(&key);
        if let Some(done) = self.preview_worker.take(slot).filter(|d| d.tag == generation)
            && let Some(rs) = self.render_state.as_ref()
        {
            let view = oa_gpu::readback::display_view(&done.texture);
            let id = rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear);
            if let Some(old) = self.thumbs.stale.remove(&key) {
                self.free_textures(std::iter::once(old));
            }
            self.thumbs.images.insert(key, (id, done.texture));
            return Some(id);
        }
        if !self.thumbs.requested.contains(&key)
            && let Some((project, t)) = self.preview_scene(item, kind, type_id, None)
        {
            let wanted = Some((self.thumbs.generation.clone(), generation));
            let scale = self.thumb_scale();
            self.request_preview(slot, generation, project, t, scale, wanted);
            self.thumbs.requested.insert(key.clone());
        }
        // Until it's ready: the one from the moment before, if there is one.
        self.thumbs.stale.get(&key).map(|(id, _)| *id)
    }

    /// The hover animation, `phase` seconds in: this frame is asked for, and the latest
    /// one done for this cell is shown (a frame or two behind — never a wait). `None`
    /// until the first is in.
    fn animated_thumb(&mut self, item: ItemId, kind: ThumbKind, type_id: &str, phase: f64) -> Option<egui::TextureId> {
        let cell = thumb_slot(&(kind, type_id.to_string()));
        if let Some((project, t)) = self.preview_scene(item, kind, type_id, Some(phase)) {
            let scale = self.thumb_scale();
            self.request_preview(crate::preview_worker::LIVE, cell, project, t, scale, None);
        }
        if let Some(done) = self.preview_worker.take(crate::preview_worker::LIVE)
            && done.tag == cell
            && let Some(rs) = self.render_state.as_ref()
        {
            // A new texture each time: point the animation's id at it (the old one drops).
            let view = oa_gpu::readback::display_view(&done.texture);
            let id = match self.thumbs.live.take() {
                Some((id, ..)) => {
                    rs.renderer.write().update_egui_texture_from_wgpu_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear, id);
                    id
                }
                None => rs.renderer.write().register_native_texture(&self.gpu.device, &view, wgpu::FilterMode::Linear),
            };
            self.thumbs.live = Some((id, done.texture, cell));
        }
        self.thumbs.live.as_ref().filter(|(.., of)| *of == cell).map(|(id, ..)| *id)
    }

    /// The scratch project a preview renders and the moment to show: `phase` is `None`
    /// for the still preview, else seconds into the hover animation.
    fn preview_scene(&self, item: ItemId, kind: ThumbKind, type_id: &str, phase: Option<f64>) -> Option<(Project, Time)> {
        let it = self.editor.item(item)?.clone();
        let d = self.registry.effect(type_id)?.clone();
        let length = it.range.duration;
        let span = IN_OUT_PREVIEW.min(length);
        let here = self.thumb_moment().max(it.range.start).min(it.range.end() - Time(1));

        let mut fx = EffectInstance::new(EffectId(u64::MAX - 7), type_id);
        let preset = &d.preview;
        for (param, value) in preset {
            fx.params.set(param.as_str(), ParamSource::Static(value.clone()));
        }
        let secs = |s: f64| Time::from_seconds_f64(s.max(0.0));
        let t = match (kind, phase) {
            (ThumbKind::Passive, None) => here,
            (ThumbKind::Passive, Some(p)) if d.time_varying || d.kind == oa_graph::EffectKind::Motion => {
                // Let it play from the playhead, wrapping inside the clip.
                let local = ((here - it.range.start) + secs(p)).0.rem_euclid(length.0.max(1));
                it.range.start + Time(local)
            }
            (ThumbKind::Passive, Some(p)) => {
                // A still effect: sweep its strength from off to full and back.
                let k = 0.5 - 0.5 * (p * std::f64::consts::PI).cos();
                for s in &d.params {
                    let (Value::Float(default), Some((lo, _))) = (&s.default, s.range) else { continue };
                    let preset = preset.iter().find(|(id, _)| *id == s.id).and_then(|(_, v)| v.as_float());
                    let (from, to) = match preset {
                        Some(to) => (*default, to),
                        None => (lo.max(0.0).min(*default), *default),
                    };
                    fx.params.set(s.id.as_str(), ParamSource::Static(Value::Float(from + (to - from) * k)));
                }
                here
            }
            (ThumbKind::Intro, _) => {
                fx.role = EffectRole::In { duration: span };
                let p = phase.map_or(0.5 * span.as_seconds_f64(), |p| (p % (span.as_seconds_f64() + HOLD)).min(span.as_seconds_f64()));
                it.range.start + secs(p).min(span - Time(1))
            }
            (ThumbKind::Outro, _) => {
                fx.role = EffectRole::Out { duration: span };
                let p = phase.map_or(0.5 * span.as_seconds_f64(), |p| (p % (span.as_seconds_f64() + HOLD)).min(span.as_seconds_f64()));
                (it.range.end() - span + secs(p)).min(it.range.end() - Time(1))
            }
        };

        // A scratch copy of the project (never touches undo): the clip frozen on the
        // playhead's frame, with the effect added — alone, for intros and outros.
        let mut project = (*self.editor.doc.snapshot()).clone();
        let seq = Arc::make_mut(project.sequences.get_mut(&self.editor.seq)?);
        if item == oa_doc::BACKGROUND {
            // The background's own effects: shown on the whole frame.
            seq.background.effects.push(fx);
            return Some((project, t));
        }
        let (ti, ii) = seq.find_item(item)?;
        let clip = &mut Arc::make_mut(&mut seq.tracks[ti]).items[ii];
        clip.time_map = oa_doc::TimeMap::new(clip.time_map.source_time(here - clip.range.start), Rational::ZERO);
        if kind != ThumbKind::Passive {
            clip.effects.retain(|e| e.role == EffectRole::Passive);
        }
        clip.effects.push(fx);
        Some((project, t))
    }

    /// The part of the canvas previews show for the selected clip (see [`framing`]).
    fn thumb_framing(&mut self) -> egui::Rect {
        let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        if let Some(f) = self.thumbs.framing {
            return f;
        }
        let Some(item) = self.selection.and_then(|id| self.editor.item(id).cloned()) else { return full };
        let here = self.thumb_moment().max(item.range.start).min(item.range.end() - Time(1));
        let s = self.editor.sequence();
        let canvas = s.variants[self.variant.min(s.variants.len() - 1)].size;
        let canvas = [canvas.width as f64, canvas.height as f64];
        let f = oa_edit::transform::placement_of(self.editor.doc.project(), self.editor.seq, self.variant_id(), item.id, here)
            .map_or(full, |p| framing(&p, canvas, canvas[0] / canvas[1]));
        self.thumbs.framing = Some(f);
        f
    }

    /// How big previews render: sharp over the framed part of the canvas, never above
    /// full resolution.
    fn thumb_scale(&mut self) -> f64 {
        let canvas = self.editor.sequence().variants[self.variant.min(self.editor.sequence().variants.len() - 1)].size;
        let shown = canvas.width as f64 * self.thumb_framing().width() as f64;
        (THUMB_WIDTH / shown.max(1.0)).min(1.0)
    }

    /// Asks the preview thread for `project`'s open timeline at `t`, `scale` × the canvas
    /// (see `preview_worker::Request`).
    pub(crate) fn request_preview(&self, slot: u64, tag: u64, project: Project, at: Time, scale: f64, wanted: Option<(Arc<AtomicU64>, u64)>) {
        self.preview_worker.request(crate::preview_worker::Request {
            slot,
            tag,
            project: Arc::new(project),
            registry: self.registry.clone(),
            seq: self.editor.seq,
            variant: self.variant_id(),
            at,
            scale,
            wanted,
            png: None,
            see_through: false,
        });
    }
}

/// The preview thread's slot for one picker cell (clear of its fixed slots).
fn thumb_slot(key: &(ThumbKind, String)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish() | 1 << 63
}
