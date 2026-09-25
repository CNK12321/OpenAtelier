//! Running compiled scripts: a small stack machine over one register file.

/// The math every script has.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Math {
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
}

impl Math {
    pub(crate) fn by_name(name: &str) -> Option<(Math, usize)> {
        use Math::*;
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
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Op {
    Const(f32),
    Load(u16),
    Store(u16),
    /// Drops the top of the stack (a statement call's result).
    Pop,
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Math(Math),
    /// `choose(i, …)` with this many values after the index.
    Choose(u8),
    /// A host function: `argc` values off the stack, its result pushed.
    Host { id: u16, site: u32, argc: u8 },
    /// Skips the next `n` ops unless this is the first run (`state` and `line` set-up).
    OnFirst(u32),
}

/// What a script's host functions do.
pub trait Host {
    /// Host function `id` at call `site` (unique per call in the script) with `args`.
    fn call(&mut self, id: u16, site: u32, args: &[f32]) -> f32;
}

/// A host without functions.
pub struct NoHost;

impl Host for NoHost {
    fn call(&mut self, _id: u16, _site: u32, _args: &[f32]) -> f32 {
        0.0
    }
}

fn truth(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

/// Runs `ops` on `regs`. `first`: the script's first run (its `state`s start).
pub fn run<H: Host + ?Sized>(ops: &[Op], regs: &mut [f32], stack: &mut Vec<f32>, host: &mut H, first: bool) {
    stack.clear();
    macro_rules! bin {
        ($f:expr) => {{
            let b = stack.pop().unwrap_or(0.0);
            let a = stack.pop().unwrap_or(0.0);
            stack.push($f(a, b));
        }};
    }
    let mut i = 0;
    while i < ops.len() {
        match ops[i] {
            Op::Const(x) => stack.push(x),
            Op::Load(r) => stack.push(regs[r as usize]),
            Op::Store(r) => regs[r as usize] = stack.pop().unwrap_or(0.0),
            Op::Pop => {
                stack.pop();
            }
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
            Op::Pow => bin!(|a: f32, b: f32| a.powf(b)),
            Op::Lt => bin!(|a, b| truth(a < b)),
            Op::Le => bin!(|a, b| truth(a <= b)),
            Op::Gt => bin!(|a, b| truth(a > b)),
            Op::Ge => bin!(|a, b| truth(a >= b)),
            Op::Eq => bin!(|a, b| truth(a == b)),
            Op::Ne => bin!(|a, b| truth(a != b)),
            Op::And => bin!(|a, b| truth(a != 0.0 && b != 0.0)),
            Op::Or => bin!(|a, b| truth(a != 0.0 || b != 0.0)),
            Op::Math(f) => math(f, stack),
            Op::Choose(n) => {
                let at = stack.len().saturating_sub(n as usize + 1);
                let index = stack.get(at).copied().unwrap_or(0.0);
                let pick = if n == 0 { 0.0 } else { stack[at + 1 + (index.round().max(0.0) as usize).min(n as usize - 1)] };
                stack.truncate(at);
                stack.push(pick);
            }
            Op::Host { id, site, argc } => {
                let at = stack.len().saturating_sub(argc as usize);
                let v = host.call(id, site, &stack[at..]);
                stack.truncate(at);
                stack.push(v);
            }
            Op::OnFirst(n) => {
                if !first {
                    i += n as usize;
                }
            }
        }
        i += 1;
    }
}

fn math(f: Math, stack: &mut Vec<f32>) {
    let mut pop = || stack.pop().unwrap_or(0.0);
    use Math::*;
    let v = match f {
        Sin | Cos | Tan | Asin | Acos | Atan | Abs | Sign | Floor | Ceil | Round | Fract | Sqrt | Exp | Log | Log2 | Tanh | Db | ToDb => {
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
