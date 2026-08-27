# `mir_llvm` — Internals

The LLVM codegen layer for OpenVAF's MIR.

---

## 1. Purpose and Position

`mir_llvm` translates a fully-optimised MIR `Function` (see [`docs/mir/INTERNALS.md`](../mir/INTERNALS.md)) into LLVM IR and then to a native object file. It sits at the very bottom of the pipeline, called by [`osdi`](../osdi/INTERNALS.md) for each of the four generated functions per Verilog-A module.

The crate has a narrow mandate:

- It owns the LLVM context, module, and target machine.
- It maps each MIR opcode to the corresponding LLVM builder call.
- It provides helpers for declaring globals, emitting constant values, linking bitcode, running the pass manager, and writing the object file.

It does **not** implement optimisation passes (those run inside `mir_opt` on the MIR), define OSDI struct layouts (that is `osdi`'s job), or assign semantic meaning to opcodes (those are fixed by the MIR spec).

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | `LLVMBackend`, `ModuleLlvm`, `LLVMString`; target machine creation; utility fns |
| `context.rs` | `CodegenCx<'a,'ll>` — per-module codegen context |
| `builder.rs` | `Builder<'a,'cx,'ll>`, `BuilderVal`, `MemLoc` — per-function instruction emitter |
| `types.rs` | `Types<'ll>` primitive type table; `const_*` / `ty_*` helpers on `CodegenCx` |
| `declarations.rs` | `declare_ext_fn`, `declare_int_fn`, `define_global`, `export_val`, `export_array`, … |
| `callbacks.rs` | `CallbackFun`, `BuiltCallbackFun`, `InlineCallbackBuilder` |
| `intrinsics.rs` | `intrinsic()` on `CodegenCx`; maps opcode names to LLVM intrinsics / libm symbols |

---

## 3. Lifetime Hierarchy

Three separate lifetime parameters appear throughout the crate. Understanding them avoids confusion when reading `Builder` signatures.

```
't  —  the Target spec (essentially 'static; valid for the entire process)
'a  —  the codegen session: the Rodeo literal table, the Target ref
'll —  the LLVM module: every &'ll LLVMValue and &'ll LLVMType is valid
        exactly as long as the owning ModuleLlvm is alive
'cx —  the CodegenCx borrow inside a Builder (subset of 'a + 'll)
```

`Builder<'a, 'cx, 'll>` borrows:
- `'cx: 'a + 'll` — a `&'a CodegenCx<'cx, 'll>`,
- `'a` — the MIR `Function` and the LLVM builder handle,
- `'ll` — all produced `&'ll LLVMValue` / `&'ll LLVMBasicBlock` refs.

Because `'ll` is tied to `ModuleLlvm`, all generated values are automatically invalidated when the module is dropped.

---

## 4. `LLVMBackend` and `ModuleLlvm`

### `LLVMBackend<'t>`

```rust
pub struct LLVMBackend<'t> {
    target: &'t Target,
    target_cpu: String,
    features: String,
}
```

The entry point for the codegen session. Constructed once per compilation unit with `LLVMBackend::new(cg_opts, target, target_cpu, target_features)`. The constructor resolves `"native"` CPU/features by calling `LLVMGetHostCPUName` / `LLVMGetHostCPUFeatures`, then merges in `target.options.features` and any extra `target_features` strings.

Two factory methods create the per-module objects:

```rust
pub unsafe fn new_module(&self, name: &str, opt_lvl: LLVMCodeGenOptLevel) -> Result<ModuleLlvm, LLVMString>
pub unsafe fn new_ctx<'a, 'll>(&'a self, literals: &'a Rodeo, module: &'ll ModuleLlvm) -> CodegenCx<'a, 'll>
```

### `ModuleLlvm`

```rust
pub struct ModuleLlvm {
    llcx: LLVMContextRef,
    llmod_raw: LLVMModuleRef,
    tm: LLVMTargetMachineRef,
    opt_lvl: LLVMCodeGenOptLevel,
}
```

Owns the LLVM context (`llcx`) and module (`llmod_raw`) so their lifetimes are tied together. The `Drop` impl calls `LLVMContextDispose` and `LLVMDisposeTargetMachine`.

Key methods:

**`include_bitcode(bitcode: &[u8])`** — parses a slice of LLVM bitcode bytes into a new in-memory module, then links it into `self` with `LLVMLinkModules2`. This is how the embedded stdlib (math helpers, `osdi_log` glue) is merged into each generated module.

**`optimize()`** — runs LLVM's new pass manager via `LLVMRunPasses`. The pipeline string is derived from `opt_lvl`:

| `LLVMCodeGenOptLevel` | pipeline string |
|---|---|
| `LLVMCodeGenLevelNone` | `"default<O0>"` |
| `LLVMCodeGenLevelLess` | `"default<O1>"` |
| `LLVMCodeGenLevelDefault` | `"default<O2>"` |
| `LLVMCodeGenLevelAggressive` | `"default<O3>"` |

**`emit_object(dst: &Path)`** — calls `LLVMTargetMachineEmitToFile` with `LLVMObjectFile` to write a native `.o`.

**`verify()` / `verify_and_print()`** — wraps `LLVMVerifyModule`; useful during development and testing.

**`to_str()`** — calls `LLVMPrintModuleToString`; returns the textual LLVM IR as an `LLVMString`. Used in tests.

The target machine is always created with `LLVMRelocPIC` (position-independent code) and `LLVMCodeModelDefault`, matching the requirements of a shared library output.

---

## 5. `CodegenCx`

```rust
pub struct CodegenCx<'a, 'll> {
    pub llmod: &'ll LLVMModule,
    pub llcx: &'ll LLVMContext,
    pub target: &'a Target,
    pub literals: &'a Rodeo,
    str_lit_cache: RefCell<AHashMap<Spur, &'ll Value>>,
    pub(crate) intrinsics: RefCell<AHashMap<&'static str, (&'ll Type, &'ll Value)>>,
    pub(crate) local_gen_sym_counter: Cell<u32>,
    pub(crate) tys: Types<'ll>,
}
```

`CodegenCx` is the per-module shared state used by every `Builder`. It holds:

- The LLVM module and context refs (borrowed from `ModuleLlvm`).
- The `Rodeo` literal table from the HIR layer, needed to resolve `Const::Str` values.
- `str_lit_cache` — deduplicates string literal globals. The first time a `Spur` is requested, `const_str()` creates an internal-linkage global `[N x i8]` constant and caches it; subsequent references return the same global.
- `intrinsics` — lazily populated by `intrinsic()` (see §12).
- `local_gen_sym_counter` — a monotonic counter used by `generate_local_symbol_name(prefix)`, which produces names like `"str.0"`, `"cb.3"`, `"arr.7"` for internal symbols.
- `tys: Types<'ll>` — the primitive type table (see §6).

`include_bitcode()` delegates to `ModuleLlvm`'s own method through the stored module pointer.

---

## 6. `Types` and Constant Helpers

`Types<'ll>` is a plain struct that pre-builds the primitive LLVM types once and caches them as `&'ll Type` references:

| Field | LLVM type |
|---|---|
| `double` | `double` (f64) |
| `char` | `i8` |
| `int` | `i32` |
| `size` | `iN` where N = `target.pointer_width` |
| `ptr` | `i8*` (opaque pointer in address space 0) |
| `fat_ptr` | `{ i8*, i64 }` (pointer + metadata word) |
| `bool` | `i1` |
| `void` | `void` |
| `null_ptr_val` | `null` constant of type `i8*` |

`CodegenCx` exposes these through `ty_double()`, `ty_int()`, `ty_ptr()`, etc., plus `ty_aint(bits)` for arbitrary-width integers, `ty_struct(name, elems)`, `ty_func(args, ret)`, and `ty_variadic_func(args, ret)`.

The `const_*` family on `CodegenCx` covers the common constant types:

```rust
const_real(f64)          // LLVMConstReal
const_int(i32)           // LLVMConstInt(i32, signed)
const_unsigned_int(u32)  // LLVMConstInt(i32, signed)  [same underlying call]
const_isize(isize)       // LLVMConstInt(iN)
const_bool(bool)         // LLVMConstInt(i1)
const_c_bool(bool)       // LLVMConstInt(i8)
const_u8(u8)             // LLVMConstInt(i8)
const_arr(elem_ty, vals) // LLVMConstArray2
const_struct(ty, vals)   // LLVMConstNamedStruct
const_null_ptr()         // cached null i8*
const_undef(ty)          // LLVMGetUndef
```

`const_val(&Const)` dispatches over `mir::Const` variants (`Float`, `Int`, `Bool`, `Str`) to the appropriate helper.

`declarations.rs` adds higher-level helpers:

- `declare_ext_fn(name, fn_type)` — C calling convention, no unnamed-address.
- `declare_int_fn(name, fn_type)` — FastCall convention, internal linkage, global-unnamed-addr (addresses are never significant; enables merging).
- `declare_int_c_fn(name, fn_type)` — C calling convention, internal linkage.
- `define_global(name, ty)` — adds a named global; returns `None` if already defined.
- `define_private_global(ty)` — unnamed global with private linkage.
- `export_val(name, ty, val, is_const)` — external linkage + DLLExport storage class; used for `OSDI_DESCRIPTORS` et al.
- `export_array(name, elem_ty, vals, is_const, add_cnt)` — array global; if `add_cnt`, also exports `<name>.cnt` as a `size_t`.
- `const_arr_ptr(elem_ty, vals)` — an internal-linkage immutable array global; returns a pointer to it.

---

## 7. `BuilderVal` and `MemLoc`

MIR SSA values do not map one-for-one to LLVM IR values: some values live in memory and must be loaded before use. `BuilderVal` represents this:

```rust
pub enum BuilderVal<'ll> {
    Undef,                         // not yet defined (initialiser for the values table)
    Eager(&'ll LLVMValue),         // already an LLVM IR value
    Load(Box<MemLoc<'ll>>),        // must be loaded with LLVMBuildLoad2
}
```

`BuilderVal::get(&builder)` materialises the value: for `Eager` it is a direct return; for `Load` it emits a `load` instruction at the current insertion point.

`MemLoc<'ll>` is a GEP descriptor:

```rust
pub struct MemLoc<'ll> {
    pub ptr: &'ll LLVMValue,       // base pointer
    pub ptr_ty: &'ll LLVMType,     // type of the pointed-to aggregate
    pub ty: &'ll LLVMType,         // type of the field being accessed
    pub indices: Box<[&'ll LLVMValue]>,
}
```

`MemLoc::struct_gep(ptr, ptr_ty, ty, idx, cx)` is the convenience constructor for a single-field struct access. `to_ptr()` emits a `getelementptr` to compute the field address; `read()` follows with a `load`.

The primary use case is Jacobian pointer slots: `osdi` populates `builder.params` with `BuilderVal::Load` entries that point into the instance data struct. When `build_inst` encounters a `Param` value, `build_consts()` has already copied `params[p]` into `values[v]`, so the load is emitted lazily on first use.

---

## 8. Callback Protocol

MIR `Call` instructions reference a `FuncRef` — an abstract handle, not a concrete symbol. The mapping from `FuncRef` to an actual LLVM function is resolved by the caller (i.e., `osdi`) before `build_func()` is invoked. The resolution is stored in `Builder::callbacks: TiVec<FuncRef, Option<CallbackFun<'ll>>>`.

```rust
pub enum CallbackFun<'ll> {
    Prebuilt(BuiltCallbackFun<'ll>),
    Inline { builder: Box<dyn InlineCallbackBuilder<'ll>>, state: Box<[&'ll LLVMValue]> },
}

pub struct BuiltCallbackFun<'ll> {
    pub fun_ty: &'ll LLVMType,
    pub fun: &'ll LLVMValue,
    pub state: Box<[&'ll LLVMValue]>,
    pub num_state: u32,
}
```

For `Prebuilt`: the builder prepends `state` values to the MIR-supplied arguments and emits `LLVMBuildCall2`. If `num_state > 0`, it indicates that `state` contains `num_state` entries per call *instance*: the builder iterates `state.len() / num_state` times, calling the function once per slice — used for multi-instance Jacobian store callbacks.

For `Inline`: `InlineCallbackBuilder::build_inline(builder, state)` is called to emit the required instructions directly, without producing a separate function. The `state` values are pre-bound closure arguments. This is used for the `$limit` dispatch table in the OSDI backend.

If a `FuncRef` is absent from `callbacks` (i.e., `None`), the `Call` instruction is silently dropped. This makes it safe to lower a function that contains callbacks not needed in the current context (e.g., an `init` function that ignores Jacobian callbacks).

`CodegenCx` also provides callback factories:
- `const_callback(args, val)` — synthesises an internal function that ignores all arguments and returns `val`.
- `trivial_callbacks(args)` — synthesises a no-op `void` function (used to zero out callbacks that are not relevant).
- `const_return(args, idx)` — synthesises a function that returns its `idx`-th argument unchanged.

---

## 9. `build_func()` — the Translation Walk

`Builder::build_func()` translates the entire MIR `Function` into LLVM IR in three steps.

**Step 0 — entry block and basic block allocation.** `Builder::new()` pre-allocates one `LLVMBasicBlock` per MIR block using `LLVMAppendBasicBlockInContext`. It also allocates a synthetic entry block, positions the builder there, and optionally allocates a stack slot (`LLVMBuildAlloca`) for the return value if the function is non-void.

**Step 1 — constant seeding.** `build_consts()` iterates `func.dfg.values()`. For each `ValueDef::Const(c)`, it calls `cx.const_val(&c)` and stores the result as `BuilderVal::Eager`. For each `ValueDef::Param(p)`, it copies `params[p]` (which was filled in by the caller). `ValueDef::Result` values are left as `Undef` until the defining instruction is processed.

**Step 2 — block iteration.** `build_func()` computes a `ControlFlowGraph`, collects a postorder traversal, reverses it to obtain RPO (dominators before uses), and calls `build_bb(bb)` for each block. Inside `build_bb`, every instruction in the block is passed to `build_inst(inst, fast_math_mode)`. The fast-math mode is determined by the instruction's `srcloc`: a negative source location flags `FastMathMode::Partial` (see §11).

**Step 3 — phi operand fix-up.** `PhiNode` instructions are emitted during the forward pass as empty `LLVMBuildPhi` placeholders added to `unfinished_phis`. After all blocks are processed, the outer loop iterates `unfinished_phis` and calls `LLVMAddIncoming` to wire each phi's predecessor values, which are all known by this point.

The entry block's only instruction is an unconditional branch to the MIR entry block (`LLVMBuildBr`). This matches the LLVM convention that `alloca`s must live in the function entry block.

---

## 10. Opcode-to-LLVM Dispatch

`build_inst` first matches on `InstructionData` variant to handle structural cases, then dispatches on `Opcode` for computation opcodes.

### Structural cases (handled first)

| `InstructionData` variant | LLVM emission |
|---|---|
| `Branch { cond, then_dst, else_dst }` | `LLVMBuildCondBr` |
| `Jump { destination }` | `LLVMBuildBr` |
| `Exit` | `ret void` if `return_void`, else load `ret_allocated` + `ret` |
| `PhiNode(phi)` | `LLVMBuildPhi` placeholder; deferred operand fill (§9) |
| `Call { func_ref, args }` | dispatch through `builder.callbacks[func_ref]` (§8) |

### Computation opcodes (`Unary` / `Binary`)

| MIR opcode | LLVM instruction / intrinsic |
|---|---|
| `Iadd` | `LLVMBuildAdd` |
| `Isub` | `LLVMBuildSub` |
| `Imul` | `LLVMBuildMul` |
| `Idiv` | `LLVMBuildSDiv` |
| `Irem` | `LLVMBuildSRem` |
| `Ishl` | `LLVMBuildShl` |
| `Ishr` | `LLVMBuildLShr` (logical right shift) |
| `Ixor` | `LLVMBuildXor` |
| `Iand` | `LLVMBuildAnd` |
| `Ior` | `LLVMBuildOr` |
| `Ineg` | `LLVMBuildNeg` |
| `Inot` / `Bnot` | `LLVMBuildNot` |
| `Fadd` | `LLVMBuildFAdd` |
| `Fsub` | `LLVMBuildFSub` |
| `Fmul` | `LLVMBuildFMul` |
| `Fdiv` | `LLVMBuildFDiv` |
| `Frem` | `LLVMBuildFRem` |
| `Fneg` | `LLVMBuildFNeg` |
| `Ilt`/`Igt`/`Ile`/`Ige` | `LLVMBuildICmp` (SLT/SGT/SLE/SGE) |
| `Ieq`/`Beq`/`Ine`/`Bne` | `LLVMBuildICmp` (EQ/NE) |
| `Flt`/`Fgt`/`Fle`/`Fge` | `LLVMBuildFCmp` (OLT/OGT/OLE/OGE) |
| `Feq`/`Fne` | `LLVMBuildFCmp` (OEQ/ONE) |
| `IFcast` | `LLVMBuildSIToFP` → `double` |
| `BFcast` | `LLVMBuildUIToFP` → `double` |
| `BIcast` | `LLVMBuildIntCast2` → `i32` |
| `IBcast` | `LLVMBuildICmp(NE, val, 0)` |
| `FBcast` | `LLVMBuildFCmp(ONE, val, 0.0)` |
| `FIcast` | `llvm.lround.i32.f64` (round-to-nearest-integer) |
| `Sqrt` | `llvm.sqrt.f64` |
| `Exp` | `llvm.exp.f64` |
| `Ln` | `llvm.log.f64` |
| `Log` | `llvm.log10.f64` |
| `Sin` | `llvm.sin.f64` |
| `Cos` | `llvm.cos.f64` |
| `Pow` | `llvm.pow.f64` |
| `Floor` | `llvm.floor.f64` |
| `Ceil` | `llvm.ceil.f64` |
| `Clog2` | `llvm.ctlz(val, true)` then `LLVMBuildSub(32, ctlz)` |
| `Tan` | `tan` (libm) |
| `Hypot` | `hypot` (Linux) / `_hypot` (Windows) |
| `Asin`/`Acos`/`Atan`/`Atan2` | `asin`/`acos`/`atan`/`atan2` (libm) |
| `Sinh`/`Cosh`/`Tanh` | `sinh`/`cosh`/`tanh` (libm) |
| `Asinh`/`Acosh`/`Atanh` | `asinh`/`acosh`/`atanh` (libm) |
| `Seq` | `strcmp(a, b) == 0` |
| `Sne` | `strcmp(a, b) != 0` |
| `OptBarrier` | transparent passthrough (returns `values[args[0]]` unchanged) |

The `Hypot` Windows special case (`_hypot`) is the only target-conditional path in the entire opcode table; it checks `target.options.is_like_windows`.

`OptBarrier` deserialises to a no-op: it exists only to prevent MIR optimisation passes from folding across the barrier. By the time we reach codegen, its operand is simply forwarded.

---

## 11. Fast-Math Signaling

LLVM's fast-math flags permit IEEE-754 relaxations that enable vectorisation and constant folding of floating-point expressions. OpenVAF applies them selectively rather than globally.

The signal is encoded in the `srcloc` field of the MIR instruction. A **negative** source location value is the convention for "this instruction was synthesised for performance and may be optimised aggressively." When `build_bb` encounters such an instruction it passes `FastMathMode::Partial`; otherwise `FastMathMode::Disabled`.

```rust
let fast_math = self.func.srclocs.get(inst).map_or(false, |loc| loc.0 < 0);
```

After emitting the LLVM instruction, `build_inst` sets the flags:

| Mode | `LLVMSetFastMathFlags` value | Semantics |
|---|---|---|
| `Partial` | `0x01 \| 0x02 \| 0x10` | Reassoc \| Reciprocal \| Contract |
| `Full` | `0x1F` | All flags (defined but not currently used by `build_bb`) |
| `Disabled` | (not called) | strict IEEE-754 |

Only float and transcendental opcodes (`Fadd`, `Fsub`, `Fmul`, `Fdiv`, `Frem`, `Fneg`, comparisons, and all math functions) receive the annotation; integer opcodes ignore it.

---

## 12. Intrinsics and libm

`CodegenCx::intrinsic(name)` is the single lookup point for both LLVM intrinsics and C library symbols. It checks `self.intrinsics` (the `RefCell<AHashMap>`) first; on a miss it uses the `ifn!` macro to declare the function and populate the cache.

LLVM intrinsics (e.g., `llvm.sqrt.f64`) are declared with `declare_ext_fn`; they are resolved by the LLVM backend without any external symbol. C library functions (e.g., `tan`, `atanh`) are also declared as `extern` with C calling convention — the linker resolves them from the system libm when the final shared library is linked.

The `snprintf` entry is variadic:

```rust
if name == "snprintf" {
    return Some(self.insert_intrinsic("snprintf", &[t_str, t_isize, t_str], t_i32, true));
}
```

This is used by `osdi`'s `print_callback` to format diagnostic messages before passing them to `osdi_log`.

---

## 13. Worked Example — Resistor `eval`

Starting from the resistor Verilog-A:

```verilog
V(a,b) <+ R * I(a,b);
```

After `hir_lower`, `sim_back` AD, and `mir_opt`, the `eval` function body contains (simplified) MIR:

```
; params: p0 = R (model param), p1 = I(a,b) (branch current)
v10 = fmul p0, p1        ; R * I(a,b)
v11 = fadd v10, v12      ; add to existing residual accumulator
exit
```

`osdi` calls `Builder::new(cx, &eval_func, llfunc, None, true)` (returns void). Before calling `build_func()` it:

1. Populates `builder.params`: `params[0] = BuilderVal::Load(MemLoc for R field)`, `params[1] = BuilderVal::Eager(branch_current_val)`.
2. Populates `builder.callbacks` with the resolved Jacobian store callbacks.
3. Sets `builder.ret_store_ptr` to the flags output pointer.

`build_consts()` copies `params[0]` and `params[1]` into `values[p0]` and `values[p1]`.

`build_func()` processes the single block. For `fmul p0, p1`:

1. `values[p0].get(builder)` emits `load double, ptr %R_ptr` (because it is `BuilderVal::Load`).
2. `values[p1].get(builder)` returns the eager branch current value directly.
3. `LLVMBuildFMul` emits `%v10 = fmul double %R_loaded, %branch_current`.

For `fadd v10, v12`: both operands are `Eager`; `LLVMBuildFAdd` emits `%v11 = fadd double %v10, %v12`.

For `exit` with `return_void = true`: `ret_void()` stores the return flags and emits `ret void`.

The resulting LLVM IR fragment (as `ModuleLlvm::to_str()` would show) is:

```llvm
define internal fastcc void @eval.0(ptr %inst, ptr %model, ...) {
entry:
  br %bb0
bb0:
  %R_loaded = load double, ptr %R_ptr
  %v10 = fmul double %R_loaded, %branch_current
  %v11 = fadd double %v10, %acc
  store i32 %flags, ptr %flags_out
  ret void
}
```

After `ModuleLlvm::optimize()`, the inlined `load` and arithmetic may be folded further or vectorised depending on the opt level. `emit_object()` then writes the native object file.
