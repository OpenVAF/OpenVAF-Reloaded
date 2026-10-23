# `mini_harness` — Custom libtest-compatible data-driven test harness

**Location:** `lib/mini_harness/`
**Role:** A minimal custom test runner that replaces Rust's built-in `libtest`
harness for data-driven tests. It discovers test cases from directories or
lists at runtime, supports the same CLI flags as `cargo test`, catches panics,
and prints output in libtest's `pretty`/`terse` formats. All test execution is
sequential on the main thread.

Cross-links: [ARCHITECTURE](../../ARCHITECTURE.md)

---

## Why a custom harness?

Rust's `#[test]` attribute requires `libtest` as the test runner. `libtest`
discovers tests at compile time; it cannot scan a directory for test files at
runtime. Data-driven tests in OpenVAF need to read `.va` source files from
`integration_tests/` and `integration_tests/data/` at runtime and create one
test case per file.

Setting `harness = false` in a `[[test]]` Cargo.toml section tells Cargo to
compile the test binary without injecting `libtest` and to call the binary's
own `main` function instead. `mini_harness` provides that `main` via the
`harness!` macro, while staying CLI-compatible with `cargo test` (which passes
libtest-style flags to the test binary regardless).

All seven data-test binaries in OpenVAF set `harness = false` and use
`mini_harness`:

```
basedb, hir_def, hir, hir_lower, openvaf, osdi, openvaf-driver
```

---

## Crate layout

```
lib/mini_harness/src/
  lib.rs      — public API: Test, Arguments, run_harness, harness!, TestSummary
  flags.rs    — xflags-generated CLI argument struct (re-exported as Arguments)
  printer.rs  — (empty)
  tests.rs    — (copied from base_n; contains no valid mini_harness tests)
```

The only external dependency is `xflags 0.3`, used to parse CLI arguments.

> **Note:** `src/tests.rs` and `README.md` appear to have been copied from the
> `base_n` crate by mistake — both reference `encode`, which does not exist in
> `mini_harness`. They do not affect the library's behaviour.

---

## Core types

### `Test<'a>`

```rust
pub struct Test<'a> {
    pub name:    String,
    pub runner:  Box<dyn FnOnce() -> Result + 'a>,
    pub ignored: bool,
}
```

Each test case is a name, a one-shot closure that returns `Result`, and an
ignored flag. The closure is `FnOnce` — it is consumed when run and cannot be
re-run.

### `Result` and `Failed`

```rust
pub type Result<T = (), E = Failed> = std::result::Result<T, E>;

pub struct Failed { msg: String }
impl<M: fmt::Display> From<M> for Failed { … }
```

Any `Display` value can be converted into `Failed` via `?`, so test functions
can use `?` to propagate errors from `std::io`, `anyhow`, or any other
error type that implements `Display`. The `msg` string is printed when the
test fails.

### `Arguments`

```rust
pub use flags::Test as Arguments;
```

Re-exported from `flags.rs`. Fields:

| Field | Type | Meaning |
|-------|------|---------|
| `filter` | `Option<String>` | Substring (or exact, with `--exact`) filter; only matching tests run |
| `skip` | `Vec<String>` | Tests whose names contain any of these strings are skipped |
| `exact` | `bool` | Filters match exactly rather than by substring |
| `ignored` | `bool` | Run only ignored tests |
| `include_ignored` | `bool` | Run both ignored and non-ignored tests |
| `list` | `bool` | Print test names and exit without running |
| `nocapture` | `bool` | Accepted but no-op (harness always runs without capture) |
| `format` | `Option<Format>` | `pretty` (default) or `terse` |

`Arguments::parse_cli()` reads `std::env::args_os()` via `xflags` and exits
with code 101 on parse error — matching `libtest`'s exit code for harness
failures.

### `TestSummary`

```rust
#[must_use = "Call `exit()` or `exit_if_failed()` to set the correct return code"]
pub struct TestSummary {
    pub failed:   Vec<String>,
    pub passed:   u32,
    pub ignored:  u32,
    pub filtered: u32,
    pub elapsed:  Duration,
}
```

`exit()` calls `process::exit(0)` on success or `101` on failure, matching
`libtest`'s convention. `exit_if_failed()` does the same but returns normally
on success — useful when you want to do cleanup after the test run.

---

## Test constructors

### `Test::new` — single closure

```rust
Test::new("my_test", &|| { /* ... */ Ok(()) })
```

Wraps a `Fn() -> Result` as a single named test.

### `Test::from_dir` — one test per file in a directory

```rust
Test::from_dir("name", &runner, &ignore_fn, dir)
```

Calls `read_dir(dir)` and creates one `Test` per directory entry. The test
name is `"{name}::{filename}"`. The `ignore` predicate marks tests as ignored
without removing them from the list (they are still printed with `--list`).

`from_dir` is a thin wrapper around `from_dir_filtered`, which adds a `filter`
predicate to skip entries entirely (rather than mark them ignored):

```rust
Test::from_dir_filtered("name", &runner, &filter_fn, &ignore_fn, dir)
```

In practice `filter_fn` is used to restrict to files with a specific extension:

```rust
Test::from_dir_filtered("ui", &ui_test, &is_va_file, &ignore_never, &openvaf_test_data("syn_ui"))
```

### `Test::from_list` — one test per item in a slice

```rust
Test::from_list("name", &runner, &ignore_fn, &[item1, item2, …])
```

Creates `"{name} {item:?}"` for each item. Used for tests parametrised over a
fixed set of values rather than a filesystem directory.

---

## The `harness!` macro

```rust
#[macro_export]
macro_rules! harness {
    ($($tests: expr),*) => {
        fn main() {
            let args = $crate::Arguments::parse_cli();
            let mut tests = ::std::vec::Vec::new();
            $($crate::TestOrTestList::push_to_list($tests, &mut tests);)*
            $crate::run_harness(&args, tests).exit()
        }
    };
}
```

Each expression in the macro can be either a `Test` (pushed as one item) or
any `IntoIterator<Item = Test>` (flattened into the list), via the
`TestOrTestList` trait:

```rust
pub trait TestOrTestList<'a> {
    fn push_to_list(self, dst: &mut Vec<Test<'a>>);
}
impl<'a> TestOrTestList<'a> for Test<'a> { … }
impl<'a, I: IntoIterator<Item = Test<'a>>> TestOrTestList<'a> for I { … }
```

This lets `Test::from_dir(…)` (which returns an iterator) and `Test::new(…)`
(which returns a single `Test`) appear in the same `harness!` invocation
without explicit flattening.

---

## `run_harness` execution model

```rust
pub fn run_harness(args: &Arguments, mut tests: Vec<Test>) -> TestSummary
```

1. **Sort** tests alphabetically by name — ensures deterministic order
   regardless of filesystem enumeration order.
2. **Filter** — retain only tests that pass `args.is_filtered_out`; count
   filtered-out tests for the summary.
3. **List mode** — if `--list` was given, print names and return an empty
   summary without running anything.
4. **Sequential execution** — iterate the remaining tests in order. For each:
   - If ignored (and `--include-ignored` not set): count as ignored.
   - Otherwise: call `Test::run(runner)`.
5. **`Test::run`** wraps the closure in `std::panic::catch_unwind`:
   - `Ok(Ok(()))` → pass
   - `Ok(Err(failed))` → fail with `failed.msg`
   - `Err(panic_payload)` → fail with `"test panicked: {payload}"` (or
     `"test panicked"` if the payload is not a `&str`/`String`)
6. Print failures, then the summary line.

Tests are always single-threaded. There is no parallelism, no test isolation
beyond panic-catching, and no output capture (`--nocapture` is accepted but
is always the effective mode).

---

## Worked example: `basedb` data tests

`basedb/tests/data_tests.rs` registers three test suites with one `harness!`
call:

```rust
harness! {
    Test::from_dir_filtered(
        "integration", &integration_test,
        &Path::is_dir,          // filter: only directories
        &ignore_dev_tests,      // ignore: directories named "dev_*"
        &project_root().join("integration_tests")
    ),
    Test::from_dir_filtered(
        "ui", &ui_test,
        &is_va_file,            // filter: only *.va files
        &ignore_never,
        &openvaf_test_data("syn_ui")
    ),
    Test::from_dir_filtered(
        "ast", &ast_test,
        &is_va_file,
        &ignore_never,
        &openvaf_test_data("ast")
    )
}
```

At runtime, `main` parses CLI args, then calls `read_dir` on each directory,
building a list of `Test` values like:

```
integration::resistor
integration::diode
ui::missing_semicolon.va
ui::unknown_nature.va
ast::resistor.va
…
```

These are sorted alphabetically, then `cargo test -- ui` would filter to only
the `ui::*` tests. Each runner constructs a `TestDataBase`, runs the compiler
frontend to a specific stage, and uses `expect_test::expect_file!` to compare
output against a `.log` or `.va_ast` snapshot file. A mismatch returns
`Err(Failed { msg: "…" })`, which `run_harness` prints as a test failure.

---

## Key design decisions

**`harness = false` + custom `main`.** The `harness!` macro generates a `main`
function, which is only valid when Cargo compiles the test binary with
`harness = false`. This is a deliberate opt-in: regular `#[test]` functions in
the same crate would require `harness = true`. Data-test binaries are separate
`[[test]]` sections in `Cargo.toml` so they can set `harness = false`
independently.

**`TestOrTestList` for uniform macro syntax.** Without this trait, callers
would need to write `…collect::<Vec<_>>()` after each `from_dir` call to
flatten iterators. The blanket `impl<I: IntoIterator<Item=Test>>` handles this
automatically, keeping the `harness!` invocation clean.

**`FnOnce` runner, not `Fn`.** Test closures capture state (e.g., the `Path`
to a file) and run exactly once. Using `FnOnce` is honest about ownership and
avoids cloning the captured state. The `from_dir` constructor clones the `Path`
into each closure's capture at construction time, so the closure itself is
`FnOnce`.

**Sequential execution.** Running tests sequentially avoids the need for
`Send + Sync` bounds on test state, allows tests to share a Salsa database
without locking, and produces deterministic interleaved output. For compiler
integration tests that each spin up a full Salsa instance and parse tens of
kilobytes, the overhead of spawning threads would not improve wall-clock time
on the CI machines this project targets.

**`catch_unwind` for panic isolation.** A panicking test should not abort the
entire test run. `AssertUnwindSafe` is required because the `FnOnce` closure
is not automatically `UnwindSafe` — the harness accepts this as a known
approximation, consistent with how `libtest` itself handles panics.
