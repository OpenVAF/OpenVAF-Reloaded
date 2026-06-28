//! Standalone VerilogA runner (`openvaf-r run <file>`).
//!
//! This is a separate execution lane from the OSDI/DAE compiler: instead of
//! emitting a shared library for a circuit simulator, it lowers a module's
//! imperative `initial`/`final` procedural blocks to MIR and *interprets* them
//! with `mir_interpret`, providing host implementations of the output/control
//! system tasks (`$display`, `$strobe`, `$finish`, `$fatal`, ...).
//!
//! No LLVM, no linking, no simulator is involved.

use std::ffi::c_void;
use std::io::Write;

use anyhow::{bail, Context, Result};
use basedb::diagnostics::ConsoleSink;
use hir::CompilationDB;
use hir_lower::fmt::{DisplayKind, FmtArg, FmtArgKind};
use hir_lower::{CallBackKind, MirBuilder, PlaceKind, RetFlag};
use lasso::{Rodeo, Spur};
use mir::{FuncRef, Value};
use mir_interpret::{Data, Func, Interpreter, InterpreterState};
use paths::AbsPathBuf;
use sim_back::collect_modules;
use typed_index_collections::TiVec;

use crate::Opts;

/// Host context for a single interpreter callback, stored behind the `*mut c_void`
/// the interpreter hands back on every call.
struct CbCtx {
    kind: CbCtxKind,
    /// Borrowed for the whole interpreter run (the `Rodeo` outlives the run).
    literals: *const Rodeo,
}

enum CbCtxKind {
    Print {
        kind: DisplayKind,
        arg_tys: Box<[FmtArg]>,
    },
    SetRetFlag(RetFlag),
    /// A callback the runner does not implement (e.g. simulator-only tasks). It is
    /// ignored at runtime after a one-line warning.
    Unsupported(String),
}

/// Run a module's behaviour and return the process exit code (0 unless
/// `$finish`/`$fatal`/`$stop` requested otherwise).
///
/// Two bodies are executed, in order: the `analog` behaviour (so the idiomatic
/// `analog begin @(initial_step) $strobe(...) end` form prints — `@(initial_step)`
/// is currently lowered unconditionally), then any standalone `initial`/`final`
/// procedural blocks. A `$finish`/`$fatal` in the first stops before the second.
pub fn run(opts: &Opts) -> Result<i32> {
    let input =
        opts.input.canonicalize().with_context(|| format!("failed to resolve {}", opts.input))?;
    let input = AbsPathBuf::assert(input);
    let db = CompilationDB::new_fs(input, &opts.include, &opts.defines, &opts.lints)?;

    let modules = match collect_modules(&db, false, &mut ConsoleSink::new(&db)) {
        Some(modules) => modules,
        // Front-end emitted fatal diagnostics already.
        None => return Ok(1),
    };
    let module = match modules.first() {
        Some(module) => module,
        None => bail!("no module found to run in `{}`", opts.input),
    };

    let mut literals = Rodeo::new();
    let is_output = |_: PlaceKind| false;

    // Lower both behavioural bodies up front (both extend `literals`).
    let (analog_func, analog_intern) =
        MirBuilder::new(&db, module.module, &is_output, &mut std::iter::empty())
            .build(&mut literals);
    let (proc_func, proc_intern) =
        MirBuilder::new(&db, module.module, &is_output, &mut std::iter::empty())
            .with_procedural()
            .build(&mut literals);
    if !analog_intern.absdelay.is_empty() || !proc_intern.absdelay.is_empty() {
        bail!("absdelay requires simulator transient history; openvaf run does not support it")
    }

    // analog behaviour first, then procedural blocks; an early-exit request from the
    // first body skips the second.
    if let Some(code) = interpret_body(&analog_func, &analog_intern, &literals) {
        return Ok(code);
    }
    if let Some(code) = interpret_body(&proc_func, &proc_intern, &literals) {
        return Ok(code);
    }
    Ok(0)
}

/// Interpret one lowered MIR body, wiring its callbacks to host implementations.
/// Returns `Some(code)` if the body requested early termination (`$finish`/`$fatal`),
/// `None` otherwise.
fn interpret_body(
    func: &mir::Function,
    intern: &hir_lower::HirInterner,
    literals: &Rodeo,
) -> Option<i32> {
    // Build the interpreter callback table: one host context per MIR callback.
    let mut ctxs: Vec<Box<CbCtx>> = Vec::with_capacity(intern.callbacks.len());
    let mut calls: TiVec<FuncRef, (Func, *mut c_void)> =
        TiVec::with_capacity(intern.callbacks.len());
    for (_func_ref, kind) in intern.callbacks.iter_enumerated() {
        let cb_kind = match kind {
            CallBackKind::Print { kind, arg_tys } => {
                CbCtxKind::Print { kind: *kind, arg_tys: arg_tys.clone() }
            }
            CallBackKind::SetRetFlag(flag) => CbCtxKind::SetRetFlag(*flag),
            other => CbCtxKind::Unsupported(format!("{other:?}")),
        };
        let mut boxed = Box::new(CbCtx { kind: cb_kind, literals });
        let ptr: *mut c_void = (&mut *boxed as *mut CbCtx).cast();
        ctxs.push(boxed);
        calls.push((host_callback as Func, ptr));
    }

    // Provide an entry value for every MIR parameter. The runner has no circuit, so
    // all inputs (node voltages, temperature, module-variable entry state, ...) default
    // to 0. (Module `parameter` defaults are not yet evaluated here — a v1 limitation.)
    let zero = Data::from(0.0f64);
    let args: TiVec<mir::Param, Data> = std::iter::repeat(zero).take(intern.params.len()).collect();
    let mut interpreter = Interpreter::new(func, calls.as_slice(), args.as_slice());
    interpreter.run();

    // `ctxs` (and the raw pointers in `calls`) stay alive until here.
    let code = interpreter.state.exit_code();
    drop(ctxs);
    code
}

/// The single `fn` dispatched for every interpreter callback; behaviour is selected
/// by the [`CbCtx`] behind `data`.
fn host_callback(state: &mut InterpreterState, args: &[Value], _rets: &[Value], data: *mut c_void) {
    // SAFETY: `data` points at the `CbCtx` we stored for this `FuncRef`, which lives
    // for the whole `run()`; `literals` likewise outlives the interpreter.
    let ctx = unsafe { &*(data as *const CbCtx) };
    let literals = unsafe { &*ctx.literals };

    match &ctx.kind {
        CbCtxKind::Print { kind, arg_tys } => {
            let fmt = literals.resolve(&state.read::<Spur>(args[0]));
            let rendered = render_format(fmt, &args[1..], arg_tys, state, literals);
            match kind {
                // Diagnostic-flavoured tasks go to stderr, the rest to stdout.
                DisplayKind::Error | DisplayKind::Fatal | DisplayKind::Warn => {
                    let _ = write!(std::io::stderr(), "{rendered}");
                }
                _ => {
                    let _ = write!(std::io::stdout(), "{rendered}");
                }
            }
        }
        CbCtxKind::SetRetFlag(flag) => {
            // `$finish`/`$stop` are normal termination; `$fatal` is an error.
            let code = match flag {
                RetFlag::Abort => 1,
                _ => 0,
            };
            state.request_exit(code);
        }
        CbCtxKind::Unsupported(name) => {
            let _ = writeln!(
                std::io::stderr(),
                "openvaf run: system task/function `{name}` is not supported by the runner; ignored"
            );
        }
    }
}

/// Minimal C-`printf`-style renderer. `fmt` has already been normalised to C
/// conversions by `hir_lower::fmt::ins_display`, and `arg_tys`/`args` line up in
/// order with the `%` conversions in `fmt`.
fn render_format(
    fmt: &str,
    args: &[Value],
    arg_tys: &[FmtArg],
    state: &InterpreterState,
    literals: &Rodeo,
) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    let mut ai = 0usize;

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // Consume the conversion specifier: flags/width/precision/length up to the
        // terminating conversion character.
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let mut conv = '\0';
        for cc in chars.by_ref() {
            if cc.is_ascii_alphabetic() {
                conv = cc;
                break;
            }
        }
        // A dangling conversion with no matching argument (e.g. the engineering `%c`
        // suffix produced for `%r`): emit nothing.
        if ai >= arg_tys.len() {
            continue;
        }
        let arg = args[ai];
        let ty = &arg_tys[ai];
        ai += 1;
        out.push_str(&render_arg(conv, ty, arg, state, literals));
    }
    out
}

fn render_arg(
    conv: char,
    ty: &FmtArg,
    arg: Value,
    state: &InterpreterState,
    literals: &Rodeo,
) -> String {
    if ty.kind == FmtArgKind::Binary {
        return format!("{:b}", state.read::<i32>(arg));
    }
    match conv {
        'd' | 'i' => format!("{}", state.read::<i32>(arg)),
        'x' => format!("{:x}", state.read::<i32>(arg)),
        'X' => format!("{:X}", state.read::<i32>(arg)),
        'o' => format!("{:o}", state.read::<i32>(arg)),
        'c' => char::from_u32(state.read::<i32>(arg) as u32).map_or(String::new(), String::from),
        's' => literals.resolve(&state.read::<Spur>(arg)).to_owned(),
        'e' | 'E' => format!("{:e}", state.read::<f64>(arg)),
        'f' | 'F' | 'g' | 'G' => format!("{}", state.read::<f64>(arg)),
        // Best-effort rendering driven by the inferred argument type.
        _ => match ty.ty {
            hir::Type::Integer => format!("{}", state.read::<i32>(arg)),
            hir::Type::String => literals.resolve(&state.read::<Spur>(arg)).to_owned(),
            _ => format!("{}", state.read::<f64>(arg)),
        },
    }
}
