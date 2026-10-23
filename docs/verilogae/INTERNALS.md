# `verilogae` — Verilog-A model evaluation library

**Location:** `verilogae/verilogae/`
**Role:** A Verilog-A compiler and runtime library with a different goal from
`openvaf`: rather than producing a full OSDI compact-model shared library for
circuit simulation, VerilogAE extracts individual model variables as
standalone callable C functions. The intended use case is parameter extraction
(curve fitting) and scripted model evaluation — scenarios where the user wants
to call `Id(Vgs, Vds, T, ...)` directly from Python, C, or MATLAB without
running a full circuit simulator.

The crate is built both as a `[lib]` (for static linking from `verilogae_ffi`)
and as a `[cdylib]` (for dynamic loading), sharing the same source tree.

Cross-links: [verilogae_ffi INTERNALS](../verilogae_ffi/INTERNALS.md) ·
[verilogae_py INTERNALS](../verilogae_py/INTERNALS.md) ·
[openvaf INTERNALS](../openvaf/INTERNALS.md) ·
[mir_autodiff INTERNALS](../mir_autodiff/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate layout

```
verilogae/verilogae/src/
  lib.rs          — export_vfs(), load(), build_local_model(), build_model()
  api.rs          — C ABI: Opts, Slice<T>, FatPtr<T>, VfsEntry, Vfs,
                    verilogae_load, verilogae_export_vfs, verilogae_call_fun_parallel,
                    verilogae_functions / verilogae_real_params / … accessors,
                    ParamFlags, ModelcardInit, VaeFun type aliases
  compiler_db.rs  — CompilationDB construction, ModelInfo, FuncSpec, ParamInfo
  middle.rs       — build_module_mir(), build_param_init_mir(), FuncSpec::slice_mir()
  back.rs         — CodegenCtx, LLVM codegen for model functions and modelcard init
  cache.rs        — content-addressed cache lookup (MD5 of preprocessed tokens)
  opts.rs         — Opts accessor impls; abs_path helper
  main.rs         — (unused standalone binary entry point)
```

---

## What VerilogAE extracts

Given a Verilog-A module, VerilogAE looks for variables marked with the
`(*retrieve*)` attribute:

```verilog-a
real Id;  (* retrieve *)
real Ig;  (* retrieve = Idiode *)   // dependency breaking
```

Each such variable becomes a separate C function in the output library. The
function evaluates that variable given:
- All branch voltages and currents the variable depends on.
- All model and instance parameters.
- Temperature.
- Optional *dependency-breaking* values (described below).

The generated library also exports:
- `init_modelcard` — initialises parameter arrays to default values and sets
  `ParamFlags` (which params are given / have min/max).
- `functions`, `functions.cnt` — list of function names.
- `params.real`, `params.real.cnt` — real parameter names (and `.unit.*`,
  `.desc.*`, `.group.*` variants).
- `params.integer.*`, `params.string.*` — integer and string parameters.
- `opvars`, `opvars.cnt` — operating-point output variables.
- `nodes`, `nodes.cnt` — module port names.
- `module_name` — the Verilog-A module name.

---

## Core types

### `ModelInfo`

```rust
pub struct ModelInfo {
    pub params:            IndexMap<Parameter, ParamInfo>,
    pub functions:         Vec<FuncSpec>,
    pub var_names:         AHashMap<Variable, SmolStr>,
    pub op_vars:           Vec<SmolStr>,
    pub module:            Module,
    pub ports:             Vec<SmolStr>,
    pub optional_currents: AHashMap<Branch, f64>,
    pub optional_voltages: AHashMap<(Node, Option<Node>), f64>,
}
```

`ModelInfo::collect` runs the full frontend (HIR, type check) and then walks
`module.rec_declarations(db)` to find:
- Variables with `(*retrieve*)` → become `FuncSpec` entries.
- Parameters → become `ParamInfo` entries (picking up `units`, `desc`,
  `group` attributes).
- Branches with `(*opt_voltage*)` / `(*opt_current*)` → default branch values
  that the caller can omit (treated as 0 by default, or a specified value if
  the attribute has a literal argument).

### `FuncSpec`

```rust
pub struct FuncSpec {
    pub var:                  Variable,
    pub dependency_breaking:  Box<[Variable]>,
    pub prefix:               String,   // e.g. "fun.0" (base-36 index)
}
```

`dependency_breaking` is the list of variables whose read-back values are
passed in as explicit inputs rather than computed internally. This breaks
dependency cycles in iterative solvers. A variable `v` with
`(*retrieve = w*)` means "when computing `v`, treat `w` as an input rather
than computing it."

`prefix` is `"fun."` followed by the base-36 index of the function. This
prefix namespaces all per-function globals in the output library
(`{prefix}.params.real`, `{prefix}.voltages`, etc.).

### `FatPtr<T>` — the vectorised-call ABI

```rust
#[repr(C)]
pub struct FatPtr<T: Copy> {
    pub ptr:  *mut T,
    pub meta: Meta<T>,
}

#[repr(C)]
pub union Meta<T: Copy> {
    pub stride: u64,
    pub scalar: T,
}
```

When `ptr` is null, `meta.scalar` holds a single scalar value shared by all
evaluation points. When `ptr` is non-null, `meta.stride` is the byte stride
between successive elements, allowing the caller to pass a NumPy array slice
directly (any stride, including 0 for broadcast).

The generated model function signature is:

```rust
pub type VaeFun = Option<extern "C" fn(
    cnt:           usize,
    voltages:      *mut FatPtr<f64>,
    currents:      *mut FatPtr<f64>,
    real_params:   *mut FatPtr<f64>,
    int_params:    *mut FatPtr<i32>,
    str_params:    *mut *const c_char,
    real_dep_break: *mut FatPtr<f64>,
    int_dep_break:  *mut FatPtr<i32>,
    temp:          *mut FatPtr<f64>,
    out:           *mut c_void,
)>;
```

`cnt` is the number of evaluation points. Each `FatPtr` array has one entry
per voltage/current/parameter input. Index `i` in the array corresponds to the
`i`-th input signal for that category.

### `ParamFlags`

```rust
pub type ParamFlags = u8;
pub const PARAM_FLAGS_MIN_INCLUSIVE: ParamFlags = 1;
pub const PARAM_FLAGS_MAX_INCLUSIVE: ParamFlags = 2;
pub const PARAM_FLAGS_INVALID:       ParamFlags = 4;
pub const PARAM_FLAGS_GIVEN:         ParamFlags = 8;
```

`GIVEN` is set after the caller assigns a value. `INVALID` is set when the
value is out of the declared range. `MIN_INCLUSIVE`/`MAX_INCLUSIVE` describe
whether the parameter's declared range bounds are inclusive or exclusive.

---

## Compilation pipeline

### `load(path, full_compile, opts) -> Result<Library>`

```
load
  └── build_local_model
        ├── compiler_db::new(path, opts)     — create Salsa CompilationDB
        ├── cache::lookup(&db, full_compile)  — content-addressed cache check
        │     (returns early if hit)
        └── build_model(db, path, full_compile, local=true, opts, dst)
              ├── ModelInfo::collect(&db)      — run HIR, collect metadata
              ├── LLVMBackend::new(...)
              ├── if full_compile:
              │     build_module_mir(&db, &info)   — build unified MIR
              │     info.intern_model(...)
              │     build_param_init_mir(...)
              │     CodegenCtx::compile_model_info(...)
              │     rayon_core::scope { for each FuncSpec:
              │         FuncSpec::slice_mir(...)     — slice + optimise per-function MIR
              │         CodegenCtx::gen_func_obj(...) — LLVM codegen → .o file
              │     }
              └── else (info-only):
                    build_param_init_mir only
                    compile_model_info only (no function objects)
              └── linker::link(...)            — link .o files → .so/.dylib/.dll
```

`full_compile = false` produces a library that has the modelcard (`init_modelcard`,
parameter metadata, node names) but no callable functions. The Python binding
exposes this as `load_info()`, which is much faster than a full compilation.

### `build_module_mir` — unified MIR for all retrieve variables

`build_module_mir` calls `hir_lower::MirBuilder` once for all retrieve
variables together, producing a single MIR `Function`. This unified function
contains the code for every output. Individual per-function MIRs are then
carved out by `FuncSpec::slice_mir`.

After `MirBuilder`:
1. All callback side-effects are disabled (`has_sideeffects = false`), because
   VerilogAE functions are expected to be pure.
2. `intern.insert_var_init` inserts the Verilog-A `initial` block assignments.
3. `dead_code_elimination` removes outputs not needed by any retrieve variable.
4. `auto_diff` adds derivative computations for branch voltages and currents
   (so that the caller can get Jacobian information alongside the function
   values).
5. `sparse_conditional_constant_propagation`, `inst_combine`, `simplify_cfg`
   clean up.

### `FuncSpec::slice_mir` — per-function MIR slicing

For a single `FuncSpec` (one retrieve variable):
1. Replace `tagged_reads` of dependency-breaking variables with the
   corresponding `ParamKind::HiddenState` inputs (treating those variables as
   external inputs rather than computing them).
2. Run `aggressive_dead_code_elimination` with the retrieve variable's output
   value as the only live root.
3. `simplify_cfg` to remove dead blocks.

The result is a minimal MIR that computes exactly one variable.

### Parallelism

`rayon_core::scope` is used to compile the per-function object files in
parallel. The shared `HirInterner` (read-only after `ensure_names`) is
accessed concurrently via Salsa's `db.snapshot()`.

---

## VFS support

`export_vfs(path, opts) -> Result<Box<[VfsEntry]>>` runs only the
preprocessor and exports the virtual filesystem: a mapping of virtual path →
file contents after `\`include` resolution. The result can be passed back to
`load()` as `opts.vfs`, allowing in-memory compilation without touching the
real filesystem. The Python binding uses this to bundle model files with the
caller's Python package.

---

## C ABI (`api.rs`)

All `verilogae_*` functions are `#[no_mangle] pub unsafe extern "C"` and wrap
their Rust counterparts in `catch_unwind` to convert panics to null/error
returns rather than unwinding across the FFI boundary.

The `expose_ptrs!`, `expose_consts!`, `expose_named_ptrs!`, and
`expose_named_consts!` macros generate the global-accessor functions that
look up symbols in a loaded library. Each accessor:
1. Reconstitutes a `Library` from the raw handle via `Library::from_raw`.
2. Looks up the symbol by name.
3. Calls `std::mem::forget(lib)` before returning so the library is not
   closed.

`verilogae_call_fun_parallel` dispatches `cnt` calls to a `VaeFun` in parallel
using `rayon_core::scope`, passing each call index `i` as the first argument.

---

## Cache (`cache.rs`)

The cache filename uses the same MD5-over-preprocessed-tokens strategy as
`openvaf::cache`. The hash additionally covers the `full_compile` flag
(so full and info-only compilations do not share a cache entry).
The default cache directory is `~/.cache/verilogae` (Linux) /
`%LOCALAPPDATA%\semimod\verilogae\cache` (Windows).

---

## Key design decisions

**Variable-at-a-time extraction instead of full OSDI.** OSDI is designed for
the circuit simulator use case where every variable is evaluated together in a
tight Newton loop. VerilogAE targets the parameter extraction use case where
the user calls individual model equations for specific bias points. Slicing the
MIR per retrieve variable avoids computing unnecessary intermediate quantities.

**`FatPtr` for stride-aware vectorization.** The `ptr == null → scalar` and
`stride` in the `Meta` union lets the caller broadcast scalar parameters
across all evaluation points without copying them into an array. This is the
same convention NumPy uses for broadcasting.

**Dependency breaking.** Compact models have algebraic loops (e.g., a current
that depends on a voltage that depends on that same current). The `retrieve`
attribute's optional argument (`retrieve = otherVar`) allows the caller to
supply the loop-breaking value from outside, enabling the extracted function
to be evaluated without the iterative solver that a full circuit simulator
would use.

**`full_compile = false` for fast metadata loading.** Building only the
modelcard (parameter lists, node names, init values) takes a fraction of the
time of a full LLVM compilation. This lets the Python binding introspect a
model's parameters without waiting for codegen.

**`rayon_core` (not `rayon`).** Using the low-level `rayon_core` API gives
VerilogAE direct control over the thread pool without depending on the
top-level `rayon` crate. The parallel scope is used once per `build_model`
call, with each per-function object file compiled on a separate rayon task.
