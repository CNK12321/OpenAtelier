use super::*;
use oa_params::{Evaluated, ParamId, ParamSchema, Unit, Value};

fn run_once(source: &str, env: Env<'_>, regs_init: &[(usize, f32)]) -> (Frame, Vec<f32>) {
    let (section, frame) = compile(source, env).unwrap_or_else(|e| panic!("{e}"));
    let mut regs = vec![0.0; frame.registers];
    for (r, v) in regs_init {
        regs[*r] = *v;
    }
    let mut stack = Vec::new();
    run(&section.block, &mut regs, &mut stack, &mut NoHost, true);
    run(&section.main, &mut regs, &mut stack, &mut NoHost, true);
    (frame, regs)
}

#[test]
fn math_precedence_and_outputs() {
    let env = Env { reads: &["a"], writes: &["out"], ..Env::default() };
    let (_, regs) = run_once("let g = 2 + 3 * a ^ 2; out = g - -1 + choose(2, 10, 20, 30) + (1 < 2 && !(3 < 2));", env, &[(0, 2.0)]);
    // 2 + 3·4 = 14; + 1 + 30 + 1.
    assert_eq!(regs[1], 46.0);
    let (_, regs) = run_once("out = -7 % 3 + select(0, 1, 5) + choose(9, 1, 2);", env, &[]);
    assert_eq!(regs[1], 2.0 + 5.0 + 2.0, "% is never negative; choose clamps its index");
}

#[test]
fn parameters_by_id_and_by_part() {
    let params = [
        ParamSchema::new("amount", Value::Float(0.5), Unit::None),
        ParamSchema::new("mode", Value::Enum("b".into()), Unit::None).options(&["a", "b", "c"]),
        ParamSchema::new("at", Value::Vec2([3.0, 4.0]), Unit::None),
        ParamSchema::new("tint", Value::Color([0.1, 0.2, 0.3, 1.0]), Unit::None),
        ParamSchema::new("mix", Value::Float(0.25), Unit::None),
    ];
    let env = Env { writes: &["out"], params: &params, ..Env::default() };
    let (section, frame) = compile("out = amount + mode + at_x * at_y + tint_b + mix(0, mix, 1);", env).unwrap();
    let mut regs = vec![0.0; frame.registers];
    let values = Evaluated(vec![(ParamId::new("amount"), Value::Float(2.0))]);
    frame.load_params(&mut regs, &params, &values);
    run(&section.main, &mut regs, &mut Vec::new(), &mut NoHost, true);
    // 2 + 1 (the second option) + 12 + 0.3 + 0.25 (a parameter may share a function's name).
    assert!((regs[0] - 15.55).abs() < 1e-5, "{}", regs[0]);
}

/// `let`s that only read steady things go to the block; ones that read the input, or
/// that are changed later, stay in the per-run code.
#[test]
fn steady_lets_move_to_the_block() {
    let params = [ParamSchema::new("cutoff", Value::Float(1000.0), Unit::None)];
    let env = Env { reads: &["in", "rate"], writes: &["out"], invariant: &["rate"], params: &params, ..Env::default() };
    let (section, _) = compile("let k = exp(-TAU * cutoff / rate); let k2 = k * k; let x = in * k2; let later = 1; later += 1; out = x + later;", env).unwrap();
    let stores_in = |ops: &[Op]| ops.iter().filter(|o| matches!(o, Op::Store(_))).count();
    assert_eq!(stores_in(&section.block), 2, "k and k2: {:?}", section.block);
    assert_eq!(stores_in(&section.main), 4, "x, later (twice) and out");
}

/// `state` starts once; later runs keep its value.
#[test]
fn state_starts_on_the_first_run() {
    let env = Env { writes: &["out"], ..Env::default() };
    let (section, frame) = compile("let seed = 10; state n = seed; n += 1; out = n;", env).unwrap();
    let mut regs = vec![0.0; frame.registers];
    let mut stack = Vec::new();
    for first in [true, false, false] {
        run(&section.block, &mut regs, &mut stack, &mut NoHost, first);
        run(&section.main, &mut regs, &mut stack, &mut NoHost, first);
    }
    assert_eq!(regs[0], 13.0);
}

/// Host functions get their arguments and a site per call; lines and names arrive as
/// indexes; statement calls are allowed only for functions that are statements.
#[test]
fn host_functions_lines_and_names() {
    struct Log(Vec<(u16, u32, Vec<f32>)>);
    impl Host for Log {
        fn call(&mut self, id: u16, site: u32, args: &[f32]) -> f32 {
            self.0.push((id, site, args.to_vec()));
            7.0
        }
    }
    const F: &[Function] = &[
        Function { name: "twice", args: &[Arg::Value], id: 1, pure: true, statement: false, intrinsic: Intrinsic::Call },
        Function { name: "write", args: &[Arg::Line, Arg::Value], id: 2, pure: false, statement: true, intrinsic: Intrinsic::Call },
        Function { name: "at", args: &[Arg::Name, Arg::Value], id: 3, pure: false, statement: false, intrinsic: Intrinsic::Call },
    ];
    let env = Env { reads: &["in"], writes: &["out"], functions: F, line: Some(9), ..Env::default() };
    let (section, frame) = compile("line echo = 0.5; let v = 4; write(echo, in); out = twice(1) + at(v, 2) + twice(2);", env).unwrap();
    assert_eq!((frame.lines, frame.sites), (1, 4));
    let mut regs = vec![0.0; frame.registers];
    regs[0] = 0.25;
    let mut host = Log(Vec::new());
    run(&section.block, &mut regs, &mut Vec::new(), &mut host, true);
    run(&section.main, &mut regs, &mut Vec::new(), &mut host, true);
    let v = frame.register("v").unwrap() as f32;
    assert_eq!(host.0[0], (9, 0, vec![0.5]), "the line allocated first");
    assert_eq!(host.0[1], (2, 0, vec![0.0, 0.25]));
    assert_eq!(host.0[3], (3, 2, vec![v, 2.0]));
    assert_eq!(regs[1], 21.0);
    let err = |s: &str| compile(s, env).expect_err(s);
    assert!(err("twice(1);").contains("gives a value"));
    assert!(err("line l = 1; out = l;").contains("delay line"));
    assert!(err("out = at(1, 2);").contains("name of a value"));
    assert!(err("out = twice(1, 2);").contains("takes 1 value"));
}

#[test]
fn mistakes_name_the_line() {
    let env = Env { reads: &["in"], writes: &["out"], ..Env::default() };
    let err = |s: &str| compile(s, env).expect_err(s);
    assert!(err("out = in *;").contains("line 1"));
    assert!(err("\n\nout = wobble;").starts_with("line 3") && err("\n\nout = wobble;").contains("\"wobble\" isn't defined"));
    assert!(err("in = 1;").contains("can't be changed"));
    assert!(err("out = pow(1);").contains("takes 2 values"));
    assert!(err("let sin = 1;").contains("taken"));
    assert!(err("out = in @ 2;").contains("unexpected"));
    assert!(err("line x = 1;").contains("expected \"=\" after \"line\""), "`line` is only a keyword where lines exist");
    assert!(compile("// just a comment\n# and another", env).is_ok());
}
