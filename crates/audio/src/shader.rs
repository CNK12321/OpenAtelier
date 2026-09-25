//! Sound shaders: the programs every sound effect is written in — Atelier Core's and
//! plugins' alike (see `plugins/README.md`). They're [OA script](oa_script), run once
//! per sample, per channel; there is nothing a sound shader can reach but its own
//! samples, a little memory and its parameters: no files, no loops, no calls out.
//!
//! ```text
//! // Tremolo: the level swings with a sine.
//! let swing = 0.5 + 0.5 * sin(TAU * speed * time);
//! out = in * (1 - depth * swing);
//! ```
//!
//! * **Reads**: `in` (this sample), `left` / `right` (this frame's two channels, for
//!   stereo effects and linked dynamics), `channel` (0 left, 1 right), `channels`,
//!   `sample_rate`, the effect's clock — `time` (seconds since it began), `progress`
//!   (0 → 1 across it), `visibility` (0 → 1 over an intro, 1 → 0 over an outro, 1
//!   otherwise) — and the parameters.
//! * **Writes**: `out` (starts as `in`; what's left in it is the sample) and
//!   `reduction` — how many dB the effect is turning the sound down, for its meter.
//! * **Memory**: `delay(s)` is the input `s` seconds ago, `delay_out(s)` the output
//!   (feedback). `line name = seconds;` declares a delay line of your own: `write(name,
//!   x);` puts this sample's value in it, `read(name, s)` reads it `s` seconds back,
//!   `line_max(name, s)` / `line_min(name, s)` the most and least of the last `s`
//!   seconds. Up to [`MAX_DELAY_SECONDS`].
//! * **Filters** (each call keeps its own state): `lowpass(x, hz, q)`, `highpass(x,
//!   hz, q)`, `bandpass(x, hz, q)`, `peak(x, hz, q, db)`, `lowshelf(x, hz, db)`,
//!   `highshelf(x, hz, db)`. `noise()` is white noise, −1 … 1.
//!
//! **Spectrum**: after `spectrum 1024;` the rest of the shader works on frequencies. The
//! sound (as the lines above leave it) is cut into overlapping windows of that many
//! samples; for each window, the lines below run once per frequency bin, reading `mag`
//! and `phase` (and writing them), `bin`, `bins`, `freq` (Hz), `size` — then the bins
//! are turned back into sound. This delays the effect by `size` samples. `state` keeps a
//! value per bin, from window to window. `pass;` starts another pass over the bins, which
//! can read what earlier passes left in other bins: `at(name, bin)` (between bins, it
//! blends), `mean(name, from, to)`.
//!
//! Output is kept finite and within ±4, and a channel whose math blows up (NaN) starts
//! over, so a bad shader can make a bad sound but never a stuck or deafening one.

use oa_script::dsp::{self, Line, Shape};
use oa_script::jit::{Keep, Memory, Native};
use oa_params::{Evaluated, ParamSchema};
use oa_script::{Arg, Compiler, Env, Frame, Function, Host, Intrinsic, Op, Section};
use std::f32::consts::PI;
use std::sync::Arc;

/// How far back `delay`, `delay_out` and lines reach.
pub const MAX_DELAY_SECONDS: f32 = 4.0;
/// The largest spectrum window.
pub const MAX_SPECTRUM: usize = 8192;
/// Loudest sample a shader may produce.
const LIMIT: f32 = 4.0;

/// Where an effect is in its run, at the start and at the end of a block of samples
/// (shaders see it glide between the two, sample by sample).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClockSpan {
    pub start: oa_doc::EffectClock,
    pub end: oa_doc::EffectClock,
}

impl ClockSpan {
    /// The same clock throughout (a passive effect that doesn't care, tests).
    pub fn still(clock: oa_doc::EffectClock) -> Self {
        ClockSpan { start: clock, end: clock }
    }
}

impl Default for ClockSpan {
    fn default() -> Self {
        ClockSpan::still(oa_doc::EffectClock { visibility: 1.0, progress: 0.0, seconds: 0.0 })
    }
}

// ---- what shaders see -------------------------------------------------------------

const NOISE: u16 = 0;
const DELAY: u16 = 1;
const DELAY_OUT: u16 = 2;
const LINE: u16 = 3;
const READ: u16 = 4;
const WRITE: u16 = 5;
const LINE_MAX: u16 = 6;
const LINE_MIN: u16 = 7;
const FILTERS: u16 = 8; // + the shape's index in `Shape::ALL`
const AT: u16 = 14;
const MEAN: u16 = 15;

/// The lines the host keeps: this channel's input and output (for `delay`, `delay_out`).
const INPUT_LINE: u16 = 0;
const OUTPUT_LINE: u16 = 1;

const V: Arg = Arg::Value;
const fn f(name: &'static str, args: &'static [Arg], id: u16, intrinsic: Intrinsic) -> Function {
    Function { name, args, id, pure: false, statement: false, intrinsic }
}
const fn filter(name: &'static str, args: &'static [Arg], shape: Shape) -> Function {
    let id = FILTERS
        + match shape {
            Shape::LowPass => 0,
            Shape::HighPass => 1,
            Shape::BandPass => 2,
            Shape::Peak => 3,
            Shape::LowShelf => 4,
            Shape::HighShelf => 5,
        };
    f(name, args, id, Intrinsic::Filter(shape))
}

const SAMPLE_READS: [&str; 9] = ["in", "left", "right", "channel", "channels", "sample_rate", "time", "progress", "visibility"];
const SAMPLE_WRITES: [&str; 2] = ["out", "reduction"];
const SAMPLE_FUNCTIONS: &[Function] = &[
    f("noise", &[], NOISE, Intrinsic::Noise),
    f("delay", &[V], DELAY, Intrinsic::Read { line: Some(INPUT_LINE), min_back: 0.0 }),
    // The output ring holds past samples only: at least one sample back.
    f("delay_out", &[V], DELAY_OUT, Intrinsic::Read { line: Some(OUTPUT_LINE), min_back: 1.0 }),
    f("read", &[Arg::Line, V], READ, Intrinsic::Read { line: None, min_back: 0.0 }),
    Function { name: "write", args: &[Arg::Line, V], id: WRITE, pure: false, statement: true, intrinsic: Intrinsic::Write },
    f("line_max", &[Arg::Line, V], LINE_MAX, Intrinsic::Extreme { max: true }),
    f("line_min", &[Arg::Line, V], LINE_MIN, Intrinsic::Extreme { max: false }),
    filter("lowpass", &[V, V, V], Shape::LowPass),
    filter("highpass", &[V, V, V], Shape::HighPass),
    filter("bandpass", &[V, V, V], Shape::BandPass),
    filter("peak", &[V, V, V, V], Shape::Peak),
    filter("lowshelf", &[V, V, V], Shape::LowShelf),
    filter("highshelf", &[V, V, V], Shape::HighShelf),
    // `line name = seconds;` allocates through the host (the compiler adds this call).
    f("", &[V], LINE, Intrinsic::Call),
];
// Registers: the reads, then the writes.
const IN: usize = 0;
const LEFT: usize = 1;
const RIGHT: usize = 2;
const CHANNEL: usize = 3;
const CHANNELS: usize = 4;
const SAMPLE_RATE: usize = 5;
const TIME: usize = 6;
const PROGRESS: usize = 7;
const VISIBILITY: usize = 8;
const OUT: usize = 9;
const REDUCTION: usize = 10;

const BIN_READS: [&str; 10] = ["bin", "bins", "freq", "size", "sample_rate", "channel", "channels", "time", "progress", "visibility"];
const BIN_WRITES: [&str; 2] = ["mag", "phase"];
const BIN_FUNCTIONS: &[Function] = &[
    f("noise", &[], NOISE, Intrinsic::Noise),
    f("at", &[Arg::Name, V], AT, Intrinsic::Call),
    f("mean", &[Arg::Name, V, V], MEAN, Intrinsic::Call),
];
const MAG: usize = 10;
const PHASE: usize = 11;

// ---- compiling ----------------------------------------------------------------------

/// A compiled sound shader.
#[derive(Debug)]
pub struct Program {
    params: Vec<ParamSchema>,
    sample: Section,
    frame: Frame,
    /// `sample`'s block and main as machine code (none where the JIT can't run: then
    /// they're interpreted).
    native: Option<Native>,
    spectrum: Option<Spectrum>,
    /// Reads `delay` or `delay_out` (so its input and output are kept).
    delays: bool,
    /// Extra delay the effect declares (a lookahead), in seconds.
    pub(crate) latency: f64,
    source: Arc<str>,
}

#[derive(Debug)]
struct Spectrum {
    size: usize,
    passes: Vec<Section>,
    frame: Frame,
    /// Each pass's block and main, one after another.
    native: Option<Native>,
    /// A pass reads or writes `phase` (otherwise the bins are scaled, not rebuilt).
    phase: bool,
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.params == other.params
    }
}
/// The shader cut at `spectrum N;` and `pass;`: the sample part, the window size, and
/// each pass — every part with the number of its first line.
#[allow(clippy::type_complexity)]
fn split(source: &str) -> Result<((String, usize), Option<(usize, Vec<(String, usize)>)>), String> {
    let mut sample = (String::new(), 1);
    let mut spectrum: Option<(usize, Vec<(String, usize)>)> = None;
    for (i, line) in source.lines().enumerate() {
        let n = i + 1;
        let code = line.split("//").next().unwrap_or("").split('#').next().unwrap_or("").trim();
        let words: Vec<&str> = code.trim_end_matches(';').split_whitespace().collect();
        let statement = code.ends_with(';');
        match (words.as_slice(), &mut spectrum) {
            (["spectrum", size], None) if statement => {
                let size: usize = size.parse().map_err(|_| format!("line {n}: \"spectrum\" takes a window size, like spectrum 1024;"))?;
                if !size.is_power_of_two() || !(64..=MAX_SPECTRUM).contains(&size) {
                    return Err(format!("line {n}: a spectrum's size is a power of two from 64 to {MAX_SPECTRUM}"));
                }
                spectrum = Some((size, vec![(String::new(), n + 1)]));
                continue;
            }
            (["spectrum", ..], Some(_)) => return Err(format!("line {n}: there's already a spectrum")),
            (["pass"], None) if statement => return Err(format!("line {n}: \"pass;\" belongs in a spectrum")),
            (["pass"], Some((_, passes))) if statement => {
                passes.push((String::new(), n + 1));
                continue;
            }
            _ => {}
        }
        let target = match &mut spectrum {
            Some((_, passes)) => &mut passes.last_mut().expect("a pass").0,
            None => &mut sample.0,
        };
        target.push_str(line);
        target.push('\n');
    }
    Ok((sample, spectrum))
}

impl Program {
    /// Compiles `source` for an effect with `params` (to machine code where it can).
    /// Errors name the line.
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Program, String> {
        Self::build(source, params, true)
    }

    /// Compiled only for the interpreter (to compare the two).
    #[cfg(test)]
    fn interpreted(source: &str, params: &[ParamSchema]) -> Result<Program, String> {
        Self::build(source, params, false)
    }

    fn build(source: &str, params: &[ParamSchema], jit: bool) -> Result<Program, String> {
        let ((sample_src, first), spectrum_src) = split(source)?;
        let env = Env {
            reads: &SAMPLE_READS,
            writes: &SAMPLE_WRITES,
            invariant: &["channel", "channels", "sample_rate"],
            params,
            functions: SAMPLE_FUNCTIONS,
            line: Some(LINE),
            reserved_lines: 2,
        };
        let mut c = Compiler::new(env)?;
        let sample = c.section(&sample_src, first)?;
        let frame = c.finish();
        let delays = sample.main.iter().any(|op| matches!(op, Op::Host { id: DELAY | DELAY_OUT, .. }));
        let native = if jit { oa_script::jit::compile(&[&sample.block, &sample.main], frame.registers, SAMPLE_FUNCTIONS, Keep::HostAnd(SAMPLE_READS.len() + SAMPLE_WRITES.len())).ok() } else { None };
        let spectrum = match spectrum_src {
            None => None,
            Some((size, parts)) => {
                let env = Env { reads: &BIN_READS, writes: &BIN_WRITES, invariant: &[], params, functions: BIN_FUNCTIONS, line: None, reserved_lines: 0 };
                let mut c = Compiler::new(env)?;
                let passes = parts.iter().map(|(src, first)| c.section(src, *first)).collect::<Result<Vec<_>, _>>()?;
                let frame = c.finish();
                let touches = |op: &Op| matches!(op, Op::Load(r) | Op::Store(r) if *r as usize == PHASE);
                let phase = passes.iter().any(|p| p.block.iter().chain(&p.main).any(touches));
                let ops: Vec<&[Op]> = passes.iter().flat_map(|p| [p.block.as_slice(), p.main.as_slice()]).collect();
                let native = if jit { oa_script::jit::compile(&ops, frame.registers, BIN_FUNCTIONS, Keep::All).ok() } else { None };
                Some(Spectrum { size, passes, frame, native, phase })
            }
        };
        Ok(Program { params: params.to_vec(), sample, frame, native, spectrum, delays, latency: 0.0, source: source.into() })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Frames of delay the effect adds: its spectrum's window, plus what it declares.
    pub fn latency_frames(&self, rate: f32) -> usize {
        self.spectrum.as_ref().map_or(0, |s| s.size) + (self.latency.max(0.0) * rate as f64) as usize
    }

    /// Whether it runs as machine code.
    pub fn is_native(&self) -> bool {
        self.native.is_some() && self.spectrum.as_ref().is_none_or(|s| s.native.is_some())
    }
}

// ---- running ------------------------------------------------------------------------

/// One channel's memory: its lines (the input and output rings first) and filter states,
/// laid out as compiled code expects.
struct ChannelMemory {
    /// The lines' samples, by index.
    buffers: Vec<Vec<f32>>,
    /// Where each line is and which slot it's on.
    table: Vec<Line>,
    sites: Vec<f32>,
}

// SAFETY: the table points only into `buffers`' heap storage, which moves with it; the
// memory is used by one thread at a time (the processor's).
unsafe impl Send for ChannelMemory {}

impl ChannelMemory {
    fn new(frame: &Frame, ring: usize) -> Self {
        let lines = frame.lines.max(2) as usize;
        let mut buffers = vec![Vec::new(); lines];
        buffers[INPUT_LINE as usize] = vec![0.0; ring.max(2)];
        buffers[OUTPUT_LINE as usize] = vec![0.0; ring.max(2)];
        let mut table = vec![Line::EMPTY; lines];
        for (t, b) in table.iter_mut().zip(&mut buffers).take(2) {
            *t = Line::over(b);
        }
        let sites = dsp::FRESH_SITE.repeat(frame.sites as usize);
        ChannelMemory { buffers, table, sites }
    }

    fn reset(&mut self) {
        for (t, b) in self.table.iter_mut().zip(&mut self.buffers) {
            b.fill(0.0);
            t.pos = 0;
        }
        for site in self.sites.chunks_mut(dsp::SITE_FLOATS) {
            site.copy_from_slice(&dsp::FRESH_SITE);
        }
    }
}

/// What a sample's shader calls: allocating lines (both ways of running), and — when
/// interpreted — the memory operations compiled code does itself.
struct SampleHost {
    memory: *mut ChannelMemory,
    rate: f32,
    seed: *mut u64,
}

impl Host for SampleHost {
    fn call(&mut self, id: u16, site: u32, a: &[f32]) -> f32 {
        // SAFETY: the processor's memory and seed, alive and not otherwise borrowed while
        // a shader runs.
        let (memory, seed) = unsafe { (&mut *self.memory, &mut *self.seed) };
        let line = |i: f32| memory.table.get(i as usize).copied().unwrap_or(Line::EMPTY);
        match id {
            NOISE => dsp::noise(seed),
            DELAY => line(INPUT_LINE as f32).read(a[0] * self.rate),
            DELAY_OUT => line(OUTPUT_LINE as f32).read((a[0] * self.rate).max(1.0)),
            LINE => {
                let i = site as usize;
                if i < memory.table.len() {
                    let len = (a[0].clamp(0.0, MAX_DELAY_SECONDS) * self.rate).ceil() as usize + 2;
                    memory.buffers[i] = vec![0.0; len];
                    memory.table[i] = Line::over(&mut memory.buffers[i]);
                }
                0.0
            }
            READ => line(a[0]).read(a[1] * self.rate),
            WRITE => {
                line(a[0]).write(a[1]);
                a[1]
            }
            LINE_MAX | LINE_MIN => line(a[0]).extreme(a[1] * self.rate, id == LINE_MAX),
            _ if (FILTERS..FILTERS + 6).contains(&id) => {
                let shape = Shape::ALL[(id - FILTERS) as usize];
                let at = site as usize * dsp::SITE_FLOATS;
                match memory.sites.get_mut(at..at + dsp::SITE_FLOATS) {
                    Some(s) => dsp::filter_run(s, shape, a[0], &a[1..], self.rate),
                    None => a[0],
                }
            }
            _ => 0.0,
        }
    }
}

/// A spectrum pass's view of the other bins.
struct BinHost {
    /// Every bin's registers, one after another (the bin being run is worked on in place,
    /// and holds its values from before this run until it's done).
    bins: *const f32,
    registers: usize,
    count: usize,
    seed: *mut u64,
}

impl Host for BinHost {
    fn call(&mut self, id: u16, _site: u32, a: &[f32]) -> f32 {
        let register = (a[0] as usize).min(self.registers - 1);
        // SAFETY: `bins` holds `count` × `registers` floats, alive while the passes run.
        let value = |k: usize| unsafe { *self.bins.add(k.min(self.count - 1) * self.registers + register) };
        match id {
            // SAFETY: the processor's seed, alive while the passes run.
            NOISE => dsp::noise(unsafe { &mut *self.seed }),
            AT => {
                let x = a[1].clamp(0.0, (self.count - 1) as f32);
                let i = x.floor() as usize;
                let k = x - x.floor();
                value(i) * (1.0 - k) + value(i + 1) * k
            }
            MEAN => {
                let last = (self.count - 1) as f32;
                let (lo, hi) = (a[1].round().clamp(0.0, last) as usize, a[2].round().clamp(0.0, last) as usize);
                let (lo, hi) = (lo.min(hi), lo.max(hi));
                (lo..=hi).map(value).sum::<f32>() / (hi - lo + 1) as f32
            }
            _ => 0.0,
        }
    }
}

/// In-place radix-2 complex FFT (`inverse` scales by 1/n).
pub(crate) fn fft(re: &mut [f32], im: &mut [f32], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * PI / len as f32 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
    if inverse {
        let k = 1.0 / n as f32;
        re.iter_mut().for_each(|x| *x *= k);
        im.iter_mut().for_each(|x| *x *= k);
    }
}

/// One channel of a spectrum: the last window of input (a ring), the output being
/// overlapped, and every bin's registers.
struct SpectrumChannel {
    input: Vec<f32>,
    at: usize,
    output: Vec<f32>,
    bins: Vec<f32>,
    first: bool,
}

/// Short-time Fourier transform around a shader's bin passes: a sqrt-Hann window at 75%
/// overlap in and out (their product sums to a constant), a fixed `size` frames late.
struct SpectrumRun {
    size: usize,
    hop: usize,
    window: Vec<f32>,
    chans: Vec<SpectrumChannel>,
    /// Input frames since the last hop.
    fill: usize,
    /// Processed samples ready to hand out, per channel.
    ready: Vec<Vec<f32>>,
    scratch: Vec<f32>,
    stack: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Each bin's magnitude as it came in (to scale the bins by, when the phase stays).
    mag_in: Vec<f32>,
}

impl SpectrumRun {
    fn new(size: usize, registers: usize, ch: usize) -> Self {
        let window = (0..size).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f32 / size as f32).cos()).sqrt()).collect();
        let bins = size / 2 + 1;
        let chan = || SpectrumChannel { input: vec![0.0; size], at: 0, output: vec![0.0; size], bins: vec![0.0; bins * registers], first: true };
        let hop = size / 4;
        // The output queue starts a hop of silence ahead, so the delay is always exactly
        // `size` frames whatever the block sizes.
        SpectrumRun {
            size,
            hop,
            window,
            chans: (0..ch).map(|_| chan()).collect(),
            fill: 0,
            ready: vec![vec![0.0; hop]; ch],
            scratch: vec![0.0; registers],
            stack: Vec::with_capacity(64),
            re: vec![0.0; size],
            im: vec![0.0; size],
            mag_in: vec![0.0; bins],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn window(&mut self, c: usize, spectrum: &Spectrum, params: &[ParamSchema], values: &Evaluated, rate: f32, channels: usize, clock: [f32; 3], seed: &mut u64) {
        let size = self.size;
        let count = size / 2 + 1;
        let n = spectrum.frame.registers;
        let ch = &mut self.chans[c];
        // The ring, oldest sample first.
        for i in 0..size {
            self.re[i] = ch.input[(ch.at + i) % size] * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im, false);
        for k in 0..count {
            self.mag_in[k] = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt();
        }
        // What every bin reads, set once per window; the parameters too.
        let common = [count as f32, 0.0, size as f32, rate, c as f32, channels as f32, clock[0], clock[1], clock[2]];
        let regs = &mut self.scratch;
        spectrum.frame.load_params(regs, params, values);
        let loaded: Vec<(usize, f32)> = spectrum.frame.params.iter().map(|s| (s.register as usize, regs[s.register as usize])).collect();
        let bins = ch.bins.as_mut_ptr();
        let mut host = BinHost { bins, registers: n, count, seed };
        let mut host_ref: &mut dyn Host = &mut host;
        let mut memory = Memory { lines: std::ptr::null_mut(), sites: std::ptr::null_mut(), seed, host: &mut host_ref as *mut &mut dyn Host as *mut std::ffi::c_void, rate };
        let native = spectrum.native.as_ref();
        for (pass, section) in spectrum.passes.iter().enumerate() {
            for k in 0..count {
                // SAFETY: bin `k`'s registers; the host reads the others through the same
                // pointer, never these while they're being run.
                let regs = unsafe { std::slice::from_raw_parts_mut(bins.add(k * n), n) };
                if pass == 0 {
                    regs[0] = k as f32;
                    regs[1..10].copy_from_slice(&common);
                    regs[2] = k as f32 * rate / size as f32;
                    for (r, v) in &loaded {
                        regs[*r] = *v;
                    }
                    regs[MAG] = self.mag_in[k];
                    regs[PHASE] = if spectrum.phase { self.im[k].atan2(self.re[k]) } else { 0.0 };
                }
                for (part, ops) in [(2 * pass, &section.block), (2 * pass + 1, &section.main)] {
                    match native {
                        // SAFETY: no lines or sites (a spectrum has none); the host lives
                        // through the call.
                        Some(code) => unsafe { code.run(part, regs, &mut memory, ch.first) },
                        // SAFETY: the host `memory` points at.
                        None => oa_script::run(ops, regs, &mut self.stack, unsafe { &mut **(memory.host as *mut &mut dyn Host) }, ch.first),
                    }
                }
            }
        }
        ch.first = false;
        for k in 0..count {
            let (mag, phase) = (ch.bins[k * n + MAG], ch.bins[k * n + PHASE]);
            let mag = if mag.is_finite() { mag.clamp(0.0, 1e6) } else { 0.0 };
            if spectrum.phase {
                let phase = if phase.is_finite() { phase } else { 0.0 };
                self.re[k] = mag * phase.cos();
                self.im[k] = mag * phase.sin();
            } else {
                // The phase as it was: scale the bin.
                let g = if self.mag_in[k] > 1e-20 { mag / self.mag_in[k] } else { 0.0 };
                self.re[k] *= g;
                self.im[k] *= g;
            }
            if k > 0 && k < size / 2 {
                self.re[size - k] = self.re[k];
                self.im[size - k] = -self.im[k];
            }
        }
        // The top bin and the constant one are real.
        self.im[0] = 0.0;
        self.im[size / 2] = 0.0;
        fft(&mut self.re, &mut self.im, true);
        // Overlap-add; the first hop is complete.
        for ((out, x), w) in ch.output.iter_mut().zip(&self.re).zip(&self.window) {
            *out += x * w * 0.5;
        }
        self.ready[c].extend_from_slice(&ch.output[..self.hop]);
        ch.output.copy_within(self.hop.., 0);
        ch.output[size - self.hop..].fill(0.0);
    }
}

/// A sound shader running on one clip: its registers and memory, per channel.
pub(crate) struct ShaderProcessor {
    program: Arc<Program>,
    ch: usize,
    rate: f32,
    regs: Vec<Vec<f32>>,
    /// The channel's `state`s still need their starting values.
    fresh: Vec<bool>,
    stack: Vec<f32>,
    memory: Vec<ChannelMemory>,
    /// The block as it arrived (outputs overwrite it channel by channel).
    block_in: Vec<f32>,
    seed: u64,
    reduction: f32,
    spectrum: Option<SpectrumRun>,
}

impl ShaderProcessor {
    pub(crate) fn new(program: Arc<Program>, channels: usize, rate: f32) -> Self {
        let ch = channels.max(1);
        let ring = if program.delays { (MAX_DELAY_SECONDS * rate) as usize + 2 } else { 2 };
        ShaderProcessor {
            ch,
            rate,
            regs: vec![vec![0.0; program.frame.registers]; ch],
            fresh: vec![true; ch],
            stack: Vec::with_capacity(64),
            memory: (0..ch).map(|_| ChannelMemory::new(&program.frame, ring)).collect(),
            block_in: Vec::new(),
            seed: 0x9E37_79B9_7F4A_7C15,
            reduction: 0.0,
            spectrum: program.spectrum.as_ref().map(|s| SpectrumRun::new(s.size, s.frame.registers, ch)),
            program,
        }
    }
}

impl crate::fx::Processor for ShaderProcessor {
    fn process(&mut self, buf: &mut [f32], v: &Evaluated, clock: &ClockSpan) {
        let ch = self.ch;
        let frames = buf.len() / ch;
        if frames == 0 {
            return;
        }
        let program = self.program.clone();
        let native = program.native.as_ref();
        let rate = self.rate;
        let (s, e) = (clock.start, clock.end);
        // Channel by channel (each has its own memory); `left` and `right` come from the
        // block as it arrived.
        self.block_in.clear();
        self.block_in.extend_from_slice(buf);
        let delays = program.delays;
        let mut reduction = 0f32;
        for c in 0..ch {
            let regs = &mut self.regs[c];
            // Parameters change per block (and the steady lets with them).
            program.frame.load_params(regs, &program.params, v);
            regs[CHANNEL] = c as f32;
            regs[CHANNELS] = ch as f32;
            regs[SAMPLE_RATE] = rate;
            let mem: *mut ChannelMemory = &mut self.memory[c];
            let mut host = SampleHost { memory: mem, rate, seed: &mut self.seed };
            let mut host_ref: &mut dyn Host = &mut host;
            // SAFETY: `mem` is this channel's memory, only reached through these pointers
            // until the channel is done.
            let (table, sites) = unsafe { ((*mem).table.as_mut_ptr(), (*mem).sites.as_mut_ptr()) };
            let mut memory = Memory { lines: table, sites, seed: &mut self.seed, host: &mut host_ref as *mut &mut dyn Host as *mut std::ffi::c_void, rate };
            let mut part = |i: usize, ops: &[Op], regs: &mut [f32], stack: &mut Vec<f32>, first: bool| match native {
                // SAFETY: the memory holds this channel's lines and sites, sized by the frame
                // the code was compiled for; the host lives through the calls.
                Some(n) => unsafe { n.run(i, regs, &mut memory, first) },
                // SAFETY: as above; the host is the one `memory` points at.
                None => oa_script::run(ops, regs, stack, unsafe { &mut **(memory.host as *mut &mut dyn Host) }, first),
            };
            part(0, &program.sample.block, regs, &mut self.stack, true);
            for f in 0..frames {
                let k = if frames > 1 { f as f64 / frames as f64 } else { 0.0 };
                let frame = &self.block_in[f * ch..f * ch + ch];
                let x = frame[c];
                regs[IN] = x;
                regs[LEFT] = frame[0];
                regs[RIGHT] = frame[1.min(ch - 1)];
                regs[TIME] = (s.seconds + f as f64 / rate as f64) as f32;
                regs[PROGRESS] = (s.progress + (e.progress - s.progress) * k) as f32;
                regs[VISIBILITY] = (s.visibility + (e.visibility - s.visibility) * k) as f32;
                regs[OUT] = x;
                // SAFETY: as above.
                let table = unsafe { &mut (*mem).table };
                if delays {
                    table[INPUT_LINE as usize].write(x);
                }
                part(1, &program.sample.main, regs, &mut self.stack, self.fresh[c]);
                self.fresh[c] = false;
                let mut y = regs[OUT];
                if !y.is_finite() {
                    // Blown up: silence this sample and start the channel over.
                    y = 0.0;
                    regs.iter_mut().for_each(|r| *r = 0.0);
                    // SAFETY: as above.
                    unsafe { (*mem).reset() };
                    self.fresh[c] = true;
                }
                let y = y.clamp(-LIMIT, LIMIT);
                if delays {
                    table[OUTPUT_LINE as usize].write(y);
                    table.iter_mut().for_each(Line::advance);
                } else {
                    table[2..].iter_mut().for_each(Line::advance);
                }
                buf[f * ch + c] = y;
            }
            if regs[REDUCTION].is_finite() {
                reduction = reduction.max(regs[REDUCTION].abs());
            }
        }
        self.reduction = reduction;
        if let (Some(run), Some(spectrum)) = (&mut self.spectrum, &program.spectrum) {
            let clock = [s.seconds as f32, s.progress as f32, s.visibility as f32];
            for f in 0..frames {
                for c in 0..ch {
                    let chan = &mut run.chans[c];
                    chan.input[chan.at] = buf[f * ch + c];
                    chan.at = (chan.at + 1) % run.size;
                }
                run.fill += 1;
                if run.fill == run.hop {
                    run.fill = 0;
                    for c in 0..ch {
                        run.window(c, spectrum, &program.params, v, rate, ch, clock, &mut self.seed);
                    }
                }
            }
            // Hand out what's processed, oldest first (the queue always holds enough).
            let have = run.ready[0].len().min(frames);
            for f in 0..frames {
                for c in 0..ch {
                    let y = if f < have { run.ready[c][f] } else { 0.0 };
                    buf[f * ch + c] = if y.is_finite() { y.clamp(-LIMIT, LIMIT) } else { 0.0 };
                }
            }
            for r in &mut run.ready {
                r.drain(..have);
            }
        }
    }

    fn reset(&mut self) {
        self.regs.iter_mut().for_each(|r| r.iter_mut().for_each(|x| *x = 0.0));
        self.fresh.iter_mut().for_each(|f| *f = true);
        self.memory.iter_mut().for_each(ChannelMemory::reset);
        self.reduction = 0.0;
        if let Some(s) = &self.program.spectrum {
            self.spectrum = Some(SpectrumRun::new(s.size, s.frame.registers, self.ch));
        }
    }

    fn latency(&self) -> usize {
        self.program.latency_frames(self.rate)
    }

    fn reduction_db(&self) -> f32 {
        self.reduction
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::Processor;
    use oa_params::{ParamId, Unit, Value};

    fn param(id: &str, v: f64) -> ParamSchema {
        ParamSchema::new(id, Value::Float(v), Unit::None)
    }

    fn values(pairs: &[(&str, f64)]) -> Evaluated {
        Evaluated(pairs.iter().map(|(k, v)| (ParamId::new(k), Value::Float(*v))).collect())
    }

    fn running(source: &str, params: &[ParamSchema]) -> ShaderProcessor {
        let program = Program::compile(source, params).unwrap_or_else(|e| panic!("{e}"));
        ShaderProcessor::new(Arc::new(program), 2, 48_000.0)
    }

    fn near(got: &[f32], want: &[f32]) -> bool {
        got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3)
    }

    #[test]
    fn math_params_and_precedence() {
        let mut p = running("let g = 2 + 3 * gain ^ 2; out = in * g - -1;", &[param("gain", 2.0)]);
        let mut buf = [1.0, 0.5];
        p.process(&mut buf, &values(&[("gain", 2.0)]), &ClockSpan::default());
        // g = 2 + 3·4 = 14; out = in·14 + 1, then held to ±4.
        assert_eq!(buf, [4.0, 4.0]);
        let mut p = running("out = select(channel == 0, left, right) * 0.25 + (1 < 2 && !(3 < 2));", &[]);
        let mut buf = [0.4, 0.8];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!((buf[0] - 1.1).abs() < 1e-6 && (buf[1] - 1.2).abs() < 1e-6, "{buf:?}");
    }

    /// `state` keeps its value from sample to sample, per channel, and starts over on a reset.
    #[test]
    fn state_persists_and_resets() {
        let mut p = running("state n = 0; n += 1; out = n / 4;", &[]);
        let mut buf = [0.0; 6];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(buf, [0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
        p.reset();
        let mut buf = [0.0; 2];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(buf, [0.25, 0.25]);
    }

    /// `delay` reads the input back in time; `delay_out` feeds the output back (an echo);
    /// a line does either, as many times as there are lines.
    #[test]
    fn delays_and_lines_reach_back() {
        let mut p = running("out = delay(2 / sample_rate);", &[]);
        let mut buf = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]), "{buf:?}");
        let mut p = running("out = in + 0.5 * delay_out(1 / sample_rate);", &[]);
        let mut buf = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[1.0, 1.0, 0.5, 0.5, 0.25, 0.25]), "{buf:?}");
        // A comb: the line read at its full length, fed back.
        let mut p = running("line comb = 2 / sample_rate; let y = read(comb, 2 / sample_rate); write(comb, in + y * 0.5); out = y;", &[]);
        let mut buf = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.5, 0.5]), "{buf:?}");
        // The loudest of the last few samples.
        let mut p = running("line l = 0.01; write(l, abs(in)); out = line_max(l, 2 / sample_rate) - line_min(l, 2 / sample_rate);", &[]);
        let mut buf = [0.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0]), "{buf:?}");
    }

    /// Filters remember per call: two low-passes in a row cut twice as hard.
    #[test]
    fn filters_keep_their_own_state() {
        let tone = |hz: f32| (0..9600).flat_map(|i| [(2.0 * PI * hz * i as f32 / 48_000.0).sin() * 0.5; 2]).collect::<Vec<f32>>();
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let through = |src: &str, hz: f32| {
            let mut p = running(src, &[]);
            let mut buf = tone(hz);
            for block in buf.chunks_mut(960) {
                p.process(block, &Evaluated(vec![]), &ClockSpan::default());
            }
            rms(&buf[4800..]) / rms(&tone(hz)[4800..])
        };
        let once = through("out = lowpass(in, 500, 0.707);", 4000.0);
        let twice = through("out = lowpass(lowpass(in, 500, 0.707), 500, 0.707);", 4000.0);
        assert!(once < 0.05 && twice < once * 0.1, "{once} {twice}");
        assert!((through("out = lowpass(in, 500, 0.707);", 100.0) - 1.0).abs() < 0.05);
        assert!((through("out = peak(in, 1000, 1, 6);", 1000.0) - 2.0).abs() < 0.05, "+6 dB at its frequency");
    }

    /// A spectrum that doesn't touch the bins gives the sound back, `size` frames late;
    /// one that zeroes the high bins keeps only the lows.
    #[test]
    fn spectrum_windows_round_trip() {
        let tone: Vec<f32> = (0..24_000).flat_map(|i| [(2.0 * PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5; 2]).collect();
        let mut p = running("spectrum 512;\nlet unused = mag;", &[]);
        assert_eq!(p.latency(), 512);
        let mut out = tone.clone();
        for block in out.chunks_mut(2 * 480) {
            p.process(block, &Evaluated(vec![]), &ClockSpan::default());
        }
        for i in 4096..8192 {
            assert!((out[2 * (i + 512)] - tone[2 * i]).abs() < 1e-3, "frame {i}: {} vs {}", out[2 * (i + 512)], tone[2 * i]);
        }
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let through = |hz: f32| {
            let mut p = running("spectrum 1024;\nmag *= freq < cutoff;", &[param("cutoff", 1000.0)]);
            let input: Vec<f32> = (0..24_000).flat_map(|i| [(2.0 * PI * hz * i as f32 / 48_000.0).sin() * 0.5; 2]).collect();
            let mut out = input.clone();
            for block in out.chunks_mut(960) {
                p.process(block, &values(&[("cutoff", 1000.0)]), &ClockSpan::default());
            }
            rms(&out[9600..]) / rms(&input[9600..])
        };
        assert!((through(440.0) - 1.0).abs() < 0.05, "{}", through(440.0));
        assert!(through(5000.0) < 0.01, "{}", through(5000.0));
    }

    /// Passes see earlier passes' values in other bins; `state` is per bin.
    #[test]
    fn spectrum_passes_share_bins() {
        // Each bin takes its neighbors' average magnitude: white noise stays about as loud.
        let noise: Vec<f32> = {
            let mut seed = 7u64;
            (0..48_000).map(|_| dsp::noise(&mut seed) * 0.3).collect()
        };
        let mut p = ShaderProcessor::new(Arc::new(Program::compile("spectrum 256;\nlet m = mag;\npass;\nmag = mean(m, bin - 2, bin + 2);\nstate frames = 0;\nframes += 1;", &[]).unwrap()), 1, 48_000.0);
        let mut out = noise.clone();
        for block in out.chunks_mut(480) {
            p.process(block, &Evaluated(vec![]), &ClockSpan::default());
        }
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let ratio = rms(&out[4800..]) / rms(&noise[4800..]);
        assert!(ratio > 0.5 && ratio < 1.2, "{ratio}");
        let err = |s: &str| Program::compile(s, &[]).expect_err(s);
        assert!(err("spectrum 1000;").contains("power of two"));
        assert!(err("pass;").contains("belongs in a spectrum"));
        assert!(err("spectrum 256;\nout = in;").contains("line 2") && err("spectrum 256;\nout = in;").contains("\"out\" isn't defined"));
    }

    /// The reduction an effect writes is what its meter shows.
    #[test]
    fn reduction_reaches_the_meter() {
        let mut p = running("reduction = 6; out = in * db(-6);", &[]);
        let mut buf = [1.0; 4];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(p.reduction_db(), 6.0);
    }

    /// The clock glides across a block: a fade written against `visibility` ramps smoothly.
    #[test]
    fn the_clock_glides() {
        let mut p = running("out = in * visibility;", &[]);
        let clock = |v| oa_doc::EffectClock { visibility: v, progress: v, seconds: 0.0 };
        let mut buf = [1.0; 8];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan { start: clock(0.0), end: clock(1.0) });
        assert_eq!(buf, [0.0, 0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
    }

    /// Atelier Core's sound effects compile and make sound (not silence, not blow-ups);
    /// a plugin's are installed beside them, and taken ids are refused.
    #[test]
    fn built_in_and_plugin_shaders() {
        let core = crate::fx::core_catalog();
        assert!(core.len() >= 17);
        for fx in core {
            let values = Evaluated(fx.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect());
            let mut p = crate::fx::processor(&fx.type_id, 2, 48_000.0).expect("runs");
            let mut buf: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * if i % 2 == 0 { 0.5 } else { 0.3 }).collect();
            for block in buf.chunks_mut(960) {
                p.process(block, &values, &ClockSpan::default());
            }
            let rms = (buf[24_000..].iter().map(|x| x * x).sum::<f32>() / 24_000.0).sqrt();
            assert!(rms > 0.01 && rms < 2.0, "{}: rms {rms}", fx.type_id);
        }
        let mine = crate::fx::FxInfo::shader("com.example.half", "Half", "", vec![], crate::fx::FxUsage::Passive, "out = in / 2;").expect("compiles");
        let taken = crate::fx::FxInfo::shader("oa.audio.fade", "Mine", "", vec![], crate::fx::FxUsage::Passive, "out = in;").expect("compiles");
        let issues = crate::fx::install(vec![mine, taken]);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(crate::fx::info("com.example.half").is_some());
        let mut p = crate::fx::processor("com.example.half", 1, 48_000.0).expect("installed");
        let mut buf = [0.8];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(buf, [0.4]);
        crate::fx::install(vec![]);
    }

    /// Every one of Atelier Core's sound effects runs as machine code, and sounds the
    /// same as when it's interpreted.
    #[test]
    fn compiled_and_interpreted_agree() {
        let input: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.031).sin() * 0.4 + (i as f32 * 0.0013).sin() * 0.3 * if i % 2 == 0 { 1.0 } else { 0.7 }).collect();
        for fx in crate::fx::core_catalog() {
            assert!(fx.shader.is_native(), "{} isn't compiled", fx.type_id);
            let values = Evaluated(fx.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect());
            let interpreted = {
                let program = Program::interpreted(fx.shader.source(), &fx.params).expect("compiles");
                assert!(!program.is_native());
                Arc::new(program)
            };
            let run = |program: Arc<Program>| {
                let mut p = ShaderProcessor::new(program, 2, 48_000.0);
                let mut buf = input.clone();
                for block in buf.chunks_mut(960) {
                    p.process(block, &values, &ClockSpan::default());
                }
                buf
            };
            let (a, b) = (run(fx.shader.clone()), run(interpreted));
            let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
            assert!(worst < 1e-3, "{}: compiled and interpreted differ by {worst}", fx.type_id);
        }
    }

    /// Blow-ups become silence rather than NaN forever.
    #[test]
    fn blowups_start_over() {
        let mut p = running("state s = 1; s = s * 1e30; out = s;", &[]);
        let mut buf = [0.0; 8];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(buf.iter().all(|x| x.is_finite() && x.abs() <= LIMIT), "{buf:?}");
    }
}
