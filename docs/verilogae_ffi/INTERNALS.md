# `verilogae_ffi` — Rust FFI wrapper for VerilogAE

**Location:** `verilogae/verilogae_ffi/`
**Role:** A thin, safe-ish Rust wrapper over the C ABI that `verilogae`
exports. Provides RAII types (`Opts`, `VfsExport`) and re-exports all
`verilogae_*` C functions so that Rust consumers (primarily `verilogae_py`)
do not need to write `unsafe` symbol lookups themselves.

The crate supports two linking modes selected by the `static` feature flag
(enabled by default):

| Feature | Linking | Source of symbols |
|---------|---------|-------------------|
| `static` (default) | Statically links `verilogae` lib | Uses `verilogae::api` directly |
| (no feature) | Dynamically loads `libverilogae` | Uses `generated.rs` bindings loaded via `links = "verilogae"` |

Cross-links: [verilogae INTERNALS](../verilogae/INTERNALS.md) ·
[verilogae_py INTERNALS](../verilogae_py/INTERNALS.md)

---

## Crate layout

```
verilogae/verilogae_ffi/src/
  lib.rs            — Opts RAII wrapper; VfsExport RAII wrapper; feature dispatch
  ffi.rs            — Slice<T> helpers (non-static mode only)
  ffi/generated.rs  — auto-generated extern "C" declarations (non-static mode)
  tests.rs          — (minimal tests)
```

The `Cargo.toml` declares `links = "verilogae"`, which tells Cargo that this
crate provides the native library named `verilogae`. In the `static` feature
mode the `verilogae` lib crate is a direct Rust dependency; in the dynamic
mode a build script would supply the linker flags to find the pre-built
`libverilogae.so`/`.dll`.

---

## Feature dispatch

```rust
// lib.rs (simplified)
#[cfg(not(feature = "static"))]
mod ffi;
pub use ffi::*;                     // dynamic: symbols from generated.rs extern "C" block

#[cfg(feature = "static")]
use verilogae::api as ffi;          // static: direct Rust path to the same types/functions
```

In `static` mode, `ffi` is literally `verilogae::api`. Every type alias and
constant in `lib.rs` therefore refers directly to the Rust type in the parent
crate — no `extern "C"` involved. In dynamic mode, `ffi::generated` contains
the auto-generated `extern "C"` declarations that match the C ABI.

---

## `Opts` — RAII options wrapper

```rust
#[derive(Default)]
pub struct Opts(Option<&'static mut ffi::Opts>);
```

`Opts` is a wrapper around a heap-allocated `ffi::Opts` struct. The inner
value is lazily allocated: calling `write()` for the first time calls
`ffi::verilogae_new_opts()` to `Box`-allocate an `ffi::Opts` and stores a
`'static` mutable reference (the allocation is live until `Drop`).

```rust
pub unsafe fn write(&mut self) -> &mut ffi::Opts { … }
```

The `'static` lifetime is a lie — the allocation is owned by `Opts` and freed
on `Drop`. The `unsafe` on `write()` documents that the caller must not store
the returned reference past the lifetime of `Opts`.

On `Drop`, each slice field is individually freed via `into_box_opt()` before
calling `verilogae_free_opts`. This matches the allocation convention
documented in `verilogae::api`: the slices are owned by the Rust side, while
the string *contents* pointed to by those slices are owned by the caller.

---

## `VfsExport` — RAII VFS export wrapper

```rust
pub struct VfsExport(ffi::Vfs);
```

`VfsExport::new(path, opts)` calls `verilogae_export_vfs`. If the returned
`Vfs` has a null `ptr`, it returns `None` (indicating a compilation error).

`entries()` returns an iterator over `(&str, &str)` pairs (virtual path,
file contents), reinterpreting the raw byte slices as UTF-8. The
`unsafe { std::str::from_utf8_unchecked(…) }` is justified by the invariant
that VerilogAE only produces UTF-8 paths and Verilog-A source text.

On `Drop`, `verilogae_free_vfs` is called to release the C-allocated memory.

---

## Re-exported constants

In `static` mode, the four `ParamFlags` constants are defined directly:

```rust
pub const PARAM_FLAGS_MIN_INCLUSIVE: ParamFlags = 1;
pub const PARAM_FLAGS_MAX_INCLUSIVE: ParamFlags = 2;
pub const PARAM_FLAGS_INVALID:       ParamFlags = 4;
pub const PARAM_FLAGS_GIVEN:         ParamFlags = 8;
```

In dynamic mode these constants come from `generated.rs`.

---

## Key design decisions

**`links = "verilogae"` without a build script.** The `links` key in
`Cargo.toml` normally requires a `build.rs` that emits `cargo:rustc-link-lib`
lines. Here, in `static` mode, the `verilogae` lib crate is a direct
`[dependencies]` entry, so Cargo links it automatically. The `links` key is
used primarily to signal to Cargo that this crate "provides" the `verilogae`
native library, preventing two crates in the same build from both trying to
provide it.

**`Option<&'static mut ffi::Opts>` instead of `Box<ffi::Opts>`.** Using
`&'static mut` rather than `Box` avoids the type system tracking the lifetime
of the inner allocation — necessary because in dynamic mode the allocation is
done via an FFI call (`verilogae_new_opts`) whose return type is a raw
pointer. The `Option` supports the lazy-allocation pattern: `None` until
`write()` is first called.
