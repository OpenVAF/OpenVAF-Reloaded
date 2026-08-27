# OpenVAF — Architecture

OpenVAF is a compiler for Verilog-A compact device models.  It reads `.va`
source files and produces native shared libraries (`.so` on Linux/macOS,
`.dll` on Windows) that expose the [OSDI 0.4](https://opensimulatorinterface.org)
interface.  Circuit simulators such as ngspice and Qucs-S can `dlopen` these
libraries and use the models directly.

---

## Repository layout

```
openvaf/          — all Rust source (read-only reference; do not edit)
  openvaf/        — the CLI entry point and top-level orchestration
  hir*/           — front end: parsing, HIR, type inference
  mir*/           — middle end: SSA IR, optimisation, AD
  sim_back/       — simulation model builder
  osdi/           — OSDI descriptor emission and full codegen driver
  mir_llvm/       — MIR → LLVM IR translation
  linker/         — platform linker invocation
  target/         — target triple and CPU definitions
  basedb/         — root Salsa database
  tokens/ lexer/ preprocessor/ parser/ syntax/ vfs/  — front-end pipeline
  stdx/ arena/ bitset/ …   — utility crates

docs/             — this documentation tree
  ARCHITECTURE.md         — this file
  mir/INTERNALS.md        — MIR SSA representation
  mir_autodiff/INTERNALS.md
  hir_lower/INTERNALS.md
  hir_def/INTERNALS.md
  hir_ty/INTERNALS.md
  hir/INTERNALS.md
  mir_llvm/INTERNALS.md
  sim_back/INTERNALS.md
  osdi/INTERNALS.md

tutorials/        — progressive tutorial series (in progress)
```

---

## Pipeline overview

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Source (.va)                                                                │
└────────────────────────────────┬─────────────────────────────────────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Preprocessing         │  preprocessor, vfs
                    │   macro expansion       │
                    │   `include / `ifdef     │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Lexing & Parsing      │  tokens, lexer, parser, syntax
                    │   lossless rowan CST    │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   HIR Construction      │  basedb, hir_def
                    │   item tree             │
                    │   name resolution       │
                    │   expression bodies     │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Type Inference        │  hir_ty, hir
                    │   builtin signatures    │
                    │   nature/discipline     │
                    │   assignment targets    │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   HIR → MIR Lowering    │  hir_lower, mir_build
                    │   SSA construction      │
                    │   HirInterner wiring    │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   MIR Optimisation      │  mir_opt
                    │   SCCP, GVN, inst-comb  │
                    │   DCE, simplify_cfg     │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Automatic Diff (AD)   │  mir_autodiff
                    │   Jacobian generation   │
                    │   source transformation │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Simulation Backend    │  sim_back
                    │   DAE system assembly   │
                    │   node collapse         │
                    │   init / noise funcs    │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   LLVM IR Codegen       │  mir_llvm, osdi
                    │   MIR → LLVM IR         │
                    │   OSDI descriptor IR    │
                    │   → .o object files     │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │   Linking               │  linker, target
                    │   .o files → .so / .dll │
                    └────────────┬────────────┘
                                 │
                    ┌────────────▼────────────┐
                    │  OSDI 0.4 Shared Library│
                    │  (.so / .dll)           │
                    └─────────────────────────┘
```

---

## Crate map

### Entry points

| Crate | Kind | Role |
|---|---|---|
| `openvaf-driver` | binary | CLI (`openvaf`); parses arguments, calls `openvaf::compile` |
| `openvaf` | lib | Orchestrates the full pipeline: `compile()` and `expand()` |

### Front end — syntax

| Crate | Role |
|---|---|
| `vfs` | Virtual filesystem; maps `FileId` to source text |
| `tokens` | `SyntaxKind` enum and `T!` macro shared by lexer and parser |
| `lexer` | Produces a flat token stream from a `&str` |
| `preprocessor` | Macro expansion, `` `include ``, `` `ifdef ``/`` `ifndef ``; integrates with `vfs` |
| `parser` | Converts token stream to rowan parse events |
| `syntax` | Constructs a lossless rowan CST (`Parse<SourceFile>`); combines parser + preprocessor |

### Front end — semantic analysis / HIR

| Crate | Role | INTERNALS |
|---|---|---|
| `basedb` | Root Salsa database; VFS queries, line indexing, lint storage | — |
| `hir_def` | `ItemTree`, name resolution (`DefMap`), expression/statement body arenas | [hir_def/INTERNALS.md](hir_def/INTERNALS.md) |
| `hir_ty` | Type inference, builtin signatures, nature/discipline/branch resolution | [hir_ty/INTERNALS.md](hir_ty/INTERNALS.md) |
| `hir` | Public OO API over `hir_def` + `hir_ty`; `CompilationDB`; `Body`/`BodyRef` | [hir/INTERNALS.md](hir/INTERNALS.md) |

### Middle end — MIR

| Crate | Role | INTERNALS |
|---|---|---|
| `mir` | SSA-form IR: `Function`, `DataFlowGraph`, `Layout`, `ControlFlowGraph`, dominators | [mir/INTERNALS.md](mir/INTERNALS.md) |
| `mir_build` | Cranelift-style `FunctionBuilder`; constructs MIR from a stream of instructions | — |
| `hir_lower` | Lowers HIR `Body` to MIR via `MirBuilder`; defines `HirInterner`, `ParamKind`, `PlaceKind` | [hir_lower/INTERNALS.md](hir_lower/INTERNALS.md) |
| `mir_opt` | Optimisation passes: SCCP, GVN, inst-combine, DCE, aggressive DCE, simplify-cfg, taint propagation | — |
| `mir_autodiff` | Automatic differentiation by source transformation; adds derivative instructions to an existing `Function` | [mir_autodiff/INTERNALS.md](mir_autodiff/INTERNALS.md) |
| `mir_interpret` | Interpreter for MIR (used in tests) | — |
| `mir_reader` | Text serialisation / deserialisation of MIR (for test fixtures) | — |

### Back end

| Crate | Role | INTERNALS |
|---|---|---|
| `sim_back` | Builds `CompiledModule` containing `DaeSystem`, eval/init/noise `Function`s, node collapse | [sim_back/INTERNALS.md](sim_back/INTERNALS.md) |
| `osdi` | Full codegen driver: calls `sim_back`, translates MIR via `mir_llvm`, emits OSDI 0.4 descriptor IR | [osdi/INTERNALS.md](osdi/INTERNALS.md) |
| `mir_llvm` | Translates MIR `Function`s to LLVM IR via `llvm-sys`; manages lifetime hierarchy `LLVMBackend` → `ModuleLlvm` → `CodegenCx` → `Builder` | [mir_llvm/INTERNALS.md](mir_llvm/INTERNALS.md) |
| `target` | Target triple, CPU, and platform definitions |  — |
| `linker` | Invokes MSVC / GCC / Clang linker to produce `.dll`/`.so` | — |

### Utility crates

`stdx`, `arena`, `bitset`, `bforest`, `list_pool`, `typed_indexmap`,
`workqueue`, `paths`, `base_n`, `mini_harness`, `sourcegen`, `xtask`.

---

## Stage-by-stage walkthrough

### 1. Source → CST

The front-end pipeline is: VFS read → preprocessor → lexer → parser → `syntax`.

The `preprocessor` expands `` `define ``/`` `ifdef ``/`` `include `` and integrates
with the `Vfs` to follow `include` chains across file boundaries.  It produces
a flat, macro-expanded token stream with source spans that map back to the
original files via a `SourceMap`.

The `parser` consumes this stream and emits rowan parse events.  The `syntax`
crate turns those events into a lossless concrete syntax tree
(`Parse<SourceFile>`).  "Lossless" means all whitespace and comments are
preserved; every byte of the original source has a node in the tree.  This
matters because `basedb` uses the tree for span-accurate error reporting.

The CST is the last representation that understands Verilog-A surface syntax.
Everything below operates on derived, computed views.

### 2. CST → item tree and name resolution (`hir_def`)

→ **Details:** [hir_def/INTERNALS.md](hir_def/INTERNALS.md)

`hir_def` builds two key structures from the CST:

**`ItemTree`** is a stripped, body-free summary of all top-level declarations.
It is the invalidation firewall: if only a function body changes, the
`ItemTree` is unchanged, and no query that depends on names or structure needs
to re-run.

**`DefMap`** is a tree of `Scope` nodes, one per lexical scope
(file root, module, named block, function).  Each scope holds an
`IndexMap<Name, ScopeDefItem>` of its declarations.  The `DefCollector` builds
this by walking the `ItemTree`.

**`Body`** holds `Arena<Expr>` and `Arena<Stmt>` for every definition with a
body (`analog` block, `analog initial` block, function, parameter default,
variable initialiser, nature attribute).  Paths are recorded as unresolved
strings at this stage.

All these structures are Salsa queries; re-running them is demand-driven and
incremental.

### 3. Type inference and discipline resolution (`hir_ty`)

→ **Details:** [hir_ty/INTERNALS.md](hir_ty/INTERNALS.md)

`hir_ty` produces an `InferenceResult` for every `DefWithBodyId`.  The five
maps it contains are the most important outputs of the front end:

| Map | Keys | Values |
|---|---|---|
| `expr_types` | `ExprId` | `Ty` (extended type; includes `Node`, `Branch`, `Param`, …) |
| `resolved_calls` | `ExprId` | `ResolvedFun` (builtin vs user vs param-sys-fun) |
| `resolved_signatures` | `ExprId` | `Signature` (which overload of a builtin) |
| `assignment_destination` | `StmtId` | `AssignDst` (variable, function arg, flow/potential contribution) |
| `casts` | `ExprId` | `Type` (required implicit cast target) |

Additionally, `hir_ty` resolves nature/discipline hierarchies (`NatureTy`,
`DisciplineTy`, `BranchTy`) and alias-parameter chains.

### 4. Public HIR API (`hir`)

→ **Details:** [hir/INTERNALS.md](hir/INTERNALS.md)

`hir` is the **only layer that `hir_lower`, `sim_back`, `osdi`, and external
tooling import**.  It presents:

- A single concrete Salsa database (`CompilationDB`) that bundles all four
  query groups.
- OO newtypes (`Module`, `Node`, `Branch`, `Variable`, `Parameter`, …) that
  wrap Salsa intern IDs and expose methods taking only `&CompilationDB`.
- `Body` + `BodyRef`: a combined view that joins `hir_def::Body` with
  `InferenceResult`.  `get_stmt()` translates raw `hir_def::Stmt +
  AssignDst` into the public `Stmt` enum, disambiguating variable assignments
  from contribution statements.  `get_expr()` resolves paths and call targets
  into the public `Expr` enum.
- `RecDeclarations`: a depth-first iterator over all declarations in a scope
  hierarchy.

### 5. HIR → MIR lowering (`hir_lower`)

→ **Details:** [hir_lower/INTERNALS.md](hir_lower/INTERNALS.md)

The entry point is `MirBuilder::build()`, which uses a Cranelift-style
`FunctionBuilder` from `mir_build` to construct an SSA `mir::Function` by
walking the HIR `BodyRef`.

The `HirInterner` is the critical bridge between the HIR and MIR worlds.
The MIR has no knowledge of Verilog-A concepts; it only knows `Value`,
`Param`, and `FuncRef`.  `HirInterner` provides the mapping:

```rust
pub struct HirInterner {
    pub outputs:            IndexMap<PlaceKind, PackedOption<Value>>,
    pub params:             TiMap<Param, ParamKind, Value>,
    pub callbacks:          TiSet<FuncRef, CallBackKind>,
    pub implicit_equations: TiVec<ImplicitEquation, ImplicitEquationKind>,
    pub lim_state:          TiMap<LimitState, Value, Vec<(Value, bool)>>,
    // …
}
```

`ParamKind` names every possible MIR input: model parameters
(`Param(Parameter)`), simulation state (`Voltage`, `Current`, `Temperature`,
`Abstime`), convergence aids (`EnableLim`, `PrevState`, `HiddenState`), and
OSDI protocol flags (`ParamGiven`, `PortConnected`).

`PlaceKind` names every possible MIR output: variable writes (`Var`),
branch contributions (`Contribute { dst, reactive, voltage_src }`), implicit
residuals, and parameter bounds.

`HirInterner` is carried alongside the `Function` through all subsequent
passes so that `sim_back` and `osdi` can interpret MIR `Param`/`Value`
indices in terms of HIR entities.

### 6. MIR optimisation (`mir_opt`)

MIR passes operate on `mir::Function` in isolation — no HIR, no database.
The following passes are exported from `mir_opt`:

| Pass | What it does |
|---|---|
| `sparse_conditional_constant_propagation` | SCCP: folds constants and eliminates unreachable branches |
| `global_value_numbering` (GVN) | Eliminates redundant computations by assigning value classes |
| `inst_combine` | Peephole rewrites (e.g. `a * 1.0 → a`, double-negation) |
| `dead_code_elimination` | Removes instructions whose results are unused |
| `aggressive_dead_code_elimination` | Removes instructions whose results are only used by other dead instructions |
| `simplify_cfg` / `simplify_cfg_no_phi_merge` | Merges trivially empty blocks, removes unreachable blocks |
| `propagate_taint` / `propagate_direct_taint` | Marks values that depend on operating-point inputs (used by AD to decide what to differentiate) |

Passes run both before AD (to simplify the function before differentiation)
and after AD (to clean up the generated derivative code).

### 7. Automatic differentiation (`mir_autodiff`)

→ **Details:** [mir_autodiff/INTERNALS.md](mir_autodiff/INTERNALS.md)

AD is performed by **source transformation directly on MIR**, not on LLVM IR.
The entry point is `auto_diff(func, dom_tree, derivatives, extra_derivatives)`.

This placement in the pipeline means:
- Derivatives are computed before final optimisation, so `mir_opt` can
  simplify the generated Jacobian code.
- The derivative instructions share the same SSA representation as
  forward-mode instructions, so all MIR passes apply uniformly.
- No LLVM-level differentiation infrastructure is needed.

`HirInterner::unknowns()` computes the `KnownDerivatives` structure that tells
AD which MIR `Value`s correspond to voltage/current unknowns (and thus need
Jacobian columns) and which `FuncRef`s are `ddx` calls.

### 8. Simulation model construction (`sim_back`)

→ **Details:** [sim_back/INTERNALS.md](sim_back/INTERNALS.md)

`sim_back::collect_modules(db, all_vars_opvars, sink)` runs the full front end
for all modules and returns `Vec<ModuleInfo>`.  For each module, the `osdi`
driver then calls `sim_back` to build a `CompiledModule`:

```rust
pub struct CompiledModule<'a> {
    pub info:               &'a ModuleInfo,
    pub dae_system:         DaeSystem,
    pub eval:               Function,      // main evaluation kernel (with HirInterner)
    pub intern:             HirInterner,
    pub init:               Initialization, // instance setup kernel
    pub model_param_setup:  Function,      // model-level parameter setup
    pub model_param_intern: HirInterner,
    pub node_collapse:      NodeCollapse,
}
```

`DaeSystem` describes the differential-algebraic equation system: the
contribution residuals, Jacobian sparsity, collapsed nodes, noise sources, and
which branches are voltage sources vs current sources.

`NodeCollapse` records which node pairs can be collapsed to zero voltage
difference (a simulator optimisation that reduces matrix size).

`Initialization` holds the instance-level setup kernel and its cache-slot
metadata (values computed once at instance creation and cached for use in
subsequent evaluations).

### 9. LLVM IR codegen (`mir_llvm`, `osdi`)

→ **Details:** [mir_llvm/INTERNALS.md](mir_llvm/INTERNALS.md) | [osdi/INTERNALS.md](osdi/INTERNALS.md)

`osdi::compile()` is the driver.  For each `CompiledModule` it:

1. Calls `mir_llvm` to translate each MIR `Function` to LLVM IR via
   `CodegenCx::build_func()`.  The four-layer ownership hierarchy is:
   `LLVMBackend` (target spec, `'t`) → `ModuleLlvm` (LLVM module, `'ll`) →
   `CodegenCx` (type cache, `'cx`) → `Builder` (instruction emitter, `'a`).

2. Emits the OSDI 0.4 descriptor as LLVM globals directly: `OSDI_DESCRIPTORS`
   (array of `osdi_descriptor` structs) and `OSDI_DESCRIPTOR_SIZE` (`u32`).

3. Runs LLVM's optimisation pipeline (`LLVMRunPasses`) at the requested
   optimisation level (O0–O3).

4. Emits a native object file (`.o`) per module via `LLVMTargetMachineEmitToFile`.

### 10. Linking

`linker::link()` invokes the platform linker (MSVC `link.exe`, GCC, or Clang)
to combine the `.o` files into a single `.so`/`.dll`.  The intermediate `.o`
files are deleted after a successful link.

---

## Incremental computation with Salsa

OpenVAF uses [Salsa](https://github.com/salsa-rs/salsa) for incremental,
demand-driven computation across the entire front end.  `CompilationDB`
implements four Salsa query groups that layer on top of each other:

```
BaseDatabase       — VFS, preprocessing, parsing, AstIdMap, lint registry
    ↑
InternDatabase     — intern_*/lookup_intern_* for all Salsa IDs
    ↑
HirDefDatabase     — item_tree, def_map, body, *_data queries
    ↑
HirTyDatabase      — inference_result, discipline_info, branch_info, resolve_alias, …
```

Each query memoises its result.  When a source file changes, Salsa invalidates
only the queries whose inputs changed.  Because `ItemTree` strips expression
bodies, a change inside a function body does not invalidate the `def_map`
query (which depends on the `ItemTree`, not on bodies).  Similarly, a change
to a parameter default value does not retrigger type inference for the module's
analog block.

The MIR and everything downstream (`mir_opt`, `mir_autodiff`, `sim_back`,
`osdi`, `mir_llvm`, `linker`) are **not Salsa queries**.  They run in a
single eagerly-executed pass, driven by `openvaf::compile()`.  The Salsa
boundary is at `hir`: once a `CompiledModule` is built, all subsequent work
is pure computation.

---

## Key data structures at stage boundaries

| Boundary | Producer | Type | Consumer |
|---|---|---|---|
| Source text | VFS | `FileId` + byte content | `preprocessor` |
| Token stream | `preprocessor` | `TokenStream` + `SourceMap` | `parser` |
| CST | `syntax` | `Parse<SourceFile>` | `hir_def` |
| Item tree | `hir_def` | `ItemTree` | `hir_ty`, `hir` |
| Name resolution | `hir_def` | `DefMap` | `hir_ty`, `hir` |
| Expression bodies | `hir_def` | `Body` + `BodySourceMap` | `hir_ty`, `hir` |
| Type inference | `hir_ty` | `InferenceResult` | `hir`, `hir_lower` |
| HIR (public API) | `hir` | `CompilationDB`, `Body`, `BodyRef` | `hir_lower`, `sim_back` |
| MIR + interner | `hir_lower` | `mir::Function` + `HirInterner` | `mir_opt`, `mir_autodiff`, `sim_back` |
| Optimised MIR | `mir_opt` | `mir::Function` | `mir_autodiff`, `sim_back` |
| Differentiated MIR | `mir_autodiff` | `mir::Function` (with derivative instrs) | `sim_back` |
| Simulation model | `sim_back` | `CompiledModule` (`DaeSystem`, eval/init `Function`s) | `osdi` |
| LLVM IR | `mir_llvm` | LLVM `Module` (via `llvm-sys`) | `osdi` (descriptor emission) |
| Object files | `osdi` | `.o` file paths | `linker` |
| Shared library | `linker` | `.so` / `.dll` with `OSDI_DESCRIPTORS` | simulator |

---

## The `compile()` entry point

`openvaf::compile(opts: &Opts)` in `openvaf/openvaf/src/lib.rs` orchestrates
the entire pipeline.  Its `Opts` struct:

```rust
pub struct Opts {
    pub dry_run:         bool,
    pub defines:         Vec<String>,       // preprocessor defines
    pub codegen_opts:    Vec<String>,       // passed to LLVMBackend
    pub lints:           Vec<(String, LintLevel)>,
    pub input:           Utf8PathBuf,
    pub output:          CompilationDestination,
    pub include:         Vec<AbsPathBuf>,
    pub opt_lvl:         LLVMCodeGenOptLevel,
    pub target:          Target,
    pub target_cpu:      String,
    pub dump_mir:        bool,
    pub dump_unopt_mir:  bool,
    pub dump_ir:         bool,
    pub dump_unopt_ir:   bool,
}
```

`CompilationDestination` is either `Path { lib_file }` (explicit output path)
or `Cache { cache_dir }` (content-addressable cache: if the library already
exists under the cache key, compilation is skipped entirely).

### Execution phases

**Phase 1 — front end (Salsa).**
`CompilationDB::new_fs(input, include, defines, lints)` constructs the
database.  `collect_modules(db, false, sink)` runs the entire Salsa front
end — preprocessing, parsing, HIR construction, type inference — and collects
all module declarations into `Vec<ModuleInfo>`.  If any fatal diagnostic is
emitted, compilation terminates with `FatalDiagnostic`.

**Phase 2 — backend (eager).**
`LLVMBackend::new(codegen_opts, target, target_cpu, &[])` initialises the
LLVM target.  `osdi::compile(db, modules, lib_file, target, back, true,
opt_lvl, dump_mir, dump_unopt_mir, dump_ir, dump_unopt_ir)` drives `sim_back`
→ `mir_opt` → `mir_autodiff` → `mir_llvm` for each module and returns
`(object_file_paths, compiled_modules, literals)`.

**Phase 3 — linking.**
`linker::link(None, target, lib_file, |linker| { linker.add_object(path) })`
invokes the platform linker.  Intermediate `.o` files are deleted on success.

### Debugging flags

| Flag | Effect |
|---|---|
| `--dump-mir` | Prints optimised MIR for each module to stdout |
| `--dump-unopt-mir` | Prints unoptimised MIR (before `mir_opt` passes) |
| `--dump-ir` | Prints optimised LLVM IR to a file |
| `--dump-unopt-ir` | Prints unoptimised LLVM IR (before `LLVMRunPasses`) |
| `--dry-run` | Stops after `collect_modules`; does not invoke `osdi::compile` |

---

## OSDI 0.4 output

The final artifact is a native shared library.  The `osdi` crate emits two
LLVM globals before handing off to the linker:

**`OSDI_DESCRIPTORS`** — an array of OSDI 0.4 descriptor structs, one per
Verilog-A `module`.  Each descriptor is typed as the LLVM struct
`OsdiTys.osdi_descriptor`, built from the per-target stdlib bitcode embedded
at build time in `osdi/build.rs`.  Each descriptor encodes:

- Model name and version string.
- Parameter and instance-variable memory layout (byte offsets, OSDI types,
  "given" flag bit positions).
- Jacobian sparsity pattern and column/row offset tables.
- Node-collapse pairs.
- Function pointers for the evaluation kernels: model setup, instance setup,
  DC evaluation, AC evaluation, noise evaluation, temperature update, and
  limit-state handling.

**`OSDI_DESCRIPTOR_SIZE`** — a `u32` constant holding the byte size of a
single descriptor.  Simulators that support both OSDI 0.3 and 0.4 use this to
stride through the array.

Simulators `dlopen` the library, locate `OSDI_DESCRIPTORS` and
`OSDI_DESCRIPTOR_SIZE`, and iterate over the descriptors to register each
model with the internal device catalogue.
