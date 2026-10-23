# `melange-core` — Embedded circuit simulator

**Location:** `melange/core/`
**Role:** An analog circuit simulator library that is layered on top of the
OpenVAF compiler. It provides a typed Rust API for building netlists, loading
Verilog-A compact models (by invoking `openvaf::compile` at runtime), and
running DC and AC operating-point analyses. `melange-core` is **not** part of
the compiler pipeline — it is a downstream consumer of the compiled `.osdi`
output.

Cross-links: [openvaf INTERNALS](../openvaf/INTERNALS.md) ·
[osdi INTERNALS](../osdi/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate layout

```
melange/core/src/
  lib.rs          — public re-exports (Circuit, CircuitDescription, Arena, Expr, Value)
  circuit.rs      — Circuit, Node, DeviceId, ModelId, InstanceId, CircuitModel, CircuitInstance
  elaboration.rs  — CircuitDescription, CircuitInstanceDescription, CircuitModelDescription
  devices.rs      — DeviceImpl / ModelImpl / InstanceImpl traits; DeviceInfo; default_devices()
  devices/
    params.rs     — DeviceParams, ParamId, Type
    resistor.rs   — built-in Resistor device
    vsource.rs    — built-in VoltageSrc device
  expr.rs         — Expr, Value, Arena, ExprEvalCtx, CircuitParam, ExprPtr
  simulation.rs   — Simulation, SimConfig, SimBuilder, SimInfo; DC/AC solvers
  simulation/
    flags.rs      — EvalFlags, SimulationState (bitflags)
    matrix.rs     — MatrixBuilder, SimulationMatrix, MatrixEntryIter (KLU wrapper)
  veriloga.rs     — compile_va(), load_osdi_lib(), Opts; OSDI log hook
  veriloga/
    osdi_0_4.rs   — auto-generated OSDI v0.3/0.4 C-struct bindings
    osdi_device.rs — OsdiDevice: DeviceImpl wrapping an OsdiDescriptor
  utils.rs        — PrettyPrint helper for table output
  tests.rs        — integration tests (currently disabled on Windows)
```

---

## Crate relationships

```
melange-core
  ├─► openvaf          (compile_va calls openvaf::compile)
  ├─► klu-rs           (KLU sparse direct solver for Newton iteration)
  ├─► num-complex      (Complex64 for AC analysis)
  ├─► libloading       (dlopen the compiled .osdi shared library)
  ├─► lasso            (string interning in Arena / Expr)
  ├─► typed_indexmap   (TiMap / TiSet in Circuit internals)
  └─► typed-index-collections  (TiVec / TiSlice throughout)
```

The library has **no binary**. Its consumers build a `Circuit`, populate it
with device instances and parameters, construct a `Simulation`, and call
`dc_op()` or `ac()`.

---

## `Circuit` — the central netlist data structure

```rust
pub struct Circuit {
    pub name: String,
    nodes:     TiSet<Node, String>,
    devices:   TiMap<DeviceId, &'static str, DeviceInfo>,
    models:    TiVec<ModelId, CircuitModel>,
    instances: TiVec<InstanceId, CircuitInstance>,
    namespace: AHashMap<String, NameSpaceEntry>,
    param_assignments: IndexMap<CircuitParam, Expr, ahash::RandomState>,
}
```

`Circuit` is always in a valid, simulation-ready state. There is no
separate "validation" step — every mutation method (`new_model`,
`new_model_instance`, `set_instance_param`, …) checks correctness
immediately and returns `Result`.

### Named entities and the namespace

Every top-level named item (nodes, devices, models, instances) is registered
in `namespace: AHashMap<String, NameSpaceEntry>` where `NameSpaceEntry` is
one of `Device(DeviceId)`, `Model(ModelId)`, or `Instance(InstanceId)`.
The namespace enforces uniqueness: inserting a name that already exists
panics with `unreachable!` (the assertion is deliberate — the API is designed
so duplicates cannot arise if used correctly).

### Index types

| Type | `u32` newtype | What it indexes |
|------|---------------|----------------|
| `Node` | `Node(u32)` | External circuit node (0 = ground) |
| `DeviceId` | `DeviceId(u32)` | Registered device type |
| `ModelId` | `ModelId(u32)` | Circuit model (explicit or implicit) |
| `InstanceId` | `InstanceId(u32)` | Circuit instance |

All four implement `impl_idx_from!(T(u32))` from `stdx` so they can serve as
`TiVec`/`TiSlice` indices.

`Node(0)` is the ground node, always created first by `Circuit::new`. Its
`matrix_idx()` is `-1` (excluded from the Jacobian matrix — ground has no
unknown potential).

### Model vs. instance

A *model* (`CircuitModel`) is a collection of model parameters bound to a
device type. An *instance* (`CircuitInstance`) is a concrete instantiation of
a model, with its own instance parameters and terminal connections.

When a user creates an instance directly with `new_device_instance`, an
*implicit* model is created automatically (its `src` field is
`CircuitModelSrc::Implicit(instance_id)`). When a model is created explicitly
with `new_model`, instances can share it.

### Built-in devices

`default_devices()` returns two built-in `DeviceImpl` implementations:

| Device name | Terminals | Instance param |
|-------------|-----------|---------------|
| `"resistor"` | `A`, `C` | `r` (resistance in Ω) |
| `"vsource"` | ... | voltage, etc. |

Both are registered in every new `Circuit` during `Circuit::new`.

### Loading Verilog-A devices

```rust
pub fn load_veriloga_file(&mut self, path: Utf8PathBuf, opts: &veriloga::Opts) -> Result<Vec<DeviceId>>
```

This method calls `veriloga::compile_va` (which invokes `openvaf::compile`),
then `libloading` to `dlopen` the resulting `.osdi` shared library, and wraps
each `OsdiDescriptor` entry point as an `OsdiDevice` (implementing
`DeviceImpl`). The new devices are registered in the circuit and returned.

If a device with the same name is already registered, a warning is emitted
and the new device is silently ignored.

---

## Device trait hierarchy

### `DeviceImpl` — type-level device factory

```rust
pub trait DeviceImpl {
    fn get_name(&self) -> &'static str;
    fn get_terminals(&self) -> Box<[&'static str]>;
    fn get_params(&self) -> DeviceParams;
    fn new_model(&self) -> Rc<dyn ModelImpl>;
}
```

One `DeviceImpl` object exists per device *type* (e.g., one `Resistor`
singleton). It creates `Rc<dyn ModelImpl>` instances when models are
instantiated.

### `ModelImpl` — per-model state

```rust
pub trait ModelImpl {
    fn process_params(&self) -> Result<()>;
    fn set_real_param(&self, param: ParamId, val: f64);
    fn set_int_param(&self, param: ParamId, val: i32) { unreachable!(...) }
    fn set_str_param(&self, param: ParamId, val: &str) { unreachable!(...) }
    fn new_instance(self: Rc<Self>) -> Box<dyn InstanceImpl>;
}
```

`ModelImpl` uses `Rc<Self>` receiver for `new_instance` so that multiple
instances can share the same model data via `Rc::clone`. Parameters are
stored in `Cell<…>` fields to allow interior mutability through the shared
`Rc`.

### `InstanceImpl` — per-instance simulation state

```rust
pub trait InstanceImpl {
    fn process_params(&mut self, temp: f64, sim_builder: &mut SimBuilder, terminals: &[Node]) -> Result<()>;
    fn populate_matrix_ptrs(&mut self, matrix_entries: MatrixEntryIter);
    fn eval(&mut self, sim_info: SimInfo<'_>) -> Result<()>;
    unsafe fn load_matrix_resist(&self);
    unsafe fn load_matrix_react(&self, alpha: f64);
    fn load_residual_resist(&self, prev_solve: &TiSlice<Node, f64>, rhs: &mut TiSlice<Node, f64>);
    fn load_residual_react(&self, prev_solve: &TiSlice<Node, f64>, rhs: &mut TiSlice<Node, f64>);
    // … AC and lead-current variants with default no-op implementations
}
```

`load_matrix_resist` and `load_matrix_react` are `unsafe` because they write
directly through raw `NonNull<Cell<f64>>` pointers that were set up during
`populate_matrix_ptrs`. This avoids hash-map or array lookups on every
Newton iteration.

### `MatrixEntry` and pointer-based matrix writes

During `prepare_solver`, `MatrixEntryIter` yields one `MatrixEntry` per
`ensure_matrix_entry` call registered by the device. Each entry contains:

```rust
pub struct MatrixEntry<'a> {
    pub(crate) resist: &'a Cell<f64>,   // real Jacobian slot
    pub(crate) react:  &'a Cell<f64>,   // imaginary part of AC Jacobian
}
```

`populate_matrix_ptrs` stores raw `NonNull<Cell<f64>>` pointers into the
KLU matrix's backing storage. On every subsequent Newton iteration,
`load_matrix_resist` uses those pointers directly — the KLU matrix's memory
is pinned once it is built.

---

## Expression system (`expr.rs`)

Circuit parameter values and instance parameter expressions are represented
as `Expr`, a sum type:

```rust
pub enum Expr {
    Eval(ExprPtr),   // pointer into the Arena
    Value(Value),    // constant: already evaluated
}

pub enum Value {
    Num(f64),
    Str(Spur),  // interned string handle
    UNDEF,
}
```

`Expr::Value` is the common case (most parameters are numeric literals).
`Expr::Eval` stores a `u32` index into the `Arena`'s expression heap, used
for parameter-dependent expressions (e.g., `r = 2 * R_global`).

### `Arena` — expression allocator and parameter registry

```rust
pub struct Arena {
    exprs:  Vec<ExprData>,                                   // expression nodes
    params: TiVec<CircuitParamCtx, TiMap<LocalCircuitParam, String, ParamInfo>>,
    intern: Rodeo,                                           // string interner
}
```

`Arena` is shared across circuits and simulations. Each `Circuit` owns a
`CircuitParamCtx` (an index into `arena.params`), which scopes its
parameters so that different circuits in the same session do not share
parameter namespaces.

`CircuitParam::TEMPERATURE` is a built-in parameter automatically created at
`CircuitParamCtx::ROOT` (index 0) with name `"temp"`.

### `ExprEvalCtx` — runtime evaluation context

`ExprEvalCtx` holds the current parameter values as a flat `Box<[Value]>`,
with per-context offsets stored in `ctx_offsets: Box<TiSlice<CircuitParamCtx, u32>>`.
A `CircuitParam{ctx, param}` is looked up as:

```rust
let off = ctx_offsets[param.ctx] + u32::from(param.param);
params[off as usize]
```

`ExprEvalCtxRef<'_>` is a borrow of `ExprEvalCtx` that is passed through
recursive `eval` calls. It has a `borrow()` method that returns a
shorter-lived `ExprEvalCtxRef` to work around the borrow checker in
recursive contexts.

### Constant folding in expression builders

The `Expr::add`, `Expr::mul`, `Expr::inv`, and `Expr::neg` methods do
partial constant folding at construction time:

- `Expr::Value + Expr::Value` → immediately computes the result.
- `Expr::Eval(lhs) + Expr::Value(rhs)` where `lhs` is already a `Commutative { Add, … }` → folds the constant into the existing node in-place, mutating `arena.exprs[lhs.0]` without allocating a new node.

This keeps the expression tree compact for the common case of scaled
parameters.

---

## `CircuitDescription` — netlist-format-independent elaboration

`CircuitDescription` is an intermediate representation intended for netlist
parsers. It stores instances and models by *name* (strings), deferring all
name resolution to `elaborate()`:

```rust
pub struct CircuitDescription {
    pub name:      String,
    pub instances: TiVec<InstanceId, CircuitInstanceDescription>,
    pub models:    TiVec<ModelId, CircuitModelDescription>,
    pub va_files:  Vec<Utf8PathBuf>,
    pub earena:    Arena,
}
```

`CircuitDescription::elaborate(earena, opts)` performs:
1. Compile and register each Verilog-A file in `va_files`.
2. For each model description, resolve the device name → `DeviceId` and call
   `Circuit::new_model`.
3. For each instance description, look up its `master` field in the namespace
   (model, device, or subcircuit) and dispatch to `new_model_instance` or
   `new_device_instance`, wiring terminal names to `Node`s via `circuit.node()`.

---

## `Simulation` — Newton-Raphson solver

```rust
pub struct Simulation<'a> {
    circ:          &'a Circuit,
    model_data:    Box<TiSlice<ModelId, Rc<dyn ModelImpl>>>,
    instance_data: Box<TiSlice<InstanceId, Box<dyn InstanceImpl>>>,
    matrix_builder: MatrixBuilder,
    matrix:         Option<SimulationMatrix>,
    nodes:          TiVec<Node, NodeInfo>,
    solution:       TiVec<Node, f64>,
    ac_solution:    TiVec<Node, Complex64>,
    residual_resist: TiVec<Node, f64>,
    residual_react:  TiVec<Node, f64>,
    pub config:     SimConfig,
    state:          SimulationState,   // bitflags: which OPs are current
    omega:          f64,
}
```

### Construction sequence

```
Circuit::prepare_simulation(eval_ctx, arena, config)
  └── Circuit::setup_simulation(config)           → allocates Simulation
  └── Simulation::prepare_solver(eval_ctx, arena)
        ├─ evaluate and set all model parameters  (ModelImpl::set_*_param, process_params)
        ├─ evaluate and set all instance params   (InstanceImpl::set_*_param)
        ├─ MatrixBuilder::reset
        ├─ for each instance:
        │    InstanceImpl::process_params(temp, builder, terminals)
        │      └─ SimBuilder::ensure_matrix_entry registers (col, row) pairs
        ├─ SimulationMatrix::new_or_reset (builds KLU symbolic factorization)
        └─ for each instance:
             InstanceImpl::populate_matrix_ptrs (stores raw pointers into KLU storage)
```

### DC operating point: `dc_op()`

`solve_op(OperatingPointAnalysis::DC)` runs a Newton-Raphson loop:

```
loop:
  for each instance:
    InstanceImpl::eval(sim_info)
    unsafe { load_matrix_resist() }
    load_residual_resist(solution, residual_resist)
  KLU: lu_factorize → solve_linear_system(residual_resist[1..])
  update solution -= delta
  check convergence: |delta| ≤ max(atol, |val| × rtol)
until converged or maxiters
```

The ground row/column (index 0) is excluded from the matrix solve — `[1..]`
slices skip it.

### AC analysis: `ac(omega)`

`ac()` first ensures a DC operating point via `ac_op()`. It then:
1. Calls `eval(EvalFlags::AC)` on each instance (the AC evaluation flag
   activates reactive-branch contributions in Verilog-A devices).
2. Calls `load_matrix_resist()` and `load_matrix_react(omega)` to fill both
   the real and imaginary parts of the complex Jacobian.
3. Solves the complex KLU system for `ac_solution`.

The `SimulationState` bitflags track which operating points are stale:
setting `omega` clears `AT_AC` so the next call re-solves.

### `SimConfig`

```rust
pub struct SimConfig {
    pub debug:        bool,    // print matrix and solution at each Newton step
    pub maxiters:     u32,     // default: 100
    pub voltage_atol: f64,     // default: 1e-6 V
    pub current_atol: f64,     // default: 1e-12 A
    pub rtol:         f64,     // default: 1e-3 (relative tolerance)
}
```

### Internal nodes

Devices can add internal nodes (e.g., a current-branch unknown) via
`SimBuilder::new_internal_branch` or `new_internal_node`. These grow the
`nodes` and `solution` vectors beyond the external node count. The matrix is
rebuilt each time `prepare_solver` is called with a changed topology.

---

## Verilog-A device loading (`veriloga.rs`)

`compile_va(path, opts)` is the bridge from Melange to OpenVAF:

```rust
pub fn compile_va(path: &Utf8Path, opts: &Opts) -> Result<Vec<Box<dyn DeviceImpl>>>
```

1. Resolves (or defaults) the cache directory, mirroring `openvaf-driver`'s
   batch-mode logic (`~/.cache/melange` on Linux).
2. Constructs `openvaf::Opts` with `target_cpu = "native"`,
   `opt_lvl = Aggressive`, and `CompilationDestination::Cache`.
3. Calls `openvaf::compile`. On `FatalDiagnostic`, returns an error (the
   compiler has already printed diagnostics to stderr).
4. `dlopen`s the resulting `.osdi` file with `libloading::Library`.
5. Reads `OSDI_VERSION_MAJOR` / `OSDI_VERSION_MINOR` and rejects anything
   other than `0.3`.
6. Reads `OSDI_NUM_DESCRIPTORS` and `OSDI_DESCRIPTORS` and slices them into
   `&'static [OsdiDescriptor]` (the library is `Box::leak`-ed to give it
   `'static` lifetime).
7. Installs `osdi_log` as the library's log callback.
8. Wraps each `OsdiDescriptor` in an `OsdiDevice` (which implements
   `DeviceImpl` via the OSDI function-pointer table).

> **TODO(verify):** The version check gates on `0.3` but the file name is
> `osdi_0_4.rs`. The auto-generated bindings may target v0.4 while the
> runtime check enforces v0.3. Check whether these need to be reconciled.

---

## `MatrixBuilder` and KLU integration

`MatrixBuilder` wraps `klu_rs::KluMatrixBuilder` and tracks, per instance,
which `(column, row)` matrix entries that instance writes:

```rust
pub(crate) struct MatrixBuilder {
    inner: KluMatrixBuilder<i32>,
    pub instance_entries: Box<TiSlice<InstanceId, Vec<(Node, Node)>>>,
    dump: NonNull<Cell<f64>>,   // ground entries redirect here
}
```

The `dump` pointer is a `Box::leak`-ed `Cell<f64>` that absorbs writes to
ground-connected matrix entries (`column == GROUND || row == GROUND`). This
avoids branches in `load_matrix_resist`/`load_matrix_react` — ground entries
just write to a dummy location and the result is discarded.

`SimulationMatrix` holds two KLU matrices sharing the same sparsity pattern
(`MatrixSpec`):
- `nonlinear_matrix: RealMatrix` — used for DC Newton iterations.
- `ac_matrix: ComplexMatrix` — used for AC small-signal analysis.

`new_or_reset` reuses the existing memory allocations (via `into_alloc` /
`new_with_alloc`) when the circuit topology has not changed, avoiding
reallocation between successive simulations with the same netlist.

---

## Worked example: DC operating point of a resistor divider

```rust
let mut earena = Arena::new();
let mut circ = Circuit::new("divider".to_owned(), &mut earena);

let vdd = circ.node("vdd".to_owned());
let mid = circ.node("mid".to_owned());
let gnd = Node::GROUND;

// V1: vdd → gnd, 5V
let (v1_inst, v1_model) = circ.new_device_instance_by_name(
    "V1".to_owned(), "vsource", vec![vdd, gnd])?;
circ.set_model_param(v1_model, "dc", Expr::from(5.0))?;

// R1: vdd → mid, 1kΩ; R2: mid → gnd, 1kΩ
let (r1_inst, r1_model) = circ.new_device_instance_by_name(
    "R1".to_owned(), "resistor", vec![vdd, mid])?;
circ.set_model_param(r1_model, "r", Expr::from(1000.0))?;

let (_, r2_model) = circ.new_device_instance_by_name(
    "R2".to_owned(), "resistor", vec![mid, gnd])?;
circ.set_model_param(r2_model, "r", Expr::from(1000.0))?;

// Run DC
let mut eval_ctx = ExprEvalCtx::new(&earena);
eval_ctx.set_param(CircuitParam::TEMPERATURE, Value::Num(300.0));

let mut sim = circ.prepare_simulation(eval_ctx.borrow(), &earena, SimConfig::default())?;
let solution = sim.dc_op()?;

// solution[mid] ≈ 2.5 V
```

---

## Key design decisions

**Trait objects for device polymorphism.** `DeviceImpl`, `ModelImpl`, and
`InstanceImpl` are `dyn` traits. This lets built-in Rust devices and OSDI
Verilog-A devices coexist in the same `TiSlice` without enums or code
generation. The cost is one vtable dispatch per `eval` call per instance —
acceptable for circuit simulation where the work per call dominates.

**`Rc<dyn ModelImpl>` for shared model state.** Multiple instances can share
the same model `Rc`. Interior mutability (`Cell<f64>`) is used because model
parameters are only mutated during `prepare_solver`, which is single-threaded.
This avoids `Arc` + `Mutex` overhead for a fundamentally single-threaded use
case.

**Pointer-based matrix writes.** Storing `NonNull<Cell<f64>>` pointers into
the KLU matrix backing store during `populate_matrix_ptrs` and writing through
them in `load_matrix_resist` eliminates all index arithmetic from the Newton
inner loop. The `unsafe` annotation on `load_matrix_resist` and
`load_matrix_react` documents this invariant explicitly.

**`dump` pointer for ground entries.** Rather than conditionally skipping
ground-connected matrix entries in the hot loop, all such entries are silently
redirected to a single dummy `Cell<f64>`. The code in `load_matrix_resist` is
then unconditional.

**`compile_va` always uses batch (cache) mode.** Melange passes
`CompilationDestination::Cache` to OpenVAF so that repeated loads of the same
`.va` file (across simulation runs or different circuits) hit the content-
addressed cache without recompiling. The cache key covers the preprocessed
token stream, so whitespace-only changes to the `.va` file do not invalidate
it.

**`Box::leak` for the OSDI library.** The `libloading::Library` is leaked to
give the `OsdiDescriptor` slice a `'static` lifetime. This is intentional —
Melange is designed for long-running simulation sessions where loaded libraries
are never unloaded.
