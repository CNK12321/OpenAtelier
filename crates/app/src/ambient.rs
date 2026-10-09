//! The viewer's glow takes its colors from the picture shown: each side of the frame
//! glows (and is outlined) in the colors near that edge, like a screen lighting the
//! wall behind it.
//!
//! The picture on screen is shrunk on the GPU to a few pixels — softly blurred by the
//! shrinking — and read back without waiting (`oa_gpu::glance`), at most every 100 ms
//! and only when a new frame has been shown. The glow takes the colors in slowly, as if
//! they burned into it the longer the picture stays.

use crate::preview_worker::FramePixels;
use crate::App;
use eframe::egui;
use std::time::{Duration, Instant};

/// The shrunk picture's short side, px: its outer eighth makes each edge's color.
const GLANCE_PX: u32 = 16;
/// Not more often than this.
const EVERY: Duration = Duration::from_millis(100);
/// How long the glow takes to take in a new color (seconds to get ~63% of the way).
const BURN_IN: f32 = 2.0;

/// What the glow knows of the picture.
pub struct Ambient {
    glance: Option<oa_gpu::glance::Glance>,
    /// A frame's been shown since the last read.
    pub(crate) fresh: bool,
    since: Instant,
    /// The edges' colors (top, right, bottom, left) of the last picture read.
    target: Option<[egui::Rgba; 4]>,
    /// What's drawn, easing towards `target`.
    shown: Option<[egui::Rgba; 4]>,
}

impl Default for Ambient {
    fn default() -> Self {
        Ambient { glance: None, fresh: false, since: Instant::now(), target: None, shown: None }
    }
}

/// The average color along each edge (top, right, bottom, left) of an RGBA picture: its
/// outer two rows or columns, weighted by how opaque they are.
pub fn edge_colors(frame: &FramePixels) -> Option<[egui::Rgba; 4]> {
    let [w, h] = frame.size;
    if w == 0 || h == 0 || frame.pixels.len() < w * h * 4 {
        return None;
    }
    let band = 2.min(w).min(h);
    let average = |pixels: &mut dyn Iterator<Item = (usize, usize)>| {
        let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
        for (x, y) in pixels {
            let p = &frame.pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
            let a = p[3] as f32 / 255.0;
            let c = egui::Rgba::from(egui::Color32::from_rgb(p[0], p[1], p[2]));
            sum = [sum[0] + c.r() * a, sum[1] + c.g() * a, sum[2] + c.b() * a];
            weight += a;
        }
        if weight <= 1e-3 { egui::Rgba::from_rgb(0.0, 0.0, 0.0) } else { egui::Rgba::from_rgb(sum[0] / weight, sum[1] / weight, sum[2] / weight) }
    };
    let top = average(&mut (0..band).flat_map(|y| (0..w).map(move |x| (x, y))));
    let bottom = average(&mut (h - band..h).flat_map(|y| (0..w).map(move |x| (x, y))));
    let left = average(&mut (0..h).flat_map(|y| (0..band).map(move |x| (x, y))));
    let right = average(&mut (0..h).flat_map(|y| (w - band..w).map(move |x| (x, y))));
    Some([top, right, bottom, left])
}

impl App {
    /// Takes in a glance at the picture that's come back, and starts the next when a new
    /// frame has been shown; eases the glow along. Called once a frame.
    pub(crate) fn ambient_tick(&mut self, ctx: &egui::Context) {
        if self.gpu_lost {
            return;
        }
        let gpu = self.gpu.clone();
        let glance = self.ambient.glance.get_or_insert_with(|| oa_gpu::glance::Glance::new(&gpu));
        if let Some((pixels, size)) = glance.poll(&gpu)
            && let Some(colors) = edge_colors(&FramePixels { pixels, size: [size[0] as usize, size[1] as usize] })
        {
            self.ambient.target = Some(colors);
        }
        if glance.busy() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.ambient.fresh
            && let Some(preview) = &self.preview
        {
            if self.ambient.since.elapsed() >= EVERY {
                glance.start(&gpu, &preview._texture, GLANCE_PX);
                self.ambient.fresh = false;
                self.ambient.since = Instant::now();
            } else {
                ctx.request_repaint_after(EVERY);
            }
        }
        // The glow takes the newest colors in slowly.
        if let Some(target) = self.ambient.target {
            let shown = self.ambient.shown.get_or_insert(target);
            let dt = ctx.input(|i| i.stable_dt).min(0.1);
            let k = 1.0 - (-dt / BURN_IN).exp();
            let mut moving = false;
            for (s, t) in shown.iter_mut().zip(target) {
                let next = *s + (t + *s * -1.0) * k;
                moving |= (next.r() - t.r()).abs() + (next.g() - t.g()).abs() + (next.b() - t.b()).abs() > 0.004;
                *s = next;
            }
            if moving {
                ctx.request_repaint_after(Duration::from_millis(33));
            }
        }
    }

    /// The edge colors to draw (top, right, bottom, left), once a picture's been read.
    pub(crate) fn ambient_colors(&self) -> Option<[egui::Color32; 4]> {
        self.ambient.shown.map(|s| s.map(|c| c.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_edge_glows_in_its_own_color() {
        // 4×4: red along the top row, blue down the right column, the rest black; a
        // see-through pixel doesn't darken its edge.
        let (w, h) = (4, 4);
        let mut pixels = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = &mut pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
                p[3] = 255;
                if y == 0 {
                    p[0] = 255;
                } else if x == w - 1 {
                    p[2] = 255;
                }
            }
        }
        pixels[(3 * w) * 4 + 3] = 0;
        let [top, right, bottom, left] = edge_colors(&FramePixels { pixels, size: [w, h] }).unwrap();
        assert!(top.r() > 0.4 && top.b() < 0.3, "the top is reddish");
        assert!(right.b() > 0.3 && right.b() > right.g(), "the right is bluish (its outer two columns, one blue)");
        assert!(bottom.r() < 0.01 && left.b() < 0.01, "the rest dark");
    }
}
