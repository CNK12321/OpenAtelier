//! Connections: a property that follows the sound. Its value gets an offset that rises
//! with how loud the mix — or one chosen clip — is at that instant, overall or in its
//! bass, mids or treble: a title that pulses with the kick, a glow that flares on the
//! hi-hats, a picture that shakes when someone shouts. Opened from a property's
//! right-click menu (Animate → "Edit connection offset…").
//!
//! Levels come from loudness envelopes analyzed in the background per file
//! (`oa_audio::envelope`); frames are planned with them installed
//! (`oa_params::signal`), so the viewer and the export agree.

use crate::band::Band;
use crate::App;
use eframe::egui;
use oa_audio::envelope::{media_of, Follower};
use oa_doc::{ItemId, MediaId};
use oa_params::{Modulator, ParamSource, SoundBand, SoundSource, Value};
use oa_time::Time;
use std::collections::HashMap;
use std::sync::Arc;

/// The open connection editor.
pub struct ConnectionEditor {
    pub item: ItemId,
    pub band: Band,
}

impl App {
    pub(crate) fn edit_connection(&mut self, item: ItemId, band: Band) {
        self.connection_editor = Some(ConnectionEditor { item, band });
    }

    /// Keeps [`App::follower`] in step with the document and the envelopes analyzed so
    /// far — only when some property is connected (otherwise nothing is analyzed).
    pub(crate) fn refresh_follower(&mut self, ctx: &egui::Context) {
        let project = self.editor.doc.snapshot();
        let doc = Arc::as_ptr(&project) as usize;
        // Same document, every file analyzed: nothing to do.
        if self.follower_complete && self.follower_key.0 == doc {
            return;
        }
        if !project.follows_sound() {
            self.follower = None;
            self.follower_key = (doc, 0);
            self.follower_complete = true;
            return;
        }
        let (clips, _) = self.audio_mix_of(self.editor.seq);
        let files = media_of(&clips);
        let mut ready = HashMap::new();
        for (media, path) in &files {
            if let Some(e) = self.clip_previews.envelope(ctx, MediaId(*media), path) {
                ready.insert(*media, e);
            }
        }
        let key = (doc, ready.len());
        if self.follower.is_none() || self.follower_key != key {
            self.follower = Some(Arc::new(Follower::new(&clips, &ready)));
            self.follower_key = key;
        }
        // Files that fail to analyze stay silent; don't wait on them forever.
        self.follower_complete = files.iter().all(|(m, _)| !self.clip_previews.envelope_pending(MediaId(*m)));
    }

    /// `param`'s value at `t` with the sound playing (connections included).
    fn value_with_sound(&self, item: ItemId, band: &Band, t: Time) -> Option<f64> {
        let read = || self.editor.param_value(item, &band.target, &band.param, t).and_then(|v| v.as_float());
        match &self.follower {
            Some(f) => oa_params::signal::with(f.at(t), read),
            None => read(),
        }
    }

    pub(crate) fn connection_window(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.connection_editor.as_ref() else { return };
        let (item, band) = (ed.item, ed.band.clone());
        let Some(it) = self.editor.item(item).cloned() else {
            self.connection_editor = None;
            return;
        };
        let mut open = true;
        let title = format!("Connection — {} · {}", it.name, band.param.rsplit('.').next().unwrap_or(&band.param));
        egui::Window::new(title)
            .id(egui::Id::new("connection-editor"))
            .open(&mut open)
            .default_width(460.0)
            .resizable(true)
            .show(ctx, |ui| self.connection_body(ui, item, &band, &it));
        if !open {
            self.connection_editor = None;
            self.editor.doc.seal();
        }
    }

    fn connection_body(&mut self, ui: &mut egui::Ui, item: ItemId, band: &Band, it: &oa_doc::Item) {
        let start = it.eval_context(it.range.start);
        let current = self.editor.param_value(item, &band.target, &band.param, it.range.start).unwrap_or(Value::Float(band.lo.max(0.0)));
        let source = self.editor.param_source(item, &band.target, &band.param).unwrap_or(ParamSource::Static(current));
        let Some((_, Modulator::Follow { source: from, band: which, amount, floor_db })) = source.find_follow().map(|(b, m)| (b.clone(), m.clone())) else {
            ui.label("Connects this value to the sound: it's offset by how loud things are at each moment — all of it, or just the bass, mids or treble.");
            if ui.button("Connect to the sound").clicked() {
                let reach = ((band.hi - band.lo) * 0.25).abs().max(1e-3);
                let src = source.follow(SoundSource::Mix, SoundBand::Loudness, reach, -48.0);
                self.editor.set_param(item, band.target.clone(), &band.param, src, "connection-editor");
                self.editor.doc.seal();
            }
            return;
        };
        let read = |s: &ParamSource| s.eval(&start).as_float().unwrap_or(0.0);
        let (mut from, mut which, mut amt, mut floor) = (from, which, read(&amount), read(&floor_db));
        let mut changed = false;
        let mut finished = false;
        let track = |r: &egui::Response, changed: &mut bool, finished: &mut bool| {
            *changed |= r.changed();
            *finished |= r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
        };

        // What it listens to: everything, or one clip that has sound.
        let (clips, _) = self.audio_mix_of(self.editor.seq);
        let mut heard: Vec<(u64, String)> = Vec::new();
        for c in &clips {
            if !heard.iter().any(|(id, _)| *id == c.id)
                && let Some(i) = self.editor.item(ItemId(c.id))
            {
                heard.push((c.id, i.name.clone()));
            }
        }
        let name_of = |s: SoundSource| match s {
            SoundSource::Mix => "Everything playing".to_string(),
            SoundSource::Item(id) => heard.iter().find(|(i, _)| *i == id).map_or_else(|| "A clip that's gone".to_string(), |(_, n)| n.clone()),
        };
        egui::Grid::new("connection-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Listens to");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("connection-source").selected_text(name_of(from)).show_ui(ui, |ui| {
                    for s in std::iter::once(SoundSource::Mix).chain(heard.iter().map(|(id, _)| SoundSource::Item(*id))) {
                        if ui.selectable_label(from == s, name_of(s)).clicked() && from != s {
                            from = s;
                            changed = true;
                            finished = true;
                        }
                    }
                });
                let picked = self.selected.iter().copied().find(|id| *id != item && heard.iter().any(|(h, _)| *h == id.0));
                if ui.add_enabled(picked.is_some(), egui::Button::new("Use selected").small()).on_hover_text("Listen to the other selected clip on the timeline").clicked()
                    && let Some(id) = picked
                {
                    from = SoundSource::Item(id.0);
                    changed = true;
                    finished = true;
                }
            });
            ui.end_row();
            ui.label("Part");
            ui.horizontal(|ui| {
                for b in SoundBand::ALL {
                    let tip = match b {
                        SoundBand::Loudness => "The whole sound",
                        SoundBand::Bass => "Below about 200 Hz: kicks, bass lines",
                        SoundBand::Mids => "About 200 Hz to 4 kHz: voices, most instruments",
                        SoundBand::Treble => "Above about 4 kHz: hi-hats, sibilance, sparkle",
                    };
                    if ui.selectable_label(which == b, b.name()).on_hover_text(tip).clicked() && which != b {
                        which = b;
                        changed = true;
                        finished = true;
                    }
                }
            });
            ui.end_row();
            ui.label("Offset");
            let r = ui
                .add(egui::DragValue::new(&mut amt).speed((band.hi - band.lo).abs() * 0.005 + 1e-3).max_decimals(3))
                .on_hover_text("Added at full level (negative pulls it the other way)");
            track(&r, &mut changed, &mut finished);
            ui.end_row();
            ui.label("Ignore below");
            let r = ui.add(egui::Slider::new(&mut floor, -80.0..=-12.0).suffix(" dB")).on_hover_text("Quieter than this counts as silence; raise it to react only to the loud parts");
            track(&r, &mut changed, &mut finished);
            ui.end_row();
        });

        // The result over the clip, with the sound playing.
        let (outer, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 110.0), egui::Sense::hover());
        let painter = ui.painter_at(outer);
        painter.rect_filled(outer, 4.0, ui.visuals().extreme_bg_color);
        let rect = outer.shrink2(egui::vec2(8.0, 10.0));
        let duration = it.range.duration;
        const STEPS: usize = 240;
        let samples: Vec<f64> = (0..=STEPS)
            .map(|i| {
                let t = it.range.start + Time::from_seconds_f64(duration.as_seconds_f64() * i as f64 / STEPS as f64).min(duration - Time(1));
                self.value_with_sound(item, band, t).unwrap_or(0.0)
            })
            .collect();
        let (vmin, vmax) = samples.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
        let pad = ((vmax - vmin) * 0.1).max(1e-3);
        let (vlo, vhi) = (vmin - pad, vmax + pad);
        let y = |v: f64| rect.bottom() - ((v - vlo) / (vhi - vlo)) as f32 * rect.height();
        for v in [vmin, vmax] {
            painter.line_segment([egui::pos2(rect.left(), y(v)), egui::pos2(rect.right(), y(v))], egui::Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.4)));
            painter.text(egui::pos2(rect.left() + 2.0, y(v) - 1.0), egui::Align2::LEFT_BOTTOM, format!("{v:.2}"), egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
        }
        let pts: Vec<egui::Pos2> = samples.iter().enumerate().map(|(i, v)| egui::pos2(rect.left() + rect.width() * i as f32 / STEPS as f32, y(*v))).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, crate::style::ACCENT)));
        let at = ((self.playhead - it.range.start).as_seconds_f64() / duration.as_seconds_f64().max(1e-9)).clamp(0.0, 1.0) as f32;
        let px = rect.left() + at * rect.width();
        painter.line_segment([egui::pos2(px, outer.top()), egui::pos2(px, outer.bottom())], egui::Stroke::new(1.0, egui::Color32::from_rgb(230, 70, 70)));
        if self.follower.as_ref().is_none_or(|f| f.is_empty()) {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Listening to the sound…", egui::FontId::proportional(12.0), ui.visuals().weak_text_color());
        }

        ui.horizontal(|ui| {
            if ui.button("Disconnect").clicked() {
                let src = source.clone().remove_follow();
                self.editor.set_param(item, band.target.clone(), &band.param, src, "connection-editor");
                self.editor.doc.seal();
            }
            ui.label(egui::RichText::new("Keyframes and waves still work: the offset rides on top.").small().weak());
        });

        if changed {
            let mut src = source.clone();
            if let Some((_, Modulator::Follow { source, band: b, amount, floor_db })) = src.find_follow_mut() {
                *source = from;
                *b = which;
                **amount = ParamSource::Static(Value::Float(if amt.is_finite() { amt } else { 0.0 }));
                **floor_db = ParamSource::Static(Value::Float(if floor.is_finite() { floor.clamp(-120.0, -7.0) } else { -48.0 }));
            }
            self.editor.set_param(item, band.target.clone(), &band.param, src, "connection-editor");
        }
        if finished {
            self.editor.doc.seal();
        }
    }
}
