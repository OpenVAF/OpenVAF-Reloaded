# `sim_back` — Internals

Simulation backend: takes HIR module information and produces a `CompiledModule` ready for LLVM codegen.

Related: [hir_lower INTERNALS](../hir_lower/INTERNALS.md) · [mir INTERNALS](../mir/INTERNALS.md) · [mir_autodiff INTERNALS](../mir_autodiff/INTERNALS.md)

---

## 1. Purpose and Position

`sim_back` sits between `hir_lower` and `osdi`. Its job is to take a fully lowered MIR function and transform it into the four artifacts that `osdi` needs to emit an OSDI 0.4 shared library:

| Artifact | Field in `CompiledModule` | Purpose |
|---|---|---|
| Eval function | `eval` + `intern` | Per-Newton-iteration kernel: residuals, Jacobian, noise |
| Init function | `init` | Per-instance-setup kernel: op-independent calculations and cache slots |
| Model-param function | `model_param_setup` + `model_param_intern` | Model-level parameter validation (bounds, defaults) |
| Node collapse table | `node_collapse` | Which node pairs the simulator may merge |

The DAE system (`dae_system`) embedded in `CompiledModule` describes the sparsity pattern and semantics of the Jacobian so that `osdi` can lay out the correct OSDI descriptor tables.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | `CompiledModule`, `SimUnknownKind`, `collect_modules`; debug helpers |
| `module_info.rs` | `ModuleInfo`, `ParamInfo`, `OpVar`, `collect_modules` |
| `context.rs` | `Context` — working state; optimization stage dispatch; taint analysis |
| `topology.rs` | `Topology`, `BranchInfo`, `Contribution` — intermediate branch representation; `ddt` linearization; small-signal network; noise |
| `topology/builder.rs` | `create_dimension` — linear contribution factoring |
| `topology/lineralize.rs` | Linearization of `ddt`/noise into implicit nodes or direct contribution dimensions |
| `topology/small_signal_network.rs` | Detection of nodes with statically zero large-signal voltage |
| `dae.rs` | `DaeSystem`, `Residual`, `MatrixEntry`, `SimUnknown` |
| `dae/builder.rs` | `Builder` — assembles `DaeSystem` from `Topology`; calls `auto_diff`; builds Jacobian |
| `init.rs` | `Initialization`, `CacheSlot` — init/eval function split |
| `node_collapse.rs` | `NodeCollapse`, `CollapsePair` |
| `noise.rs` | `NoiseSource`, `NoiseSourceKind` |
| `util.rs` | `add`, `is_op_dependent`, `strip_optbarrier_if_const`, `update_optbarrier` helpers |

---

## 3. `collect_modules` and `ModuleInfo`

```rust
pub fn collect_modules(
    db: &CompilationDB,
    all_vars_opvars: bool,
    sink: &mut ConsoleSink,
) -> Option<Vec<ModuleInfo>>
```

`collect_modules` is the entry point called by `openvaf::compile()`. It iterates over every `hir::Module` in the compilation unit and builds a `ModuleInfo` for each.

```rust
pub struct ModuleInfo {
    pub module: Module,
    pub params: IndexMap<Parameter, ParamInfo, ahash::RandomState>,
    pub sys_fun_alias: IndexMap<ParamSysFun, Vec<SmolStr>, ahash::RandomState>,
    pub op_vars: IndexMap<Variable, OpVar, ahash::RandomState>,
}
```

`ModuleInfo::collect` walks every declaration in the module via `module.rec_declarations(db)` and applies two filters:

**Parameters** — every `Parameter` is collected if it has a `desc` or `units` attribute. The attribute values populate `ParamInfo`:

```rust
pub struct ParamInfo {
    pub name: SmolStr,
    pub alias: Vec<SmolStr>,
    pub unit: String,
    pub description: String,
    pub group: String,
    pub is_instance: bool,
}
```

The `type` attribute distinguishes model parameters (`type = "model"`, the default) from instance parameters (`type = "instance"`). Instance parameters are included in the per-instance init function; model parameters go into the separate `model_param_setup` function.

**Operating-point variables** — a `Variable` becomes an `OpVar` only when it is declared at module scope (not inside a named block or function) and has at least one of `desc` or `units`. These variables are marked as outputs in the MIR so their final values are readable by the simulator as operating-point data.

---

## 4. `CompiledModule::new()` Pipeline

`CompiledModule::new` runs eight sequential steps. Each step consumes and transforms the central `Context` object.

```
Step 1  Context::new
          MirBuilder (with_equations + with_tagged_writes)
          → (Function, HirInterner)
          insert_var_init patches HiddenState params

Step 2  compute_outputs(true) → compute_cfg()
        optimize(Initial)
          dead_code_elimination
          sparse_conditional_constant_propagation
          inst_combine
          simplify_cfg_no_phi_merge
          GVN

Step 3  Topology::new
          resolve branches from HirInterner outputs
          linearize ddt / noise
          detect small-signal network

Step 4  DaeSystem::new
          assign SimUnknowns (ports first, then internal nodes)
          build residuals and noise sources per branch
          call auto_diff → Jacobian derivatives
          build_jacobian (dense → sparse)
          build_lim_rhs (limiting correction terms)
          ensure_optbarriers + mfactor scaling

Step 5  compute_cfg() → optimize(PostDerivative) → dae_system.sparsify

Step 6  refresh_op_dependent_insts
          taint from op_dependent ParamKinds and op_dependent callbacks
          propagate_taint → op_dependent_insts bitset

Step 7  Initialization::new
          split_block: copy non-op-dependent insts to init function
          build_init_itern: copy params to init interner
          build_init_cache: CacheSlot per GVN equivalence class
          optimize init function (ADCE + simplify_cfg)
          NodeCollapse::new

Step 8  insert_param_init (instance params on init.func)
        insert_param_init (model params → model_param_setup function)
        optimize model_param_setup (SCCP + simplify_cfg)
```

---

## 5. `Context` and Optimization Stages

```rust
pub(crate) struct Context<'a> {
    pub(crate) func: Function,
    pub(crate) cfg: ControlFlowGraph,
    pub(crate) dom_tree: DominatorTree,
    pub(crate) intern: HirInterner,
    pub(crate) db: &'a CompilationDB,
    pub(crate) module: &'a ModuleInfo,
    pub(crate) output_values: BitSet<Value>,
    pub(crate) op_dependent_insts: BitSet<Inst>,
    pub(crate) op_dependent_vals: Vec<Value>,
}
```

`Context::new` calls `MirBuilder` with:
- `is_output`: `Contribute`, `ImplicitResidual`, `CollapseImplicitEquation`, `IsVoltageSrc`, and `Var(v)` for each `op_var` in `ModuleInfo`.
- `with_equations()` — enables contribution and noise lowering.
- `with_tagged_writes()` — marks every variable write with an `OptBarrier` so the init split can identify and cache them.

After build, `intern.insert_var_init` replaces `HiddenState(var)` `Param`s in the function with the actual variable initializer expressions.

### Optimization stages

`optimize(stage)` runs:

| Pass | Initial | PostDerivative | Final |
|---|---|---|---|
| `dead_code_elimination` | ✓ | | |
| `sparse_conditional_constant_propagation` | ✓ | ✓ | ✓ |
| `inst_combine` | ✓ | ✓ | ✓ |
| `simplify_cfg_no_phi_merge` | ✓ | ✓ | |
| `simplify_cfg` (with phi merge) | | | ✓ |
| `GVN` | ✓ | ✓ | ✓ |
| Aggressive DCE | | | ✓ |

The `PostDerivative` stage runs after `auto_diff` has added Jacobian instructions, allowing GVN and inst-combine to simplify them.

### Op-dependent taint analysis

`refresh_op_dependent_insts` seeds a taint set with:
- All live `Param`s whose `ParamKind::op_dependent()` returns `true` (voltages, currents, implicit unknowns, abstime, enabling flags, limiting state).
- All `op_dependent` callback results (noise calls, `$limit`, `$simparam`, `analysis`).

`propagate_taint` then marks every instruction reachable in the data-flow graph from those seeds as op-dependent. The resulting `op_dependent_insts` bitset is used by `Initialization::new` to decide which instructions stay in the eval function and which are moved to init.

---

## 6. `Topology`

`Topology` is an intermediate representation between the raw `HirInterner` outputs and the `DaeSystem`. It resolves the flat set of `PlaceKind::Contribute` values into structured `BranchInfo` entries.

```rust
pub(crate) struct BranchInfo {
    pub is_voltage_src: Value,   // FALSE / TRUE / dynamic
    pub voltage_src: Contribution,
    pub current_src: Contribution,
}

pub(crate) struct Contribution {
    pub unknown: Option<Value>,
    pub resist: Value,
    pub react: Value,
    pub resist_small_signal: Value,
    pub react_small_signal: Value,
    pub noise: Vec<Noise>,
}
```

`is_voltage_src` is the `IsVoltageSrc` output value from the MIR. If it is the constant `FALSE`, the branch is a pure current source; if `TRUE`, a voltage source; otherwise a runtime-switched branch.

**`ddt` handling.** Each `TimeDerivative` callback (`ddt(x)`) in the MIR is either linearized into a direct reactive contribution (when `x` is linear in the circuit unknowns) or replaced with an implicit internal node carrying the reactive equation.

**Small-signal network.** `small_signal_network` identifies nodes whose large-signal voltage is statically zero. Contributions from these nodes are separated into the `resist_small_signal` / `react_small_signal` fields of `Contribution`. This allows `build_jacobian` to skip Jacobian entries for these terms during large-signal simulation.

**Noise extraction.** Each noise callback in `HirInterner::callback_uses` is extracted into the `noise` list of the appropriate `Contribution`. Each `NoiseSource` carries `kind` (`WhiteNoise`, `FlickerNoise`, `NoiseTable`), `hi`/`lo` `SimUnknown` indices, and a `factor` value.

---

## 7. `DaeSystem`

```rust
pub struct DaeSystem {
    pub unknowns: TiSet<SimUnknown, SimUnknownKind>,
    pub residual: TiVec<SimUnknown, Residual>,
    pub jacobian: TiVec<MatrixEntryId, MatrixEntry>,
    pub small_signal_parameters: IndexSet<Value, ahash::RandomState>,
    pub noise_sources: Vec<NoiseSource>,
    pub model_inputs: Vec<(u32, u32)>,
    pub num_resistive: u32,
    pub num_reactive: u32,
}
```

### `SimUnknown` and `SimUnknownKind`

Each `SimUnknown` is a newtype index into `unknowns`. The three kinds are:

- `KirchoffLaw(Node)` — a KCL node equation; residual = sum of currents flowing into the node.
- `Current(CurrentKind)` — a branch with a probe on its current (named branch, unnamed `I(a,b)`, or port flow); introduces a separate equation `I_branch − I_computed = 0`.
- `Implicit(ImplicitEquation)` — an internal implicit node introduced by `ddt` or `idt`.

Port nodes are always assigned the first `SimUnknown` indices; internal nodes follow.

### `Residual`

```rust
pub struct Residual {
    pub resist: Value,           // large-signal I
    pub react: Value,            // large-signal Q  (ddt term)
    pub resist_small_signal: Value,
    pub react_small_signal: Value,
    pub resist_lim_rhs: Value,   // J*(x_lim - x) correction, resistive
    pub react_lim_rhs: Value,    // J*(x_lim - x) correction, reactive
}
```

The DAE for unknown `i` is `I_i(x) + ddt(Q_i(x)) = 0`. The Newton step solves `J·Δx = I + ddt(Q)`. `resist_lim_rhs` and `react_lim_rhs` provide a corrective right-hand side term needed when `$limit` is active: since the simulator evaluates the model at `x_lim` rather than `x`, it needs `J(x_lim)·(x_lim − x)` subtracted from the residual so the Newton iteration converges to the correct solution.

### `MatrixEntry`

```rust
pub struct MatrixEntry {
    pub row: SimUnknown,
    pub col: SimUnknown,
    pub resist: Value,   // ∂I_row/∂x_col
    pub react: Value,    // ∂Q_row/∂x_col
}
```

The Jacobian is stored sparsely. `build_jacobian` constructs a dense row per residual and then emits only non-zero entries. `num_resistive` and `num_reactive` count how many entries have non-zero `resist` and `react` respectively; `osdi` uses these counts to size the OSDI descriptor tables.

### Jacobian construction

`DaeSystem::new` (via `Builder::finish`):

1. Calls `intern.unknowns(func, sim_derivatives=true)` to produce `KnownDerivatives`, marking all voltage/current/implicit `Param`s as differentiation unknowns.
2. Calls `jacobian_derivatives` to build the `extra_derivatives` list: `(residual_value, unknown)` pairs for every non-constant residual.
3. Calls `mir_autodiff::auto_diff` to insert derivative instructions into the function.
4. `build_jacobian` iterates residuals, applies each derivative to the dense row, and sparsifies.

### `sparsify`

After the `PostDerivative` optimization pass, `DaeSystem::sparsify` strips redundant `OptBarrier`s from residual and Jacobian values when the underlying value is already a constant or parameter (i.e., no computation is needed). Zero entries are removed from `jacobian` entirely.

---

## 8. `Initialization` — The Init/Eval Split

```rust
pub struct Initialization {
    pub func: Function,
    pub intern: HirInterner,
    pub cached_vals: IndexMap<Value, CacheSlot, RandomState>,
    pub cache_slots: TiMap<CacheSlot, (PackedOption<ClassId>, u32), hir::Type>,
}
```

`Initialization::new` splits the single eval `Function` into two functions: one that runs once at instance setup (init) and one that runs each Newton iteration (eval).

### `split_block`

For each block in the eval function, `split_block` iterates its instructions:

- **Op-independent instruction**: copied to `init.func` (with values remapped via `val_map`), then zapped from `eval.func` if it is safe to remove.
- **Op-dependent terminator** (`Branch` or `Jump`): instead of copying, a `jump` to the else destination is emitted in `init.func`. This prevents op-dependent branches from fragmenting the init function's control flow.
- **Op-dependent `CollapseHint` callback**: `ignore_if_op_dependent` returns `true` for this kind, so it is zapped from eval without being copied to init.

### Cache slots

An op-independent instruction whose result is used in the eval function must be communicated via a cache slot — a shared memory location written by init and read by eval.

After `split_block`, tagged values (writes to `op_var` variables) and output `OptBarrier`s over op-independent values are candidates for caching. `build_init_cache` runs ADCE on eval to determine which cached values are actually consumed, then creates a `CacheSlot` for each, grouped by GVN equivalence class. In eval, the corresponding `Value` is replaced with a `Param` referencing that slot; in init, the slot is written with an `OptBarrier` over the computed value.

---

## 9. `NodeCollapse`

```rust
pub struct NodeCollapse {
    pairs: TiSet<CollapsePair, (SimUnknown, Option<SimUnknown>)>,
    extra_pairs: TiVec<CollapsePair, HybridBitSet<CollapsePair>>,
}
```

Node collapsing allows a simulator to merge two circuit nodes into one, removing a degree of freedom. `NodeCollapse` enumerates all pairs that can legally be collapsed.

Two sources contribute collapse pairs:

1. **`CollapseImplicitEquation` outputs** in `init.intern.outputs` — when an implicit equation is always collapsed (its `CollapseImplicitEquation` value is `TRUE` at instance-setup time), the associated implicit node disappears and its `SimUnknown` pair is registered.

2. **`CollapseHint(hi, lo)` callbacks** in `init.intern.callbacks` — emitted by `hir_lower` when `V(hi, lo) <+ 0.0` is seen. Translated to `(KirchoffLaw(hi), KirchoffLaw(lo))` pairs.

`extra_pairs` handles a secondary effect: if a branch current `I(a, b)` is probed (creating a `Current(kind)` unknown) and the underlying branch `(a, b)` is collapsible, then the current unknown must also be collapsed when the node pair collapses. These dependent pairs are stored in `extra_pairs[primary_pair]` as a `HybridBitSet`.

`NodeCollapse::hint(hi, lo, f)` is the API used by `osdi`: given a collapsing signal for a pair, it calls `f` for the primary pair and all its `extra_pairs`.

---

## 10. Worked Example — `resistor_va`

```verilog
module resistor_va(A, B);
    inout A, B; electrical A, B;
    branch (A, B) br_a_b;
    parameter real R    = 0.0 from [0:inf];   (* desc="Ohmic resistance", units="Ohm" *)
    parameter real zeta = 0.0 from [-20:20];  (* desc="Temperature coeff" *)
    parameter real tnom = 300.0 from [0:inf]; (* desc="Reference Temp.", units="Kelvin" *)
    real res, vres;
    analog begin
        vres = V(br_a_b);
        res  = R * pow($temperature / tnom, zeta);
        I(br_a_b) <+ vres / res;
    end
endmodule
```

### `ModuleInfo`

`collect` finds three parameters (all have `desc` or `units`): `R`, `zeta`, `tnom`. None has `type = "instance"`, so all are model parameters. `res` and `vres` have no attributes, so `op_vars` is empty. `sys_fun_alias` is also empty.

### `Context::new` — eval function sketch

After `MirBuilder::build` and `insert_var_init` (which replaces the `HiddenState(res)` and `HiddenState(vres)` params with `F_ZERO` since the variables have no initializer):

```
p0 = Voltage{A, Some(B)}    ; V(A,B) — op_dependent
p1 = Param(R)               ; not op_dependent
p2 = Temperature            ; not op_dependent
p3 = Param(tnom)
p4 = Param(zeta)

v0  = param p0              ; V(A,B)
v1  = param p1              ; R
v2  = param p2              ; $temperature
v3  = param p3              ; tnom
v4  = param p4              ; zeta
v5  = fdiv  v2, v3          ; temp/tnom
v6  = pow   v5, v4          ; (temp/tnom)^zeta
v7  = fmul  v1, v6          ; R * (temp/tnom)^zeta
v8  = fdiv  v0, v7          ; V(A,B) / R*(temp/tnom)^zeta
v9  = optbarrier v8
ret
```

`intern.outputs[Contribute{br_a_b, resistive, current}] = v9`

### `Topology`

`br_a_b` has `IsVoltageSrc = FALSE` (constant), so it is a pure current branch. Its `current_src.resist = v8`, `current_src.react = F_ZERO`. No noise sources.

### `DaeSystem`

Unknowns (ports first): `sim_node0 = KirchoffLaw(A)`, `sim_node1 = KirchoffLaw(B)`.

`build_branch(br_a_b, ...)` calls `add_kirchoff_law`:
- `residual[KirchoffLaw(A)].resist += v8`
- `residual[KirchoffLaw(B)].resist -= v8`

`sim_unknown_reads` = `[(Voltage{A,B}, v0)]`.

`auto_diff` is called with `extra_derivatives = [(v8, Unknown(V(A,B)))]`:
- `d(v8)/d(V(A,B))` = `d(V(A,B)/v7)/d(V(A,B))` = `1/v7`
- New instruction: `v10 = fdiv F_ONE, v7`

`build_jacobian`:

| Row | Col | resist | react |
|---|---|---|---|
| `sim_node0` (A) | `sim_node0` (A) | `v10` (= 1/R) | `F_ZERO` |
| `sim_node0` (A) | `sim_node1` (B) | `fneg(v10)` (= −1/R) | `F_ZERO` |
| `sim_node1` (B) | `sim_node0` (A) | `fneg(v10)` | `F_ZERO` |
| `sim_node1` (B) | `sim_node1` (B) | `v10` | `F_ZERO` |

After `sparsify` and GVN, identical `fneg(v10)` subexpressions are deduplicated.

### `Initialization` split

`refresh_op_dependent_insts` seeds taint at `v0` (the `Voltage{A,B}` param). Taint propagates to `v8` and `v9`. Instructions `v5`, `v6`, `v7` are op-independent.

`split_block` copies `v5 = fdiv v2, v3`, `v6 = pow v5, v4`, `v7 = fmul v1, v6` to `init.func`. Since `v7` is used in the eval function (by `v8`), it becomes a `CacheSlot`. In eval, `v7` is replaced with a `Param` reading that slot.

Final eval function (after init split):

```
p0 = Voltage{A, B}   ; op_dependent
pN = CacheSlot(0)    ; v7 cached from init: R*(temp/tnom)^zeta

v0  = param p0
v7c = param pN       ; cached result
v8  = fdiv v0, v7c
v9  = optbarrier v8
...
```

`init.func` computes `v7` once and stores it in `CacheSlot(0)`.

### `NodeCollapse`

No `CollapseHint` calls and no `CollapseImplicitEquation` outputs → `node_collapse.num_pairs() = 0`. The simulator will not collapse any nodes for this model.
