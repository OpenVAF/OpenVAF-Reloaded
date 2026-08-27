# `osdi` — Internals

LLVM codegen and OSDI 0.4 descriptor emission: the final compilation stage.

Related: [sim_back INTERNALS](../sim_back/INTERNALS.md) · [ARCHITECTURE](../ARCHITECTURE.md)

---

## 1. Purpose and Position

`osdi` receives the `CompiledModule` collection produced by `sim_back` and turns it into native object files. It has two responsibilities that are easy to conflate:

**Codegen** — for each module, generate four LLVM functions (`access`, `setup_model`, `setup_instance`, `eval`) by translating the MIR `Function`s from `CompiledModule` into LLVM IR, wiring runtime callbacks to stdlib implementations, and laying out instance and model data structs.

**Descriptor emission** — collect all per-module metadata (node count, parameter layout, Jacobian sparsity, function pointers, memory offsets) into a flat LLVM constant struct array `OSDI_DESCRIPTORS`, plus the well-known size sentinel `OSDI_DESCRIPTOR_SIZE`. A circuit simulator `dlopen`s the resulting shared library, reads these globals, and uses the function pointers and layout tables to call into the model.

The `linker` crate then links all object files into a single `.so`/`.dll`.

---

## 2. Module Map

| File | Role |
|---|---|
| `lib.rs` | `compile()` entry point; `OsdiTys` instantiation; `OsdiLimId`; exported-globals emission; `lltype` helper; `intern_names` |
| `compilation_unit.rs` | `OsdiModule`, `OsdiCompilationUnit`; `new_codegen`; `general_callbacks`; `print_callback` |
| `metadata/osdi_0_4.rs` | Generated: `OsdiTys`, `OsdiDescriptor`, `OsdiNode`, `OsdiJacobianEntry`, `OsdiParamOpvar`, `OsdiNoiseSource`, `OsdiNodePair`, `OsdiLimFunction` (Rust structs); all OSDI flag constants; `stdlib_bitcode()` |
| `metadata.rs` | `OsdiLimFunction` (interned form); re-exports `osdi_0_4` |
| `inst_data.rs` | `OsdiInstanceData` — LLVM struct for per-instance memory; `EvalOutput` enum; constant field index offsets |
| `model_data.rs` | `OsdiModelData` — LLVM struct for per-model memory |
| `access.rs` | `access_function()` — read/write model and instance parameters by integer ID |
| `setup.rs` | `setup_model()`, `setup_instance()` — parameter initialization kernels |
| `eval.rs` | `eval()` — main Newton-iteration kernel; gated by `CALC_*` flags |
| `load.rs` | `load_noise()`, `load_residual_*`, `load_jacobian_*` helper functions; `JacobianLoadType` |
| `noise.rs` | Noise contribution helpers used by `load_noise` |
| `bitfield.rs` | `is_flag_set`, `is_flag_set_mem`, `is_flag_unset` — inline bitfield helpers |

---

## 3. `compile()` Overview

```rust
pub fn compile<'a>(
    db: &'a CompilationDB,
    modules: &'a [ModuleInfo],
    dst: &'a Utf8Path,
    target: &'a Target,
    back: &'a LLVMBackend,
    emit: bool,
    opt_lvl: LLVMCodeGenOptLevel,
    ...
) -> (Vec<Utf8PathBuf>, Vec<CompiledModule<'a>>, Rodeo)
```

`compile()` runs in two phases.

**Phase 1 — sequential setup.** For each `ModuleInfo`, `CompiledModule::new` builds the complete sim_back output (MIR, DAE system, init/eval split). During this loop, any `CallBackKind::BuiltinLimit` callbacks are collected into a shared `lim_table` — a deduplicated set of `OsdiLimFunction` entries keyed by name and argument count.

`OsdiModule::new` then wraps each `CompiledModule`. The module's `sym` field is set to a base-n encoding of the module's UUID: `base_n::encode(module.info.module.uuid(db) as u128, CASE_INSENSITIVE)`. This symbol suffix is appended to every generated function name to avoid collisions in the linked library.

`intern_names` walks all parameter names, aliases, units, descriptions, and system-function aliases and pre-interns them into the shared `Rodeo` string table. This ensures string literals are deduplicated across modules before codegen begins.

**Phase 2 — parallel codegen.** `rayon_core::scope` spawns four tasks per module plus one shared descriptor task:

| Task (per module) | LLVM module name | Object file index | Function generated |
|---|---|---|---|
| Access | `access_{sym}` | `i*4` | `access_{sym}` |
| Setup model | `setup_model_{sym}` | `i*4+1` | `setup_model_{sym}` |
| Setup instance | `setup_instance_{sym}` | `i*4+2` | `setup_instance_{sym}` |
| Eval | `eval_{sym}` | `i*4+3` | `eval_{sym}` |

Each task calls `new_codegen` to create an `LLVMBackend`-owned LLVM module, loads the stdlib bitcode, constructs `OsdiTys` and `OsdiCompilationUnit`, generates its function, optimizes, and emits an object file.

The **descriptor task** (no rayon spawn — runs on the scope's thread) creates one more LLVM module, builds the `OsdiDescriptor` constant for each module by calling `cguint.descriptor(...)`, converts it to an LLVM constant via `descriptor.to_ll_val(&cx, &tys)`, then calls `cx.export_array` and `cx.export_val` to emit the well-known globals.

---

## 4. `OsdiTys` and the Stdlib Bitcode

Every LLVM module created for OSDI codegen loads a pre-compiled bitcode file:

```rust
pub fn stdlib_bitcode(target: &Target) -> &'static [u8] { ... }
```

`osdi/build.rs` compiles a C/LLVM-IR stdlib once per supported target triple and embeds the result as `include_bytes!` constants. The supported triples are: `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `x86_64-pc-windows-gnu`, `x86_64-apple-macosx10.15.0`, `aarch64-unknown-linux-gnu`, `aarch64-pc-windows-msvc`, `arm64-apple-macosx11.0.0`.

`new_codegen` loads the bitcode via `cx.include_bitcode(stdlib_bitcode(back.target()))`, then sets all non-declaration functions to `LLVMInternalLinkage` so they are not exported. It also looks up two constants that format-string helpers need — `EXP` (a table of floating-point exponents) and `FMT_CHARS` (a table of SI prefix characters) — and sets these to internal linkage as well.

`OsdiTys` is a bundle of LLVM struct types that mirror the OSDI 0.4 C structs. It is built by `OsdiTyBuilder::new(ctx, target_data)`, which constructs each type in dependency order:

```
OsdiLimFunction → OsdiSimParas → OsdiSimInfo
OsdiInitErrorPayload → OsdiInitError → OsdiInitInfo
OsdiNodePair → OsdiJacobianEntry → OsdiNode
OsdiParamOpvar → OsdiNoiseSource → OsdiDescriptor
```

`target_data` (an `LLVMTargetDataRef` created from `target.data_layout`) is needed to compute `OsdiInitErrorPayload`'s size, which must match the ABI size of `i32` on the target.

---

## 5. `OsdiModule` and `OsdiCompilationUnit`

```rust
pub struct OsdiModule<'a> {
    pub info: &'a ModuleInfo,
    pub dae_system: &'a DaeSystem,
    pub eval: &'a Function,
    pub intern: &'a HirInterner,
    pub init: &'a Initialization,
    pub model_param_setup: &'a Function,
    pub model_param_intern: &'a HirInterner,
    pub lim_table: &'a TiSet<OsdiLimId, OsdiLimFunction>,
    pub node_collapse: &'a NodeCollapse,
    pub sym: String,
}
```

`OsdiModule` is a pure reference wrapper around `CompiledModule`. It adds `sym` and `lim_table` (shared across all modules in the compilation).

```rust
pub struct OsdiCompilationUnit<'a, 'b, 'll> {
    pub db: &'a CompilationDB,
    pub inst_data: OsdiInstanceData<'ll>,
    pub model_data: OsdiModelData<'ll>,
    pub tys: &'a OsdiTys<'ll>,
    pub cx: &'a CodegenCx<'b, 'll>,
    pub module: &'a OsdiModule<'b>,
    pub lim_dispatch_table: Option<&'ll llvm_sys::LLVMValue>,
}
```

`OsdiCompilationUnit::new` constructs `OsdiInstanceData` and `OsdiModelData` (which build the LLVM struct types for the instance and model blobs), then — if this is the eval task and the module has `$limit` calls — creates an external `OSDI_LIM_TABLE` global that the eval function will index at runtime.

---

## 6. `OsdiInstanceData` and `OsdiModelData`

### Instance struct layout

The per-instance LLVM struct has a fixed prefix of `NUM_CONST_FIELDS = 8` fields at known indices, followed by variable-length parameter, cache-slot, and Jacobian-pointer fields:

| Index constant | Field | Purpose |
|---|---|---|
| `PARAM_GIVEN = 0` | bitfield | One bit per parameter: was it explicitly set? |
| `JACOBIAN_PTR_RESIST = 1` | `*f64` | Pointer into the simulator's resistive stamp array |
| `JACOBIAN_PTR_REACT = 2` | `*f64` | Pointer into the simulator's reactive stamp array |
| `NODE_MAPPING = 3` | `u32[num_nodes]` | Maps OSDI node index → simulator node index |
| `COLLAPSED = 4` | bitfield | One bit per collapsible pair: was it collapsed? |
| `TEMPERATURE = 5` | `f64` | Instance temperature |
| `CONNECTED = 6` | bitfield | One bit per port: is it connected? |
| `STATE_IDX = 7` | `u32` | Starting index in the simulator's `$limit` state array |

After these: cached eval outputs (written by `setup_instance`, read by `eval`), user instance parameters, and operating-point variable slots.

### `EvalOutput`

When generating `eval`, each MIR output value maps to one of four `EvalOutput` variants:

```rust
pub enum EvalOutput {
    Calculated(EvalOutputSlot),  // computed during eval, stored in instance struct
    Const(Const, PackedOption<EvalOutputSlot>), // LLVM constant; may also have a slot
    Param(Param),                // already in model struct (Param/Temperature/ParamSysFun kinds)
    Cache(CacheSlot),            // pre-computed by setup_instance, read from instance struct
}
```

### Model struct

`OsdiModelData` holds model-level parameters (all parameters, not just instance ones) plus `$mfactor` and other system function values. Its layout is built in `model_data.rs` using the same `lltype` helper.

---

## 7. The Four Generated Functions

Each function is emitted into its own LLVM module and compiled to a separate object file. The `OsdiDescriptor` stores a pointer to each.

### `access_{sym}(inst, model, param_id, flags) → *void`

The parameter read/write gateway. Signature:

```
fn(inst: *opaque, model: *opaque, param_id: u32, flags: u32) -> *opaque
```

Dispatches on the upper bits of `param_id` using a switch over three cases (`PARA_KIND_MODEL`, `PARA_KIND_INST`, `PARA_KIND_OPVAR`), then within each case uses a second switch on the lower bits to find the correct struct field via `LLVMBuildStructGEP2`. The `ACCESS_FLAG_SET` bit in `flags` selects write mode; `ACCESS_FLAG_INSTANCE` routes to the instance struct for op-vars. Returns a pointer to the field; the simulator performs the actual load or store.

### `setup_model_{sym}(model, sim_info, ret_flags, handle)`

Translates `model_param_setup` (the `Function` from `CompiledModule`) to LLVM IR, then writes each evaluated output into the model struct. Parameter bound violations emit `OsdiInitError` records via a runtime callback. The `sim_info` argument carries the `OsdiSimInfo` struct with simulator-provided system parameters.

### `setup_instance_{sym}(inst, model, sim_info, ret_flags, handle, node_mapping, connected)`

Translates `init.func` (the op-independent init `Function`). Writes evaluated cache-slot values into the instance struct. Also applies any initial-condition (`IC`) overrides from `sim_info`. For each `CollapseImplicitEquation` output: if the value is `TRUE`, marks the corresponding bit in `COLLAPSED` and updates `NODE_MAPPING` to merge nodes.

### `eval_{sym}(inst, model, sim_info, ret_flags)`

The Newton-iteration kernel. The `sim_info.flags` field is a bitmask gating which computations to run:

| Flag | Value | Effect |
|---|---|---|
| `CALC_RESIST_RESIDUAL` | 1 | Compute and store resistive residual |
| `CALC_REACT_RESIDUAL` | 2 | Compute and store reactive residual |
| `CALC_RESIST_JACOBIAN` | 4 | Compute and store resistive Jacobian entries |
| `CALC_REACT_JACOBIAN` | 8 | Compute and store reactive Jacobian entries |
| `CALC_NOISE` | 16 | Compute noise contributions |
| `CALC_OP` | 32 | Compute operating-point variables |
| `CALC_RESIST_LIM_RHS` | 64 | Compute resistive limiting correction RHS |
| `CALC_REACT_LIM_RHS` | 128 | Compute reactive limiting correction RHS |
| `ENABLE_LIM` | 256 | Limiting is active this Newton step |
| `INIT_LIM` | 512 | Initialize limit state |

The function translates `eval` (the `Function` from `CompiledModule`) via `mir_llvm`, materializing the `CALC_*` flag checks as LLVM conditional branches. Results are stored via the `JACOBIAN_PTR_RESIST`/`JACOBIAN_PTR_REACT` pointers in the instance struct (written at each Newton step by the simulator before calling `eval`) and via separate load-helper functions. Return flags `EVAL_RET_FLAG_LIM`, `EVAL_RET_FLAG_FATAL`, `EVAL_RET_FLAG_FINISH`, `EVAL_RET_FLAG_STOP` are written into `ret_flags`.

---

## 8. `OsdiDescriptor`

`OsdiDescriptor<'ll>` is a Rust struct with 46 fields. Selected fields and their sources:

| Field | Source |
|---|---|
| `name` | `module.info.module.name(db)` |
| `num_nodes` / `num_terminals` | `DaeSystem::unknowns` count of `KirchoffLaw` / port count |
| `nodes: Vec<OsdiNode>` | One entry per `KirchoffLaw` unknown: name, units, residual offsets in instance struct |
| `num_jacobian_entries` / `jacobian_entries` | `DaeSystem::jacobian` entries; each `OsdiJacobianEntry` = `OsdiNodePair` + `flags` |
| `num_collapsible` / `collapsible` | `NodeCollapse::pairs()` as `OsdiNodePair` values |
| `collapsed_offset` | Byte offset of `COLLAPSED` field in instance struct |
| `noise_sources` | `DaeSystem::noise_sources`; each `OsdiNoiseSource` = name + `OsdiNodePair` |
| `num_params` / `num_instance_params` / `num_opvars` | Counts from `ModuleInfo` |
| `param_opvar: Vec<OsdiParamOpvar>` | One entry per parameter and op-var: names, aliases, units, description, `flags` (type + kind bits), `len` for arrays |
| `node_mapping_offset` | Byte offset of `NODE_MAPPING` in instance struct |
| `jacobian_ptr_resist_offset` | Byte offset of `JACOBIAN_PTR_RESIST` in instance struct |
| `instance_size` / `model_size` | `LLVMABISizeOfType` of the instance/model LLVM structs |
| `access` / `setup_model` / `setup_instance` / `eval` | LLVM function value pointers (declared in the descriptor LLVM module as external) |
| `load_noise` / `load_residual_resist` / `load_jacobian_resist` / … | Pointers to the load-helper functions |
| `num_resistive_jacobian_entries` / `num_reactive_jacobian_entries` | From `DaeSystem::num_resistive` / `num_reactive` |
| `num_inputs` / `inputs` | `DaeSystem::model_inputs` as `OsdiNodePair` values |

`OsdiJacobianEntry::flags` encodes which of the four values (resist/react × const/computed) are non-zero:

```
JACOBIAN_ENTRY_RESIST_CONST = 1   // resist value is a compile-time constant
JACOBIAN_ENTRY_REACT_CONST  = 2   // react value is a compile-time constant
JACOBIAN_ENTRY_RESIST       = 4   // resist value present
JACOBIAN_ENTRY_REACT        = 8   // react value present
```

`OsdiParamOpvar::flags` encodes type and kind:

```
flags = PARA_TY_REAL(0) | PARA_TY_INT(1) | PARA_TY_STR(2)  — bits 1:0
      | PARA_KIND_MODEL(0<<30) | PARA_KIND_INST(1<<30) | PARA_KIND_OPVAR(2<<30)  — bits 31:30
```

`to_ll_val(&cx, &tys)` converts the Rust struct to a `ctx.const_struct(tys.osdi_descriptor, &fields)` LLVM constant, with embedded `const_arr_ptr` sub-arrays for nodes, jacobian entries, collapsible pairs, noise sources, and param/opvar entries.

---

## 9. Callback Resolution

`general_callbacks` maps each `CallBackKind` in `HirInterner::callbacks` to a `TiVec<FuncRef, Option<CallbackFun<'ll>>>`:

```rust
pub fn general_callbacks<'ll>(
    intern: &HirInterner,
    builder: &mut mir_llvm::Builder<'_, '_, 'll>,
    ret_flags: &'ll LLVMValue,
    handle: &'ll LLVMValue,
    simparam: &'ll LLVMValue,
) -> TiVec<FuncRef, Option<CallbackFun<'ll>>>
```

Selected mappings:

| `CallBackKind` | Resolution |
|---|---|
| `SimParam` | `simparam` stdlib function; state = `[simparam, handle, ret_flags]` |
| `SimParamOpt` | `simparam_opt` stdlib function; state = `[simparam]` |
| `SimParamStr` | `simparam_str` stdlib function |
| `Derivative(_)` / `NodeDerivative(_)` | `const_callback` returning `0.0` — if these survived to codegen, they were zero-valued |
| `Print { kind, arg_tys }` | `print_callback` (hand-built LLVM IR): `snprintf` into a heap buffer, then `osdi_log(handle, msg, flags)` |
| `SetRetFlag(flag)` | `set_ret_flag_fatal` / `set_ret_flag_finish` / `set_ret_flag_stop` stdlib functions; state = `[ret_flags]` |
| `ParamInfo`, `CollapseHint`, `BuiltinLimit`, `StoreLimit`, `LimDiscontinuity`, `Analysis`, noise, `TimeDerivative` | `None` — handled by their respective setup/eval generation paths, not as general callbacks |

`print_callback` generates a complete LLVM function inline: it calls `snprintf` twice (first to measure, then to write), allocates a heap buffer, handles errors via a phi node, then calls the `osdi_log` function pointer (loaded from the `osdi_log` global). The log level constants (`LOG_LVL_DEBUG` through `LOG_LVL_FATAL`) and `LOG_FMT_ERR` are OR'd into the flags argument.

### `$limit` dispatch table

When `eval` is built with `eval=true` and the module has `$limit` calls, `OsdiCompilationUnit::new` creates an external `OSDI_LIM_TABLE` array global. At runtime, the simulator links this with the actual limit function table emitted in the descriptor object. The eval function indexes into this table to dispatch user-defined limit functions by name.

---

## 10. Exported Globals

The descriptor LLVM module (the last object file, at index `modules.len() * 4`) exports:

| Symbol | Type | Value |
|---|---|---|
| `OSDI_DESCRIPTORS` | `OsdiDescriptor[N]` | Array of N descriptor structs, one per module |
| `OSDI_DESCRIPTOR_SIZE` | `u32` | `LLVMABISizeOfType(tys.osdi_descriptor)` — byte stride for simulators supporting multiple OSDI versions |
| `OSDI_NUM_DESCRIPTORS` | `u32` | N (number of modules) |
| `OSDI_VERSION_MAJOR` | `u32` | `0` |
| `OSDI_VERSION_MINOR` | `u32` | `4` |
| `OSDI_LIM_TABLE` | `OsdiLimFunction[M]` | Emitted only if `lim_table` is non-empty; M = number of unique `$limit` functions |
| `OSDI_LIM_TABLE_LEN` | `u32` | M |
| `osdi_log` | `*fn(handle, msg, flags)` | Initialized to null pointer; the simulator fills this in after `dlopen` so models can emit log messages |

A simulator loads the shared library, reads `OSDI_NUM_DESCRIPTORS`, then strides through `OSDI_DESCRIPTORS` by `OSDI_DESCRIPTOR_SIZE` bytes per entry. This stride-based iteration lets a simulator handle libraries compiled against a newer minor version without recompiling against the new header.
