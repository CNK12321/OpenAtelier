//! The OpenAtelier mark: the "A" in `assets/logo.svg`, drawn from the SVG's own path —
//! filled in egui (the start page) and rasterized for the window's icon — so there's one
//! source for it and no image files to keep in step.

use eframe::egui;
use std::sync::OnceLock;

const SVG: &str = include_str!("../../../assets/logo.svg");

/// The mark's outline, in fractions of its bounding square (0..1, centered), and the
/// triangles that fill it.
struct Shape {
    points: Vec<[f32; 2]>,
    triangles: Vec<[usize; 3]>,
}

fn shape() -> &'static Shape {
    static SHAPE: OnceLock<Shape> = OnceLock::new();
    SHAPE.get_or_init(|| {
        let d = path_data(SVG).unwrap_or_default();
        let raw = flatten(d);
        let points = normalize(&raw);
        let triangles = triangulate(&points);
        Shape { points, triangles }
    })
}

/// The first path's `d` that draws something.
fn path_data(svg: &str) -> Option<&str> {
    svg.split("<path").skip(1).filter_map(|p| p.split(" d=\"").nth(1)?.split('"').next()).find(|d| d.trim().len() > 20)
}

/// The path's outline as points: `M`, `L`, `H`, `V`, `C` and `Z`, absolute or relative;
/// curves as 16 straight pieces each.
fn flatten(d: &str) -> Vec<[f32; 2]> {
    let mut tokens: Vec<Result<char, f32>> = Vec::new();
    let mut number = String::new();
    let flush = |number: &mut String, tokens: &mut Vec<Result<char, f32>>| {
        if let Ok(x) = number.parse::<f32>() {
            tokens.push(Err(x));
        }
        number.clear();
    };
    for ch in d.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' if ch != 'e' && ch != 'E' => {
                flush(&mut number, &mut tokens);
                tokens.push(Ok(ch));
            }
            '-' if !number.is_empty() && !number.ends_with(['e', 'E']) => {
                flush(&mut number, &mut tokens);
                number.push(ch);
            }
            '.' if number.contains('.') => {
                flush(&mut number, &mut tokens);
                number.push(ch);
            }
            '0'..='9' | '.' | '-' | 'e' | 'E' | '+' => number.push(ch),
            _ => flush(&mut number, &mut tokens),
        }
    }
    flush(&mut number, &mut tokens);

    let mut out: Vec<[f32; 2]> = Vec::new();
    let (mut pos, mut command) = ([0.0f32; 2], 'M');
    let mut i = 0;
    let take = |i: &mut usize, n: usize| -> Option<Vec<f32>> {
        let v: Option<Vec<f32>> = tokens.get(*i..*i + n)?.iter().map(|t| t.err()).collect();
        if v.is_some() {
            *i += n;
        }
        v
    };
    while i < tokens.len() {
        if let Ok(c) = tokens[i] {
            command = c;
            i += 1;
            if c.eq_ignore_ascii_case(&'z') {
                continue;
            }
        }
        let rel = command.is_ascii_lowercase();
        let base = if rel { pos } else { [0.0, 0.0] };
        match command.to_ascii_uppercase() {
            'M' | 'L' => {
                let Some(v) = take(&mut i, 2) else { break };
                pos = [base[0] + v[0], base[1] + v[1]];
                out.push(pos);
                // Numbers after a moveto are linetos.
                if command == 'M' {
                    command = 'L';
                } else if command == 'm' {
                    command = 'l';
                }
            }
            'H' => {
                let Some(v) = take(&mut i, 1) else { break };
                pos[0] = base[0] + v[0];
                out.push(pos);
            }
            'V' => {
                let Some(v) = take(&mut i, 1) else { break };
                pos[1] = base[1] + v[0];
                out.push(pos);
            }
            'C' => {
                let Some(v) = take(&mut i, 6) else { break };
                let p0 = pos;
                let p1 = [base[0] + v[0], base[1] + v[1]];
                let p2 = [base[0] + v[2], base[1] + v[3]];
                let p3 = [base[0] + v[4], base[1] + v[5]];
                for k in 1..=16 {
                    let t = k as f32 / 16.0;
                    let u = 1.0 - t;
                    let (a, b, c, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    out.push([a * p0[0] + b * p1[0] + c * p2[0] + e * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + e * p3[1]]);
                }
                pos = p3;
            }
            _ => i += 1,
        }
    }
    out.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3);
    if out.len() > 2 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// Into a 0..1 square, centered, keeping its proportions.
fn normalize(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in points {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-6);
    let pad = [(size - (hi[0] - lo[0])) / 2.0, (size - (hi[1] - lo[1])) / 2.0];
    points.iter().map(|p| [(p[0] - lo[0] + pad[0]) / size, (p[1] - lo[1] + pad[1]) / size]).collect()
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Ear clipping: triangles filling a simple polygon (the mark isn't convex).
fn triangulate(points: &[[f32; 2]]) -> Vec<[usize; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }
    let area: f32 = (0..n).map(|i| cross([0.0, 0.0], points[i], points[(i + 1) % n])).sum();
    let sign = area.signum();
    let mut left: Vec<usize> = (0..n).collect();
    let mut out = Vec::new();
    let mut guard = 0;
    while left.len() > 3 && guard < n * n {
        guard += 1;
        let m = left.len();
        let mut clipped = false;
        for k in 0..m {
            let (a, b, c) = (left[(k + m - 1) % m], left[k], left[(k + 1) % m]);
            if cross(points[a], points[b], points[c]) * sign <= 0.0 {
                continue; // a reflex corner
            }
            let inside = left.iter().any(|&p| {
                p != a && p != b && p != c && {
                    let q = points[p];
                    let (d1, d2, d3) = (cross(points[a], points[b], q) * sign, cross(points[b], points[c], q) * sign, cross(points[c], points[a], q) * sign);
                    d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0
                }
            });
            if !inside {
                out.push([a, b, c]);
                left.remove(k);
                clipped = true;
                break;
            }
        }
        if !clipped {
            break;
        }
    }
    if left.len() == 3 {
        out.push([left[0], left[1], left[2]]);
    }
    out
}

/// The mark, filled, in `rect` (kept square and centered).
pub fn paint(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let s = shape();
    let side = rect.width().min(rect.height());
    let origin = rect.center() - egui::vec2(side, side) / 2.0;
    let mut mesh = egui::Mesh::default();
    for p in &s.points {
        mesh.colored_vertex(origin + egui::vec2(p[0] * side, p[1] * side), color);
    }
    for t in &s.triangles {
        mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
    }
    painter.add(mesh);
    // Meshes aren't antialiased; a hairline around the edge (which is) smooths it.
    let outline = s.points.iter().map(|p| origin + egui::vec2(p[0] * side, p[1] * side)).collect();
    painter.add(egui::Shape::closed_line(outline, egui::Stroke::new(0.75, color)));
}

/// The app's badge: the mark in white on a rounded square of the accent color.
pub fn badge(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, rect.width() * 0.24, crate::style::ACCENT);
    paint(painter, rect.shrink(rect.width() * 0.2), egui::Color32::WHITE);
}

/// Is `p` (0..1) inside the mark?
fn contains(points: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The badge as a `size`² RGBA picture (the window and taskbar icon), 4×4 samples a pixel.
pub fn icon(size: u32) -> egui::IconData {
    let s = shape();
    let accent = crate::style::ACCENT;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let n = size as f32;
    let radius = 0.24;
    for y in 0..size {
        for x in 0..size {
            let (mut square, mut mark) = (0.0f32, 0.0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let u = (x as f32 + (sx as f32 + 0.5) / 4.0) / n;
                    let v = (y as f32 + (sy as f32 + 0.5) / 4.0) / n;
                    // The rounded square.
                    let dx = (u - 0.5).abs() - (0.5 - radius);
                    let dy = (v - 0.5).abs() - (0.5 - radius);
                    let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
                    if d <= radius {
                        square += 1.0 / 16.0;
                        // The mark, in the middle 60%.
                        let m = [(u - 0.2) / 0.6, (v - 0.2) / 0.6];
                        if (0.0..=1.0).contains(&m[0]) && (0.0..=1.0).contains(&m[1]) && contains(&s.points, m) {
                            mark += 1.0 / 16.0;
                        }
                    }
                }
            }
            let white = if square > 0.0 { mark / square } else { 0.0 };
            let mix = |c: u8| (c as f32 * (1.0 - white) + 255.0 * white).round() as u8;
            rgba.extend([mix(accent.r()), mix(accent.g()), mix(accent.b()), (square * 255.0).round() as u8]);
        }
    }
    egui::IconData { rgba, width: size, height: size }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logo's path reads as one outline and fills completely: the triangles cover
    /// exactly the area inside it.
    #[test]
    fn the_mark_is_read_from_the_svg_and_filled() {
        let s = shape();
        assert!(s.points.len() > 40, "{} points", s.points.len());
        assert_eq!(s.triangles.len(), s.points.len() - 2, "every corner clipped");
        let area = |t: &[usize; 3]| cross(s.points[t[0]], s.points[t[1]], s.points[t[2]]).abs() / 2.0;
        let filled: f32 = s.triangles.iter().map(area).sum();
        let n = s.points.len();
        let outline = (0..n).map(|i| cross([0.0, 0.0], s.points[i], s.points[(i + 1) % n])).sum::<f32>().abs() / 2.0;
        assert!((filled - outline).abs() < 1e-3, "{filled} vs {outline}");
        // An "A": solid at the bottom left leg, open between the legs.
        assert!(contains(&s.points, [0.12, 0.95]));
        assert!(!contains(&s.points, [0.5, 0.9]));
    }

    /// Writes `assets/logo.ico` — the program's icon in Windows (Explorer, the Start
    /// menu, the installer), embedded by `build.rs` — from the same drawing as the window
    /// icon. Run it after changing the logo:
    /// `cargo test -p oa-app write_the_windows_icon -- --ignored`.
    #[test]
    #[ignore]
    fn write_the_windows_icon() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/logo.ico");
        std::fs::write(&path, windows_icon(&[16, 24, 32, 48, 64, 256])).unwrap();
    }

    /// Writes `assets/logo-256.png`, the icon Linux desktops show (the `.deb` and the
    /// `install.sh` in the Linux package put it beside the desktop entry):
    /// `cargo test -p oa-app write_the_linux_icon -- --ignored`.
    #[test]
    #[ignore]
    fn write_the_linux_icon() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/logo-256.png");
        let icon = icon(256);
        let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut enc = png::Encoder::new(file, 256, 256);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&icon.rgba).unwrap();
    }

    /// An `.ico` holding the icon at each size (PNG-compressed, as Windows reads them).
    fn windows_icon(sizes: &[u32]) -> Vec<u8> {
        let images: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&s| {
                let icon = icon(s);
                let mut png = Vec::new();
                let mut enc = png::Encoder::new(&mut png, s, s);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&icon.rgba).unwrap();
                png
            })
            .collect();
        let mut out = Vec::new();
        out.extend(0u16.to_le_bytes()); // reserved
        out.extend(1u16.to_le_bytes()); // an icon
        out.extend((sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (&s, image) in sizes.iter().zip(&images) {
            let side = if s >= 256 { 0 } else { s as u8 }; // 0 means 256
            out.extend([side, side, 0, 0]);
            out.extend(1u16.to_le_bytes()); // planes
            out.extend(32u16.to_le_bytes()); // bits per pixel
            out.extend((image.len() as u32).to_le_bytes());
            out.extend(offset.to_le_bytes());
            offset += image.len() as u32;
        }
        for image in images {
            out.extend(image);
        }
        out
    }

    #[test]
    fn the_windows_icon_is_well_formed() {
        let ico = windows_icon(&[16, 256]);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
        // The second entry's image starts where it says, with a PNG signature.
        let offset = u32::from_le_bytes(ico[6 + 16 + 12..6 + 16 + 16].try_into().unwrap()) as usize;
        assert_eq!(&ico[offset..offset + 8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(ico[6 + 16], 0, "256 px is written as 0");
    }
}
