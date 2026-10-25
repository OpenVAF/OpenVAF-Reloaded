# `openvaf` — Compilation pipeline library

**Location:** `openvaf/openvaf/`
**Role:** The library crate that ties the entire compiler together. It exposes
two entry-point functions — `compile` and `expand` — plus the `Opts` struct
that carries every compilation option. The binary crate `openvaf-driver`
depends on this crate; it is also the natural integration point for any tool
that wants to embed the OpenVAF compiler.

Cross-links: [basedb INTERNALS](../basedb/INTERNALS.md) ·
[hir INTERNALS](../hir/INTERNALS.md) ·
[sim_back INTERNALS](../sim_back/INTERNALS.md) ·
[osdi INTERNALS](../osdi/INTERNALS.md) ·
[mir_llvm INTERNALS](../mir_llvm/INTERNALS.md) ·
[linker_target INTERNALS](../linker_target/INTERNALS.md) ·
[base_n INTERNALS](../base_n/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate layout

```
openvaf/openvaf/src/
  lib.rs     — Opts, CompilationDestination, CompilationTermination, compile(), expand()
  cache.rs   — cache file name derivation (MD5 hash → base-36 filename)
```

The crate has no `main.rs`; it is a `[lib]` that is called by `openvaf-driver`
and by the integration tests (`tests/integration.rs`, `harness = false`).

---

## Public types

### `Opts`

```rust
pub struct Opts {
    pub dry_run:        bool,
    pub defines:        Vec<String>,           // -D MACRO[=VALUE]
    pub codegen_opts:   Vec<String>,           // -C OPT[=VALUE] (passed to LLVM)
    pub lints:          Vec<(String, LintLevel)>,
    pub input:          Utf8PathBuf,           // root .va file
    pub output:         CompilationDestination,
    pub include:        Vec<AbsPathBuf>,       // -I directories
    pub opt_lvl:        LLVMCodeGenOptLevel,   // LLVM optimisation level (0–3)
    pub target:         Target,                // target triple
    pub target_cpu:     String,                // "native", "generic", or specific CPU
    pub dump_mir:       bool,                  // print optimised MIR to stdout
    pub dump_unopt_mir: bool,                  // print unoptimised MIR to stdout
    pub dump_ir:        bool,                  // print optimised LLVM IR to stdout
    pub dump_unopt_ir:  bool,                  // print unoptimised LLVM IR to stdout
}
```

`Opts` is `Clone` so the driver can stash a copy in the crash-report mutex
before compilation starts.

### `CompilationDestination`

```rust
pub enum CompilationDestination {
    Path  { lib_file:  Utf8PathBuf },   // explicit -o output path
    Cache { cache_dir: Utf8PathBuf },   // batchmode: content-addressed cache
}
```

### `CompilationTermination`

```rust
pub enum CompilationTermination {
    Compiled      { lib_file: Utf8PathBuf },
    FatalDiagnostic,                    // errors were emitted; caller should exit
}
```

`FatalDiagnostic` means the compiler already printed the errors to stderr via
`ConsoleSink`; the caller should exit with a non-zero code without printing
anything further.

---

## `compile` — the full pipeline

```rust
pub fn compile(opts: &Opts) -> Result<CompilationTermination>
```

This function is the top-level orchestration of the entire compiler. The steps
in order:

### 1. Resolve input and create the Salsa database

```rust
let input = opts.input.canonicalize()?;
let input = AbsPathBuf::assert(input);
let db = CompilationDB::new_fs(input, &opts.include, &opts.defines, &opts.lints)?;
```

`CompilationDB` is the Salsa incremental compilation database. It owns the
VFS, all parsed source, the HIR, type information, and every other
query-computed value. Building it registers the root file with the VFS and
sets up the preprocessor include path.

### 2. Cache check (batchmode only)

```rust
if let CompilationDestination::Cache { cache_dir } = &opts.output {
    let file_name = cache::file_name(&db, opts);
    let lib_file = cache_dir.join(file_name);
    if cfg!(not(debug_assertions)) && lib_file.exists() {
        return Ok(CompilationTermination::Compiled { lib_file });
    }
    create_dir_all(cache_dir)?;
}
```

In batchmode the output filename is a content-addressed hash of the input
(see [cache logic](#cache-logic) below). If the file already exists and this
is a release build, compilation is skipped entirely and the cached path is
returned. This is the only early exit in `compile`.

### 3. Module collection

```rust
let modules = collect_modules(&db, false, &mut ConsoleSink::new(&db))?;
```

`sim_back::collect_modules` runs the full frontend (preprocessor → parser →
HIR lowering → type checking → `sim_back` model extraction). It returns a
`Vec<Module>` — one entry per `module … endmodule` block. If any fatal
diagnostic is emitted the function returns `None` and `compile` returns
`FatalDiagnostic`.

### 4. LLVM backend initialisation

```rust
let back = LLVMBackend::new(&opts.codegen_opts, &opts.target, opts.target_cpu.clone(), &[]);
```

`LLVMBackend` initialises the LLVM target machine for the requested triple and
CPU. Codegen options (`-C` flags) are forwarded directly to LLVM.

If `opts.dry_run` is set, compilation returns here — the frontend ran
(catching any parse/type errors) but no object files are produced.

### 5. OSDI compilation

```rust
let (paths, compiled_modules, literals) = osdi::compile(
    &db, &modules, &lib_file, &opts.target, &back,
    /*emit_ir=*/true, opts.opt_lvl,
    opts.dump_mir, opts.dump_unopt_mir,
    opts.dump_ir,  opts.dump_unopt_ir,
);
```

`osdi::compile` runs MIR construction, optimisation, AD differentiation, LLVM
codegen, and writes one temporary object file per module per compilation unit.
The `dump_*` flags cause intermediate representations to be printed to stdout
at the relevant stages. `paths` is the list of temporary `.oN` object files
to link.

### 6. MIR dump (optional)

If `dump_mir` or `dump_unopt_mir` is set, the function prints module names,
the HIR string interner contents, and MIR for each compiled module using
`sim_back::print_module` and `sim_back::print_intern`.

### 7. Linking

```rust
link(None, &opts.target, lib_file.as_ref(), |linker| {
    for path in &paths { linker.add_object(path); }
})?;
```

The `linker::link` function writes the final `.osdi` shared library. The
closure adds each temporary object file to the linker command line.

### 8. Cleanup and timing

Temporary object files are deleted, then a green `Finished building … in Xs`
message is printed to stderr.

---

## `expand` — preprocessor-only mode

```rust
pub fn expand(opts: &Opts) -> Result<CompilationTermination>
```

Runs only the preprocessor (triggered by `--print-expansion`). It creates
`CompilationDB`, runs `cu.preprocess(&db)`, and prints each token's source
text to stdout — with a newline after line comments and a space after all
other tokens — approximating the expanded source. Diagnostics are collected
and summarised; if fatal errors are present `FatalDiagnostic` is returned.

---

## Cache logic (`cache.rs`)

```rust
pub fn file_name(db: &CompilationDB, opts: &Opts) -> String
```

Computes a deterministic cache filename for a given compilation. The filename
is `{hash}.osdi` where `hash` is the base-36 encoding of the MD5 digest (as a
`u128`) of:

| Input | Notes |
|-------|-------|
| `root_file().0.to_ne_bytes()` | File ID of the root `.va` file |
| `defines.len()` + each `define` string | Preprocessor macro definitions |
| `env!("CARGO_PKG_VERSION")` | Compiler version (invalidates cache on upgrade) |
| lint overwrite bytes | `LintLevel` values, cast to bytes via `slice::from_raw_parts` |
| All non-trivia preprocessor tokens | The source content after macro expansion, one `" "` separator per token |

The MD5 is computed over the **preprocessed** token stream (not the raw
source), so `\`include` files are transitively included in the hash. Two
compilations that produce identical preprocessed output and use the same
compiler version, defines, and lints will always share a cache entry.

The MD5 digest is reinterpreted as a `u128` via `u128::from_ne_bytes` and
encoded with `base_n::encode(hash, base_n::CASE_INSENSITIVE)` (base 36,
digits + lowercase letters), giving a 25-character filename that is safe on
case-insensitive filesystems.

> **TODO(verify):** The cache does not cover `opts.opt_lvl`, `opts.target`, or
> `opts.target_cpu`. Two compilations with different optimisation levels or
> target triples will produce the same cache filename if the source and defines
> match, and the second will find and return the first's output. This may be
> intentional (OSDI output is target-independent in practice) or a limitation.

---

## Re-exported items

`openvaf` re-exports several items from its dependencies to give `openvaf-driver`
a single import point:

| Re-export | Source |
|-----------|--------|
| `builtin_lints` | `basedb::lints::builtin` |
| `LintLevel` | `basedb::lints` |
| `LLVMCodeGenOptLevel` | `llvm_sys::target_machine` |
| `AbsPathBuf` | `paths` |
| `host_triple` | `target` |
| `get_target_names`, `Target` | `target::spec` |

---

## Integration tests (`tests/integration.rs`)

The integration test binary (`harness = false`) uses `mini_harness` to run one
test per directory in `integration_tests/`. Each test calls `compile` with
`dry_run = true`, checks that no fatal diagnostic was produced, and compares
the diagnostic output against a snapshot. This verifies the complete pipeline
from source text to compiled output without requiring a simulator.

---

## Key design decisions

**Library, not binary.** Exposing `compile` and `expand` as a library API
rather than inline in `main` makes the compiler embeddable and testable without
spawning a process. The integration tests call `compile` directly and inspect
`CompilationTermination` without any subprocess overhead.

**`CompilationTermination::FatalDiagnostic` instead of `Err`.** Fatal
diagnostics (type errors, undefined names, etc.) are not `anyhow::Error`s —
they have already been formatted and printed to stderr by `ConsoleSink`. The
`FatalDiagnostic` variant signals the caller to exit with a non-zero code
without double-printing the error. Only I/O errors (failed to read file, failed
to write output) propagate as `Err`.

**Cache keyed on preprocessed tokens, not raw source bytes.** Hashing the
preprocessed token stream rather than the raw bytes means that changes to
whitespace, comments, or `\`include` file structure that don't affect the
semantics don't invalidate the cache. Two `.va` files with different formatting
but identical token sequences produce the same cache entry.
