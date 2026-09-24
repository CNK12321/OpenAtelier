//! Canvas formats, aspect-ratio presets, format variants and reframing.
//!
//! A sequence has one edit (timing, clips, effects) and one or more **format
//! variants** — e.g. "YouTube 16:9" and "Shorts 9:16". Variants share frame rate and
//! timing; each has its own canvas size and may override any visual parameter of any
//! item (reframe focus, position, scale, text size...). Exporting every variant from
//! one edit is the intended workflow.

use crate::model::{ItemId, VariantId};
use oa_params::ParamSet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanvasSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl CanvasSize {
    pub const fn new(width: u32, height: u32) -> Self {
        CanvasSize { width, height }
    }

    pub fn aspect(self) -> f64 {
        self.width as f64 / self.height as f64
    }

    pub fn orientation(self) -> Orientation {
        match self.width.cmp(&self.height) {
            std::cmp::Ordering::Greater => Orientation::Landscape,
            std::cmp::Ordering::Less => Orientation::Portrait,
            std::cmp::Ordering::Equal => Orientation::Square,
        }
    }

    /// Swap horizontal ↔ vertical (16:9 1920×1080 → 9:16 1080×1920).
    pub fn rotated(self) -> Self {
        CanvasSize { width: self.height, height: self.width }
    }

    pub fn short_edge(self) -> u32 {
        self.width.min(self.height)
    }

    /// The preset whose ratio this size matches (within half a pixel), if any.
    pub fn matching_preset(self) -> Option<&'static AspectPreset> {
        PRESETS.iter().find(|p| p.size(self.short_edge()) == self)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AspectPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub ratio: (u32, u32),
    pub platforms: &'static str,
}

impl AspectPreset {
    /// Every preset, in menu order.
    pub fn all() -> &'static [AspectPreset] {
        PRESETS
    }

    pub fn by_id(id: &str) -> Option<&'static AspectPreset> {
        PRESETS.iter().find(|p| p.id == id)
    }

    /// Canvas size with the given short edge (e.g. 720, 1080, 1440, 2160). The long
    /// edge is rounded to an even number, which video encoders require.
    pub fn size(&self, short_edge: u32) -> CanvasSize {
        let (rw, rh) = self.ratio;
        let (lo, hi) = (rw.min(rh) as u64, rw.max(rh) as u64);
        let long = ((short_edge as u64 * hi + lo) / (2 * lo)) * 2;
        let long = long as u32;
        if rw >= rh { CanvasSize::new(long, short_edge) } else { CanvasSize::new(short_edge, long) }
    }

    pub fn orientation(&self) -> Orientation {
        CanvasSize::new(self.ratio.0, self.ratio.1).orientation()
    }
}

pub const PRESETS: &[AspectPreset] = &[
    AspectPreset { id: "landscape-16x9", name: "Landscape 16:9", ratio: (16, 9), platforms: "YouTube, TV, most screens" },
    AspectPreset { id: "vertical-9x16", name: "Vertical 9:16", ratio: (9, 16), platforms: "TikTok, Reels, Shorts, Stories" },
    AspectPreset { id: "square-1x1", name: "Square 1:1", ratio: (1, 1), platforms: "Instagram, X, LinkedIn feeds" },
    AspectPreset { id: "portrait-4x5", name: "Portrait 4:5", ratio: (4, 5), platforms: "Instagram / Facebook feed" },
    AspectPreset { id: "classic-4x3", name: "Classic 4:3", ratio: (4, 3), platforms: "Presentations, retro" },
    AspectPreset { id: "portrait-3x4", name: "Portrait 3:4", ratio: (3, 4), platforms: "Tablets, Pinterest" },
    AspectPreset { id: "ultrawide-21x9", name: "Ultrawide 21:9", ratio: (21, 9), platforms: "Ultrawide monitors" },
    AspectPreset { id: "cinema-2.39", name: "Cinemascope 2.39:1", ratio: (239, 100), platforms: "Cinematic letterbox" },
];

/// How a layer's source is placed onto the canvas before the user's transform.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FitMode {
    /// Whole source visible; may letterbox/pillarbox.
    Fit,
    /// Canvas fully covered; source cropped around the reframe focus point.
    #[default]
    Fill,
    /// Non-uniform scale to exactly the canvas.
    Stretch,
    /// Source pixels map 1:1 to canvas pixels, centered.
    None,
}

impl FitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FitMode::Fit => "fit",
            FitMode::Fill => "fill",
            FitMode::Stretch => "stretch",
            FitMode::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fit" => FitMode::Fit,
            "fill" => FitMode::Fill,
            "stretch" => FitMode::Stretch,
            "none" => FitMode::None,
            _ => return None,
        })
    }
}

/// Base placement: `canvas = source * scale + offset` (canvas pixels, y down).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Reframe {
    pub scale: [f64; 2],
    pub offset: [f64; 2],
}

impl Reframe {
    /// `focus` is a point in the source (0..1 per axis) that `Fill` keeps as close to
    /// the canvas center as possible without exposing empty canvas.
    pub fn compute(source: [f64; 2], canvas: CanvasSize, fit: FitMode, focus: [f64; 2]) -> Self {
        let cw = canvas.width as f64;
        let ch = canvas.height as f64;
        let (sw, sh) = (source[0].max(1e-9), source[1].max(1e-9));
        let uniform = |s: f64| [s, s];
        let scale = match fit {
            FitMode::Fit => uniform((cw / sw).min(ch / sh)),
            FitMode::Fill => uniform((cw / sw).max(ch / sh)),
            FitMode::Stretch => [cw / sw, ch / sh],
            FitMode::None => [1.0, 1.0],
        };
        let size = [sw * scale[0], sh * scale[1]];
        let axis = |i: usize, canvas_len: f64| {
            let centered = (canvas_len - size[i]) / 2.0;
            if fit != FitMode::Fill {
                return centered;
            }
            let want = canvas_len / 2.0 - focus[i].clamp(0.0, 1.0) * size[i];
            // Fill covers the canvas, so the offset lives in [canvas - size, 0]. Rounding
            // can leave `size` a hair under `canvas_len` on the fitted axis, which would
            // make that an empty range — clamp the low end to 0 so it stays valid.
            want.clamp((canvas_len - size[i]).min(0.0), 0.0)
        };
        Reframe { scale, offset: [axis(0, cw), axis(1, ch)] }
    }

    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        [p[0] * self.scale[0] + self.offset[0], p[1] * self.scale[1] + self.offset[1]]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormatVariant {
    pub id: VariantId,
    pub name: String,
    pub size: CanvasSize,
    /// Per-item parameter overrides for this variant only.
    #[serde(default)]
    pub overrides: BTreeMap<ItemId, ParamSet>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_sizes() {
        let s = |id: &str, e| AspectPreset::by_id(id).unwrap().size(e);
        assert_eq!(s("landscape-16x9", 1080), CanvasSize::new(1920, 1080));
        assert_eq!(s("landscape-16x9", 2160), CanvasSize::new(3840, 2160));
        assert_eq!(s("landscape-16x9", 720), CanvasSize::new(1280, 720));
        assert_eq!(s("vertical-9x16", 1080), CanvasSize::new(1080, 1920));
        assert_eq!(s("portrait-4x5", 1080), CanvasSize::new(1080, 1350));
        assert_eq!(s("ultrawide-21x9", 1080), CanvasSize::new(2520, 1080));
        assert_eq!(s("cinema-2.39", 1080), CanvasSize::new(2582, 1080));
        for p in PRESETS {
            let c = p.size(1080);
            assert!(c.width % 2 == 0 && c.height % 2 == 0, "{}", p.id);
            assert_eq!(c.matching_preset().map(|m| m.id), Some(p.id));
        }
    }

    #[test]
    fn rotate_orientation() {
        let c = CanvasSize::new(1920, 1080);
        assert_eq!(c.orientation(), Orientation::Landscape);
        assert_eq!(c.rotated().orientation(), Orientation::Portrait);
        assert_eq!(c.rotated().matching_preset().unwrap().id, "vertical-9x16");
    }

    #[test]
    fn reframe_landscape_into_vertical() {
        let src = [3840.0, 2160.0];
        let canvas = CanvasSize::new(1080, 1920);
        // Fill: height matches, width overflows; focus on the right third.
        let r = Reframe::compute(src, canvas, FitMode::Fill, [2.0 / 3.0, 0.5]);
        assert!((r.scale[0] - 1920.0 / 2160.0).abs() < 1e-12);
        let focus_px = r.apply([3840.0 * 2.0 / 3.0, 1080.0]);
        assert!((focus_px[0] - 540.0).abs() < 1e-9 && (focus_px[1] - 960.0).abs() < 1e-9);
        // Focus at the far edge clamps so no empty canvas shows.
        let edge = Reframe::compute(src, canvas, FitMode::Fill, [1.0, 0.5]);
        assert!((edge.apply([3840.0, 0.0])[0] - 1080.0).abs() < 1e-9);
        // Fit: letterboxed, centered vertically.
        let fit = Reframe::compute(src, canvas, FitMode::Fit, [0.0, 0.0]);
        let top = fit.apply([0.0, 0.0]);
        assert!((top[0]).abs() < 1e-9 && (top[1] - (1920.0 - 607.5) / 2.0).abs() < 1e-9);
    }

    /// The axis that exactly fits can land a hair over or under the canvas; both must
    /// produce a valid, centered offset rather than panicking.
    #[test]
    fn fill_handles_rounding_on_the_fitted_axis() {
        for source in [[235.0, 192.0], [1170.0, 606.0], [1920.0, 1080.0], [150.0, 150.0]] {
            for canvas in [CanvasSize::new(1920, 1080), CanvasSize::new(1080, 1920), CanvasSize::new(1080, 1080)] {
                let r = Reframe::compute(source, canvas, FitMode::Fill, [0.5, 0.5]);
                assert!(r.offset.iter().all(|o| o.is_finite()), "{source:?} into {canvas:?}");
                let covered = r.apply([source[0], source[1]]);
                assert!(covered[0] >= canvas.width as f64 - 1e-6 && covered[1] >= canvas.height as f64 - 1e-6);
            }
        }
    }

    #[test]
    fn fit_mode_round_trips() {
        for m in [FitMode::Fit, FitMode::Fill, FitMode::Stretch, FitMode::None] {
            assert_eq!(FitMode::parse(m.as_str()), Some(m));
        }
    }
}
