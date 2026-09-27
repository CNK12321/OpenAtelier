//! Video scopes for the Color tab, drawn from a small copy of the frame (sRGB, 8-bit):
//! a luma **waveform**, the RGB **parade**, a **vectorscope** and a **histogram**. Each
//! is a picture — counts piled up and brightened on a log scale, the way a hardware scope
//! glows where many pixels land — with its graticule drawn over it by the tab.

use eframe::egui;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Waveform, Scope::Parade, Scope::Vectorscope, Scope::Histogram];

    pub fn name(self) -> &'static str {
        match self {
            Scope::Waveform => "Waveform",
            Scope::Parade => "Parade",
            Scope::Vectorscope => "Vectorscope",
            Scope::Histogram => "Histogram",
        }
    }

    /// The picture's size.
    pub fn size(self) -> [usize; 2] {
        match self {
            Scope::Vectorscope => [180, 180],
            _ => [256, 150],
        }
    }
}

/// BT.709 luma of 8-bit sRGB-encoded values, 0..1.
fn luma(p: &[u8]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Counts to light: 0 dark, busier brighter (log, so a few pixels still show).
fn glow(count: u32, most: u32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    ((1.0 + count as f32).ln() / (1.0 + most.max(1) as f32).ln()).clamp(0.0, 1.0).powf(0.6)
}

/// `scope` of the frame `pixels` (`size`, straight RGBA8, sRGB).
pub fn draw(scope: Scope, pixels: &[u8], size: [usize; 2]) -> egui::ColorImage {
    let [w, h] = scope.size();
    let mut out = egui::ColorImage::new([w, h], vec![egui::Color32::from_rgb(8, 9, 11); w * h]);
    let px = |x: usize, y: usize| &pixels[(y * size[0] + x) * 4..(y * size[0] + x) * 4 + 4];
    match scope {
        Scope::Waveform | Scope::Parade => {
            // Channels drawn, each over its own stretch of the width (one for the waveform).
            let lanes: &[(usize, egui::Color32)] = if scope == Scope::Waveform {
                &[(3, egui::Color32::from_rgb(140, 255, 160))]
            } else {
                &[(0, egui::Color32::from_rgb(255, 90, 90)), (1, egui::Color32::from_rgb(90, 255, 110)), (2, egui::Color32::from_rgb(100, 140, 255))]
            };
            let lane_w = w / lanes.len();
            for (lane, (channel, color)) in lanes.iter().enumerate() {
                let mut counts = vec![0u32; lane_w * h];
                for y in 0..size[1] {
                    for x in 0..size[0] {
                        let p = px(x, y);
                        let v = if *channel == 3 { luma(p) } else { p[*channel] as f32 / 255.0 };
                        let (cx, cy) = (x * lane_w / size[0].max(1), ((1.0 - v) * (h - 1) as f32).round() as usize);
                        counts[cy.min(h - 1) * lane_w + cx.min(lane_w - 1)] += 1;
                    }
                }
                let most = counts.iter().copied().max().unwrap_or(1);
                for y in 0..h {
                    for x in 0..lane_w {
                        let g = glow(counts[y * lane_w + x], most);
                        if g > 0.0 {
                            out[(lane * lane_w + x, y)] = color.gamma_multiply(g);
                        }
                    }
                }
            }
        }
        Scope::Vectorscope => {
            // Cb across, Cr up (BT.709), ±0.5 to the edge.
            let mut counts = vec![0u32; w * h];
            let mut tint = vec![[0f32; 3]; w * h];
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let p = px(x, y);
                    let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
                    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    let (cb, cr) = ((b - l) / 1.8556, (r - l) / 1.5748);
                    let (sx, sy) = (vector_xy(cb, cr, w), vector_xy_y(cr, h));
                    let i = sy * w + sx;
                    counts[i] += 1;
                    let t = &mut tint[i];
                    t[0] += r;
                    t[1] += g;
                    t[2] += b;
                }
            }
            let most = counts.iter().copied().max().unwrap_or(1);
            for (i, c) in counts.iter().enumerate() {
                let g = glow(*c, most);
                if g > 0.0 {
                    // Lit in the colors that land there, never too dark to see.
                    let n = *c as f32;
                    let col = tint[i].map(|v| ((v / n) * 0.6 + 0.4).min(1.0));
                    out[(i % w, i / w)] = egui::Color32::from_rgb((col[0] * 255.0 * g) as u8, (col[1] * 255.0 * g) as u8, (col[2] * 255.0 * g) as u8);
                }
            }
        }
        Scope::Histogram => {
            let mut bins = [[0u32; 256]; 4];
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let p = px(x, y);
                    for c in 0..3 {
                        bins[c][p[c] as usize] += 1;
                    }
                    bins[3][(luma(p) * 255.0).round() as usize] += 1;
                }
            }
            let most = bins.iter().flat_map(|b| b.iter()).copied().max().unwrap_or(1) as f32;
            let colors = [[255.0, 60.0, 60.0], [60.0, 255.0, 80.0], [80.0, 120.0, 255.0]];
            for x in 0..w {
                let bin = x * 256 / w;
                let tops = [0, 1, 2, 3].map(|c| ((bins[c][bin] as f32 / most).sqrt() * (h - 1) as f32) as usize);
                for y in 0..h {
                    let up = h - 1 - y;
                    // The three channels add up where they overlap; the luma line on top.
                    let mut rgb = [0.0f32; 3];
                    for c in 0..3 {
                        if bins[c][bin] > 0 && up <= tops[c] {
                            for k in 0..3 {
                                rgb[k] += colors[c][k] * 0.45;
                            }
                        }
                    }
                    if bins[3][bin] > 0 && up == tops[3] {
                        rgb = [230.0, 230.0, 230.0];
                    }
                    if rgb != [0.0; 3] {
                        out[(x, y)] = egui::Color32::from_rgb(rgb[0].min(255.0) as u8, rgb[1].min(255.0) as u8, rgb[2].min(255.0) as u8);
                    }
                }
            }
        }
    }
    out
}

/// Where a Cb value lands across the vectorscope (±0.5 fills it).
pub fn vector_xy(cb: f32, _cr: f32, w: usize) -> usize {
    (((cb / 0.5) * 0.5 + 0.5) * (w - 1) as f32).round().clamp(0.0, (w - 1) as f32) as usize
}

/// ...and a Cr value up it.
pub fn vector_xy_y(cr: f32, h: usize) -> usize {
    ((0.5 - (cr / 0.5) * 0.5) * (h - 1) as f32).round().clamp(0.0, (h - 1) as f32) as usize
}

/// The vectorscope's targets: where 75% red, yellow, green, cyan, blue and magenta land,
/// as fractions of its width and height, with their names.
pub fn vector_targets() -> Vec<(&'static str, [f32; 2], egui::Color32)> {
    let at = |r: f32, g: f32, b: f32| {
        let (r, g, b) = (r * 0.75, g * 0.75, b * 0.75);
        let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let (cb, cr) = ((b - l) / 1.8556, (r - l) / 1.5748);
        [cb + 0.5, 0.5 - cr]
    };
    vec![
        ("R", at(1.0, 0.0, 0.0), egui::Color32::from_rgb(255, 80, 80)),
        ("Yl", at(1.0, 1.0, 0.0), egui::Color32::from_rgb(240, 230, 80)),
        ("G", at(0.0, 1.0, 0.0), egui::Color32::from_rgb(80, 240, 100)),
        ("Cy", at(0.0, 1.0, 1.0), egui::Color32::from_rgb(80, 230, 240)),
        ("B", at(0.0, 0.0, 1.0), egui::Color32::from_rgb(100, 140, 255)),
        ("Mg", at(1.0, 0.0, 1.0), egui::Color32::from_rgb(240, 90, 240)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(color: [u8; 3], size: [usize; 2]) -> Vec<u8> {
        (0..size[0] * size[1]).flat_map(|_| [color[0], color[1], color[2], 255]).collect()
    }

    fn lit(img: &egui::ColorImage) -> Vec<(usize, usize)> {
        let [w, h] = img.size;
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| img[(x, y)].r() > 20 || img[(x, y)].g() > 20 || img[(x, y)].b() > 20).collect()
    }

    /// A flat mid grey sits on one line half-way up the waveform, in the middle of the
    /// vectorscope, and in one column of the histogram.
    #[test]
    fn a_grey_frame_draws_where_it_should() {
        let size = [32, 18];
        let grey = frame([128, 128, 128], size);
        let wave = draw(Scope::Waveform, &grey, size);
        let rows: std::collections::BTreeSet<usize> = lit(&wave).iter().map(|p| p.1).collect();
        assert_eq!(rows.len(), 1, "one line: {rows:?}");
        let row = *rows.iter().next().unwrap();
        assert!((row as f32 / 149.0 - 0.5).abs() < 0.02, "half-way: {row}");
        let vector = draw(Scope::Vectorscope, &grey, size);
        assert_eq!(lit(&vector), vec![(90, 90)], "grey has no color: the center");
        let hist = draw(Scope::Histogram, &grey, size);
        let columns: std::collections::BTreeSet<usize> = lit(&hist).iter().map(|p| p.0).collect();
        assert_eq!(columns.len(), 1, "{columns:?}");
    }

    /// Pure red lands on the red target's side of the vectorscope, and in the parade only
    /// the red lane is high.
    #[test]
    fn red_goes_to_red() {
        let size = [16, 9];
        let red = frame([255, 0, 0], size);
        let v = lit(&draw(Scope::Vectorscope, &red, size));
        let (x, y) = v[0];
        let target = vector_targets()[0].1;
        assert!((x as f32 / 179.0 > 0.5) == (target[0] > 0.5) && (y as f32 / 179.0 < 0.5) == (target[1] < 0.5), "{v:?} vs {target:?}");
        let parade = draw(Scope::Parade, &red, size);
        let top_of = |lane: usize| lit(&parade).iter().filter(|p| p.0 / 85 == lane).map(|p| p.1).min();
        assert_eq!(top_of(0), Some(0), "red at the top");
        assert_eq!(top_of(1), Some(149), "green at the bottom");
    }
}
