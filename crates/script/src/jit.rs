//! Compiling scripts to machine code, with Cranelift, so a sound shader runs about as
//! fast as the same effect written in Rust would.
//!
//! Each list of ops (a [`Section`](crate::Section)'s `block` or `main`) becomes one
//! function `(regs, memory, first)`. The registers a run reads are loaded once into SSA
//! values and the ones it writes stored once at the end; `OnFirst` becomes a branch.
//! Math that has an instruction is inline, the rest calls small Rust functions with the
//! interpreter's exact behavior. The memory operations ([`Intrinsic`]) — reading and
//! writing delay lines, filters — are inline too, on [`Memory`], laid out as
//! [`dsp`](crate::dsp) describes; ordinary host functions call back through
//! [`Memory::host`]. The interpreter ([`crate::run`]) stays the reference: tests run both
//! and compare.

use crate::dsp::{self, Line, Shape, SITE_FLOATS};
use crate::vm::{Math, Op};
use crate::{Function, Host, Intrinsic};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, Type, UserFuncName, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::offset_of;
use std::sync::Arc;

/// What compiled code reads and writes besides its registers.
#[repr(C)]
pub struct Memory {
    /// The delay lines, by index ([`crate::Frame::lines`] of them).
    pub lines: *mut Line,
    /// [`SITE_FLOATS`] per call site ([`crate::Frame::sites`]).
    pub sites: *mut f32,
    /// `noise()`'s state.
    pub seed: *mut u64,
    /// A `*mut &mut dyn Host`, for [`Intrinsic::Call`] functions (may be null if the
    /// script has none).
    pub host: *mut c_void,
    /// Samples per second: seconds → line slots.
    pub rate: f32,
}

type Entry = unsafe extern "C" fn(*mut f32, *mut Memory, i32);

/// Owns the machine code; freed when the last [`Native`] using it goes.
struct Code(Option<JITModule>);

impl Drop for Code {
    fn drop(&mut self) {
        if let Some(m) = self.0.take() {
            // SAFETY: nothing calls into the code once the last `Native` is gone.
            unsafe { m.free_memory() };
        }
    }
}

// SAFETY: the module is only touched to free it; the code itself is immutable.
unsafe impl Send for Code {}
unsafe impl Sync for Code {}

/// Compiled parts of a script.
#[derive(Clone)]
pub struct Native {
    _code: Arc<Code>,
    entries: Vec<Entry>,
    registers: usize,
}

impl std::fmt::Debug for Native {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Native({} parts)", self.entries.len())
    }
}

impl Native {
    /// Runs part `part` on `regs`.
    ///
    /// # Safety
    /// `memory` must hold every line and call site the script uses: `lines` at least
    /// [`crate::Frame::lines`] entries (each allocated line pointing at a live buffer of
    /// its `len`, with `pos < len`, and the reserved ones at least 2 long), `sites` at
    /// least [`crate::Frame::sites`] × [`SITE_FLOATS`] floats, `seed` valid, and `host` a
    /// live `*mut &mut dyn Host` if the script calls host functions.
    pub unsafe fn run(&self, part: usize, regs: &mut [f32], memory: &mut Memory, first: bool) {
        assert!(regs.len() >= self.registers, "a register file too small for the script");
        // SAFETY: per this function's contract.
        unsafe { (self.entries[part])(regs.as_mut_ptr(), memory, first as i32) }
    }
}

// ---- what compiled code calls ----

extern "C" fn oa_sin(x: f32) -> f32 {
    x.sin()
}
extern "C" fn oa_cos(x: f32) -> f32 {
    x.cos()
}
extern "C" fn oa_tan(x: f32) -> f32 {
    x.tan()
}
extern "C" fn oa_asin(x: f32) -> f32 {
    x.clamp(-1.0, 1.0).asin()
}
extern "C" fn oa_acos(x: f32) -> f32 {
    x.clamp(-1.0, 1.0).acos()
}
extern "C" fn oa_atan(x: f32) -> f32 {
    x.atan()
}
extern "C" fn oa_exp(x: f32) -> f32 {
    x.min(80.0).exp()
}
extern "C" fn oa_log(x: f32) -> f32 {
    x.max(1e-30).ln()
}
extern "C" fn oa_log2(x: f32) -> f32 {
    x.max(1e-30).log2()
}
extern "C" fn oa_tanh(x: f32) -> f32 {
    x.tanh()
}
extern "C" fn oa_db(x: f32) -> f32 {
    10f32.powf(x.min(60.0) / 20.0)
}
extern "C" fn oa_to_db(x: f32) -> f32 {
    20.0 * x.abs().max(1e-10).log10()
}
extern "C" fn oa_round(x: f32) -> f32 {
    x.round()
}
extern "C" fn oa_sign(x: f32) -> f32 {
    if x == 0.0 { 0.0 } else { x.signum() }
}
extern "C" fn oa_atan2(a: f32, b: f32) -> f32 {
    a.atan2(b)
}
extern "C" fn oa_pow(a: f32, b: f32) -> f32 {
    a.powf(b)
}
extern "C" fn oa_rem(a: f32, b: f32) -> f32 {
    if b == 0.0 { 0.0 } else { a.rem_euclid(b) }
}
extern "C" fn oa_smoothstep(a: f32, b: f32, c: f32) -> f32 {
    let t = if b == a { 0.0 } else { ((c - a) / (b - a)).clamp(0.0, 1.0) };
    t * t * (3.0 - 2.0 * t)
}
extern "C" fn oa_noise(seed: *mut u64) -> f32 {
    // SAFETY: `Memory::seed` is valid (`Native::run`'s contract).
    unsafe { dsp::noise(&mut *seed) }
}
extern "C" fn oa_extreme(line: *const Line, samples: f32, max: i32) -> f32 {
    // SAFETY: a line of the table (`Native::run`'s contract).
    unsafe { (*line).extreme(samples, max != 0) }
}
extern "C" fn oa_filter_update(site: *mut f32, shape: i32, k0: f32, k1: f32, k2: f32, rate: f32) {
    // SAFETY: a call site's floats (`Native::run`'s contract).
    let site = unsafe { std::slice::from_raw_parts_mut(site, SITE_FLOATS) };
    dsp::filter_update(site, Shape::ALL[shape as usize], [k0, k1, k2], rate);
}
extern "C" fn oa_host(host: *mut c_void, id: i32, site: i32, args: *const f32, argc: i32) -> f32 {
    if host.is_null() {
        return 0.0;
    }
    // SAFETY: `Memory::host` is a live `*mut &mut dyn Host` (`Native::run`'s contract).
    let host = unsafe { &mut *(host as *mut &mut dyn Host) };
    let args = if argc > 0 { unsafe { std::slice::from_raw_parts(args, argc as usize) } } else { &[] };
    host.call(id as u16, site as u32, args)
}
extern "C" fn oa_floorf(x: f32) -> f32 {
    x.floor()
}
extern "C" fn oa_ceilf(x: f32) -> f32 {
    x.ceil()
}
extern "C" fn oa_truncf(x: f32) -> f32 {
    x.trunc()
}
extern "C" fn oa_nearbyintf(x: f32) -> f32 {
    x.round_ties_even()
}

#[derive(Clone, Copy)]
enum Sig {
    F1,
    F2,
    F3,
    Noise,
    Extreme,
    FilterUpdate,
    Host,
}

const IMPORTS: &[(&str, Sig, *const u8)] = &[
    ("oa_sin", Sig::F1, oa_sin as *const u8),
    ("oa_cos", Sig::F1, oa_cos as *const u8),
    ("oa_tan", Sig::F1, oa_tan as *const u8),
    ("oa_asin", Sig::F1, oa_asin as *const u8),
    ("oa_acos", Sig::F1, oa_acos as *const u8),
    ("oa_atan", Sig::F1, oa_atan as *const u8),
    ("oa_exp", Sig::F1, oa_exp as *const u8),
    ("oa_log", Sig::F1, oa_log as *const u8),
    ("oa_log2", Sig::F1, oa_log2 as *const u8),
    ("oa_tanh", Sig::F1, oa_tanh as *const u8),
    ("oa_db", Sig::F1, oa_db as *const u8),
    ("oa_to_db", Sig::F1, oa_to_db as *const u8),
    ("oa_round", Sig::F1, oa_round as *const u8),
    ("oa_sign", Sig::F1, oa_sign as *const u8),
    ("oa_atan2", Sig::F2, oa_atan2 as *const u8),
    ("oa_pow", Sig::F2, oa_pow as *const u8),
    ("oa_rem", Sig::F2, oa_rem as *const u8),
    ("oa_smoothstep", Sig::F3, oa_smoothstep as *const u8),
    ("oa_noise", Sig::Noise, oa_noise as *const u8),
    ("oa_extreme", Sig::Extreme, oa_extreme as *const u8),
    ("oa_filter_update", Sig::FilterUpdate, oa_filter_update as *const u8),
    ("oa_host", Sig::Host, oa_host as *const u8),
];

// ---- compiling ----

/// Compiles each of `parts` (op lists sharing a register file of `registers`), whose
/// host functions are `functions`, leaving the registers `keep` says in the file. Fails
/// on a machine Cranelift can't target; callers then interpret.
pub fn compile(parts: &[&[Op]], registers: usize, functions: &[Function], keep: Keep) -> Result<Native, String> {
    let keep: Vec<bool> = match keep {
        Keep::All => vec![true; registers],
        Keep::HostAnd(n) => {
            let mut k: Vec<bool> = (0..registers).map(|r| r < n).collect();
            for ops in parts {
                for (k, read) in k.iter_mut().zip(plan(ops, registers).0) {
                    *k |= read;
                }
            }
            k
        }
    };
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
    flags.set("use_colocated_libcalls", "false").map_err(|e| e.to_string())?;
    flags.set("is_pic", if cfg!(target_arch = "x86_64") { "true" } else { "false" }).map_err(|e| e.to_string())?;
    let isa = cranelift_native::builder().map_err(|e| e.to_string())?.finish(settings::Flags::new(flags)).map_err(|e| e.to_string())?;
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    for (name, _, ptr) in IMPORTS {
        builder.symbol(*name, *ptr);
    }
    // Rounding that the CPU may lack, if Cranelift falls back to a library call.
    builder.symbol("floorf", oa_floorf as *const u8);
    builder.symbol("ceilf", oa_ceilf as *const u8);
    builder.symbol("truncf", oa_truncf as *const u8);
    builder.symbol("nearbyintf", oa_nearbyintf as *const u8);
    let mut module = JITModule::new(builder);
    let ptr = module.target_config().pointer_type();

    let mut imports = HashMap::new();
    for (name, sig, _) in IMPORTS {
        let mut s = module.make_signature();
        let f = AbiParam::new(types::F32);
        let (params, ret): (Vec<AbiParam>, Option<AbiParam>) = match sig {
            Sig::F1 => (vec![f], Some(f)),
            Sig::F2 => (vec![f, f], Some(f)),
            Sig::F3 => (vec![f, f, f], Some(f)),
            Sig::Noise => (vec![AbiParam::new(ptr)], Some(f)),
            Sig::Extreme => (vec![AbiParam::new(ptr), f, AbiParam::new(types::I32)], Some(f)),
            Sig::FilterUpdate => (vec![AbiParam::new(ptr), AbiParam::new(types::I32), f, f, f, f], None),
            Sig::Host => (vec![AbiParam::new(ptr), AbiParam::new(types::I32), AbiParam::new(types::I32), AbiParam::new(ptr), AbiParam::new(types::I32)], Some(f)),
        };
        s.params = params;
        s.returns = ret.into_iter().collect();
        let id = module.declare_function(name, Linkage::Import, &s).map_err(|e| e.to_string())?;
        imports.insert(*name, id);
    }

    let mut sig = module.make_signature();
    sig.params = vec![AbiParam::new(ptr), AbiParam::new(ptr), AbiParam::new(types::I32)];
    let by_id: HashMap<u16, Function> = functions.iter().map(|f| (f.id, *f)).collect();
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    let mut ids = Vec::new();
    for (i, ops) in parts.iter().enumerate() {
        let id = module.declare_function(&format!("part{i}"), Linkage::Local, &sig).map_err(|e| e.to_string())?;
        ctx.func.signature = sig.clone();
        ctx.func.name = UserFuncName::user(0, id.as_u32());
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let t = Translator { b, module: &mut module, imports: &imports, refs: HashMap::new(), functions: &by_id, ptr, vars: vec![None; registers], stack: Vec::new(), mem: None };
            t.function(ops, registers, &keep)?;
        }
        module.define_function(id, &mut ctx).map_err(|e| format!("{e:?}"))?;
        module.clear_context(&mut ctx);
        ids.push(id);
    }
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let entries = ids
        .iter()
        // SAFETY: each is a finalized function of exactly this signature.
        .map(|id| unsafe { std::mem::transmute::<*const u8, Entry>(module.get_finalized_function(*id)) })
        .collect();
    Ok(Native { _code: Arc::new(Code(Some(module))), entries, registers })
}

/// Which registers a compiled part must leave in the register file when it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Every one it writes (something may read any of them: a spectrum's other bins).
    All,
    /// The first `n` (the host's inputs and outputs), and any a part reads before writing
    /// (`state`s, and what one part leaves for another). A `let` used only within a run
    /// stays in machine registers.
    HostAnd(usize),
}

/// Which registers `ops` reads before writing (loaded at the start), and which it writes
/// (stored at the end). A write that `OnFirst` may skip still needs the old value on the
/// other path.
fn plan(ops: &[Op], registers: usize) -> (Vec<bool>, Vec<bool>) {
    let (mut load, mut store, mut defined) = (vec![false; registers], vec![false; registers], vec![false; registers]);
    let mut conditional_until = 0;
    for (i, op) in ops.iter().enumerate() {
        match *op {
            Op::OnFirst(n) => conditional_until = conditional_until.max(i + 1 + n as usize),
            Op::Load(r) if !defined[r as usize] => load[r as usize] = true,
            Op::Store(r) => {
                store[r as usize] = true;
                if i >= conditional_until {
                    defined[r as usize] = true;
                } else if !defined[r as usize] {
                    load[r as usize] = true;
                }
            }
            _ => {}
        }
    }
    (load, store)
}

/// Loaded once at the top of a function.
#[derive(Clone, Copy)]
struct Mem {
    first: Value,
    lines: Value,
    sites: Value,
    seed: Value,
    host: Value,
    rate: Value,
}

struct Translator<'a, 'b> {
    b: FunctionBuilder<'b>,
    module: &'a mut JITModule,
    imports: &'a HashMap<&'static str, FuncId>,
    refs: HashMap<&'static str, cranelift_codegen::ir::FuncRef>,
    functions: &'a HashMap<u16, Function>,
    ptr: Type,
    vars: Vec<Option<Variable>>,
    /// The values the ops push, and each one's value when it's a constant.
    stack: Vec<(Value, Option<f32>)>,
    mem: Option<Mem>,
}

impl Translator<'_, '_> {
    fn function(mut self, ops: &[Op], registers: usize, keep: &[bool]) -> Result<(), String> {
        let (load, written) = plan(ops, registers);
        // A value no later run and no other part reads needn't go back to the registers.
        let store: Vec<bool> = written.iter().zip(keep).map(|(w, k)| *w && *k).collect();
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        let params = self.b.block_params(entry).to_vec();
        let (regs, memory, first) = (params[0], params[1], params[2]);
        let flags = MemFlagsData::trusted();
        let ptr = self.ptr;
        let lines = self.b.ins().load(ptr, flags, memory, offset_of!(Memory, lines) as i32);
        let sites = self.b.ins().load(ptr, flags, memory, offset_of!(Memory, sites) as i32);
        let seed = self.b.ins().load(ptr, flags, memory, offset_of!(Memory, seed) as i32);
        let host = self.b.ins().load(ptr, flags, memory, offset_of!(Memory, host) as i32);
        let rate = self.b.ins().load(types::F32, flags, memory, offset_of!(Memory, rate) as i32);
        self.mem = Some(Mem { first, lines, sites, seed, host, rate });
        for r in 0..registers {
            if load[r] || written[r] {
                let var = self.b.declare_var(types::F32);
                self.vars[r] = Some(var);
                if load[r] {
                    let v = self.b.ins().load(types::F32, flags, regs, (r * 4) as i32);
                    self.b.def_var(var, v);
                }
            }
        }
        self.ops(ops)?;
        for (r, s) in store.iter().enumerate() {
            if *s {
                let v = self.b.use_var(self.vars[r].expect("declared"));
                self.b.ins().store(flags, v, regs, (r * 4) as i32);
            }
        }
        self.b.ins().return_(&[]);
        self.b.seal_all_blocks();
        let config = self.module.target_config();
        self.b.finalize(config);
        Ok(())
    }

    fn mem(&self) -> Mem {
        self.mem.expect("set at the top of the function")
    }

    fn import(&mut self, name: &'static str) -> cranelift_codegen::ir::FuncRef {
        if let Some(r) = self.refs.get(name) {
            return *r;
        }
        let r = self.module.declare_func_in_func(self.imports[name], self.b.func);
        self.refs.insert(name, r);
        r
    }

    fn call(&mut self, name: &'static str, args: &[Value]) -> Option<Value> {
        let f = self.import(name);
        let inst = self.b.ins().call(f, args);
        self.b.inst_results(inst).first().copied()
    }

    fn f32(&mut self, x: f32) -> Value {
        self.b.ins().f32const(x)
    }

    fn truth(&mut self, cond: Value) -> Value {
        let (one, zero) = (self.f32(1.0), self.f32(0.0));
        self.b.ins().select(cond, one, zero)
    }

    fn pop(&mut self) -> Result<Value, String> {
        self.stack.pop().map(|v| v.0).ok_or_else(|| "the script's stack ran dry".to_string())
    }

    fn pop_const(&mut self) -> Result<f32, String> {
        self.stack.pop().and_then(|v| v.1).ok_or_else(|| "a line's index isn't a constant".to_string())
    }

    fn push(&mut self, v: Value) {
        self.stack.push((v, None));
    }

    fn ops(&mut self, ops: &[Op]) -> Result<(), String> {
        let mut i = 0;
        while i < ops.len() {
            match ops[i] {
                Op::OnFirst(n) => {
                    let (body, rest) = (self.b.create_block(), self.b.create_block());
                    let first = self.mem().first;
                    self.b.ins().brif(first, body, &[], rest, &[]);
                    self.b.switch_to_block(body);
                    let end = (i + 1 + n as usize).min(ops.len());
                    self.ops(&ops[i + 1..end])?;
                    self.b.ins().jump(rest, &[]);
                    self.b.switch_to_block(rest);
                    i = end;
                    continue;
                }
                op => self.op(op)?,
            }
            i += 1;
        }
        Ok(())
    }

    fn op(&mut self, op: Op) -> Result<(), String> {
        macro_rules! bin {
            ($f:ident) => {{
                let b = self.pop()?;
                let a = self.pop()?;
                let v = self.b.ins().$f(a, b);
                self.push(v);
            }};
        }
        macro_rules! cmp {
            ($cc:expr, $swap:expr) => {{
                let b = self.pop()?;
                let a = self.pop()?;
                let (x, y) = if $swap { (b, a) } else { (a, b) };
                let c = self.b.ins().fcmp($cc, x, y);
                let v = self.truth(c);
                self.push(v);
            }};
        }
        match op {
            Op::Const(x) => {
                let v = self.f32(x);
                self.stack.push((v, Some(x)));
            }
            Op::Load(r) => {
                let var = self.vars[r as usize].ok_or("a register read that was never planned")?;
                let v = self.b.use_var(var);
                self.push(v);
            }
            Op::Store(r) => {
                let v = self.pop()?;
                let var = self.vars[r as usize].ok_or("a register write that was never planned")?;
                self.b.def_var(var, v);
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::Neg => {
                let a = self.pop()?;
                let v = self.b.ins().fneg(a);
                self.push(v);
            }
            Op::Not => {
                let a = self.pop()?;
                let zero = self.f32(0.0);
                let c = self.b.ins().fcmp(FloatCC::Equal, a, zero);
                let v = self.truth(c);
                self.push(v);
            }
            Op::Add => bin!(fadd),
            Op::Sub => bin!(fsub),
            Op::Mul => bin!(fmul),
            Op::Div => {
                let b = self.pop()?;
                let a = self.pop()?;
                let q = self.b.ins().fdiv(a, b);
                let zero = self.f32(0.0);
                let z = self.b.ins().fcmp(FloatCC::Equal, b, zero);
                let v = self.b.ins().select(z, zero, q);
                self.push(v);
            }
            Op::Rem => {
                let b = self.pop()?;
                let a = self.pop()?;
                let v = self.call("oa_rem", &[a, b]).expect("returns");
                self.push(v);
            }
            Op::Pow => {
                let b = self.pop()?;
                let a = self.pop()?;
                let v = self.call("oa_pow", &[a, b]).expect("returns");
                self.push(v);
            }
            Op::Lt => cmp!(FloatCC::LessThan, false),
            Op::Le => cmp!(FloatCC::LessThanOrEqual, false),
            Op::Gt => cmp!(FloatCC::GreaterThan, false),
            Op::Ge => cmp!(FloatCC::GreaterThanOrEqual, false),
            Op::Eq => cmp!(FloatCC::Equal, false),
            Op::Ne => cmp!(FloatCC::NotEqual, false),
            Op::And | Op::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                let zero = self.f32(0.0);
                let ca = self.b.ins().fcmp(FloatCC::NotEqual, a, zero);
                let cb = self.b.ins().fcmp(FloatCC::NotEqual, b, zero);
                let c = if op == Op::And { self.b.ins().band(ca, cb) } else { self.b.ins().bor(ca, cb) };
                let v = self.truth(c);
                self.push(v);
            }
            Op::Math(f) => self.math(f)?,
            Op::Choose(n) => {
                let mut values = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    values.push(self.pop()?);
                }
                values.reverse();
                let index = self.pop()?;
                if values.is_empty() {
                    let v = self.f32(0.0);
                    self.push(v);
                } else {
                    // round(index), kept to the values there are (index ≥ 0 rounds like floor(i + ½)).
                    let half = self.f32(0.5);
                    let shifted = self.b.ins().fadd(index, half);
                    let k = self.b.ins().floor(shifted);
                    let zero = self.f32(0.0);
                    let top = self.f32((values.len() - 1) as f32);
                    let k = self.b.ins().fmax(k, zero);
                    let k = self.b.ins().fmin(k, top);
                    let mut v = values[0];
                    for (j, value) in values.iter().enumerate().skip(1) {
                        let at = self.f32(j as f32);
                        let c = self.b.ins().fcmp(FloatCC::Equal, k, at);
                        v = self.b.ins().select(c, *value, v);
                    }
                    self.push(v);
                }
            }
            Op::Host { id, site, argc } => self.host(id, site, argc)?,
            Op::OnFirst(_) => unreachable!("handled by `ops`"),
        }
        Ok(())
    }

    fn math(&mut self, f: Math) -> Result<(), String> {
        let extern1 = |f: Math| -> Option<&'static str> {
            Some(match f {
                Math::Sin => "oa_sin",
                Math::Cos => "oa_cos",
                Math::Tan => "oa_tan",
                Math::Asin => "oa_asin",
                Math::Acos => "oa_acos",
                Math::Atan => "oa_atan",
                Math::Exp => "oa_exp",
                Math::Log => "oa_log",
                Math::Log2 => "oa_log2",
                Math::Tanh => "oa_tanh",
                Math::Db => "oa_db",
                Math::ToDb => "oa_to_db",
                Math::Round => "oa_round",
                Math::Sign => "oa_sign",
                _ => return None,
            })
        };
        if let Some(name) = extern1(f) {
            let a = self.pop()?;
            let v = self.call(name, &[a]).expect("returns");
            self.push(v);
            return Ok(());
        }
        let v = match f {
            Math::Abs => {
                let a = self.pop()?;
                self.b.ins().fabs(a)
            }
            Math::Floor => {
                let a = self.pop()?;
                self.b.ins().floor(a)
            }
            Math::Ceil => {
                let a = self.pop()?;
                self.b.ins().ceil(a)
            }
            Math::Fract => {
                let a = self.pop()?;
                let fl = self.b.ins().floor(a);
                self.b.ins().fsub(a, fl)
            }
            Math::Sqrt => {
                let a = self.pop()?;
                let zero = self.f32(0.0);
                let a = self.b.ins().fmax(a, zero);
                self.b.ins().sqrt(a)
            }
            Math::Atan2 | Math::Pow => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.call(if f == Math::Atan2 { "oa_atan2" } else { "oa_pow" }, &[a, b]).expect("returns")
            }
            Math::Min | Math::Max => {
                let b = self.pop()?;
                let a = self.pop()?;
                if f == Math::Min { self.b.ins().fmin(a, b) } else { self.b.ins().fmax(a, b) }
            }
            Math::Step => {
                let b = self.pop()?;
                let a = self.pop()?;
                let c = self.b.ins().fcmp(FloatCC::LessThan, b, a);
                let (zero, one) = (self.f32(0.0), self.f32(1.0));
                self.b.ins().select(c, zero, one)
            }
            Math::Clamp | Math::Mix | Math::Smoothstep | Math::Select => {
                let c = self.pop()?;
                let b = self.pop()?;
                let a = self.pop()?;
                match f {
                    Math::Clamp => {
                        let lo = self.b.ins().fmax(a, b);
                        self.b.ins().fmin(lo, c)
                    }
                    Math::Mix => {
                        let d = self.b.ins().fsub(b, a);
                        let m = self.b.ins().fmul(d, c);
                        self.b.ins().fadd(a, m)
                    }
                    Math::Smoothstep => self.call("oa_smoothstep", &[a, b, c]).expect("returns"),
                    _ => {
                        let zero = self.f32(0.0);
                        let cond = self.b.ins().fcmp(FloatCC::NotEqual, a, zero);
                        self.b.ins().select(cond, b, c)
                    }
                }
            }
            _ => unreachable!("handled above"),
        };
        self.push(v);
        Ok(())
    }

    /// The address of line `index`'s entry in the table.
    fn line_entry(&mut self, index: f32) -> Value {
        let lines = self.mem().lines;
        self.b.ins().iadd_imm_s(lines, index as i64 * size_of::<Line>() as i64)
    }

    /// A slot's address: `ptr + i * 4` (`i` an i32 within the line).
    fn slot(&mut self, ptr: Value, i: Value) -> Value {
        let i = self.b.ins().uextend(self.ptr, i);
        let bytes = self.b.ins().imul_imm_s(i, 4);
        self.b.ins().iadd(ptr, bytes)
    }

    fn host(&mut self, id: u16, site: u32, argc: u8) -> Result<(), String> {
        let f = *self.functions.get(&id).ok_or_else(|| format!("host function {id} isn't known"))?;
        let flags = MemFlagsData::trusted();
        let v = match f.intrinsic {
            Intrinsic::Call => {
                let mut args = Vec::with_capacity(argc as usize);
                for _ in 0..argc {
                    args.push(self.pop()?);
                }
                args.reverse();
                let slot = self.b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, (args.len().max(1) * 4) as u32, 2));
                for (k, a) in args.iter().enumerate() {
                    self.b.ins().stack_store(self.ptr, *a, slot, (k * 4) as i32);
                }
                let addr = self.b.ins().stack_addr(self.ptr, slot, 0);
                let host = self.mem().host;
                let id = self.b.ins().iconst(types::I32, id as i64);
                let site = self.b.ins().iconst(types::I32, site as i64);
                let n = self.b.ins().iconst(types::I32, argc as i64);
                self.call("oa_host", &[host, id, site, addr, n]).expect("returns")
            }
            Intrinsic::Noise => {
                let seed = self.mem().seed;
                self.call("oa_noise", &[seed]).expect("returns")
            }
            Intrinsic::Read { line, min_back } => {
                let seconds = self.pop()?;
                let index = match line {
                    Some(l) => l as f32,
                    None => self.pop_const()?,
                };
                let entry = self.line_entry(index);
                let ptr = self.b.ins().load(self.ptr, flags, entry, offset_of!(Line, ptr) as i32);
                let len = self.b.ins().load(types::I32, flags, entry, offset_of!(Line, len) as i32);
                let pos = self.b.ins().load(types::I32, flags, entry, offset_of!(Line, pos) as i32);
                // How far back, in slots: at least `min_back`, at most the line's reach.
                let rate = self.mem().rate;
                let samples = self.b.ins().fmul(seconds, rate);
                let min = self.f32(min_back.max(0.0));
                let d = self.b.ins().fmax(samples, min);
                let lenf = self.b.ins().fcvt_from_uint(types::F32, len);
                let two = self.f32(2.0);
                let reach = self.b.ins().fsub(lenf, two);
                let d = self.b.ins().fmin(d, reach);
                let whole = self.b.ins().floor(d);
                let k = self.b.ins().fsub(d, whole);
                let w = self.b.ins().fcvt_to_sint_sat(types::I32, whole);
                // Wrap round the ring, backwards.
                let wrap = |t: &mut Self, i: Value| {
                    let neg = t.b.ins().icmp_imm_s(IntCC::SignedLessThan, i, 0);
                    let up = t.b.ins().iadd(i, len);
                    t.b.ins().select(neg, up, i)
                };
                let i = self.b.ins().isub(pos, w);
                let i = wrap(self, i);
                let j = self.b.ins().iadd_imm_s(i, -1);
                let j = wrap(self, j);
                let at_i = self.slot(ptr, i);
                let at_j = self.slot(ptr, j);
                let a = self.b.ins().load(types::F32, flags, at_i, 0);
                let b = self.b.ins().load(types::F32, flags, at_j, 0);
                let diff = self.b.ins().fsub(b, a);
                let step = self.b.ins().fmul(diff, k);
                self.b.ins().fadd(a, step)
            }
            Intrinsic::Write => {
                let x = self.pop()?;
                let index = self.pop_const()?;
                let entry = self.line_entry(index);
                let ptr = self.b.ins().load(self.ptr, flags, entry, offset_of!(Line, ptr) as i32);
                let pos = self.b.ins().load(types::I32, flags, entry, offset_of!(Line, pos) as i32);
                let at = self.slot(ptr, pos);
                self.b.ins().store(flags, x, at, 0);
                x
            }
            Intrinsic::Extreme { max } => {
                let seconds = self.pop()?;
                let index = self.pop_const()?;
                let entry = self.line_entry(index);
                let rate = self.mem().rate;
                let samples = self.b.ins().fmul(seconds, rate);
                let max = self.b.ins().iconst(types::I32, max as i64);
                self.call("oa_extreme", &[entry, samples, max]).expect("returns")
            }
            Intrinsic::Filter(shape) => {
                let mut args = Vec::with_capacity(argc as usize);
                for _ in 0..argc {
                    args.push(self.pop()?);
                }
                args.reverse();
                let x = args[0];
                let zero = self.f32(0.0);
                let key = match shape {
                    Shape::Peak => [args[1], args[2], args[3]],
                    _ => [args[1], args[2], zero],
                };
                let sites = self.mem().sites;
                let site = self.b.ins().iadd_imm_s(sites, (site as usize * SITE_FLOATS * 4) as i64);
                // Remade only when its settings change (NaN, for a fresh site, never matches).
                let mut changed = None;
                for (k, v) in key.iter().enumerate() {
                    let old = self.b.ins().load(types::F32, flags, site, (k * 4) as i32);
                    let c = self.b.ins().fcmp(FloatCC::NotEqual, old, *v);
                    changed = Some(match changed {
                        None => c,
                        Some(prev) => self.b.ins().bor(prev, c),
                    });
                }
                let (remake, run) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(changed.expect("three settings"), remake, &[], run, &[]);
                self.b.switch_to_block(remake);
                let shape_index = Shape::ALL.iter().position(|s| *s == shape).expect("a shape") as i64;
                let shape_value = self.b.ins().iconst(types::I32, shape_index);
                let rate = self.mem().rate;
                self.call("oa_filter_update", &[site, shape_value, key[0], key[1], key[2], rate]);
                self.b.ins().jump(run, &[]);
                self.b.switch_to_block(run);
                let c: Vec<Value> = (3..10).map(|k: i32| self.b.ins().load(types::F32, flags, site, k * 4)).collect();
                let (b0, b1, b2, a1, a2, z1, z2) = (c[0], c[1], c[2], c[3], c[4], c[5], c[6]);
                let t = self.b.ins().fmul(b0, x);
                let y = self.b.ins().fadd(t, z1);
                let bx = self.b.ins().fmul(b1, x);
                let ay = self.b.ins().fmul(a1, y);
                let n1 = self.b.ins().fsub(bx, ay);
                let n1 = self.b.ins().fadd(n1, z2);
                let bx2 = self.b.ins().fmul(b2, x);
                let ay2 = self.b.ins().fmul(a2, y);
                let n2 = self.b.ins().fsub(bx2, ay2);
                self.b.ins().store(flags, n1, site, 32);
                self.b.ins().store(flags, n2, site, 36);
                y
            }
        };
        self.push(v);
        Ok(())
    }
}
