//! Time-stretching: a clip at another speed that keeps its pitch.
//!
//! WSOLA (waveform-similarity overlap-add). Windowed pieces of the source (40 ms,
//! Hann) are laid down every half window; each is read `speed` × half a window further
//! into the source than the last, nudged by up to ±12 ms to wherever its waveform best
//! continues what was just laid down (cross-correlation), so the pieces join without
//! the phase jumps that would otherwise warble. The pitch is the source's own at any
//! speed. This replaced resampling followed by a two-tap delay-line pitch shifter, which
//! was cheap but audibly phasey at ½× and 2×.

use std::collections::VecDeque;

pub struct Stretch {
    ch: usize,
    /// Piece length and the step between pieces laid down, in frames.
    win: usize,
    hop: usize,
    /// How far a piece may move to line up (± frames).
    search: usize,
    window: Vec<f32>,
    /// Source frames read and still needed (interleaved).
    input: Vec<f32>,
    /// Where the next piece is due in `input` (frames), before lining up.
    pos: f64,
    /// Where the last piece started in `input`.
    prev: Option<usize>,
    /// The last piece's second half, to add the next piece's first half to.
    overlap: Vec<f32>,
    /// Stretched frames not yet handed out.
    ready: VecDeque<f32>,
}

impl Stretch {
    pub fn new(ch: usize, sample_rate: u32) -> Self {
        let ch = ch.max(1);
        let hop = ((sample_rate as f64 * 0.02) as usize).max(16);
        let win = hop * 2;
        // Periodic Hann: two of them half a window apart sum to exactly 1.
        let window = (0..win).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / win as f32).cos()).collect();
        Stretch {
            ch,
            win,
            hop,
            search: ((sample_rate as f64 * 0.012) as usize).max(4),
            window,
            input: Vec::new(),
            pos: 0.0,
            prev: None,
            overlap: vec![0.0; hop * ch],
            ready: VecDeque::new(),
        }
    }

    /// Starts over (after a seek): nothing carried over.
    pub fn reset(&mut self) {
        self.input.clear();
        self.pos = 0.0;
        self.prev = None;
        self.overlap.fill(0.0);
        self.ready.clear();
    }

    /// Fills `out` with the source (read through `read`, which returns samples written, 0
    /// at its end) played `speed` times as fast at its own pitch.
    pub fn pull(&mut self, out: &mut [f32], speed: f64, read: &mut dyn FnMut(&mut [f32]) -> usize) {
        while self.ready.len() < out.len() {
            self.piece(speed.clamp(0.1, 10.0), read);
        }
        for o in out.iter_mut() {
            *o = self.ready.pop_front().unwrap_or(0.0);
        }
    }

    /// Mono sample at frame `i` of `input` (for lining up).
    fn mono(&self, i: usize) -> f32 {
        let s = &self.input[i * self.ch..(i + 1) * self.ch];
        s.iter().sum::<f32>()
    }

    /// Lays down one more piece: `hop` frames out.
    fn piece(&mut self, speed: f64, read: &mut dyn FnMut(&mut [f32]) -> usize) {
        let (ch, win, hop, search) = (self.ch, self.win, self.hop, self.search);
        let nominal = self.pos.round().max(0.0) as usize;
        // Enough read for every candidate piece and the natural continuation.
        let need = (nominal + search + win).max(self.prev.map_or(0, |p| p + hop + win)) * ch;
        let mut chunk = vec![0.0f32; 1024 * ch];
        while self.input.len() < need {
            let n = read(&mut chunk);
            if n == 0 {
                // The file ran out: silence from here.
                self.input.resize(need, 0.0);
            } else {
                self.input.extend_from_slice(&chunk[..n - n % ch]);
            }
        }
        let start = match self.prev {
            // The first piece is where it's due, and comes out unfaded.
            None => nominal,
            Some(prev) => self.line_up(nominal, prev + hop),
        };
        let first = self.prev.is_none();
        for i in 0..win {
            let w = self.window[i];
            for c in 0..ch {
                let x = self.input[(start + i) * ch + c];
                if i < hop {
                    // First half: onto the last piece's second half (at the very start,
                    // onto the sample's own complement, so there's no fade-in).
                    let under = if first { x * (1.0 - w) } else { self.overlap[i * ch + c] };
                    self.ready.push_back(under + x * w);
                } else {
                    self.overlap[(i - hop) * ch + c] = x * w;
                }
            }
        }
        self.prev = Some(start);
        self.pos += hop as f64 * speed;
        // Forget what no piece can reach any more.
        let keep_from = start.min(self.pos.floor().max(0.0) as usize).saturating_sub(search);
        if keep_from > 0 {
            self.input.drain(..keep_from * ch);
            self.pos -= keep_from as f64;
            self.prev = Some(start - keep_from);
        }
    }

    /// The start within ±`search` of `nominal` whose waveform best continues the one at
    /// `natural` (normalized cross-correlation over half a window: coarse, then refined).
    fn line_up(&self, nominal: usize, natural: usize) -> usize {
        let (hop, search) = (self.hop, self.search);
        let score = |k: usize| {
            let (mut dot, mut energy) = (0.0f32, 1e-9f32);
            for j in (0..hop).step_by(4) {
                let a = self.mono(k + j);
                dot += a * self.mono(natural + j);
                energy += a * a;
            }
            dot / energy.sqrt()
        };
        let lo = nominal.saturating_sub(search);
        let hi = nominal + search;
        let best_of = |range: &mut dyn Iterator<Item = usize>| range.max_by(|a, b| score(*a).total_cmp(&score(*b))).unwrap_or(nominal);
        let coarse = best_of(&mut (lo..=hi).step_by(4));
        best_of(&mut (coarse.saturating_sub(3).max(lo)..=(coarse + 3).min(hi)))
    }
}

#[cfg(test)]
mod tests {
    use super::Stretch;

    /// A 440 Hz tone stretched to half and double speed keeps its pitch (counted zero
    /// crossings), takes as long as the speed says (the source read), and plays on
    /// smoothly (no sample jumps more than the tone itself does).
    #[test]
    fn stretching_keeps_the_pitch() {
        const RATE: u32 = 48_000;
        for speed in [0.5, 2.0, 1.5] {
            let mut phase = 0usize;
            let mut read = |buf: &mut [f32]| {
                for frame in buf.chunks_mut(2) {
                    let v = (2.0 * std::f32::consts::PI * 440.0 * phase as f32 / RATE as f32).sin() * 0.5;
                    frame.fill(v);
                    phase += 1;
                }
                buf.len()
            };
            let mut s = Stretch::new(2, RATE);
            let mut out = vec![0.0f32; RATE as usize * 2]; // one second out
            s.pull(&mut out, speed, &mut read);
            let mono: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
            let crossings = mono.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            assert!((crossings as f64 - 440.0).abs() < 440.0 * 0.03, "{speed}×: {crossings} Hz");
            let read_seconds = phase as f64 / RATE as f64;
            assert!((read_seconds - speed).abs() < 0.1, "{speed}×: read {read_seconds:.2} s for 1 s out");
            let jump = mono.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
            assert!(jump < 0.08, "{speed}×: a jump of {jump}");
        }
    }
}
