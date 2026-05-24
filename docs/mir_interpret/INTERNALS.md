# `mir_interpret` — MIR tree-walking interpreter

**Location:** `openvaf/mir_interpret/`
**Role:** A simple tree-walking interpreter for MIR `Function`s. Given a
function and a set of parameter values, it evaluates every instruction in
basic-block order and returns the final value of any result `Value`. Used
primarily in tests to numerically verify that auto-differentiation produces
correct derivatives.

Cross-links: [mir INTERNALS](../mir/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
mir_interpret   (openvaf/mir_interpret/)
  └─► mir_autodiff   (tests: numerical verification of AD output)
  (also listed as a dependency in sim_back, osdi, mir_build — unused in those
   crates' source at the time of writing)
```

The crate depends on `mir` (for `Function`, `Opcode`, `Value`, etc.),
`typed-index-collections` (for `TiVec`/`TiSlice`), and `lasso` (for `Spur`,
the interned string handle type).

---

## `Data` — the value type

```rust
#[repr(C)]
pub union Data {
    raw:   [u8; 8],
    float: f64,
    int:   i32,
    str:   Spur,
    bool:  bool,
}
```

`Data` is an 8-byte untagged union that can hold any of the four MIR primitive
types plus a raw byte view. There is no discriminant — the caller must know
which variant is active, just as in MIR itself (each `Value` has a statically
known type).

### The UNDEF sentinel

```rust
pub const UNDEF: Data = Data { raw: [u8::MAX; 8] };  // 0xFFFFFFFFFFFFFFFF
```

Uninitialized values (results of instructions not yet evaluated) are filled
with `UNDEF`. The all-ones bit pattern is a quiet NaN for `f64`, a non-zero
value for `i32`/`bool`, and a non-zero value for `Spur` (which uses a
`NonZeroU32` internally). This makes use-before-define distinguishable in
debug scenarios via `is_undef()`, though the interpreter does not enforce it at
runtime.

### Conversions

All conversions go through the union's field reads, guarded by `unsafe`:

| Direction | Safety argument |
|-----------|----------------|
| `f64 → Data` | `mut res = UNDEF; res.float = val` — `float` field is always written before read |
| `i32 → Data` | same pattern with `res.int` |
| `bool → Data` | same with `res.bool` |
| `Spur → Data` | same with `res.str` |
| `Data → f64` | all bit patterns are valid `f64` values |
| `Data → i32` | all bit patterns are valid `i32` values |
| `Data → bool` | all bit patterns are valid `bool` values |
| `Data → Spur` | `Spur` is a `NonZeroU32`; the conversion `assert`s the low 4 bytes are non-zero |

`From<mir::Const> for Data` bridges MIR compile-time constants into `Data`
values, handling all four `Const` variants (`Float`, `Int`, `Str`, `Bool`).

### `from_f64_slice`

```rust
pub fn from_f64_slice(data: &[f64]) -> &[Data] {
    unsafe { transmute(data) }
}
```

Zero-copy reinterpretation of a `&[f64]` as `&[Data]`. This is safe because:
- `Data` is `#[repr(C)]` and 8 bytes — the same size and alignment as `f64`.
- The values will only be read through the `float` field.

This is the primary way test code passes a batch of `f64` parameters to the
interpreter without allocating a separate `Vec<Data>`.

---

## `InterpreterState`

```rust
pub struct InterpreterState {
    vals:      TiVec<Value, Data>,  // current value for every Value in the function
    prev_bb:   Block,               // the basic block we came from (for phi resolution)
    next_inst: Option<Inst>,        // None means execution has finished
}
```

`vals` is indexed by `Value` and holds the current computed `Data` for each
SSA value. It is initialised in `Interpreter::new`:

- `ValueDef::Param(param)` — copied from the `args` slice.
- `ValueDef::Const(c)` — converted from `mir::Const` via `Data::from`.
- `ValueDef::Result(…)` and `ValueDef::Invalid` — set to `Data::UNDEF`.

`prev_bb` is updated on every jump or branch so that phi nodes can read the
correct incoming value.

`next_inst` starts as `func.layout.first_inst(entry_block)` and advances
instruction-by-instruction. Setting it to `None` terminates the main loop.

### `write` and `read`

```rust
pub fn write(&mut self, dst: Value, val: impl Into<Data>)
pub fn read<T: From<Data>>(&self, val: Value) -> T
```

These are the public accessors for external call handlers (see `Func<'a>`) to
read arguments and write results.

---

## `Interpreter`

```rust
pub struct Interpreter<'a> {
    pub state: InterpreterState,
    calls: &'a TiSlice<FuncRef, (Func<'a>, *mut c_void)>,
    func:  &'a Function,
}
```

`calls` maps each `FuncRef` in the function to a native Rust function pointer
plus a `*mut c_void` context pointer. This allows the interpreter to dispatch
`Call` instructions to host code without knowing the function bodies.

### Construction

```rust
Interpreter::new(func, calls, args)   // full constructor
Interpreter::test(func)               // shorthand: no calls, no params
```

`test` is used in unit tests that only exercise pure arithmetic MIR without
any external call dependencies.

### The `run` loop

```rust
pub fn run(&mut self) {
    while let Some(inst) = self.state.next_inst {
        self.eval(inst)
    }
}
```

`eval` advances `next_inst` before performing the computation (so early
returns from control-flow instructions are clean). The loop terminates when:
- An `Exit` instruction sets `next_inst = None`.
- The last instruction of a block falls off the end (unreachable in well-formed
  MIR — every block must end with a terminator).

---

## `eval` — instruction dispatch

`eval` matches on `InstructionData` variants:

### Control flow

| Variant | Action |
|---------|--------|
| `Branch { cond, then_dst, else_dst }` | Reads `vals[cond]` as `bool`; calls `jmp` to the appropriate block |
| `Jump { destination }` | Calls `jmp` unconditionally |
| `Exit` | Sets `next_inst = None` and returns |

`jmp` records `prev_bb` (the block of the current instruction) before
installing `first_inst(dst)` as `next_inst`.

### Phi nodes

```rust
InstructionData::PhiNode(ref phi) => {
    let val = func.dfg.phi_edge_val(phi, state.prev_bb).unwrap();
    let res = func.dfg.first_result(inst);
    state.vals[res] = state.vals[val];
    (Opcode::Phi, [].as_slice())
}
```

The phi node looks up the value corresponding to the predecessor block
(`prev_bb`) and copies it into the result. This requires that `jmp` always
records the source block before moving to the destination.

### External calls

```rust
InstructionData::Call { func_ref, ref args } => {
    let (fun, data) = calls[func_ref];
    let args = args.as_slice(&func.dfg.insts.value_lists);
    let rets = func.dfg.inst_results(inst);
    fun(&mut state, args, rets, data);
    state.next_inst = func.layout.next_inst(inst);
    return;
}
```

The call handler receives mutable access to `InterpreterState` (so it can
call `state.read`/`state.write`), the argument `Value` slice, the result
`Value` slice, and the opaque context pointer. It is responsible for writing
all result values before returning.

### Arithmetic and comparison opcodes

Every `Unary` and `Binary` opcode maps directly to a Rust expression on the
appropriate typed field:

```rust
Opcode::Fadd  => (args(0).f64() + args(1).f64()).into(),
Opcode::Imul  => (args(0).i32() * args(1).i32()).into(),
Opcode::Flt   => (args(0).f64() < args(1).f64()).into(),
Opcode::Sqrt  => f64::sqrt(args(0).f64()).into(),
Opcode::Clog2 => {
    let val = args(0).i32();
    let val = 8 * size_of_val(&val) as i32 - val.leading_zeros() as i32;
    val.into()
}
```

`args` is a local closure `let args = |i| state.vals[args[i]]` that reads from
the current `vals` table. The result is written to `state.vals[res]` after the
match.

Cast opcodes are also handled inline:

| Opcode | Semantics |
|--------|-----------|
| `FIcast` | `f64 as i32` (truncation) |
| `IFcast` | `i32 as f64` |
| `BIcast` | `bool as i32` (0 or 1) |
| `IBcast` | `i32 != 0` |
| `FBcast` | `f64.round() as i32` |
| `BFcast` | `bool as i32 as f64` |
| `OptBarrier` | identity (pass-through) |

String comparisons (`Seq`, `Sne`) compare `Spur` handles directly — because
`lasso` interns strings, equality of `Spur` values implies equality of the
underlying strings.

---

## `Func<'a>` — external call handler type

```rust
pub type Func<'a> = fn(&mut InterpreterState, &[Value], &[Value], *mut c_void);
```

- First argument: mutable state (to call `read`/`write`)
- Second argument: argument `Value` indices (caller reads them with `state.read(args[i])`)
- Third argument: result `Value` indices (caller writes them with `state.write(rets[i], val)`)
- Fourth argument: opaque context (used for closures that capture state as a raw pointer)

This is a raw function pointer, not a closure, so it is `Send`, has no
implicit lifetime, and can be stored in a `TiSlice` without boxing.

---

## Worked example: numerically verifying autodiff

`mir_autodiff/src/builder/tests.rs` uses the interpreter to check that
auto-differentiation produces numerically correct results. The pattern is:

```rust
fn check_num(src: &str, expected_ir: Expect, args: &[f64], expected_val: f64) {
    // 1. Parse MIR text into a Function
    let (mut func, _) = parse_function(src).unwrap();

    // 2. Run auto-differentiation (modifies func in place)
    auto_diff(&mut func, &dom_tree, &unknowns, &[]);

    // 3. Run the interpreter with the given float arguments
    let mut interp = Interpreter::new(
        &func,
        TiSlice::from_ref(&[]),                         // no external calls
        TiSlice::from_ref(Data::from_f64_slice(args)),  // params as Data
    );
    interp.run();

    // 4. Read the derivative result value (always Value(100) in these tests)
    let val: f64 = interp.state.read(100u32.into());

    // 5. Compare numerically with tolerance
    assert!(val.approx_eq(expected_val, margin));
}
```

`Data::from_f64_slice(args)` reinterprets the `&[f64]` test arguments as
`&[Data]` without copying. The interpreter evaluates the AD-augmented function
and the derivative result is read back as an `f64`. If it deviates from the
symbolic expectation by more than 10×ε, the test prints the MIR and fails.

---

## Key design decisions

**Untagged union over enum for `Data`.** An `enum { Float(f64), Int(i32), … }`
would cost an extra byte for the discriminant and require match arms everywhere.
Since MIR is typed (each `Value` has a statically known type, determined during
`hir_lower`), the interpreter already knows which field to read. The union
keeps each `Data` at exactly 8 bytes — the same size as `f64` — so
`from_f64_slice` can safely transmute.

**`UNDEF = [0xFF; 8]` rather than zero.** Zero is a valid, common value for
all four types (0.0, 0, false, and technically a null-like `Spur`). Using
all-ones makes use-before-define more visually obvious in debug output and is
safe for `f64` (a quiet NaN rather than zero or a meaningful number).

**`Func<'a>` as a raw fn pointer + `*mut c_void`.** Storing closures would
require boxing or lifetime-erased trait objects. The raw pointer design matches
the C ABI convention (function + context), avoids heap allocation per call
entry, and makes the call table a flat `TiSlice` without indirection.

**No type checking at runtime.** The interpreter trusts that the MIR is
well-typed (which it is, having been produced by `hir_lower` and verified by
`mir_opt`). Adding a runtime type tag to `Data` and checking it on every
opcode would double the overhead for a tool used only in tests.

**`prev_bb` on `InterpreterState` rather than `Interpreter`.** The call
handler `Func<'a>` receives `&mut InterpreterState`, not `&mut Interpreter`.
Keeping `prev_bb` on the state rather than on the outer struct makes the full
CFG context available to call handlers without exposing the rest of the
interpreter's internals.
