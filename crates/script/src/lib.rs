//! **OA script**: the small language plugins write the parts of an effect that aren't a
//! GPU shader in — sound shaders (run once per sample), motion (how an intro moves a
//! layer), bounds (where an effect may draw), pass counts. Atelier Core's own effects are
//! written in it, so a plugin can do whatever a built-in does.
//!
//! ```text
//! // A one-pole low-pass.
//! let k = 1 - exp(-TAU * cutoff / sample_rate);
//! state low = 0;
//! low += (in - low) * k;
//! out = mix(in, low, amount);
//! ```
//!
//! A script is a list of statements, each ending in `;`:
//!
//! * `let x = …;` a value, computed each run. `state x = …;` a value that persists from
//!   run to run, starting at `…` on the first. `x = …;` (and `+=`, `-=`, `*=`, `/=`)
//!   change a `let`, a `state` or one of the host's outputs.
//! * `line name = seconds;` (where the host offers it) a delay line that long.
//! * `f(…);` calls a host function for its effect (`write(name, x);`).
//! * Math: `+ - * / %` (`%` never negative), `^` (power), comparisons and `&& || !`
//!   (true is 1, false 0); `sin cos tan asin acos atan atan2 abs sign floor ceil round
//!   fract sqrt exp log log2 pow min max clamp mix smoothstep step tanh`, `db(x)`
//!   (decibels → gain), `to_db(g)`, `select(c, a, b)` (`a` when `c`, else `b`) and
//!   `choose(i, a, b, …)` (the `i`th of the values after it). `PI`, `TAU`.
//! * Parameters read by id: numbers, switches (0/1) and choices (the option's index);
//!   a point's parts as `id_x`, `id_y` (`_z`), a color's as `id_r`, `id_g`, `id_b`, `id_a`.
//! * Comments start with `//` or `#`.
//!
//! There are no loops, no branches and nothing to reach but the script's own values and
//! what its host hands it, so a script always finishes, quickly. Each host ([`Env`])
//! decides the names a script reads and writes and the extra functions it may call.
//!
//! `let`s whose value can't change during a block — they read only parameters, the
//! host's steady inputs and each other — are computed once per block rather than on
//! every run ([`Section::block`]): a filter's coefficients, a threshold's gain.

mod compile;
pub mod dsp;
pub mod jit;
mod lex;
mod parse;
mod vm;

pub use compile::{compile, param_number, Compiler, Frame, ParamSlot, Section};
pub use vm::{run, Host, NoHost, Op};

use oa_params::ParamSchema;

/// Longest script accepted.
pub const MAX_SOURCE: usize = 64 * 1024;
/// Most instructions a script may compile to.
pub const MAX_OPS: usize = 40_000;

/// What an argument of a host function is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// Any expression.
    Value,
    /// The name of a `line`.
    Line,
    /// The name of a value (`let`, `state`, input, parameter): the host gets its register.
    Name,
}

/// A function the host offers scripts, beyond the math every script has.
#[derive(Copy, Clone, Debug)]
pub struct Function {
    pub name: &'static str,
    pub args: &'static [Arg],
    /// Handed back to [`Host::call`].
    pub id: u16,
    /// Same arguments, same result, nothing remembered: a `let` using it may be computed
    /// once per block.
    pub pure: bool,
    /// May be called as a statement of its own, for its effect.
    pub statement: bool,
    /// What it does, when it's one of the memory operations compiled code does itself
    /// ([`jit`]) rather than calling back into the host.
    pub intrinsic: Intrinsic,
}

impl Function {
    /// A host function that gives a value.
    pub const fn call(name: &'static str, args: &'static [Arg], id: u16) -> Self {
        Function { name, args, id, pure: false, statement: false, intrinsic: Intrinsic::Call }
    }
}

/// The host functions compiled code runs itself, on [`jit::Memory`]; the interpreter
/// hands them to [`Host::call`] like any other, and hosts implement them with the same
/// [`dsp`] helpers.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Intrinsic {
    /// An ordinary call into the host.
    Call,
    /// `noise()`: white noise from [`jit::Memory::seed`].
    Noise,
    /// Reads a line `seconds` back (at least `min_back` samples): line `line`, or the
    /// line named by the first argument.
    Read { line: Option<u16>, min_back: f32 },
    /// `write(line, x)`.
    Write,
    /// `line_max` / `line_min`.
    Extreme { max: bool },
    /// A filter call (the input, then its settings), with its state at its site.
    Filter(dsp::Shape),
}

/// What a kind of script may read, write and call. Registers are laid out as `reads`,
/// then `writes`, then parameters, then the script's own names.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env<'a> {
    /// Inputs the host sets before each run.
    pub reads: &'a [&'a str],
    /// Outputs: the host sets their starting values and reads them back.
    pub writes: &'a [&'a str],
    /// The inputs (of `reads`) that stay the same through a block.
    pub invariant: &'a [&'a str],
    pub params: &'a [ParamSchema],
    pub functions: &'a [Function],
    /// Whether `line name = seconds;` is allowed: the host function allocating one (its
    /// argument the length in seconds, its site the line's index).
    pub line: Option<u16>,
    /// Lines the host keeps itself (a sound's input and output), numbered before the
    /// script's own.
    pub reserved_lines: u16,
}

impl Env<'_> {
    /// The register of input `reads[i]`.
    pub fn read_register(&self, i: usize) -> u16 {
        i as u16
    }

    /// The register of output `writes[i]`.
    pub fn write_register(&self, i: usize) -> u16 {
        (self.reads.len() + i) as u16
    }
}

#[cfg(test)]
mod tests;
