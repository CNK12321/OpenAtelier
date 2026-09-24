//! The keyframe line on a timeline clip: one property per clip — volume for sound,
//! opacity for pictures by default; right-click any number property in the inspector →
//! "Default keyframe property" to show that one instead — drawn across the clip with
//! its keyframes as points. On a selected clip you can drag a point (time and value),
//! drag the line to raise or lower it (all keys together when keyframed), Ctrl+click the
//! line to add a key and Ctrl+click a key to remove it.

use crate::App;
use eframe::egui;
use oa_doc::{schema, ItemId, ParamTarget, TrackKind};
use oa_params::{KeyframeAnchor, ParamSource, Value};
use oa_time::Time;

/// The property a clip's line shows, and the range the clip's height spans.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub target: ParamTarget,
    pub param: String,
    pub lo: f64,
    pub hi: f64,
}

/// Keyframe points and the line, in screen px.
pub struct BandShape {
    pub line: Vec<egui::Pos2>,
    /// Index into the curve's keys, and where it's drawn.
    pub keys: Vec<(usize, egui::Pos2)>,
}

const INSET: f32 = 3.0;

fn y_of(rect: egui::Rect, band: &Band, v: f64) -> f32 {
    let f = ((v - band.lo) / (band.hi - band.lo).max(1e-9)).clamp(0.0, 1.0) as f32;
    rect.bottom() - INSET - f * (rect.height() - 2.0 * INSET)
}

/// Value per screen px of height, for drags.
pub fn per_px(rect: egui::Rect, band: &Band) -> f64 {
    (band.hi - band.lo) / (rect.height() - 2.0 * INSET).max(1.0) as f64
}

impl App {
    /// The property `item`'s line shows.
    pub(crate) fn clip_band(&self, item: ItemId) -> Option<Band> {
        if let Some(b) = self.bands.get(&item) {
            return Some(b.clone());
        }
        let s = self.editor.sequence();
        let (t, _) = s.find_item(item)?;
        Some(if s.tracks[t].kind == TrackKind::Audio {
            Band { target: ParamTarget::Item, param: schema::AUDIO_GAIN.into(), lo: -60.0, hi: 12.0 }
        } else {
            Band { target: ParamTarget::Item, param: schema::OPACITY.into(), lo: 0.0, hi: 1.0 }
        })
    }

    /// Where the line and its keys are drawn over `rect` (a clip from `start` to
    /// `end`), sampling every few px.
    pub(crate) fn band_shape(&self, item: ItemId, band: &Band, rect: egui::Rect, x_of: &dyn Fn(Time) -> f32, time_at: &dyn Fn(f32) -> Time) -> Option<BandShape> {
        let it = self.editor.item(item)?;
        let value = |t: Time| self.editor.param_value(item, &band.target, &band.param, t).and_then(|v| v.as_float());
        let default = || if band.param == schema::OPACITY { 1.0 } else { 0.0 };
        let mut line = Vec::new();
        let mut x = rect.left();
        while x <= rect.right() {
            let t = time_at(x).max(it.range.start).min(it.range.end() - Time(1));
            line.push(egui::pos2(x, y_of(rect, band, value(t).unwrap_or_else(default))));
            x += 3.0;
        }
        let keys = self
            .editor
            .param_source(item, &band.target, &band.param)
            .and_then(|s| s.curve().filter(|c| c.anchor == KeyframeAnchor::ClipStart).cloned())
            .map(|c| {
                c.keys
                    .iter()
                    .enumerate()
                    .filter_map(|(i, k)| Some((i, egui::pos2(x_of(it.range.start + k.t), y_of(rect, band, k.value.as_float()?)))))
                    .collect()
            })
            .unwrap_or_default();
        Some(BandShape { line, keys })
    }

    /// Drags the line: a key (`key`) to the pointer, or the whole line by the vertical
    /// distance moved since the grab (`original` is how it was stored then).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn drag_band(&mut self, item: ItemId, band: &Band, key: Option<usize>, original: &ParamSource, dy: f32, pos: egui::Pos2, rect: egui::Rect, time_at: &dyn Fn(f32) -> Time) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let clamp = |v: f64| v.clamp(band.lo, band.hi);
        let dv = -(dy as f64) * per_px(rect, band);
        let mut src = original.clone();
        match (key, src.curve_mut()) {
            (Some(i), Some(curve)) if i < curve.keys.len() => {
                // Between its neighbors (a frame apart), inside the clip.
                let frame = self.editor.sequence().rate.frame_start(1).max(Time(1));
                let lo = if i > 0 { curve.keys[i - 1].t + frame } else { Time::ZERO };
                let hi = curve.keys.get(i + 1).map_or(it.range.duration - Time(1), |k| k.t - frame);
                let t = (time_at(pos.x) - it.range.start).max(lo).min(hi.max(lo));
                let v = clamp(band.lo + ((rect.bottom() - INSET - pos.y) as f64) * per_px(rect, band));
                curve.keys[i].t = t;
                curve.keys[i].value = Value::Float(v);
            }
            (_, Some(curve)) => {
                for k in &mut curve.keys {
                    if let Some(v) = k.value.as_float() {
                        k.value = Value::Float(clamp(v + dv));
                    }
                }
            }
            (_, None) => {
                let now = self.editor.param_value(item, &band.target, &band.param, it.range.start).and_then(|v| v.as_float());
                let base = match original {
                    ParamSource::Static(Value::Float(v)) => *v,
                    _ => now.unwrap_or(if band.param == schema::OPACITY { 1.0 } else { 0.0 }),
                };
                src = ParamSource::Static(Value::Float(clamp(base + dv)));
            }
        }
        self.editor.set_param(item, band.target.clone(), &band.param, src, "keyframe-line");
    }

    /// Ctrl+click on the line: a key at `t` with the value there (keyframing the property
    /// if it wasn't). On a key (`key`): removes it.
    pub(crate) fn toggle_band_key(&mut self, item: ItemId, band: &Band, key: Option<usize>, t: Time) {
        let Some(it) = self.editor.item(item).cloned() else { return };
        let ctx = it.eval_context(t);
        let current = self.editor.param_value(item, &band.target, &band.param, t).unwrap_or(Value::Float(if band.param == schema::OPACITY { 1.0 } else { 0.0 }));
        let mut src = self.editor.param_source(item, &band.target, &band.param).unwrap_or(ParamSource::Static(current.clone()));
        match (key, src.curve_mut()) {
            (Some(i), Some(curve)) if curve.keys.len() > 1 => {
                curve.keys.remove(i);
            }
            (Some(_), Some(curve)) => {
                // Last key: back to a plain value.
                let v = curve.keys[0].value.clone();
                src = ParamSource::Static(v);
            }
            _ => {
                src = src.keyframed(&ctx, KeyframeAnchor::ClipStart);
                src.set_at(&ctx, current);
            }
        }
        self.editor.set_param(item, band.target.clone(), &band.param, src, "keyframe-line-key");
        self.editor.doc.seal();
    }
}
