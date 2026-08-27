# `mir_autodiff` — Internals

Automatic differentiation pass for OpenVAF's MIR.

---

## 1. Purpose and Context

Circuit simulators need the Jacobian of a device model's DAE system — partial derivatives of branch currents/voltages with respect to node potentials. OpenVAF computes these derivatives at compile time by transforming the MIR itself, not by differentiating LLVM IR.

Working at the MIR level has two advantages:
- The resulting derivative instructions are in the same SSA form as the primal instructions, so every subsequent `mir_opt` pass (constant propagation, GVN, DCE, inst-combine) applies to derivatives automatically.
- The transformation is portable: it runs before any target-specific codegen.

The technique is **forward-mode source transformation**. For each `Unknown` variable (a node voltage or current that the simulator may probe), the pass inserts instructions that compute `d(value)/d(unknown)` alongside each existing instruction. Higher-order and mixed partial derivatives are handled by composing multiple rounds of differentiation.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | Public entry point `auto_diff()`; `zero_derivative()` helper |
| `intern.rs` | `Derivative` / `DerivativeInfo` types; `DerivativeIntern` table |
| `live_derivatives.rs` | `LiveDerivatives` dataflow analysis; `ChainRule` type |
| `postorder.rs` | Reverse-postorder block iterator used by the builder |
| `builder.rs` | `DerivativeBuilder` — inserts derivative instructions into the function |
| `subgraph.rs` | `SubGraphExplorer` — subgraph sharing optimisation |

---

## 3. Entry Point

```rust
pub fn auto_diff(
    mut func: impl AsMut<Function>,
    dom_tree: &DominatorTree,
    derivatives: &KnownDerivatives,
    extra_derivatives: &[(Value, mir::Unknown)],
) -> AHashMap<(Value, mir::Unknown), Value>
```

`func` is the MIR `Function` to be transformed in place. `dom_tree` must be pre-computed. `derivatives` carries the set of `Unknown`s declared in the function and the `ddx()` call sites. `extra_derivatives` seeds additional `(value, unknown)` pairs that the caller needs but that are not implied by `ddx()` calls alone.

The return value maps each `(primal_value, unknown)` pair to the SSA `Value` holding that derivative after the transformation. Callers in `sim_back` use this map to wire Jacobian entries.

---

## 4. `Unknown` and `Derivative` Types

### `Unknown` (defined in `mir`)

```rust
pub struct Unknown(pub u32);
```

A newtype index identifying one differentiation variable — typically a node voltage or branch current. `Unknown`s are interned into the `unknowns` table inside `KnownDerivatives`; the associated `Value` is the SSA value that represents the variable in the primal computation.

### `Derivative` (defined in `intern.rs`)

```rust
struct Derivative(u32);

struct DerivativeInfo {
    previous_order: Option<Derivative>,
    base: Unknown,
}
```

`Derivative` is an index into a `TiSet<Derivative, DerivativeInfo>` table owned by `DerivativeIntern`. Each entry records:
- `base` — the `Unknown` differentiated at this level.
- `previous_order` — if `Some(d)`, this is a higher-order derivative built on top of `d`.

This forms a linked list encoding mixed partial derivatives. For example:
- First-order ∂/∂x: `DerivativeInfo { base: x, previous_order: None }`
- Second-order ∂²/∂x²: `DerivativeInfo { base: x, previous_order: Some(d_x) }`
- Mixed ∂²/∂x∂y: `DerivativeInfo { base: y, previous_order: Some(d_x) }`

New derivatives are created with `raise_order(base)` (adds one level over an existing `Derivative`) or `raise_order_with(unknown)` (starts a fresh first-order derivative).

---

## 5. `KnownDerivatives` and `DerivativeIntern`

### `KnownDerivatives` (defined in `mir`)

```rust
pub struct KnownDerivatives {
    pub unknowns: TiSet<Unknown, Value>,
    pub ddx_calls: AHashMap<FuncRef, (HybridBitSet<Unknown>, HybridBitSet<Unknown>)>,
}
```

`unknowns` maps each `Unknown` index to the SSA `Value` that holds the probed quantity. `ddx_calls` maps each `ddx()` `FuncRef` to a pair of bitsets `(first_order, higher_order)` specifying which `Unknown`s that particular `ddx()` call differentiates.

### `DerivativeIntern` (defined in `intern.rs`)

```rust
struct DerivativeIntern<'a> {
    unknowns: TiSet<Unknown, Value>,
    ddx_calls: &'a AHashMap<FuncRef, (HybridBitSet<Unknown>, HybridBitSet<Unknown>)>,
    derivatives: TiSet<Derivative, DerivativeInfo>,
    buf: Vec<Unknown>,
}
```

`DerivativeIntern` is the working intern table used throughout the pass. It wraps the data from `KnownDerivatives` and adds the `derivatives` table (which grows as higher-order derivatives are requested) plus a scratch `buf`. It is constructed once at the start of `auto_diff` and passed through the liveness and builder phases.

---

## 6. Phase 1 — `LiveDerivatives`

Before inserting any instructions, the pass must determine which `(instruction, derivative)` pairs are actually needed. This avoids computing derivatives of dead values.

```rust
pub(crate) struct LiveDerivatives {
    pub mat: SparseBitMatrix<Inst, Derivative>,
    pub conversions: AHashMap<Inst, Vec<ChainRule>>,
}
```

`mat` is a sparse bit matrix: `mat[inst]` is the set of `Derivative`s whose value at the output of `inst` will be consumed downstream. `conversions` records chain-rule steps that must be performed at specific instructions (used when subgraph optimisation introduces synthetic unknowns).

`LiveDerivatives::build()` runs in three steps, then calls the subgraph optimiser:

**Step 1 — `populate_reachable_unknowns`**  
Seeds the matrix. For each `ddx()` call instruction, marks the requested derivative as live at that instruction. For `OptBarrier` instructions, marks all derivatives that pass through the barrier as live.

**Step 2 — `live_derivative_fixpoint`**  
Propagates liveness backwards through the data-flow graph. If derivative `d` is live at an instruction `i`, then for each input operand `v` of `i`, the derivative of `v` is marked live at the defining instruction of `v`. This iterates until no new bits are set. For `PhiNode` instructions, liveness flows into all predecessor definitions.

**Step 3 — `strip_unneeded_derivatives`**  
Removes derivatives that became live only as intermediate results but whose final consumers were already eliminated by a previous optimisation. This keeps the matrix tight.

After the three steps, `run_subgraph_opt()` is called to find instructions differentiated with respect to multiple unknowns; it may add new synthetic `Unknown`s and entries in `conversions`.

---

## 7. Subgraph Optimisation

```rust
struct SubGraphExplorer { ... }
```

When two derivatives share a common sub-expression, computing them independently duplicates work. `SubGraphExplorer` finds "subgraphs" — contiguous sets of instructions differentiated with respect to multiple unknowns — and replaces them with a single computation for a synthetic unknown, followed by chain-rule multiplications.

The cost criterion before committing to a subgraph:

```rust
saved_insts > extra_inst_approx + 4
    && (saved - extra) * 100 / num_insts > 15
```

Both conditions must hold: the absolute saving must exceed the overhead by at least 4 instructions, and the relative gain must be at least 15 % of the subgraph size. This avoids regressing small subgraphs where the chain-rule overhead dominates.

When a subgraph is accepted, `SubGraphExplorer` adds synthetic `Unknown`s to `DerivativeIntern` and records the chain-rule multiplications in `LiveDerivatives::conversions`.

---

## 8. Phase 2 — `DerivativeBuilder`

`build_derivatives()` is the public entry within the builder module. It constructs a `DerivativeBuilder` and walks the function's blocks in reverse postorder (dominators before uses).

```rust
struct DerivativeBuilder<'a, 'b> { ... }
```

**Seeding.** Before the walk begins, for each `Unknown` `u` whose associated `Value` is `x`, the seed `(x, u) → F_ONE` is inserted into the derivative map (the derivative of a variable with respect to itself is 1).

**Per-instruction processing.** For each instruction, `inst_derivative()` is called for every `Derivative` marked live at that instruction in the `LiveDerivatives` matrix. It emits the appropriate derivative instruction(s) into the function and records the resulting `Value` in the map.

**Cache mechanism.** `inst_cache()` is called before differentiation of an instruction. It pre-computes sub-expressions that multiple derivative rules reuse. For example, for `Fdiv(a, b)`, the cache stores `b * b` so that the quotient rule `(a' * b - a * b') / b²` only computes the denominator once across all differentiated operands.

**Cyclic phi fix-up.** Loops introduce `PhiNode` instructions that are cyclic in the data-flow graph. The builder uses a two-pass approach: in the first pass it inserts placeholder `PhiNode`s for the derivative of each loop variable; in the second pass it fills in the correct operands once the derivative of the loop body is known.

---

## 9. Differentiation Rules

### Zero-derivative set

`zero_derivative()` returns `true` for opcodes whose output is always constant with respect to any unknown. These instructions are skipped entirely:

- Integer arithmetic and comparison opcodes (`Iadd`, `Isub`, `Imul`, etc.)
- Boolean logic (`BAnd`, `BOr`, `BXor`, `BNot`)
- Type conversions from integer to float that act as constants (`Iconst`, `Uicast`, `Iicast`)
- Control-flow affecting opcodes (`Jump`, `Branch`, `Exit`, `Phi` over integer types)
- `Iabs`, `IsNeg`, `IsZero`, `IsInf`

### Per-opcode rules

| Opcode | Primal | Derivative rule |
|---|---|---|
| `Fadd(a, b)` | a + b | a' + b' |
| `Fsub(a, b)` | a − b | a' − b' |
| `Fmul(a, b)` | a · b | a' · b + a · b' |
| `Fdiv(a, b)` | a / b | (a' · b − a · b') / b² |
| `Fneg(a)` | −a | −a' |
| `Fabs(a)` | \|a\| | a' · sign(a) |
| `Sqrt(a)` | √a | a' / (2 · √a) |
| `Exp(a)` | eᵃ | a' · eᵃ |
| `Ln(a)` | ln a | a' / a |
| `Log(a)` | log₁₀ a | a' / (a · ln 10) |
| `Sin(a)` | sin a | a' · cos a |
| `Cos(a)` | cos a | −a' · sin a |
| `Tan(a)` | tan a | a' / cos²(a) |
| `Hypot(a, b)` | √(a²+b²) | (a·a' + b·b') / √(a²+b²) |
| `Atan2(a, b)` | atan(a/b) | (a'·b − a·b') / (a²+b²) |
| `Pow(a, b)` | aᵇ | b·aᵇ⁻¹·a' + aᵇ·ln(a)·b' |
| `PhiNode` | φ(preds) | φ(pred derivatives) |

### Special case: `Pow` with `base == 0`

The term `aᵇ·ln(a)` is undefined when `a = 0` and `b` involves unknowns. The builder inserts a guard block:

```
if a == 0.0:
    d/du (Pow) = 0.0          ; ln(0) term vanishes, aᵇ⁻¹ term handled by limit
else:
    d/du (Pow) = b*a^(b-1)*a' + a^b * ln(a) * b'
```

The two paths are joined with a `PhiNode` in the merge block.

### Special case: cyclic `PhiNode`

For a loop with induction variable `x_next = phi(x_entry, x_body)`, the derivative is:

```
dx_next' = phi(x_entry', x_body')
```

The entry derivative is known (often 0 or 1 from the seed); the body derivative depends on the loop computation. The builder emits a placeholder `PhiNode` for `x_next'`, processes the loop body deriving `x_body'`, then patches the placeholder's operands.

---

## 10. Worked Example — `sin_second_order`

This example traces the `sin_second_order` test in `builder/tests.rs` end-to-end.

### Primal MIR (simplified)

```
v12 = fmul v10, v11      ; v10 is Unknown(0), v11 is a parameter
v13 = sin v12
v14 = call ddx(v13)      ; first-order ∂(v13)/∂v10
v15 = call ddx(v14)      ; second-order ∂²(v13)/∂v10²
v100 = optbarrier v15
```

`ddx_calls` tells the pass that both `call ddx` instructions differentiate with respect to `Unknown(0)` (associated with `v10`).

### Liveness analysis

Working backwards from `v100`:
- `optbarrier v15` → `v15` is live for derivative `d_x` (first order).
- `call ddx(v14)` consuming `v14` → `v14` is live for `d_x`.
- `call ddx(v13)` consuming `v13` → `v13` is live for `d_x`.
- `sin v12` → `v12` is live for `d_x` (needed for `cos v12`).
- `fmul v10, v11` → `v10` is the unknown itself; seed covers this.

For the second-order request, `v14 = call ddx(v13)` is itself a `ddx()` instruction, so the liveness for the outer `ddx` seeds `d_x` liveness at `v14`'s definition, which then propagates through `v13` again — this time with derivative `d_x²` (second order).

### Instruction insertion

The builder processes blocks in reverse postorder and emits (in the same block, after the primal instructions):

```
; First-order ∂(sin(v10·v11))/∂v10:
v101 = cos v12           ; cos(v10·v11)
v102 = fmul v11, v101    ; v11 · cos(v10·v11)   [= ∂(v13)/∂v10]

; Second-order ∂²(sin(v10·v11))/∂v10²:
v103 = sin v12           ; sin(v10·v11)
v104 = fneg v103         ; −sin(v10·v11)
v105 = fmul v11, v104    ; v11 · (−sin(v10·v11))
v106 = fmul v105, v11    ; v11² · (−sin(v10·v11))  [= ∂²(v13)/∂v10²]
```

The `optbarrier` operand is updated: `v100 = optbarrier v106`.

### Mathematical verification

Let `u = v10·v11`. Then:
- `v13 = sin(u)`
- `∂v13/∂v10 = cos(u) · v11` ✓ (`v102`)
- `∂²v13/∂v10² = −sin(u) · v11²` ✓ (`v106`)

Note that `cos v12` and `sin v12` are both emitted separately: the first-order rule for `sin` needs `cos`, and the second-order rule for `cos` needs `−sin`. `mir_opt` (GVN) will deduplicate these after the pass if the same value is already live.
