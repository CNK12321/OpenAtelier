//! The start page's backdrop: a ghost of a timeline playing behind it. Faint track
//! rows — pictures above, sound below — full of clips of every length, some with a
//! keyframe line, the sound ones with their waveforms, all scrolling slowly by under a
//! ruler of running timecodes, a slow drifting light bringing some stretches up and
//! letting others fade. The pointer is the playhead: it glides after the pointer and
//! fades in, and what's near it brightens gently. Drawn in the theme's text color,
//! very faint, with a soft backing in the page's color under its panels so they read
//! clearly over it; off with the interface's UI effects.

use eframe::egui;

/// A track row's height and the gap under it (points).
const ROW: f32 = 30.0;
const ROW_GAP: f32 = 8.0;
/// The ruler along the top (points).
const RULER: f32 = 22.0;
/// How fast it plays: points a second the timeline scrolls by.
const SPEED: f32 = 22.0;
/// A second of the ghost timeline, in points (for its timecodes).
const SECOND: f32 = 60.0;
/// The timeline is laid out in stretches this long, each row's clips packed into them.
const CHUNK: f32 = 1600.0;

/// A well-mixed number from three integers.
fn hash(x: i32, y: i32, z: i32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841) ^ (z as u32).wrapping_mul(0xCB1A_B31F);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5BD1_E995);
    h ^ (h >> 15)
}

/// 0 → 1 from a hash.
fn unit(h: u32) -> f32 {
    (h & 0xFFFF) as f32 / 65535.0
}

/// One clip of the ghost timeline: where it starts and ends (timeline points), and a
/// number of its own (for its look).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ghost {
    pub start: f32,
    pub end: f32,
    pub seed: u32,
}

/// Row `row`'s clips that reach into `from..to` (timeline points): laid out stretch by
/// stretch, each packed with clips of 70–420 points and gaps up to 140 (often none, the
/// clips butting together as cuts do), never overlapping.
pub(crate) fn row_clips(row: i32, from: f32, to: f32) -> Vec<Ghost> {
    let mut out = Vec::new();
    let first = (from / CHUNK).floor() as i32;
    let last = (to / CHUNK).floor() as i32;
    for chunk in first..=last {
        let mut x = chunk as f32 * CHUNK + unit(hash(row, chunk, 1)) * 120.0;
        let end = (chunk + 1) as f32 * CHUNK;
        for k in 0.. {
            let h = hash(row, chunk, 10 + k);
            let len = 70.0 + unit(h) * 350.0;
            if x + len > end - 10.0 {
                break;
            }
            if x + len > from && x < to {
                out.push(Ghost { start: x, end: x + len, seed: h });
            }
            let gap = if unit(hash(row, chunk, 1000 + k)) < 0.45 { 0.0 } else { 8.0 + unit(hash(row, chunk, 2000 + k)) * 130.0 };
            x += len + gap;
        }
    }
    out
}

/// `seconds` as a timecode, hours:minutes:seconds:frames at 24 fps.
fn timecode(seconds: f32) -> String {
    let frames = (seconds * 24.0).floor().max(0.0) as u64;
    let (f, s) = (frames % 24, frames / 24);
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, f)
}

/// Smooth value noise, 0 → 1.
fn noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let s = |f: f32| f * f * (3.0 - 2.0 * f);
    let (sx, sy) = (s(fx), s(fy));
    let at = |dx: i32, dy: i32| unit(hash(ix + dx, iy + dy, 7));
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * sx;
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * sx;
    top + (bottom - top) * sy
}

/// How bright the ghost timeline is at page point (`x`, `y`) at `time` (seconds): a
/// slow noise drifting across it, so some stretches show and others all but fade — 0.15
/// at its dimmest to 1.25 at its brightest.
pub(crate) fn glow(x: f32, y: f32, time: f32) -> f32 {
    let n = 0.7 * noise(x / 320.0 + time * 0.035, y / 220.0 - time * 0.02) + 0.3 * noise(x / 140.0 - time * 0.05, y / 110.0 + time * 0.03);
    let b = ((n - 0.3) / 0.45).clamp(0.0, 1.0);
    0.15 + 1.1 * b * b * (3.0 - 2.0 * b)
}

/// Where the playhead is drawn and how much it shows, eased after the pointer: it glides
/// to where the pointer is and fades in or out, never jumping (kept between frames).
#[derive(Clone, Copy)]
struct Playhead {
    x: f32,
    shown: f32,
    at: f64,
}

/// Paints the ghost timeline over `rect` (under whatever's drawn after it), and asks
/// for the next frame of it.
pub(crate) fn ghost_timeline(ui: &egui::Ui, rect: egui::Rect) {
    let now = ui.input(|i| i.time);
    let time = now as f32;
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
    // The playhead, eased towards the pointer.
    let key = egui::Id::new("ghost-timeline-playhead");
    let last: Option<Playhead> = ui.data(|d| d.get_temp(key));
    let dt = last.map_or(0.0, |l| (now - l.at).clamp(0.0, 0.25) as f32);
    let mut head = last.unwrap_or(Playhead { x: pointer.map_or(rect.center().x, |p| p.x), shown: 0.0, at: now });
    let ease = |rate: f32| 1.0 - (-dt * rate).exp();
    if let Some(p) = pointer {
        // Back after being away: it fades in where the pointer is, not from far off.
        head.x = if head.shown < 0.05 { p.x } else { head.x + (p.x - head.x) * ease(7.0) };
    }
    head.shown += (pointer.map_or(0.0, |_| 1.0) - head.shown) * ease(4.0);
    head.at = now;
    ui.data_mut(|d| d.insert_temp(key, head));

    let painter = ui.painter_at(rect);
    let ink = ui.visuals().text_color();
    let faint = |a: f32| ink.gamma_multiply(a.min(1.0));
    // The timeline point at the page's left edge.
    let scroll = time * SPEED;
    let to_x = |t: f32| rect.left() + t - scroll;
    let (from, to) = (scroll, scroll + rect.width());

    // The ruler: a tick every half second, a timecode every two.
    let ruler = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), RULER));
    painter.line_segment([ruler.left_bottom(), ruler.right_bottom()], egui::Stroke::new(1.0, faint(0.04)));
    let half = SECOND / 2.0;
    let mut tick = (from / half).floor() * half;
    while tick <= to {
        let x = to_x(tick);
        let g = glow(x, ruler.center().y, time);
        let n = (tick / half).round() as i64;
        let tall = n % 4 == 0;
        painter.line_segment([egui::pos2(x, ruler.bottom() - if tall { 8.0 } else { 4.0 }), egui::pos2(x, ruler.bottom())], egui::Stroke::new(1.0, faint(0.06 * g)));
        if tall {
            painter.text(egui::pos2(x + 3.0, ruler.top() + 3.0), egui::Align2::LEFT_TOP, timecode(tick / SECOND), egui::FontId::monospace(9.0), faint(0.08 * g));
        }
        tick += half;
    }

    // The rows: about two thirds pictures, then sound.
    let rows = (((rect.height() - RULER) / (ROW + ROW_GAP)).ceil() as i32).max(1);
    let pictures = ((rows as f32 * 0.62).round() as i32).max(1);
    // How much the playhead lights what's at page x: most right under it, fading over a
    // couple of hundred points either side — so moving over a clip brightens it gently.
    let lit = |x0: f32, x1: f32| {
        let d = if head.x < x0 { x0 - head.x } else if head.x > x1 { head.x - x1 } else { 0.0 };
        head.shown * (-(d / 160.0).powi(2)).exp()
    };
    for row in 0..rows {
        let top = ruler.bottom() + 6.0 + row as f32 * (ROW + ROW_GAP);
        let sound = row >= pictures;
        for clip in row_clips(row, from, to) {
            let (x0, x1) = (to_x(clip.start), to_x(clip.end));
            let r = egui::Rect::from_min_max(egui::pos2(x0 + 1.0, top), egui::pos2(x1 - 1.0, top + ROW));
            let k = glow(r.center().x, r.center().y, time) * (1.0 + 0.9 * lit(x0, x1));
            painter.rect(r, 4.0, faint(0.02 * k), egui::Stroke::new(1.0, faint(0.035 * k)), egui::StrokeKind::Inside);
            if sound {
                // Its waveform: a line every 3 points, as loud as a gentle noise says,
                // brighter and dimmer along the clip with the drifting light.
                let mid = r.center().y;
                let mut x = r.left() + 3.0;
                while x < r.right() - 2.0 {
                    let i = ((x - x0) / 3.0) as i32;
                    let a = 0.5 * unit(hash(clip.seed as i32, i, 3)) + 0.5 * unit(hash(clip.seed as i32, i / 6, 4));
                    let h = (ROW * 0.42) * (0.15 + 0.85 * a);
                    let k = glow(x, mid, time) * (1.0 + 0.9 * lit(x, x));
                    painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], egui::Stroke::new(1.0, faint(0.04 * k)));
                    x += 3.0;
                }
            } else {
                // Its name bar along the top.
                let label = egui::Rect::from_min_size(r.min + egui::vec2(6.0, 5.0), egui::vec2((r.width() - 12.0) * (0.25 + 0.4 * unit(clip.seed >> 3)), 4.0));
                if label.width() > 6.0 {
                    painter.rect_filled(label, 2.0, faint(0.04 * k));
                }
                // Some carry a keyframe line, with a diamond at each key.
                if clip.seed % 3 == 0 && r.width() > 60.0 {
                    let keys = 2 + (clip.seed >> 5) % 3;
                    let y = |j: u32| r.bottom() - 7.0 - unit(hash(clip.seed as i32, j as i32, 5)) * (ROW - 18.0);
                    let at = |j: u32| egui::pos2(r.left() + 10.0 + (r.width() - 20.0) * j as f32 / keys as f32, y(j));
                    let line: Vec<egui::Pos2> = (0..=keys).map(at).collect();
                    painter.add(egui::Shape::line(line.clone(), egui::Stroke::new(1.0, faint(0.045 * k))));
                    for p in line {
                        let d = 3.0;
                        painter.add(egui::Shape::convex_polygon(vec![p + egui::vec2(0.0, -d), p + egui::vec2(d, 0.0), p + egui::vec2(0.0, d), p + egui::vec2(-d, 0.0)], faint(0.07 * k), egui::Stroke::NONE));
                    }
                }
            }
        }
    }

    // The playhead: with its head on the ruler and the moment it's at, fading in and out.
    if head.shown > 0.01 {
        let x = head.x;
        painter.line_segment([egui::pos2(x, ruler.top() + 2.0), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, faint(0.1 * head.shown)));
        let tip = egui::pos2(x, ruler.bottom());
        painter.add(egui::Shape::convex_polygon(vec![tip + egui::vec2(-5.0, -9.0), tip + egui::vec2(5.0, -9.0), tip], faint(0.16 * head.shown), egui::Stroke::NONE));
        let at = (x - rect.left() + scroll) / SECOND;
        painter.text(egui::pos2(x + 7.0, ruler.bottom() + 3.0), egui::Align2::LEFT_TOP, timecode(at), egui::FontId::monospace(9.0), faint(0.16 * head.shown));
    }
    // About 20 frames a second: enough for a slow, even scroll.
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
}

/// A soft backing under a panel of the start page, in the page's own color: its edges
/// feathered into the backdrop, so the panel reads clearly over the timeline without a
/// hard box round it.
pub(crate) fn backing(ui: &egui::Ui, rect: egui::Rect) {
    let fill = ui.visuals().panel_fill;
    ui.painter().add(egui::epaint::Shadow { offset: [0, 0], blur: 36, spread: 4, color: fill.gamma_multiply(0.9) }.as_shape(rect, 14.0));
    ui.painter().rect_filled(rect, 14.0, fill.gamma_multiply(0.93));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's clips never overlap, are of sensible lengths, fill much of the row but
    /// not all of it, and come out the same whatever stretch is asked for.
    #[test]
    fn ghost_clips_fill_their_rows_without_overlapping() {
        for row in 0..12 {
            let clips = row_clips(row, 0.0, 8000.0);
            assert!(clips.windows(2).all(|w| w[0].end <= w[1].start), "row {row}: no overlaps");
            assert!(clips.iter().all(|c| (70.0..=420.0).contains(&(c.end - c.start))));
            let filled: f32 = clips.iter().map(|c| c.end.min(8000.0) - c.start.max(0.0)).sum::<f32>() / 8000.0;
            assert!(filled > 0.4 && filled < 0.97, "row {row}: {filled}");
            // A part of it asked for alone: the same clips.
            let part = row_clips(row, 2500.0, 4000.0);
            assert!(part.iter().all(|c| clips.contains(c)) && !part.is_empty());
        }
    }

    /// The drifting light leaves some of it dim and brings some of it up — not all one
    /// brightness — and it moves.
    #[test]
    fn the_light_drifts_over_bright_and_dim_stretches() {
        let samples: Vec<f32> = (0..60).flat_map(|i| (0..30).map(move |j| glow(i as f32 * 25.0, j as f32 * 25.0, 0.0))).collect();
        let (dim, bright) = (samples.iter().filter(|g| **g < 0.4).count(), samples.iter().filter(|g| **g > 0.9).count());
        assert!(dim > 100 && bright > 100, "dim {dim}, bright {bright}");
        assert!(samples.iter().all(|g| (0.15..=1.26).contains(g)));
        let moved = (0..60).filter(|i| (glow(*i as f32 * 25.0, 300.0, 0.0) - glow(*i as f32 * 25.0, 300.0, 30.0)).abs() > 0.1).count();
        assert!(moved > 15, "it drifts: {moved}");
    }

    #[test]
    fn timecodes_count_frames() {
        assert_eq!(timecode(0.0), "00:00:00:00");
        assert_eq!(timecode(61.5), "00:01:01:12");
        assert_eq!(timecode(3600.0 + 2.0), "01:00:02:00");
    }
}
