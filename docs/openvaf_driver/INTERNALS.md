# `openvaf-driver` — CLI binary

**Location:** `openvaf/openvaf-driver/`
**Role:** The `openvaf` executable. Parses command-line arguments with `clap`,
translates them into an `Opts` struct, installs a crash-report panic hook, and
calls `openvaf::compile` or `openvaf::expand`. Contains no compiler logic of
its own — all compilation is delegated to the `openvaf` library crate.

Cross-links: [openvaf INTERNALS](../openvaf/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate layout

```
openvaf/openvaf-driver/src/
  main.rs        — entry point, global allocator, error printing
  cli_def.rs     — clap Command definition and flag name constants
  cli_process.rs — ArgMatches → Opts translation
  crash_report.rs — panic hook that writes a crash log to /tmp
```

`openvaf-driver` is a binary-only crate (no `[lib]`). Its only public surface
is the `openvaf` executable.

---

## `main.rs`

### Global allocator

```rust
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
```

`mimalloc` replaces the default system allocator for the process. This gives
measurable throughput improvements on multi-module compilations where many
small allocations are made by the Salsa query system.

### `ARGS` mutex

```rust
static ARGS: Mutex<Option<Opts>> = Mutex::new(None);
```

After `matches_to_opts` succeeds, the resolved `Opts` is stored here. The
crash reporter reads it in the panic hook to include the compilation arguments
in the crash log. The mutex is necessary because the panic hook runs on an
arbitrary thread.

### `main` flow

```
main()
  ├─ clap::main_command().get_matches()   — parse CLI
  ├─ crash_report::install_panic_handler()
  ├─ env_logger::init()                   — OPENVAF_LOG / OPENVAF_LOG_STYLE env vars
  └─ wrapped_main(matches)
        ├─ matches_to_opts(matches)       — may exit(0) for --lints / --supported-targets
        ├─ *ARGS.lock() = Some(opts)
        ├─ if --print-expansion → expand(&opts) → exit(0 or 65)
        ├─ if --dump-json → bail! (unimplemented)
        └─ compile(&opts) → print lib_file if Cache mode → exit(0 or 65)
```

Errors from `wrapped_main` are printed as a `clap`-style chain of red `error:`
lines, one per `anyhow` cause. The process then exits without a code (falls off
`main`, which exits 0 — the non-zero code is only set via `exit()` inside
`wrapped_main` for `FatalDiagnostic`).

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `65` (`DATA_ERROR`) | Fatal diagnostic — compiler errors in the input |
| non-zero from `anyhow` | I/O or configuration error (file not found, bad target, etc.) |

`65` is the POSIX `EX_DATAERR` code, indicating that the input data was
malformed. It is distinct from generic failure so that scripts can distinguish
"source has errors" from "the compiler itself failed."

---

## `cli_def.rs` — command definition

All flag names are `pub const &str` constants so `cli_process.rs` can refer to
them by name without string literals:

```rust
pub const INPUT: &str = "input";
pub const OUTPUT: &str = "output";
pub const DEFINE: &str = "define";
pub const INCLUDE: &str = "include";
pub const TARGET: &str = "target";
pub const OPT_LVL: &str = "opt_lvl";
// … etc.
```

`main_command()` builds a `clap::Command` with the following flags:

| Flag | Short | Type | Default | Notes |
|------|-------|------|---------|-------|
| `input` | — | FILE | required | Root `.va` file |
| `--output` / `-o` | `-o` | FILE | `{input}.osdi` | Conflicts with `--batch` |
| `--include` / `-I` | `-I` | DIR | — | Repeatable; directory must exist |
| `--define` / `-D` | `-D` | `MACRO[=VALUE]` | — | Repeatable |
| `--allow` / `-A` | `-A` | LINT | — | Repeatable |
| `--warn` / `-W` | `-W` | LINT | — | Repeatable |
| `--deny` / `-E` | `-E` | LINT | — | Repeatable |
| `--lints` | — | flag | false | Print lint list and exit |
| `--target` | — | TARGET | host triple | Must be one of `get_target_names()` |
| `--supported-targets` | — | flag | false | Print targets and exit |
| `--target_cpu` | — | CPU | `native`/`generic` | Passed to LLVM |
| `--opt_lvl` / `-O` | `-O` | 0–3 | `3` | LLVM optimisation level |
| `--codegen` / `-C` | `-C` | `OPT[=VALUE]` | — | Repeatable; forwarded to LLVM |
| `--batch` / `-b` | `-b` | flag | false | Batchmode (content-addressed cache) |
| `--cache-dir` | — | DIR | platform cache dir | Requires `--batch` |
| `--interface` / `-i` | `-i` | `OSDI` | `OSDI` | Output format; only OSDI is implemented |
| `--dry-run` | — | flag | false | Parse+typecheck only, no output |
| `--dump-mir` | — | flag | false | Print optimised MIR to stdout |
| `--dump-unopt-mir` | — | flag | false | Print unoptimised MIR to stdout |
| `--dump-ir` | — | flag | false | Print optimised LLVM IR to stdout |
| `--dump-unopt-ir` | — | flag | false | Print unoptimised LLVM IR to stdout |
| `--print-expansion` | — | flag | false | Run preprocessor only, print result |
| `--dump-json` | — | flag | false | Unimplemented; bails immediately |

Path arguments use custom `clap` `ValueParser`s that validate existence and
type (file vs. directory) at parse time, so argument errors are reported
before any compilation starts.

---

## `cli_process.rs` — `matches_to_opts`

`matches_to_opts(matches: ArgMatches) -> Result<Opts>` is the only public
function. It handles two early exits before constructing `Opts`:

```rust
if matches.get_flag(LINTS)             { print_lints();   exit(0) }
if matches.get_flag(SUPPORTED_TARGETS) { print_targets(); exit(0) }
```

**Output destination resolution:**

- `--batch` → `CompilationDestination::Cache`. Cache directory is taken from
  `--cache-dir` if given, otherwise from
  `directories_next::ProjectDirs::from("com", "semimod", "openvaf").cache_dir()` —
  the platform-appropriate user cache directory (`~/.cache/openvaf` on Linux,
  `%LOCALAPPDATA%\semimod\openvaf\cache` on Windows).
- No `--batch` → `CompilationDestination::Path`. Output path is `--output` if
  given, otherwise `{input}.osdi` (same directory as the input file).

**Target and CPU resolution:**

```rust
let host = host_triple();
let target = matches.get_one(TARGET).cloned().unwrap_or_else(|| host.to_owned());
let default_cpu = if host != target { "generic" } else { "native" };
```

Cross-compilation defaults to `"generic"` CPU to avoid emitting host-specific
instructions in the output. Native compilation defaults to `"native"` for best
performance.

**`print_lints` / `print_targets`:** Print to stdout with colour: errors in
red, warnings in yellow, allowed lints in green; targets in yellow. Each exits
with code 0.

---

## `crash_report.rs` — panic handler

`install_panic_handler()` replaces the default Rust panic handler with a
custom hook that:

1. Extracts the panic message from the `PanicHookInfo` payload (tries `&str`
   then `String`).
2. Records the panic location (file + line number if available).
3. Appends a symbolicated backtrace using `backtrace` + `backtrace_ext`'s
   `short_frames_strict` (deduplicates inlined frames).
4. Prepends the OpenVAF version and the current `ARGS` (the `Opts` that were
   being compiled, from the global mutex).
5. Writes everything to a timestamped file in the system temp directory:
   `openvaf-crash-{unix_timestamp}.log`.
6. Prints a red error message to stderr naming the log file and asking the
   user to file an issue.

The hook is **not installed** in debug builds (`cfg(debug_assertions)`), so
development panics surface normally with the default Rust handler.

The `Report` struct accumulates the log text in a `String` using `write!` /
`writeln!`. If writing to disk fails, the report is printed to stderr instead
and `handle_dump` returns `None`.

---

## Key design decisions

**`mimalloc` as the global allocator.** The Salsa incremental database and the
MIR make many small, short-lived allocations. `mimalloc` is significantly
faster than the system allocator (glibc `malloc`, Windows `HeapAlloc`) for
this allocation pattern. It is opt-in here at the binary level so the library
crate remains allocator-agnostic.

**`ARGS` mutex for crash reporting.** The panic hook must access the `Opts`
that were being compiled to include them in the crash log. Storing `Opts` in a
`static Mutex<Option<Opts>>` is the simplest way to make it available to the
hook without thread-local storage or `Arc` threading. The mutex is only
contended if a panic occurs while another thread holds it, which cannot happen
in the current single-threaded compilation model.

**Early exit for `--lints` and `--supported-targets`.** These informational
flags exit immediately inside `matches_to_opts` rather than returning a special
`CompilationTermination` variant. This keeps the `Opts` type simple (no
`ListLints` or `ListTargets` variant) at the cost of making `matches_to_opts`
impure (it can `exit`). The trade-off is acceptable for a CLI binary where
these paths don't need to be testable in isolation.

**Custom path validators in `clap`.** Validating that input files exist and
output directories are writable at argument-parse time gives the user a clean
error before any compiler work starts. Without this, a missing include
directory would surface as a VFS error deep inside Salsa, with a less
informative message.
