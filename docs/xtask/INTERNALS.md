# `xtask` — Auxiliary build commands

**Location:** `xtask/`
**Role:** A [cargo-xtask](https://github.com/matklad/cargo-xtask) binary that
implements build automation tasks too complex for plain `cargo` commands.
Invoked as `cargo xtask <subcommand>` via an alias in `.cargo/config`.

This crate contains no compiler logic — it is a shell-scripting harness that
orchestrates `cargo build`, `pip wheel`, `auditwheel`, and `twine`.

Cross-links: [verilogae INTERNALS](../verilogae/INTERNALS.md) ·
[verilogae_py INTERNALS](../verilogae_py/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate layout

```
xtask/src/
  main.rs       — entry point; project_root(); subcommand dispatch
  flags.rs      — xflags CLI definition + generated parser
  build_py.rs   — verilogae build / test / publish subcommands
  msvcrt.rs     — gen-msvcrt subcommand
  cache.rs      — (module present but commented out / unused)
  vendor.rs     — (module present but commented out / unused)
```

Dependencies are intentionally minimal: `xshell` (shell commands), `xflags`
(CLI parsing), `anyhow` (error handling), `md5` and `base_n` (unused in
active code — leftovers from the commented-out `cache` module).

---

## Subcommands

### `cargo xtask verilogae build [--force] [--manylinux] [--windows] [--install]`

Builds the `verilogae_py` Python extension wheels for Python 3.8–3.11.

**Steps:**

1. Sets `RUSTFLAGS="-C strip=symbols"` to strip debug symbols from the
   release build.
2. Selects the Rust target:
   - `--windows` → `x86_64-pc-windows-msvc`
   - otherwise → `x86_64-unknown-linux-gnu`
3. Runs `cargo build --release -p verilogae --target {target}` to produce
   `libverilogae.so` / `verilogae.dll`.
4. If `--force`, clears the `wheels/` directory. Otherwise, aborts if
   `wheels/` is non-empty.
5. For each Python version in 3.8–3.11 (filtered to those actually
   installed on the host):
   - Linux: `pip wheel . -w ./wheels --no-deps` with `PYO3_PYTHON={py}`
   - Windows: same with `PYO3_CROSS_PYTHON_VERSION={version}` and
     `CARGO_BUILD_TARGET=x86_64-pc-windows-msvc`
6. Audits wheels with `auditwheel repair`:
   - `--manylinux` → repairs to a `manylinux` tag (portable Linux binary).
   - otherwise (Linux) → repairs with `--plat linux_x86_64`.
   - `--windows` → skips `auditwheel` (not applicable on Windows).
7. If `--install`, installs each built wheel into the matching Python
   installation via `pip install --force-reinstall`.

`find_py` discovers Python versions by probing `python3.8` … `python3.11`
executables. On Windows it uses the versions directly; on Linux it requires
the interpreter to be in `PATH`.

### `cargo xtask verilogae test`

Installs NumPy into each available Python 3.8–3.11 interpreter and runs
`verilogae/tests/test_hicum.py` for each.

### `cargo xtask verilogae publish [--windows]`

Convenience wrapper: runs `build --force --manylinux --install` (or
`--windows`), then (Linux only) `test`, then `twine upload wheels/*` to
upload all wheels to PyPI.

### `cargo xtask gen-msvcrt`

Generates the Universal C Runtime (UCRT) `.def` export-definition files
that the linker needs to link against `api-ms-win-crt-*.dll` on Windows
cross-compilation targets.

**Steps:**

1. Clones `mingw-w64` at tag `v10.0.0` (shallow, single-branch) into
   `./mingw/`.
2. For each of the 15 UCRT DLL names in `UCRT_FILES` and each architecture
   (`X64`, `ARM64`):
   - If a `.def` file exists in `mingw-w64-crt/lib-common/`, sanitize it
     (strip comments, `DATA` entries, and preprocessor directives) and copy
     it to `openvaf/target/ucrt/defs/{arch}/`.
   - If only a `.def.in` template exists, run it through `clang -E`
     (C preprocessor) with `-D DEF_{ARCH}` to expand the architecture
     guard macros, then sanitize and write the result.
3. Removes the `./mingw/` checkout.

`sanitize_def` strips:
- Lines containing `==` (version-compare guards).
- Lines ending with `DATA` or `\tDATA` (data-export entries that cause
  linker errors).
- Empty lines, `;` comments, and `#` preprocessor lines.
- Replaces the special `"; strnlen replaced by emu"` comment with a real
  `strnlen` export entry.

---

## `flags.rs` — CLI definition

The CLI is declared with `xflags::xflags!` and the generated parser code
is committed alongside the declaration (the `// generated start … end`
block). Regeneration is triggered by `UPDATE_XFLAGS=1 cargo build`.

```
xtask
├── verilogae
│   ├── build   [--force] [--manylinux] [--windows] [--install]
│   ├── test
│   └── publish [--windows]
└── gen-msvcrt
```

---

## `project_root()`

```rust
fn project_root() -> PathBuf {
    Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR")))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}
```

Walks one level up from the `xtask/` manifest to reach the workspace root.
`main` immediately calls `sh.change_dir(project_root())` so that all
subsequent `xshell` commands run from the workspace root regardless of where
`cargo xtask` was invoked.

---

## Key design decisions

**`xshell` for shell commands.** `xshell` provides a Rust-native, cross-
platform shell API with interpolation (`cmd!(sh, "cargo build {target}")`)
and `push_env`/`push_dir` RAII guards. This avoids shell-escaping bugs and
makes the build scripts work on both Linux and Windows without `bash`.

**Commented-out modules.** `cache.rs` and `vendor.rs` are present in the
source tree but their `mod` declarations in `main.rs` are commented out.
They are dead code preserved for potential future reactivation. The active
`cache` module was likely intended to implement the same content-addressed
caching as `openvaf::cache` and `verilogae::cache`, but it is not wired up.

**`gen-msvcrt` uses `clang -E`.** The MinGW-w64 `.def.in` files use C
preprocessor guards (`DEF_X64`, `DEF_ARM64`) to select architecture-specific
exports. Invoking `clang -E` is simpler than implementing a C preprocessor
in Rust and is justified for a one-time code-generation task.
