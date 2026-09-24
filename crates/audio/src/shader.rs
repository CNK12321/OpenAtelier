//! Sound shaders: the small per-sample programs sound effects can be written in.
//!
//! A picture effect is a WGSL function run once per pixel; a sound effect can be a sound
//! shader run once per sample, per channel. Plugins ship them next to their WGSL (see
//! `plugins/README.md`), and several of Atelier Core's own sound effects are written in
//! it, so plugins use exactly the path built-ins do. Like WGSL shaders, there is nothing
//! a sound shader can reach but its own sample, a little memory and its parameters:
//! no files, no loops, no calls out.
//!
//! ```text
//! // Tremolo: the level swings with a sine.
//! let swing = 0.5 + 0.5 * sin(TAU * speed * time);
//! out = in * (1 - depth * swing);
//! ```
//!
//! * **Reads**: `in` (this sample), `left` / `right` (this frame's two channels, for
//!   stereo effects), `channel` (0 left, 1 right), `channels`, `sample_rate`, the
//!   effect's clock — `time` (seconds since it began), `progress` (0 → 1 across it),
//!   `visibility` (0 → 1 over an intro, 1 → 0 over an outro, 1 otherwise) — and every
//!   number, switch and choice parameter by its id. `PI`, `TAU`.
//! * **Writes**: `out` (starts as `in`; what's left in it is the sample).
//! * `let x = …;` a value for this sample. `state x = …;` a value that persists from
//!   sample to sample (per channel), starting at `…` — filters, envelopes, phases.
//!   `x = …;` `x += …;` (also `-=`, `*=`, `/=`) change them.
//! * Math: `+ - * / %`, comparisons and `&& || !` (1 is true, 0 false), and `sin cos
//!   tan asin acos atan atan2 abs sign floor ceil round fract sqrt exp log log2 pow min
//!   max clamp mix smoothstep step tanh`, `db(x)` (decibels → gain), `to_db(g)`,
//!   `select(c, a, b)` (`a` if `c`, else `b`), `noise()` (white, −1 … 1).
//! * Memory: `delay(s)` — the input `s` seconds ago; `delay_out(s)` — the output `s`
//!   seconds ago (feedback: echoes, combs). Up to [`MAX_DELAY_SECONDS`].
//!
//! Output is kept finite and within ±4, and a channel whose math blows up (NaN) starts
//! over, so a bad shader can make a bad sound but never a stuck or deafening one.

use oa_params::{Evaluated, ParamSchema, ParamType, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// How far back `delay` and `delay_out` reach.
pub const MAX_DELAY_SECONDS: f32 = 4.0;
const MAX_SOURCE: usize = 64 * 1024;
const MAX_OPS: usize = 20_000;
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

// ---- the program -------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Func {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Abs,
    Sign,
    Floor,
    Ceil,
    Round,
    Fract,
    Sqrt,
    Exp,
    Log,
    Log2,
    Pow,
    Min,
    Max,
    Clamp,
    Mix,
    Smoothstep,
    Step,
    Tanh,
    Db,
    ToDb,
    Select,
    Delay,
    DelayOut,
    Noise,
}

impl Func {
    fn by_name(name: &str) -> Option<(Func, usize)> {
        use Func::*;
        Some(match name {
            "sin" => (Sin, 1),
            "cos" => (Cos, 1),
            "tan" => (Tan, 1),
            "asin" => (Asin, 1),
            "acos" => (Acos, 1),
            "atan" => (Atan, 1),
            "atan2" => (Atan2, 2),
            "abs" => (Abs, 1),
            "sign" => (Sign, 1),
            "floor" => (Floor, 1),
            "ceil" => (Ceil, 1),
            "round" => (Round, 1),
            "fract" => (Fract, 1),
            "sqrt" => (Sqrt, 1),
            "exp" => (Exp, 1),
            "log" => (Log, 1),
            "log2" => (Log2, 1),
            "pow" => (Pow, 2),
            "min" => (Min, 2),
            "max" => (Max, 2),
            "clamp" => (Clamp, 3),
            "mix" => (Mix, 3),
            "smoothstep" => (Smoothstep, 3),
            "step" => (Step, 2),
            "tanh" => (Tanh, 1),
            "db" => (Db, 1),
            "to_db" => (ToDb, 1),
            "select" => (Select, 3),
            "delay" => (Delay, 1),
            "delay_out" => (DelayOut, 1),
            "noise" => (Noise, 0),
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Op {
    Const(f32),
    Load(u16),
    Store(u16),
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Call(Func),
}

// Registers every program has, in this order.
const IN: u16 = 0;
const LEFT: u16 = 1;
const RIGHT: u16 = 2;
const CHANNEL: u16 = 3;
const CHANNELS: u16 = 4;
const SAMPLE_RATE: u16 = 5;
const TIME: u16 = 6;
const PROGRESS: u16 = 7;
const VISIBILITY: u16 = 8;
const OUT: u16 = 9;
const BUILTINS: [&str; 10] = ["in", "left", "right", "channel", "channels", "sample_rate", "time", "progress", "visibility", "out"];

/// A compiled sound shader.
#[derive(Debug)]
pub struct Program {
    main: Vec<Op>,
    /// Sets every `state` to its starting value (run on a channel's first sample).
    init: Vec<Op>,
    registers: usize,
    /// Parameter registers and the parameters they hold.
    params: Vec<(u16, ParamSchema)>,
    delays: bool,
    source: Arc<str>,
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.params.len() == other.params.len()
    }
}

impl Program {
    /// Compiles `source` for an effect with `params`. Errors name the line.
    pub fn compile(source: &str, params: &[ParamSchema]) -> Result<Program, String> {
        if source.len() > MAX_SOURCE {
            return Err(format!("the shader is too long ({} KB; the limit is {} KB)", source.len() / 1024, MAX_SOURCE / 1024));
        }
        let tokens = lex(source)?;
        let mut c = Compiler { tokens, at: 0, names: HashMap::new(), registers: BUILTINS.len(), main: Vec::new(), init: Vec::new(), params: Vec::new(), delays: false };
        for (i, name) in BUILTINS.iter().enumerate() {
            c.names.insert((*name).to_string(), Name { register: i as u16, writable: i as u16 == OUT });
        }
        for p in params {
            let numeric = matches!(p.ty, ParamType::Float | ParamType::Int | ParamType::Bool | ParamType::Enum);
            let id = p.id.as_str();
            // A name followed by "(" is a function, so a parameter may share one's name
            // (`mix`); colors, gradients and names clashing with built-ins aren't readable.
            if !numeric || !is_ident(id) || c.names.contains_key(id) || matches!(id, "let" | "state" | "PI" | "TAU") {
                continue;
            }
            let register = c.next_register()?;
            c.names.insert(id.to_string(), Name { register, writable: false });
            c.params.push((register, p.clone()));
        }
        c.program()?;
        if c.main.len() + c.init.len() > MAX_OPS {
            return Err("the shader is too long".into());
        }
        Ok(Program { main: c.main, init: c.init, registers: c.registers, params: c.params, delays: c.delays, source: source.into() })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---- lexing ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Ident(String),
    Sym(&'static str),
}

const SYMBOLS: [&str; 24] = [
    "+=", "-=", "*=", "/=", "==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/", "%", "(", ")", ",", ";", "=", "<", ">", "!", "^",
];

fn lex(source: &str) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let (mut i, mut line) = (0, 1);
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '#' || source[i..].starts_with("//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c.is_ascii_digit() || (c == '.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                i += 1;
                if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text = &source[start..i];
            let n = text.parse::<f32>().map_err(|_| format!("line {line}: \"{text}\" isn't a number"))?;
            out.push((Tok::Num(n), line));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((Tok::Ident(source[start..i].to_string()), line));
        } else if let Some(s) = SYMBOLS.iter().find(|s| source[i..].starts_with(**s)) {
            out.push((Tok::Sym(s), line));
            i += s.len();
        } else {
            let ch = source[i..].chars().next().unwrap_or('?');
            return Err(format!("line {line}: unexpected \"{ch}\""));
        }
    }
    Ok(out)
}

// ---- parsing and code generation ------------------------------------------------

#[derive(Copy, Clone)]
struct Name {
    register: u16,
    writable: bool,
}

struct Compiler {
    tokens: Vec<(Tok, usize)>,
    at: usize,
    names: HashMap<String, Name>,
    registers: usize,
    main: Vec<Op>,
    init: Vec<Op>,
    params: Vec<(u16, ParamSchema)>,
    delays: bool,
}

impl Compiler {
    fn line(&self) -> usize {
        self.tokens.get(self.at).or(self.tokens.last()).map_or(1, |t| t.1)
    }

    fn fail<T>(&self, what: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {what}", self.line()))
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|t| &t.0)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, sym: &str) -> Result<(), String> {
        if self.eat(sym) {
            Ok(())
        } else {
            let got = match self.peek() {
                Some(Tok::Num(n)) => format!("{n}"),
                Some(Tok::Ident(s)) => format!("\"{s}\""),
                Some(Tok::Sym(s)) => format!("\"{s}\""),
                None => "the end".into(),
            };
            self.fail(format!("expected \"{sym}\", found {got}"))
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Ident(s)) => {
                self.at += 1;
                Ok(s)
            }
            _ => self.fail("expected a name"),
        }
    }

    fn next_register(&mut self) -> Result<u16, String> {
        if self.registers >= u16::MAX as usize {
            return self.fail("too many names");
        }
        self.registers += 1;
        Ok((self.registers - 1) as u16)
    }

    fn declare(&mut self, name: &str) -> Result<u16, String> {
        if self.names.contains_key(name) || Func::by_name(name).is_some() || matches!(name, "let" | "state" | "PI" | "TAU") {
            return self.fail(format!("\"{name}\" is already taken"));
        }
        let register = self.next_register()?;
        self.names.insert(name.to_string(), Name { register, writable: true });
        Ok(register)
    }

    fn program(&mut self) -> Result<(), String> {
        while self.at < self.tokens.len() {
            self.statement()?;
        }
        Ok(())
    }

    fn statement(&mut self) -> Result<(), String> {
        let word = self.ident()?;
        match word.as_str() {
            "let" => {
                let name = self.ident()?;
                self.expect("=")?;
                let mut code = Vec::new();
                self.expr(&mut code)?;
                let register = self.declare(&name)?;
                code.push(Op::Store(register));
                self.main.extend(code);
            }
            "state" => {
                let name = self.ident()?;
                self.expect("=")?;
                let mut code = Vec::new();
                self.expr(&mut code)?;
                let register = self.declare(&name)?;
                code.push(Op::Store(register));
                self.init.extend(code);
            }
            _ => {
                let Some(target) = self.names.get(&word).copied() else { return self.fail(format!("\"{word}\" isn't defined")) };
                if !target.writable {
                    return self.fail(format!("\"{word}\" can't be changed"));
                }
                let op = if self.eat("=") {
                    None
                } else if self.eat("+=") {
                    Some(Op::Add)
                } else if self.eat("-=") {
                    Some(Op::Sub)
                } else if self.eat("*=") {
                    Some(Op::Mul)
                } else if self.eat("/=") {
                    Some(Op::Div)
                } else {
                    return self.fail(format!("expected \"=\" after \"{word}\""));
                };
                let mut code = Vec::new();
                if op.is_some() {
                    code.push(Op::Load(target.register));
                }
                self.expr(&mut code)?;
                code.extend(op);
                code.push(Op::Store(target.register));
                self.main.extend(code);
            }
        }
        self.expect(";")
    }

    fn expr(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.and(code)?;
        while self.eat("||") {
            self.and(code)?;
            code.push(Op::Or);
        }
        Ok(())
    }

    fn and(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.compare(code)?;
        while self.eat("&&") {
            self.compare(code)?;
            code.push(Op::And);
        }
        Ok(())
    }

    fn compare(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.sum(code)?;
        for (sym, op) in [("==", Op::Eq), ("!=", Op::Ne), ("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)] {
            if self.eat(sym) {
                self.sum(code)?;
                code.push(op);
                break;
            }
        }
        Ok(())
    }

    fn sum(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.product(code)?;
        loop {
            if self.eat("+") {
                self.product(code)?;
                code.push(Op::Add);
            } else if self.eat("-") {
                self.product(code)?;
                code.push(Op::Sub);
            } else {
                return Ok(());
            }
        }
    }

    fn product(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.unary(code)?;
        loop {
            if self.eat("*") {
                self.unary(code)?;
                code.push(Op::Mul);
            } else if self.eat("/") {
                self.unary(code)?;
                code.push(Op::Div);
            } else if self.eat("%") {
                self.unary(code)?;
                code.push(Op::Rem);
            } else {
                return Ok(());
            }
        }
    }

    fn unary(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        if self.eat("-") {
            self.unary(code)?;
            code.push(Op::Neg);
        } else if self.eat("!") {
            self.unary(code)?;
            code.push(Op::Not);
        } else {
            self.power(code)?;
        }
        Ok(())
    }

    /// `a ^ b` is `pow(a, b)` (right-associative, tighter than `*`).
    fn power(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        self.primary(code)?;
        if self.eat("^") {
            self.unary(code)?;
            code.push(Op::Call(Func::Pow));
        }
        Ok(())
    }

    fn primary(&mut self, code: &mut Vec<Op>) -> Result<(), String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.at += 1;
                code.push(Op::Const(n));
            }
            Some(Tok::Sym("(")) => {
                self.at += 1;
                self.expr(code)?;
                self.expect(")")?;
            }
            Some(Tok::Ident(name)) => {
                self.at += 1;
                if self.eat("(") {
                    let Some((func, arity)) = Func::by_name(&name) else { return self.fail(format!("there's no function called \"{name}\"")) };
                    let mut args = 0;
                    if !self.eat(")") {
                        loop {
                            self.expr(code)?;
                            args += 1;
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    if args != arity {
                        return self.fail(format!("{name}() takes {arity} value{}, not {args}", if arity == 1 { "" } else { "s" }));
                    }
                    if matches!(func, Func::Delay | Func::DelayOut) {
                        self.delays = true;
                    }
                    code.push(Op::Call(func));
                } else if name == "PI" {
                    code.push(Op::Const(std::f32::consts::PI));
                } else if name == "TAU" {
                    code.push(Op::Const(std::f32::consts::TAU));
                } else {
                    let Some(n) = self.names.get(&name) else { return self.fail(format!("\"{name}\" isn't defined")) };
                    code.push(Op::Load(n.register));
                }
            }
            Some(Tok::Sym(s)) => return self.fail(format!("unexpected \"{s}\"")),
            None => return self.fail("the shader ends in the middle of a line"),
        }
        Ok(())
    }
}

// ---- running -------------------------------------------------------------------

/// What `delay`, `delay_out` and `noise` read while one channel's sample runs.
struct Memory<'a> {
    input: &'a [f32],
    output: &'a [f32],
    /// Where this frame is written in the ring.
    pos: usize,
    rate: f32,
    seed: &'a mut u64,
}

impl Memory<'_> {
    fn read(ring: &[f32], pos: usize, samples: f32) -> f32 {
        if ring.is_empty() {
            return 0.0;
        }
        let len = ring.len();
        let d = samples.clamp(0.0, (len - 2) as f32);
        let whole = d.floor();
        let k = d - whole;
        let a = ring[(pos + len - whole as usize) % len];
        let b = ring[(pos + len - whole as usize - 1) % len];
        a + (b - a) * k
    }
}

fn run(ops: &[Op], regs: &mut [f32], stack: &mut Vec<f32>, mem: &mut Memory<'_>) {
    stack.clear();
    macro_rules! bin {
        ($f:expr) => {{
            let b = stack.pop().unwrap_or(0.0);
            let a = stack.pop().unwrap_or(0.0);
            stack.push($f(a, b));
        }};
    }
    let truth = |b: bool| if b { 1.0 } else { 0.0 };
    for op in ops {
        match *op {
            Op::Const(x) => stack.push(x),
            Op::Load(r) => stack.push(regs[r as usize]),
            Op::Store(r) => regs[r as usize] = stack.pop().unwrap_or(0.0),
            Op::Neg => {
                let a = stack.pop().unwrap_or(0.0);
                stack.push(-a);
            }
            Op::Not => {
                let a = stack.pop().unwrap_or(0.0);
                stack.push(truth(a == 0.0));
            }
            Op::Add => bin!(|a, b| a + b),
            Op::Sub => bin!(|a, b| a - b),
            Op::Mul => bin!(|a, b| a * b),
            Op::Div => bin!(|a: f32, b: f32| if b == 0.0 { 0.0 } else { a / b }),
            Op::Rem => bin!(|a: f32, b: f32| if b == 0.0 { 0.0 } else { a.rem_euclid(b) }),
            Op::Lt => bin!(|a, b| truth(a < b)),
            Op::Le => bin!(|a, b| truth(a <= b)),
            Op::Gt => bin!(|a, b| truth(a > b)),
            Op::Ge => bin!(|a, b| truth(a >= b)),
            Op::Eq => bin!(|a, b| truth(a == b)),
            Op::Ne => bin!(|a, b| truth(a != b)),
            Op::And => bin!(|a, b| truth(a != 0.0 && b != 0.0)),
            Op::Or => bin!(|a, b| truth(a != 0.0 || b != 0.0)),
            Op::Call(f) => call(f, stack, mem),
        }
    }
}

fn call(f: Func, stack: &mut Vec<f32>, mem: &mut Memory<'_>) {
    let mut pop = || stack.pop().unwrap_or(0.0);
    use Func::*;
    let v = match f {
        Noise => {
            // xorshift64*
            let mut x = *mem.seed;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            *mem.seed = x;
            ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
        Sin | Cos | Tan | Asin | Acos | Atan | Abs | Sign | Floor | Ceil | Round | Fract | Sqrt | Exp | Log | Log2 | Tanh | Db | ToDb | Delay | DelayOut => {
            let a = pop();
            match f {
                Sin => a.sin(),
                Cos => a.cos(),
                Tan => a.tan(),
                Asin => a.clamp(-1.0, 1.0).asin(),
                Acos => a.clamp(-1.0, 1.0).acos(),
                Atan => a.atan(),
                Abs => a.abs(),
                Sign => {
                    if a == 0.0 {
                        0.0
                    } else {
                        a.signum()
                    }
                }
                Floor => a.floor(),
                Ceil => a.ceil(),
                Round => a.round(),
                Fract => a - a.floor(),
                Sqrt => a.max(0.0).sqrt(),
                Exp => a.min(80.0).exp(),
                Log => a.max(1e-30).ln(),
                Log2 => a.max(1e-30).log2(),
                Tanh => a.tanh(),
                Db => 10f32.powf(a.min(60.0) / 20.0),
                ToDb => 20.0 * a.abs().max(1e-10).log10(),
                Delay => Memory::read(mem.input, mem.pos, a * mem.rate),
                // The output ring holds past samples only: at least one sample back.
                DelayOut => Memory::read(mem.output, mem.pos, (a * mem.rate).max(1.0)),
                _ => unreachable!(),
            }
        }
        Atan2 | Pow | Min | Max | Step => {
            let b = pop();
            let a = pop();
            match f {
                Atan2 => a.atan2(b),
                Pow => a.powf(b),
                Min => a.min(b),
                Max => a.max(b),
                Step => {
                    if b < a {
                        0.0
                    } else {
                        1.0
                    }
                }
                _ => unreachable!(),
            }
        }
        Clamp | Mix | Smoothstep | Select => {
            let c = pop();
            let b = pop();
            let a = pop();
            match f {
                Clamp => a.max(b).min(c),
                Mix => a + (b - a) * c,
                Smoothstep => {
                    let t = if b == a { 0.0 } else { ((c - a) / (b - a)).clamp(0.0, 1.0) };
                    t * t * (3.0 - 2.0 * t)
                }
                Select => {
                    if a != 0.0 {
                        b
                    } else {
                        c
                    }
                }
                _ => unreachable!(),
            }
        }
    };
    stack.push(v);
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
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
    pos: usize,
    seed: u64,
}

impl ShaderProcessor {
    pub(crate) fn new(program: Arc<Program>, channels: usize, rate: f32) -> Self {
        let ch = channels.max(1);
        let ring = if program.delays { (MAX_DELAY_SECONDS * rate) as usize + 2 } else { 0 };
        ShaderProcessor {
            ch,
            rate,
            regs: vec![vec![0.0; program.registers]; ch],
            fresh: vec![true; ch],
            stack: Vec::with_capacity(64),
            input: vec![vec![0.0; ring]; ch],
            output: vec![vec![0.0; ring]; ch],
            pos: 0,
            seed: 0x9E37_79B9_7F4A_7C15,
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
        // Parameters change per block; the clock glides across it.
        let params: Vec<(u16, f32)> = self
            .program
            .params
            .iter()
            .map(|(r, schema)| {
                let x = match v.get(schema.id.as_str()) {
                    Some(Value::Float(x)) => *x as f32,
                    Some(Value::Int(i)) => *i as f32,
                    Some(Value::Bool(b)) => *b as u8 as f32,
                    Some(e @ Value::Enum(_)) => schema.option_index(e).unwrap_or(0) as f32,
                    _ => 0.0,
                };
                (*r, x)
            })
            .collect();
        let (s, e) = (clock.start, clock.end);
        let ring = self.input.first().map_or(0, Vec::len);
        let mut frame_in = vec![0.0f32; ch];
        for f in 0..frames {
            let k = if frames > 1 { f as f64 / frames as f64 } else { 0.0 };
            let time = (s.seconds + f as f64 / self.rate as f64) as f32;
            let progress = (s.progress + (e.progress - s.progress) * k) as f32;
            let visibility = (s.visibility + (e.visibility - s.visibility) * k) as f32;
            frame_in.copy_from_slice(&buf[f * ch..f * ch + ch]);
            let (left, right) = (frame_in[0], frame_in[1.min(ch - 1)]);
            for c in 0..ch {
                if ring > 0 {
                    self.input[c][self.pos] = frame_in[c];
                }
                let regs = &mut self.regs[c];
                regs[IN as usize] = frame_in[c];
                regs[LEFT as usize] = left;
                regs[RIGHT as usize] = right;
                regs[CHANNEL as usize] = c as f32;
                regs[CHANNELS as usize] = ch as f32;
                regs[SAMPLE_RATE as usize] = self.rate;
                regs[TIME as usize] = time;
                regs[PROGRESS as usize] = progress;
                regs[VISIBILITY as usize] = visibility;
                regs[OUT as usize] = frame_in[c];
                for (r, x) in &params {
                    regs[*r as usize] = *x;
                }
                let mut mem = Memory { input: &self.input[c], output: &self.output[c], pos: self.pos, rate: self.rate, seed: &mut self.seed };
                if self.fresh[c] {
                    run(&self.program.init, regs, &mut self.stack, &mut mem);
                    self.fresh[c] = false;
                }
                run(&self.program.main, regs, &mut self.stack, &mut mem);
                let mut y = regs[OUT as usize];
                if !y.is_finite() {
                    // Blown up: silence this sample and start the channel over.
                    y = 0.0;
                    regs.iter_mut().for_each(|r| *r = 0.0);
                    self.fresh[c] = true;
                }
                let y = y.clamp(-LIMIT, LIMIT);
                if ring > 0 {
                    self.output[c][self.pos] = y;
                }
                buf[f * ch + c] = y;
            }
            if ring > 0 {
                self.pos = (self.pos + 1) % ring;
            }
        }
    }

    fn reset(&mut self) {
        self.regs.iter_mut().for_each(|r| r.iter_mut().for_each(|x| *x = 0.0));
        self.fresh.iter_mut().for_each(|f| *f = true);
        self.input.iter_mut().chain(self.output.iter_mut()).for_each(|r| r.iter_mut().for_each(|x| *x = 0.0));
        self.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::Processor;
    use oa_params::{ParamId, Unit};

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
        let mut p = running("state n = 10; n += 1; out = n;", &[]);
        let mut buf = [0.0; 6];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        // Two channels, three frames each: 11, 12, 13 (held to ±4).
        assert_eq!(buf, [4.0; 6]);
        let mut p = running("state n = 0; n += 1; out = n / 4;", &[]);
        let mut buf = [0.0; 6];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(buf, [0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
        p.reset();
        let mut buf = [0.0; 2];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert_eq!(buf, [0.25, 0.25]);
    }

    /// `delay` reads the input back in time; `delay_out` feeds the output back (an echo).
    #[test]
    fn delays_reach_back() {
        let mut p = running("out = delay(2 / sample_rate);", &[]);
        let near = |got: &[f32], want: &[f32]| got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3);
        let mut buf = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]), "{buf:?}");
        let mut p = running("out = in + 0.5 * delay_out(1 / sample_rate);", &[]);
        let mut buf = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(near(&buf, &[1.0, 1.0, 0.5, 0.5, 0.25, 0.25]), "{buf:?}");
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

    /// Atelier Core's shader effects compile and make sound (not silence, not blow-ups);
    /// a plugin's are installed beside them, and taken ids are refused.
    #[test]
    fn built_in_and_plugin_shaders() {
        let core = crate::fx::core_catalog();
        let shaders: Vec<_> = core.iter().filter(|f| f.shader.is_some()).collect();
        assert!(shaders.len() >= 5);
        for fx in shaders {
            let values = Evaluated(fx.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect());
            let mut p = crate::fx::processor(&fx.type_id, 2, 48_000.0).expect("runs");
            let mut buf: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * if i % 2 == 0 { 0.5 } else { 0.3 }).collect();
            p.process(&mut buf, &values, &ClockSpan::default());
            let rms = (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt();
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

    /// Mistakes name the line; blow-ups become silence rather than NaN forever.
    #[test]
    fn errors_and_blowups() {
        let err = |s: &str| Program::compile(s, &[param("amount", 0.0)]).expect_err(s);
        assert!(err("out = in *;").contains("line 1"));
        assert!(err("\n\nout = wobble;").starts_with("line 3") && err("\n\nout = wobble;").contains("\"wobble\" isn't defined"));
        assert!(err("in = 1;").contains("can't be changed"));
        assert!(err("amount = 1;").contains("can't be changed"));
        assert!(err("out = pow(1);").contains("takes 2 values"));
        assert!(err("let sin = 1;").contains("taken"));
        assert!(err("out = in @ 2;").contains("unexpected"));
        assert!(Program::compile("// just a comment\n# and another", &[]).is_ok());

        let mut p = running("state s = 1; s = s * 1e30; out = s;", &[]);
        let mut buf = [0.0; 8];
        p.process(&mut buf, &Evaluated(vec![]), &ClockSpan::default());
        assert!(buf.iter().all(|x| x.is_finite() && x.abs() <= LIMIT), "{buf:?}");
    }
}
