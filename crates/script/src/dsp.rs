//! The memory scripts keep between runs — delay lines and filter states — laid out so
//! the interpreter and compiled code ([`crate::jit`]) share it, and the helpers both use.

/// A delay line: a ring of samples. `pos` is the slot this run writes; the host moves it
/// on after each run.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub ptr: *mut f32,
    pub len: u32,
    pub pos: u32,
}

impl Line {
    pub const EMPTY: Line = Line { ptr: std::ptr::null_mut(), len: 0, pos: 0 };

    /// A line over `buf` (which must outlive it and not move).
    pub fn over(buf: &mut [f32]) -> Line {
        Line { ptr: buf.as_mut_ptr(), len: buf.len() as u32, pos: 0 }
    }

    fn slice(&self) -> &[f32] {
        if self.ptr.is_null() {
            &[]
        } else {
            // SAFETY: a line always points at a live buffer of `len` samples.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) }
        }
    }

    /// The value `samples` back from this run's slot (0: this slot), blending between
    /// slots; as far back as the line reaches.
    pub fn read(&self, samples: f32) -> f32 {
        let buf = self.slice();
        let len = buf.len();
        if len < 2 {
            return 0.0;
        }
        let d = samples.max(0.0).min((len - 2) as f32);
        let whole = d.floor();
        let k = d - whole;
        let pos = self.pos as i64;
        let wrap = |i: i64| if i < 0 { (i + len as i64) as usize } else { i as usize };
        let i = wrap(pos - whole as i64);
        let j = wrap(i as i64 - 1);
        buf[i] + (buf[j] - buf[i]) * k
    }

    pub fn write(&self, x: f32) {
        if !self.ptr.is_null() && self.pos < self.len {
            // SAFETY: in bounds of the live buffer.
            unsafe { *self.ptr.add(self.pos as usize) = x };
        }
    }

    /// The most (or least) of the last `samples` (rounded) slots, this one included.
    pub fn extreme(&self, samples: f32, max: bool) -> f32 {
        let buf = self.slice();
        let len = buf.len();
        if len == 0 {
            return 0.0;
        }
        let back = (samples.round().max(0.0) as usize).min(len - 1);
        let pos = self.pos as usize;
        // The slots from `back` ago up to this one: at most two runs of the ring.
        let (older, newer) = if back <= pos { (&buf[..0], &buf[pos - back..=pos]) } else { (&buf[len - (back - pos)..], &buf[..=pos]) };
        let values = older.iter().chain(newer).copied();
        if max { values.fold(f32::MIN, f32::max) } else { values.fold(f32::MAX, f32::min) }
    }

    /// On to the next run's slot.
    pub fn advance(&mut self) {
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
    }
}

/// The biquad filters scripts have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    LowPass,
    HighPass,
    BandPass,
    Peak,
    LowShelf,
    HighShelf,
}

impl Shape {
    pub const ALL: [Shape; 6] = [Shape::LowPass, Shape::HighPass, Shape::BandPass, Shape::Peak, Shape::LowShelf, Shape::HighShelf];
}

/// RBJ cookbook biquad coefficients: (b0, b1, b2, a1, a2), normalized.
pub fn biquad(shape: Shape, freq: f32, q: f32, gain_db: f32, rate: f32) -> [f32; 5] {
    use std::f32::consts::PI;
    let w = 2.0 * PI * freq.clamp(10.0, rate * 0.45) / rate;
    let (sw, cw) = w.sin_cos();
    let a = 10f32.powf(gain_db / 40.0);
    let alpha = sw / (2.0 * q.max(0.01));
    let (b0, b1, b2, a0, a1, a2) = match shape {
        Shape::LowPass => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        Shape::HighPass => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        Shape::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        Shape::Peak => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
        Shape::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) - (a - 1.0) * cw + s), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - s), (a + 1.0) + (a - 1.0) * cw + s, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - s)
        }
        Shape::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (a * ((a + 1.0) + (a - 1.0) * cw + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - s), (a + 1.0) - (a - 1.0) * cw + s, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - s)
        }
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Floats a filter call keeps: the three settings its coefficients were made for, the
/// five coefficients, and its two samples of memory.
pub const SITE_FLOATS: usize = 10;

/// A filter call's memory before its first run: settings no call matches (NaN).
pub const FRESH_SITE: [f32; SITE_FLOATS] = [f32::NAN, f32::NAN, f32::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

/// A filter's settings from its arguments after the input: `(hz, q)` for the passes,
/// `(hz, q, db)` for a peak, `(hz, db)` for the shelves.
pub fn filter_key(shape: Shape, args: &[f32]) -> [f32; 3] {
    let at = |i: usize| args.get(i).copied().unwrap_or(0.0);
    match shape {
        Shape::Peak => [at(0), at(1), at(2)],
        _ => [at(0), at(1), 0.0],
    }
}

/// Remakes a filter call's coefficients for `key`.
pub fn filter_update(site: &mut [f32], shape: Shape, key: [f32; 3], rate: f32) {
    let (q, gain) = match shape {
        Shape::LowShelf | Shape::HighShelf => (0.707, key[1]),
        Shape::Peak => (key[1], key[2]),
        _ => (key[1], 0.0),
    };
    let c = biquad(shape, key[0], q, gain, rate);
    site[..3].copy_from_slice(&key);
    site[3..8].copy_from_slice(&c);
}

/// One sample through a filter call (transposed direct form II).
pub fn filter_run(site: &mut [f32], shape: Shape, x: f32, args: &[f32], rate: f32) -> f32 {
    let key = filter_key(shape, args);
    // NaN never equals, so a fresh site is made on its first sample.
    if site[0] != key[0] || site[1] != key[1] || site[2] != key[2] {
        filter_update(site, shape, key, rate);
    }
    let y = site[3] * x + site[8];
    site[8] = site[4] * x - site[6] * y + site[9];
    site[9] = site[5] * x - site[7] * y;
    y
}

/// White noise in −1 … 1 (xorshift64*).
pub fn noise(seed: &mut u64) -> f32 {
    let mut x = *seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}
