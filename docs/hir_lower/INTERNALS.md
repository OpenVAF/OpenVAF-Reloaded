# `hir_lower` — Internals

HIR → MIR lowering: converts typed HIR constructs into SSA-form MIR.

---

## 1. Purpose and Position

`hir_lower` is the bridge between the semantic frontend (`hir`, `hir_def`, `hir_ty`) and the middle-end IR (`mir`). Its input is a `hir::Module` — a fully resolved, type-checked Verilog-A module — together with a `CompilationDB` Salsa database. Its output is a pair `(mir::Function, HirInterner)`.

The `Function` is a complete SSA-form MIR function encoding the module's analog behavior: voltages and currents as function parameters, contributions as accumulated SSA values, and runtime service calls (noise, `$limit`, `ddt`) as opaque MIR `call` instructions.

The `HirInterner` is the reverse-mapping table: given an SSA `Value` or `FuncRef` in the `Function`, it tells you which HIR concept it corresponds to. Downstream crates (`sim_back`, `mir_autodiff`) need this to interpret the MIR without importing the full HIR.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | `MirBuilder` (public API), `HirInterner`, and all key enums: `ParamKind`, `PlaceKind`, `CurrentKind`, `IdtKind`, `ImplicitEquationKind`, `LimitState`, `ImplicitEquation` |
| `ctx.rs` | `LoweringCtx` — wraps `FunctionBuilder` and `&mut HirInterner`; manages places, params, and callbacks |
| `body.rs` | `BodyLoweringCtx` — thin wrapper adding a `BodyRef` (HIR statement/expression body) to `LoweringCtx` |
| `stmt.rs` | `lower_stmt` dispatch: assignment, contribution, control flow, loops, case |
| `expr.rs` | `lower_expr` dispatch: literals, reads, binary/unary ops, builtins, nature access, `$limit` |
| `callbacks.rs` | `CallBackKind` enum and `FunctionSignature` generation for runtime service calls |
| `parameters.rs` | `HirInterner::insert_param_init` — builds the parameter-validation MIR (bounds checking, default values) |
| `state.rs` | `HirInterner::insert_var_init` — patches hidden-state `Param`s with variable initializer expressions |
| `dimensions.rs` | Array dimension helpers |
| `fmt.rs` | `DisplayKind` and `FmtArg` — support types for `$display`/`$debug` format string lowering |

---

## 3. `MirBuilder`

`MirBuilder` is the public entry point. It follows a builder pattern: callers set options on it, then call `build()`.

```rust
pub struct MirBuilder<'a> {
    db: &'a CompilationDB,
    module: Module,
    is_output: &'a dyn Fn(PlaceKind) -> bool,
    required_vars: &'a mut dyn Iterator<Item = Variable>,
    tagged_reads: AHashSet<Variable>,
    tag_writes: bool,
    ctx: Option<&'a mut FunctionBuilderContext>,
    lower_equations: bool,
}
```

The key options:
- `is_output` — a predicate the caller supplies to say which `PlaceKind`s should appear in `outputs` of the `HirInterner`. Typically `PlaceKind::Contribute { .. } | PlaceKind::ImplicitResidual { .. }`.
- `required_vars` — variables that must have their `Place`s declared even if they are never written in the analog block (needed for `HirInterner::insert_var_init`).
- `lower_equations` — when `false` the `no_equations` flag is set, suppressing contribution accumulation and noise/limit lowering (used for parameter-setup functions).
- `tag_writes` / `tagged_reads` — mark specific variable reads/writes with `OptBarrier`s so `sim_back` can identify them as observable outputs.

`build()` orchestrates the full lowering:

1. Creates an empty `Function` and `HirInterner`.
2. Wraps the `Function` in a `FunctionBuilder` (Cranelift-style SSA builder from `mir_build`).
3. Constructs `LoweringCtx` over the builder and interner.
4. Calls `body_ctx.lower_entry_stmts()` twice — first for the `analog initial` block, then for the `analog` block. The `analog initial` block runs once at simulation setup; the `analog` block runs each Newton iteration.
5. Declares places for any `required_vars` via `dec_place`.
6. Iterates all declared places; for those where `is_output` returns `true`, reads the current SSA value, wraps it in `ensure_optbarrier`, and stores the result in `interner.outputs`.
7. Emits `ret()` and finalizes the SSA construction.

---

## 4. `HirInterner`

```rust
pub struct HirInterner {
    pub outputs: IndexMap<PlaceKind, PackedOption<Value>, ahash::RandomState>,
    pub params: TiMap<Param, ParamKind, Value>,
    pub callbacks: TiSet<FuncRef, CallBackKind>,
    pub callback_uses: TiVec<FuncRef, Vec<Inst>>,
    pub tagged_reads: IndexMap<Value, Variable, ahash::RandomState>,
    pub implicit_equations: TiVec<ImplicitEquation, ImplicitEquationKind>,
    pub lim_state: TiMap<LimitState, Value, Vec<(Value, bool)>>,
}
```

`outputs` — maps each output `PlaceKind` to the SSA `Value` holding its final accumulated value (wrapped in `OptBarrier`), or `None` if the place was not written.

`params` — the central HIR↔MIR parameter table. Every MIR `Param` (a function input) has a corresponding `ParamKind` entry here. Downstream crates look up `ParamKind` to know what each `Param` means at the circuit level.

`callbacks` and `callback_uses` — the set of runtime-service `FuncRef`s used in the `Function`. `callback_uses` records every call site for each callback; `sim_back` uses this to identify `ddt` and noise calls.

`tagged_reads` — maps `OptBarrier`-wrapped values back to the `Variable` they were read from. Used by `sim_back` to locate variable probe points.

`implicit_equations` — each `ImplicitEquation` index maps to an `ImplicitEquationKind` (`Ddt`, `NoiseSrc`, or one of the `Idt` variants), recording why an implicit node was introduced.

`lim_state` — records each `$limit` call site: maps the probe `Value` to a list of `(stored_value, negated)` pairs that `sim_back` uses to thread the limiting iteration.

### `unknowns()`

`HirInterner::unknowns()` converts the interner into the `KnownDerivatives` struct that `mir_autodiff` consumes. It walks all `params` entries and assigns an `Unknown` index to each `Param` that the AD pass must differentiate:

- `ParamKind::Voltage { hi, lo }` — gets an `Unknown` if a `NodeDerivative(hi)` or `NodeDerivative(lo)` callback exists in `callbacks`, or if `sim_derivatives` is set (the caller's flag indicating the simulator will ask for Jacobian entries).
- `ParamKind::Current(_)` and `ParamKind::ImplicitUnknown(_)` — get an `Unknown` only if `sim_derivatives` is set.
- Other kinds (parameters, temperature, etc.) — get an `Unknown` if a `Derivative(param)` callback exists (i.e., the model called `ddx(expr, param)`).

---

## 5. `ParamKind` and `PlaceKind`

### `ParamKind` — what an MIR `Param` represents

`ParamKind` is the enumeration of all quantities that enter the MIR function as SSA function parameters (i.e., values the simulator supplies at runtime).

| Variant | Meaning |
|---|---|
| `Param(Parameter)` | A model parameter (e.g., `R`, `tnom`) |
| `Voltage { hi, lo }` | Potential difference V(hi,lo); `lo=None` means potential to ground |
| `Current(CurrentKind)` | Branch or port current |
| `Temperature` | Simulation temperature (`$temperature`) |
| `Abstime` | Absolute simulation time (`$abstime`) |
| `EnableIntegration` | Flag: is time-domain integration active |
| `EnableLim` | Flag: is limiting active this Newton step |
| `PrevState(LimitState)` | Value of a `$limit` probe from the previous iteration |
| `NewState(LimitState)` | Value stored by a `$limit` call |
| `ParamGiven { param }` | Boolean: was `param` explicitly given by the netlist |
| `PortConnected { port }` | Boolean: is port node connected |
| `ParamSysFun(ParamSysFun)` | System function like `$mfactor`, `$xposition` |
| `HiddenState(Variable)` | Initial value of a real variable before the analog block runs |
| `ImplicitUnknown(ImplicitEquation)` | Voltage of an implicit internal node |

`ParamKind::op_dependent()` returns `true` for kinds whose value changes each Newton iteration (voltages, currents, time, limiting state). This distinction matters in `sim_back` when splitting the function into init and eval kernels.

### `PlaceKind` — mutable SSA slots

`PlaceKind` names every mutable memory location that the Cranelift-style SSA builder tracks. The builder transparently inserts `PhiNode` instructions at join points.

| Variant | Initialized to | Notes |
|---|---|---|
| `Var(Variable)` | `HiddenState(var)` param | The HIR variable; reads go through `read_variable` |
| `Contribute { dst, reactive, voltage_src }` | `F_ZERO` | Accumulated contribution to a branch |
| `ImplicitResidual { equation, reactive }` | `F_ZERO` | Residual of an implicit equation |
| `IsVoltageSrc(BranchWrite)` | `FALSE` | Set to `true` when a potential contribution is made |
| `CollapseImplicitEquation(eq)` | `TRUE` | `false` means "don't collapse"; init-only |
| `FunctionReturn(fun)` | `F_ZERO` or `ZERO` | Return value of an inline user function |
| `FunctionArg(arg)` | caller expression | Argument slot for an inlined function call |
| `Param / ParamMin / ParamMax` | (no init) | Written during parameter initialization |
| `BoundStep` | `INFINITY` | Simulator time-step bound |

---

## 6. `LoweringCtx` and SSA Construction

```rust
pub struct LoweringCtx<'a, 'c> {
    pub db: &'a CompilationDB,
    pub func: FunctionBuilder<'c>,
    pub no_equations: bool,
    pub intern: &'a mut HirInterner,
    pub places: TiSet<Place, PlaceKind>,
    tagged_vars: AHashSet<Variable>,
    pub inside_lim: bool,
    pub num_noise_sources: u32,
}
```

`LoweringCtx` owns the `FunctionBuilder` (the Cranelift-style SSA construction API from `mir_build`) and a mutable borrow of the `HirInterner` being built.

### Place protocol

`dec_place(kind)` declares a new SSA variable slot for `kind`. If the slot is new, it initializes it in the function entry block with the appropriate default value (see the table above). Returns the `Place` handle.

`def_place(kind, val)` writes `val` to the slot for `kind` at the current program point. `use_place(kind)` reads the current value, inserting a `PhiNode` at block joins automatically.

### `use_param`

```rust
pub fn use_param(&mut self, kind: ParamKind) -> Value {
    let len = self.intern.params.len();
    let entry = self.intern.params.raw.entry(kind);
    *entry.or_insert_with(|| self.func.func.dfg.make_param(len.into()))
}
```

If the requested `ParamKind` has not been seen before, a new `Param` is allocated in the `Function`'s `DataFlowGraph` and registered in `interner.params`. The associated `Value` is returned. Subsequent calls with the same `kind` return the same `Value` without allocating.

### Ground-node elision

In Verilog-A, node `gnd` is implicit ground (potential = 0). `LoweringCtx::node(n)` returns `None` when `n.is_gnd(db)`. The `nodes(hi, lo, kind)` helper handles all four cases:

- `(Some(hi), None)` → `use_param(kind(hi, None))`
- `(None, Some(lo))` → `use_param(kind(lo, None))` followed by `fneg`
- `(Some(hi), Some(lo))` → canonical `use_param(kind(hi, Some(lo)))`; checks if the inverted pair was already allocated and negates if so
- `(None, None)` → `F_ZERO` directly, no allocation

### `no_equations`

When `no_equations` is `true`, the lowering context skips contribution accumulation, noise calls, and `$limit` calls. This mode is used by `sim_back` when building the model-parameter-setup function, which evaluates parameter expressions but does not need circuit equations.

---

## 7. Statement Lowering

`BodyLoweringCtx::lower_stmt` dispatches on `Stmt`:

**`Stmt::Assignment { lhs, rhs }`** — evaluates `rhs` to an SSA `Value`, then calls `def_place(lhs.into(), val)`. The `From<hir::AssignmentLhs>` impl converts variable/function-return/function-arg lhs into the corresponding `PlaceKind`.

**`Stmt::Contribute { kind, branch, rhs }`** — calls `contribute(voltage_src, branch, rhs)`:
1. Sets `IsVoltageSrc(branch)` to reflect whether this is a potential (`V <+`) or flow (`I <+`) contribution.
2. For `V(node) <+ 0.0` (ideal voltage source): emits a `CollapseHint(hi, lo)` callback (tells the simulator the two nodes may merge).
3. Initializes `Contribute { reactive: false, voltage_src }` to `F_ZERO` if not already present.
4. Evaluates `rhs`; if `rhs == F_ZERO`, returns early (no-op contribution).
5. Reads the current accumulated value `old`, computes `new = old + rhs` (or `old - rhs` for negated branches), and writes back via `def_place`.

**`Stmt::If { cond, then_branch, else_branch }`** — evaluates `cond`, calls `make_cond` which creates three blocks (then, else, merge), seals them, and lowers each branch into its block.

**`Stmt::ForLoop` / `Stmt::WhileLoop`** — `lower_loop` creates three blocks: loop-condition head, loop body, loop exit. Emits `jump` → cond block, `br_loop` (a conditional branch that also marks the back-edge for the SSA builder), and a back-edge `jump` at the end of the body to re-seal the condition block.

**`Stmt::Case`** — `lower_case` compiles a chain of equality tests. Each case value generates a `br` into the body block or the next test. The default case, if present, is lowered inline at the fall-through point. All body blocks `jump` to the shared `end` block.

---

## 8. Expression Lowering

`lower_expr` evaluates a HIR `ExprId` and returns its SSA `Value`.

**Literals** — `Literal::Float(v)` → `fconst(v)`, `Literal::Int(v)` → `iconst(v)`, `Literal::String(s)` → `sconst(s)`, `Literal::Inf` → `INFINITY` or `iconst(i32::MAX)` depending on type.

**Variable reads** — `Expr::Read(Ref::Variable(var))` calls `read_variable`, which calls `use_place(Var(var))` and, if `var` is in `tagged_vars`, wraps the result in `optbarrier` and records it in `interner.tagged_reads`.

**Parameter reads** — `Expr::Read(Ref::Parameter(p))` → `use_param(ParamKind::Param(p))`.

**Binary operators** — `match_signature!` dispatches on the HIR type signature to select the correct opcode. Boolean `||` and `&&` are short-circuit: `a || b` becomes `if a { true } else { b }` (a `make_select` call), ensuring the right-hand side is not evaluated when unnecessary.

**Nature access** (`BuiltIn::potential` and `BuiltIn::flow`) — the core circuit probes:
- `V(a, b)` → `nodes_from_args(args, |hi, lo| ParamKind::Voltage{hi, lo})` which calls `ctx.nodes(hi, lo, ...)`, handling ground elision.
- `V(branch)` → resolves the branch's hi and lo nodes, then calls `ctx.nodes(...)`.
- `I(branch)` → `use_param(ParamKind::Current(CurrentKind::Branch(br)))`.
- `I(a, b)` → `use_param(ParamKind::Current(CurrentKind::Unnamed{hi, lo}))`.
- `I(<port>)` → `use_param(ParamKind::Current(CurrentKind::Port(node)))`.

**`BuiltIn::abs`** — lowered as a conditional: `if val < 0 { -val } else { val }` via `lower_select_with`.

**`BuiltIn::limexp`** — lowered as a linearized exponential to prevent overflow:

```
if arg > ln(1e30):
    (arg - ln(1e30)) * 1e30 + 1e30
else:
    exp(arg)
```

**`$limit` calls** — two-phase protocol described in section 8 below.

**`$fatal`** — emits the display message, then `SetRetFlag(Abort)` callback, then `exit` instruction. Creates an unreachable block afterward (required because the SSA builder must always have an active block, but the code after `$fatal` is dead).

---

## 9. `CallBackKind` and Runtime Callbacks

Runtime services that cannot be expressed as pure arithmetic are lowered as MIR `call` instructions. Each `CallBackKind` variant has a unique `FunctionSignature` name; the MIR is agnostic about what the call does.

```rust
pub enum CallBackKind {
    Print { kind: DisplayKind, arg_tys: Box<[FmtArg]> },
    SimParam,
    SimParamOpt,
    SimParamStr,
    Derivative(Param),
    NodeDerivative(Node),
    ParamInfo(ParamInfoKind, Parameter),
    CollapseHint(Node, Option<Node>),
    LimDiscontinuity,
    Analysis,
    BuiltinLimit { name: Spur, num_args: u32 },
    StoreLimit(LimitState),
    TimeDerivative,
    WhiteNoise { name: Spur, idx: u32 },
    FlickerNoise { name: Spur, idx: u32 },
    NoiseTable(Box<NoiseTable>),
    SetRetFlag(RetFlag),
}
```

**`Derivative(Param)` and `NodeDerivative(Node)`** — represent `ddx(expr, param)` and `ddx(expr, V(node))` calls in the Verilog-A source. They are recorded in `callbacks` so that `HirInterner::unknowns()` can later identify which `Param`s are differentiation targets.

**`SimParam` / `SimParamOpt` / `SimParamStr`** — query simulator-defined string-keyed parameters (`$simparam`).

**`WhiteNoise`, `FlickerNoise`, `NoiseTable`** — each noise source gets a unique `idx` to prevent the optimizer from treating `white_noise(x) - white_noise(x)` as zero (they are statistically independent sources).

**`StoreLimit(LimitState)`** — called by `finish_limit`; stores the `$limit` result for the next iteration's `PrevState` parameter.

**`CollapseHint(hi, lo)`** — emitted when `V(hi, lo) <+ 0.0`; tells the simulator the two nodes may be collapsed into one. Has `ignore_if_op_dependent = true`, so `sim_back` omits it from operating-point functions.

**Tracking.** `CallBackKind::tracked()` returns `false` only for `Print` variants. All other callbacks have their call sites recorded in `interner.callback_uses`. `sim_back` uses this to find `TimeDerivative` (i.e., `ddt()`) calls and noise calls when constructing the DAE system.

### `$limit` two-phase protocol

Lowering a `$limit(probe, fn, ...)` call:

1. `start_limit(probe)` — allocates a `LimitState` index; registers the probe in `intern.lim_state`; returns the state handle.
2. Reads `PrevState(state)` and `EnableLim` as `Param`s.
3. `lower_select_with(enable_lim, ...)` — in the `true` branch, evaluates the limit function body with `new_val` and `old_val` as arguments; in the `false` branch, returns `new_val` unchanged.
4. `finish_limit(state, result)` — emits `call StoreLimit(state)` to persist the result; patches `intern.lim_state` with the stored value.

---

## 10. Worked Example — `resistor.va`

```verilog
module resistor_va(A, B);
    inout A, B; electrical A, B;
    branch (A, B) br_a_b;
    parameter real R    = 0.0 from [0:inf];
    parameter real zeta = 0.0 from [-20:20];
    parameter real tnom = 300.0 from [0:inf];
    real res, vres;
    analog begin
        vres = V(br_a_b);
        res  = R * pow($temperature / tnom, zeta);
        I(br_a_b) <+ vres / res;
    end
endmodule
```

`MirBuilder::build()` is called with `is_output = |k| matches!(k, PlaceKind::Contribute{..})`.

**Step 1 — `vres = V(br_a_b)`**

`lower_stmt(Assignment { lhs: Var(vres), rhs: potential(br_a_b) })`:
- `lower_expr(potential(br_a_b))`: resolves `br_a_b` to `hi=A`, `lo=B`; calls `ctx.nodes(A, Some(B), |hi,lo| ParamKind::Voltage{hi,lo})`.
- Neither A nor B is ground, so allocates `p0: Param` for `ParamKind::Voltage{hi:A, lo:Some(B)}`. Result SSA value: `v0 = param p0`.
- `dec_place(Var(vres))` → initializes `place0` to `HiddenState(vres)` param `p1` in entry block.
- `def_place(Var(vres), v0)` → `v0` is the current definition of `place0`.

**Step 2 — `res = R * pow($temperature / tnom, zeta)`**

`lower_expr(R * pow(...))`:
- `R` → `use_param(ParamKind::Param(R))` → `v1 = param p2`.
- `$temperature` → `use_param(ParamKind::Temperature)` → `v2 = param p3`.
- `tnom` → `use_param(ParamKind::Param(tnom))` → `v3 = param p4`.
- `zeta` → `use_param(ParamKind::Param(zeta))` → `v4 = param p5`.
- `v5 = fdiv v2, v3`  (temperature / tnom)
- `v6 = pow v5, v4`  (pow(temp/tnom, zeta))
- `v7 = fmul v1, v6`  (R * pow(...))
- `def_place(Var(res), v7)`.

**Step 3 — `I(br_a_b) <+ vres / res`**

`lower_stmt(Contribute { kind: Flow, branch: br_a_b, rhs: vres/res })`:
- `def_place(IsVoltageSrc(br_a_b), FALSE)` — this is a flow contribution.
- `dec_place(Contribute{dst:br_a_b, reactive:false, voltage_src:false})` → initialized to `F_ZERO`.
- `lower_expr(vres / res)`:
  - `vres` → `use_place(Var(vres))` = `v0`.
  - `res`  → `use_place(Var(res))`  = `v7`.
  - `v8 = fdiv v0, v7`.
- `old = use_place(Contribute{...})` = `F_ZERO`.
- Since `old == F_ZERO`: `new = v8` (no `fadd` needed).
- `def_place(Contribute{dst:br_a_b, ...}, v8)`.

**Step 4 — finalization**

`build()` iterates `places`; for `Contribute{dst:br_a_b, ...}`:
- `use_place(...)` = `v8`.
- `v9 = optbarrier v8`.
- `interner.outputs[Contribute{...}] = Some(v9)`.

Emits `ret`. Final `Function` sketch (entry block only):

```
function resistor_va:
    p0 = Voltage{A, Some(B)}   ; V(A,B)
    p1 = HiddenState(vres)
    p2 = Param(R)
    p3 = Temperature
    p4 = Param(tnom)
    p5 = Param(zeta)

    v0 = param p0
    v1 = param p2
    v2 = param p3
    v3 = param p4
    v4 = param p5
    v5 = fdiv v2, v3
    v6 = pow  v5, v4
    v7 = fmul v1, v6
    v8 = fdiv v0, v7
    v9 = optbarrier v8
    ret
```

`interner.outputs[Contribute{dst:br_a_b, reactive:false, voltage_src:false}] = v9`.

`sim_back` reads this output value and connects it to the DAE branch current equation for the `br_a_b` branch.
