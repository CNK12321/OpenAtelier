//! Statements → ops, with names resolved to registers and steady `let`s moved to the block.

use crate::lex::{is_ident, lex};
use crate::parse::{Bin, Expr, Parser, Stmt};
use crate::vm::{Math, Op};
use crate::{Arg, Env, Function, MAX_OPS, MAX_SOURCE};
use oa_params::{Evaluated, ParamSchema, ParamType, Value};
use std::collections::{HashMap, HashSet};

const KEYWORDS: [&str; 6] = ["let", "state", "line", "PI", "TAU", "choose"];

/// Compiled statements: what runs once per block, and what runs every time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    /// The steady `let`s, run once at the start of each block (before `main`).
    pub block: Vec<Op>,
    pub main: Vec<Op>,
}

/// Which register holds which parameter (or part of one).
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSlot {
    pub register: u16,
    /// Index into the [`Env::params`] the script was compiled with.
    pub param: usize,
    /// Which number of a point or color (0 otherwise).
    pub component: u8,
}

/// The register file a script's sections share.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub registers: usize,
    pub params: Vec<ParamSlot>,
    /// Host-function call sites (each has its own `site` number, 0..sites).
    pub sites: u32,
    /// Delay lines, the host's reserved ones included (their indexes are 0..lines).
    pub lines: u16,
    names: HashMap<String, u16>,
}

impl Frame {
    /// The register a name was given.
    pub fn register(&self, name: &str) -> Option<u16> {
        self.names.get(name).copied()
    }

    /// Puts the parameters' values into their registers.
    pub fn load_params(&self, regs: &mut [f32], schemas: &[ParamSchema], values: &Evaluated) {
        for slot in &self.params {
            let schema = &schemas[slot.param];
            regs[slot.register as usize] = param_number(schema, values.get(schema.id.as_str()), slot.component);
        }
    }
}

/// A parameter's value as a script reads it: numbers as they are, switches 0/1, a choice
/// as its option's index, one number of a point or color.
pub fn param_number(schema: &ParamSchema, value: Option<&Value>, component: u8) -> f32 {
    let c = component as usize;
    match value.unwrap_or(&schema.default) {
        Value::Float(x) => *x as f32,
        Value::Int(i) => *i as f32,
        Value::Bool(b) => *b as u8 as f32,
        e @ Value::Enum(_) => schema.option_index(e).unwrap_or(0) as f32,
        Value::Vec2(v) => v.get(c).copied().unwrap_or(0.0) as f32,
        Value::Vec3(v) => v.get(c).copied().unwrap_or(0.0) as f32,
        Value::Color(v) => v.get(c).copied().unwrap_or(0.0) as f32,
        _ => 0.0,
    }
}

/// The names a parameter is read by: its id, or its parts'.
fn param_names(schema: &ParamSchema) -> Vec<String> {
    let id = schema.id.as_str();
    let parts: &[&str] = match schema.ty {
        ParamType::Float | ParamType::Int | ParamType::Bool | ParamType::Enum => return vec![id.to_string()],
        ParamType::Vec2 => &["x", "y"],
        ParamType::Vec3 => &["x", "y", "z"],
        ParamType::Color => &["r", "g", "b", "a"],
        _ => return Vec::new(),
    };
    parts.iter().map(|p| format!("{id}_{p}")).collect()
}

#[derive(Copy, Clone)]
struct Name {
    register: u16,
    writable: bool,
    /// Can't change during a block.
    steady: bool,
}

/// Compiles one or more sections sharing a register file (a spectrum's passes).
pub struct Compiler<'a> {
    env: Env<'a>,
    names: HashMap<String, Name>,
    lines: HashMap<String, u16>,
    registers: usize,
    params: Vec<ParamSlot>,
    sites: u32,
    ops: usize,
}

/// Compiles a one-section script.
pub fn compile(source: &str, env: Env<'_>) -> Result<(Section, Frame), String> {
    let mut c = Compiler::new(env)?;
    let section = c.section(source, 1)?;
    Ok((section, c.finish()))
}

impl<'a> Compiler<'a> {
    pub fn new(env: Env<'a>) -> Result<Self, String> {
        let mut c = Compiler { env, names: HashMap::new(), lines: HashMap::new(), registers: 0, params: Vec::new(), sites: 0, ops: 0 };
        for name in env.reads {
            let steady = env.invariant.contains(name);
            c.add(name, false, steady)?;
        }
        for name in env.writes {
            c.add(name, true, false)?;
        }
        for (i, p) in env.params.iter().enumerate() {
            for (component, name) in param_names(p).into_iter().enumerate() {
                // A name followed by "(" is a function, so a parameter may share one's name
                // (`mix`); ones clashing with the host's names or keywords aren't readable.
                if !is_ident(&name) || c.names.contains_key(&name) || KEYWORDS.contains(&name.as_str()) {
                    continue;
                }
                let register = c.add(&name, false, true)?;
                c.params.push(ParamSlot { register, param: i, component: component as u8 });
            }
        }
        Ok(c)
    }

    fn add(&mut self, name: &str, writable: bool, steady: bool) -> Result<u16, String> {
        if self.registers >= u16::MAX as usize {
            return Err("too many names".into());
        }
        let register = self.registers as u16;
        self.registers += 1;
        self.names.insert(name.to_string(), Name { register, writable, steady });
        Ok(register)
    }

    fn function(&self, name: &str) -> Option<&Function> {
        self.env.functions.iter().find(|f| f.name == name)
    }

    fn taken(&self, name: &str) -> bool {
        self.names.contains_key(name) || self.lines.contains_key(name) || KEYWORDS.contains(&name) || Math::by_name(name).is_some() || self.function(name).is_some()
    }

    /// Compiles `source` (whose first line is `first_line`) as a new section; names it
    /// declares stay visible to later sections.
    pub fn section(&mut self, source: &str, first_line: usize) -> Result<Section, String> {
        if source.len() > MAX_SOURCE {
            return Err(format!("the script is too long ({} KB; the limit is {} KB)", source.len() / 1024, MAX_SOURCE / 1024));
        }
        let mut parser = Parser::new(lex(source, first_line)?, self.env.line.is_some());
        let mut stmts = Vec::new();
        while let Some(s) = parser.statement()? {
            stmts.push(s);
        }
        // A `let` changed later can't be moved to the block.
        let assigned: HashSet<&str> = stmts.iter().filter_map(|(s, _)| if let Stmt::Assign(n, ..) = s { Some(n.as_str()) } else { None }).collect();
        let mut out = Section::default();
        for (stmt, line) in &stmts {
            let fail = |e: String| format!("line {line}: {e}");
            match stmt {
                Stmt::Let(name, e) => {
                    let mut code = Vec::new();
                    self.expr(e, &mut code).map_err(fail)?;
                    let steady = !assigned.contains(name.as_str()) && self.steady(e);
                    if self.taken(name) {
                        return Err(fail(format!("\"{name}\" is already taken")));
                    }
                    let register = self.add(name, true, steady).map_err(fail)?;
                    code.push(Op::Store(register));
                    if steady { out.block.extend(code) } else { out.main.extend(code) }
                }
                Stmt::State(name, e) => {
                    let mut code = Vec::new();
                    self.expr(e, &mut code).map_err(fail)?;
                    if self.taken(name) {
                        return Err(fail(format!("\"{name}\" is already taken")));
                    }
                    let register = self.add(name, true, false).map_err(fail)?;
                    code.push(Op::Store(register));
                    out.main.push(Op::OnFirst(code.len() as u32));
                    out.main.extend(code);
                }
                Stmt::Line(name, e) => {
                    let Some(alloc) = self.env.line else { return Err(fail("delay lines aren't available here".into())) };
                    let mut code = Vec::new();
                    self.expr(e, &mut code).map_err(fail)?;
                    if self.taken(name) {
                        return Err(fail(format!("\"{name}\" is already taken")));
                    }
                    let index = self.env.reserved_lines + self.lines.len() as u16;
                    self.lines.insert(name.clone(), index);
                    code.push(Op::Host { id: alloc, site: index as u32, argc: 1 });
                    code.push(Op::Pop);
                    out.main.push(Op::OnFirst(code.len() as u32));
                    out.main.extend(code);
                }
                Stmt::Assign(name, op, e) => {
                    let Some(target) = self.names.get(name).copied() else { return Err(fail(format!("\"{name}\" isn't defined"))) };
                    if !target.writable {
                        return Err(fail(format!("\"{name}\" can't be changed")));
                    }
                    if op.is_some() {
                        out.main.push(Op::Load(target.register));
                    }
                    self.expr(e, &mut out.main).map_err(fail)?;
                    if let Some(op) = op {
                        out.main.push(bin_op(*op));
                    }
                    out.main.push(Op::Store(target.register));
                }
                Stmt::Call(e) => {
                    let Expr::Call(name, _) = e else { unreachable!() };
                    if !self.function(name).is_some_and(|f| f.statement) {
                        return Err(fail(format!("{name}() gives a value: use it in an expression")));
                    }
                    self.expr(e, &mut out.main).map_err(fail)?;
                    out.main.push(Op::Pop);
                }
            }
        }
        self.ops += out.block.len() + out.main.len();
        if self.ops > MAX_OPS {
            return Err("the script is too long".into());
        }
        Ok(out)
    }

    pub fn finish(self) -> Frame {
        Frame {
            registers: self.registers,
            params: self.params,
            sites: self.sites,
            lines: self.env.reserved_lines + self.lines.len() as u16,
            names: self.names.into_iter().map(|(k, v)| (k, v.register)).collect(),
        }
    }

    /// Whether `e` can't change during a block.
    fn steady(&self, e: &Expr) -> bool {
        match e {
            Expr::Num(_) => true,
            Expr::Name(n) => matches!(n.as_str(), "PI" | "TAU") || self.names.get(n).is_some_and(|n| n.steady),
            Expr::Neg(a) | Expr::Not(a) => self.steady(a),
            Expr::Bin(_, a, b) => self.steady(a) && self.steady(b),
            Expr::Call(name, args) => {
                let pure = name == "choose" || Math::by_name(name).is_some() || self.function(name).is_some_and(|f| f.pure && f.args.iter().all(|a| *a == Arg::Value));
                pure && args.iter().all(|a| self.steady(a))
            }
        }
    }

    fn expr(&mut self, e: &Expr, code: &mut Vec<Op>) -> Result<(), String> {
        match e {
            Expr::Num(n) => code.push(Op::Const(*n)),
            Expr::Name(n) => match n.as_str() {
                "PI" => code.push(Op::Const(std::f32::consts::PI)),
                "TAU" => code.push(Op::Const(std::f32::consts::TAU)),
                _ => {
                    if self.lines.contains_key(n) {
                        return Err(format!("\"{n}\" is a delay line: read it with read({n}, seconds)"));
                    }
                    let Some(name) = self.names.get(n) else { return Err(format!("\"{n}\" isn't defined")) };
                    code.push(Op::Load(name.register));
                }
            },
            Expr::Neg(a) => {
                self.expr(a, code)?;
                code.push(Op::Neg);
            }
            Expr::Not(a) => {
                self.expr(a, code)?;
                code.push(Op::Not);
            }
            Expr::Bin(op, a, b) => {
                self.expr(a, code)?;
                self.expr(b, code)?;
                code.push(bin_op(*op));
            }
            Expr::Call(name, args) => {
                let plural = |n: usize| if n == 1 { "" } else { "s" };
                if name == "choose" {
                    if args.len() < 2 || args.len() > 33 {
                        return Err("choose() takes an index and 1 to 32 values".into());
                    }
                    for a in args {
                        self.expr(a, code)?;
                    }
                    code.push(Op::Choose((args.len() - 1) as u8));
                } else if let Some((f, arity)) = Math::by_name(name) {
                    if args.len() != arity {
                        return Err(format!("{name}() takes {arity} value{}, not {}", plural(arity), args.len()));
                    }
                    for a in args {
                        self.expr(a, code)?;
                    }
                    code.push(Op::Math(f));
                } else if let Some(f) = self.function(name).copied() {
                    if args.len() != f.args.len() {
                        return Err(format!("{name}() takes {} value{}, not {}", f.args.len(), plural(f.args.len()), args.len()));
                    }
                    for (a, kind) in args.iter().zip(f.args) {
                        match kind {
                            Arg::Value => self.expr(a, code)?,
                            Arg::Line => {
                                let index = match a {
                                    Expr::Name(n) => self.lines.get(n).copied(),
                                    _ => None,
                                };
                                let Some(index) = index else { return Err(format!("{name}() needs the name of a delay line (declared with `line`)")) };
                                code.push(Op::Const(index as f32));
                            }
                            Arg::Name => {
                                let register = match a {
                                    Expr::Name(n) => self.names.get(n).map(|n| n.register),
                                    _ => None,
                                };
                                let Some(register) = register else { return Err(format!("{name}() needs the name of a value")) };
                                code.push(Op::Const(register as f32));
                            }
                        }
                    }
                    let site = self.sites;
                    self.sites += 1;
                    code.push(Op::Host { id: f.id, site, argc: f.args.len() as u8 });
                } else {
                    return Err(format!("there's no function called \"{name}\""));
                }
            }
        }
        Ok(())
    }
}

fn bin_op(op: Bin) -> Op {
    match op {
        Bin::Add => Op::Add,
        Bin::Sub => Op::Sub,
        Bin::Mul => Op::Mul,
        Bin::Div => Op::Div,
        Bin::Rem => Op::Rem,
        Bin::Pow => Op::Pow,
        Bin::Lt => Op::Lt,
        Bin::Le => Op::Le,
        Bin::Gt => Op::Gt,
        Bin::Ge => Op::Ge,
        Bin::Eq => Op::Eq,
        Bin::Ne => Op::Ne,
        Bin::And => Op::And,
        Bin::Or => Op::Or,
    }
}
