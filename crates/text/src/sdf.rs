//! Glyph distance fields: each glyph rasterized once per size bucket into a signed
//! distance field, which the GPU scales to any size and uses for crisp edges, outlines
//! and soft glows.

use crate::fonts::{self, FontId};
use ab_glyph::Font as _;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;

/// Em sizes glyphs are rasterized at. Drawn text picks the smallest bucket at least as
/// large as it is on screen (the largest for anything bigger).
pub const BUCKETS: [u32; 3] = [32, 64, 128];

/// Distance range each side of the edge, in bucket px, as a fraction of the em.
pub const SPREAD_EM: f32 = 0.2;

pub fn bucket_for(em_px: f64) -> u32 {
    BUCKETS.iter().copied().find(|b| *b as f64 >= em_px).unwrap_or(BUCKETS[BUCKETS.len() - 1])
}

pub fn spread_px(bucket: u32) -> f32 {
    (bucket as f32 * SPREAD_EM).ceil()
}

/// A glyph's distance field: 8-bit, 0.5 on the outline, rising inside.
pub struct GlyphSdf {
    pub size: [u32; 2],
    pub pixels: Vec<u8>,
    /// The bitmap's top-left relative to the pen position on the baseline, bucket px.
    pub offset: [f32; 2],
    pub bucket: u32,
    /// Distance (bucket px) that 0 and 255 stand for.
    pub spread: f32,
}

type Key = (FontId, u16, u32);

enum Slot {
    Pending,
    Done(Option<Arc<GlyphSdf>>),
}

struct Workers {
    slots: Mutex<HashMap<Key, Slot>>,
    queue: Mutex<std::sync::mpsc::Sender<Key>>,
}

fn workers() -> &'static Workers {
    static W: OnceLock<Workers> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Key>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..2 {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("oa-glyphs-{i}"))
                .spawn(move || loop {
                    let Ok(key) = rx.lock().unwrap_or_else(|e| e.into_inner()).recv() else { return };
                    let field = glyph_sdf(key.0, key.1, key.2).map(Arc::new);
                    let w = workers();
                    w.slots.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Slot::Done(field));
                })
                .expect("spawn glyph worker");
        }
        Workers { slots: Mutex::new(HashMap::new()), queue: Mutex::new(tx) }
    })
}

/// The glyph's distance field without waiting: `Ready` once a worker thread has made it
/// (handed over once — the caller keeps it), `Pending` meanwhile (the first call queues
/// it). `Ready(None)` for glyphs without an outline.
pub fn glyph_sdf_nonblocking(font: FontId, glyph: u16, bucket: u32) -> Poll<Option<Arc<GlyphSdf>>> {
    let w = workers();
    let key = (font, glyph, bucket);
    let mut slots = w.slots.lock().unwrap_or_else(|e| e.into_inner());
    match slots.remove(&key) {
        Some(Slot::Done(field)) => Poll::Ready(field),
        Some(Slot::Pending) => {
            slots.insert(key, Slot::Pending);
            Poll::Pending
        }
        None => {
            slots.insert(key, Slot::Pending);
            let _ = w.queue.lock().unwrap_or_else(|e| e.into_inner()).send(key);
            Poll::Pending
        }
    }
}

/// 1-D squared distance transform (Felzenszwalb & Huttenlocher).
fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    let meet = |q: usize, p: usize| ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
    for q in 1..n {
        let mut s = meet(q, v[k]);
        // z[0] is -inf, so this stops at k = 0.
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        *out = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// Squared distance from each pixel to the nearest pixel where `seed` is true.
fn edt(seed: &[bool], w: usize, h: usize) -> Vec<f32> {
    const FAR: f32 = 1e20;
    let mut grid: Vec<f32> = seed.iter().map(|s| if *s { 0.0 } else { FAR }).collect();
    let n = w.max(h);
    let (mut f, mut d, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&grid[y * w..(y + 1) * w]);
        edt_1d(&f[..w], &mut d[..w], &mut v, &mut z);
        grid[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    grid
}

/// Rasterizes one glyph's distance field at `bucket` px per em. `None` for glyphs
/// without an outline (spaces).
pub fn glyph_sdf(font: FontId, glyph: u16, bucket: u32) -> Option<GlyphSdf> {
    let face = fonts::face(font);
    let f = &face.font;
    let scale = bucket as f32 * f.height_unscaled() / face.units_per_em();
    let g = ab_glyph::GlyphId(glyph).with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
    let outlined = f.outline_glyph(g)?;
    let b = outlined.px_bounds();
    let spread = spread_px(bucket);
    let pad = spread.ceil() as u32 + 1;
    let (gw, gh) = (b.width().ceil() as u32, b.height().ceil() as u32);
    let (w, h) = ((gw + 2 * pad) as usize, (gh + 2 * pad) as usize);
    let mut cover = vec![0.0f32; w * h];
    outlined.draw(|x, y, c| {
        let (x, y) = ((x + pad) as usize, (y + pad) as usize);
        if x < w && y < h {
            cover[y * w + x] = c.clamp(0.0, 1.0);
        }
    });
    let inside: Vec<bool> = cover.iter().map(|c| *c >= 0.5).collect();
    let outside: Vec<bool> = inside.iter().map(|i| !i).collect();
    let to_inside = edt(&inside, w, h);
    let to_outside = edt(&outside, w, h);
    let pixels = (0..w * h)
        .map(|i| {
            let c = cover[i];
            // Signed distance to the outline in px, positive inside. Pixels the edge
            // passes through use their coverage (sub-pixel accurate); the rest the
            // distance transform.
            let far = if inside[i] { to_outside[i].sqrt() - 0.5 } else { -(to_inside[i].sqrt() - 0.5) };
            let s = if far.abs() <= 1.0 && c > 0.0 && c < 1.0 { c - 0.5 } else { far };
            ((0.5 + s / (2.0 * spread)).clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();
    Some(GlyphSdf {
        size: [w as u32, h as u32],
        pixels,
        offset: [b.min.x - pad as f32, b.min.y - pad as f32],
        bucket,
        spread,
    })
}
