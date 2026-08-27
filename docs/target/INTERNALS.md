# `target` — Compilation Target Specifications

**Location:** `openvaf/target/`
**Role:** Defines what platforms OpenVAF can compile for. The crate provides
the `Target` struct (LLVM triple, data layout, pointer width, linker flavor,
and per-platform link arguments), a fixed table of seven built-in targets, and
the compile-time `host_triple()` function. On Windows it also builds and embeds
a UCRT import library so the linker can reference the C runtime without
requiring Visual Studio to be installed.

Cross-links: [linker\_target INTERNALS](../linker_target/INTERNALS.md) ·
[mir\_llvm INTERNALS](../mir_llvm/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
target   (no runtime dependencies)
  └─► linker   (selects linker flavor; passes pre/post link args)
  └─► mir_llvm (reads llvm_target, data_layout, pointer_width, cpu, features)
  └─► osdi     (reads is_like_windows for symbol-naming decisions)
  └─► openvaf  (CLI; calls Target::host_target() and Target::search())
```

`target` is intentionally a leaf crate with no runtime dependencies. All
platform knowledge is encoded at compile time. The `build.rs` script uses the
`cc` crate (a build-time dependency only) to compile the UCRT shim; the
resulting bytes are embedded via `include_bytes!` so the final binary needs
nothing installed at link time beyond the system linker.

---

## Module map

| File | Contents |
|------|----------|
| `src/lib.rs` | Public re-exports; `host_triple()`; `supported_targets!` macro |
| `src/spec.rs` | `LinkerFlavor`, `Target`, `TargetOptions`, `LinkArgs`; `Target::search*` |
| `src/spec/linux_base.rs` | Shared options for all Linux targets |
| `src/spec/apple_base.rs` | Shared options for macOS and Apple Silicon |
| `src/spec/windows_base.rs` | Shared options for all Windows targets |
| `src/spec/windows_msvc_base.rs` | MSVC-specific options layered over windows_base |
| `src/spec/x86_64_unknown_linux.rs` | Concrete target: x86-64 Linux |
| `src/spec/aarch64_unknown_linux.rs` | Concrete target: AArch64 Linux |
| `src/spec/x86_64_pc_windows.rs` | Concrete target: x86-64 Windows (MSVC) |
| `src/spec/aarch64_pc_windows.rs` | Concrete target: AArch64 Windows (MSVC) |
| `src/spec/x86_64_apple_darwin.rs` | Concrete target: x86-64 macOS |
| `src/spec/aarch64_apple_darwin.rs` | Concrete target: Apple Silicon macOS |
| `src/spec/x86_64_unknown_linux_musl.rs` | Concrete target: musl-libc Linux |
| `src/ucrt.c` | UCRT `snprintf` shim (compiled by build.rs) |
| `build.rs` | Compiles ucrt.c; emits `CFG_COMPILER_HOST_TRIPLE` |

---

## `LinkerFlavor`

```rust
pub enum LinkerFlavor {
    Ld,    // GNU ld / lld on Linux and musl
    Ld64,  // Apple ld64 on macOS
    Msvc,  // link.exe on Windows
}
```

`LinkerFlavor` is the dispatch key for two independent lookups:

1. **Linker selection** in the `linker` crate: `LinkerFlavor::Msvc` maps to
   `MsvcLinker`; `Ld` and `Ld64` both map to `LdLinker` (with the macOS
   `-dylib` flag instead of `-shared`).

2. **Link-argument lookup** in `TargetOptions::pre_link_args` and
   `post_link_args`: these are `BTreeMap<LinkerFlavor, Vec<String>>`. Every
   target module inserts arguments under the flavor it uses, and the linker
   crate iterates only over the entry for the active flavor.

The `flavor_mappings!` macro in `spec.rs` builds a `&[(LinkerFlavor, &str)]`
mapping used by serialization; all three flavors have a canonical string name
(`"ld"`, `"ld64"`, `"msvc"`).

---

## `Target` and `TargetOptions`

```rust
pub struct Target {
    pub llvm_target:   String,       // LLVM triple: "x86_64-unknown-linux-gnu"
    pub pointer_width: u32,          // 32 or 64
    pub arch:          String,       // "x86_64" | "aarch64"
    pub data_layout:   String,       // LLVM datalayout string
    pub options:       TargetOptions,
}
```

`llvm_target` is passed directly to `llvm::TargetMachine::create` in `mir_llvm`
and appears verbatim in the object file's ELF/Mach-O/COFF machine type.
`data_layout` seeds `llvm::Module::setDataLayout`; it must match the target
triple or LLVM will emit incorrect code for struct padding and vector alignment.

```rust
pub struct TargetOptions {
    pub is_builtin:      bool,
    pub cpu:             String,     // e.g. "x86-64", "apple-m1"
    pub features:        String,     // LLVM feature string e.g. "+avx2,-x87"
    pub linker_flavor:   LinkerFlavor,
    pub pre_link_args:   LinkArgs,   // BTreeMap<LinkerFlavor, Vec<String>>
    pub post_link_args:  LinkArgs,
    pub import_lib:      &'static [u8], // embedded .lib bytes, empty on non-Windows
    pub is_like_windows: bool,
    pub is_like_osx:     bool,
}
```

`cpu` and `features` are forwarded to LLVM's `TargetMachine` builder, which
uses them to select instruction-set extensions. The default `cpu = "generic"`
and empty `features` produce fully portable code; target modules override this
when the platform guarantees a specific baseline (e.g. `"x86-64"` enables
SSE2, which is mandatory on 64-bit x86).

`import_lib` is `&'static [u8]` rather than `Option<Vec<u8>>` because the
bytes come from `include_bytes!` at compile time and live in the binary's
read-only data segment. On all non-Windows targets it is the empty slice `&[]`.

---

## Base modules and inheritance

OpenVAF avoids copy-pasting by factoring shared options into base modules.
Each concrete target calls its base's function and then overrides specific
fields:

```
linux_base::opts()
  └─ pre_link_args[Ld] += ["--no-add-needed", "--hash-style=gnu"]
       ├─ x86_64_unknown_linux   cpu="x86-64", pre_link += ["-m", "elf_x86_64"]
       ├─ aarch64_unknown_linux  cpu="generic" (AArch64 has no legacy sub-ISAs)
       └─ x86_64_unknown_linux_musl  (same as x86_64 linux)

apple_base::opts()
  └─ linker_flavor = Ld64, is_like_osx = true
       ├─ x86_64_apple_darwin    cpu="core2"
       └─ aarch64_apple_darwin   cpu="apple-m1"

windows_base::opts()
  └─ is_like_windows = true
       └─ windows_msvc_base::opts()
            └─ pre_link_args[Msvc]  += ["/NOLOGO"]
               post_link_args[Msvc] += ["msvcrt.lib"]
                 ├─ x86_64_pc_windows   import_lib = UCRT_IMPORTLIB (x64)
                 └─ aarch64_pc_windows  import_lib = UCRT_IMPORTLIB (arm64)
```

`linux_base`'s `--hash-style=gnu` improves dynamic-linker startup time on
modern Linux distributions; `--no-add-needed` prevents the linker from adding
implicit `NEEDED` entries for shared libraries that the object file references
transitively. Together they give cleaner and faster shared-object loading for
the `.osdi` plugin.

---

## The `supported_targets!` macro

```rust
supported_targets! {
    ("x86_64-unknown-linux-gnu",     x86_64_unknown_linux),
    ("aarch64-unknown-linux-gnu",    aarch64_unknown_linux),
    ("x86_64-unknown-linux-musl",    x86_64_unknown_linux_musl),
    ("x86_64-pc-windows-msvc",       x86_64_pc_windows),
    ("aarch64-pc-windows-msvc",      aarch64_pc_windows),
    ("x86_64-apple-darwin",          x86_64_apple_darwin),
    ("aarch64-apple-darwin",         aarch64_apple_darwin),
}
```

The macro expands to:

- An array `TARGETS: &[(&str, fn() -> Target)]` pairing each LLVM triple
  string with a constructor function.
- A `load_specific(target: &str) -> Option<Target>` function that walks the
  array and calls the constructor on a match.

`Target::search(triple)` calls `load_specific` and returns `Err` if the triple
is not in the table. There is no JSON loading path; the target set is closed
and checked at compile time. Adding a new target requires adding a source file
and a macro entry, then rebuilding the compiler.

---

## Target lookup API

```rust
impl Target {
    pub fn search(target_triple: &str) -> Result<Target, String>;
    pub fn search_llvm_triple(llvm_target: &str) -> Result<Target, String>;
    pub fn host_target() -> Result<Target, String>;
}
```

**`search`** takes a target triple exactly as it appears in the
`supported_targets!` table (e.g. `"x86_64-unknown-linux-gnu"`).

**`search_llvm_triple`** iterates the same table but matches on the
`Target::llvm_target` field. For the current seven targets the two strings are
identical, but the separation exists because LLVM triples and Rust target names
occasionally differ (e.g. `"x86_64-apple-macosx10.7.0"` vs
`"x86_64-apple-darwin"`).

**`host_target`** calls `host_triple()` and passes the result to `search`.

---

## `host_triple()` — compile-time platform detection

```rust
pub fn host_triple() -> &'static str {
    // CFG_COMPILER_HOST_TRIPLE is set by build.rs
    let triple = env!("CFG_COMPILER_HOST_TRIPLE");

    // MSYS2 GNU environment appears as windows-gnu but the MSVC linker is used
    if triple.contains("windows-gnu") {
        return "x86_64-pc-windows-msvc";
    }
    // Normalize older Apple triples to the canonical name
    if triple.contains("apple") {
        if triple.contains("aarch64") {
            return "aarch64-apple-darwin";
        } else {
            return "x86_64-apple-darwin";
        }
    }
    triple
}
```

`CFG_COMPILER_HOST_TRIPLE` is emitted by `build.rs` as:

```rust
println!("cargo:rustc-env=CFG_COMPILER_HOST_TRIPLE={}", triple);
```

where `triple` comes from `std::env::var("TARGET")` — the Cargo-supplied
target triple for the compilation host. Because `env!` is evaluated at compile
time, `host_triple()` is a zero-cost `&'static str` with no syscalls at
runtime.

The two special cases patch over real-world mismatches:

- **`windows-gnu`**: The MSYS2 build environment reports itself as
  `x86_64-pc-windows-gnu`, but OpenVAF uses the MSVC linker on all Windows
  hosts. Normalizing to `windows-msvc` means `host_target()` always returns
  the MSVC target on Windows regardless of the Rust toolchain flavour used to
  compile OpenVAF.

- **`apple`**: older Xcode toolchains produced triples like
  `x86_64-apple-macosx10.15.0` rather than the bare `x86_64-apple-darwin`.
  The normalization collapses all Apple variants to the two canonical entries
  in the target table.

---

## The UCRT import library

### Why it exists

On Windows, the OSDI `.osdi` shared library must link against the Universal C
Runtime (UCRT) for functions like `printf`, `malloc`, and `snprintf`. The UCRT
import library (`ucrt.lib`) ships with the Windows SDK and the MSVC toolchain,
but OpenVAF's linker runs at compile time on any machine — including CI runners
and developer machines that may not have the full Visual Studio installation.

To avoid requiring the Windows SDK as a prerequisite, `target/build.rs`
produces a minimal import library at build time and embeds it in the binary.
The embedded bytes are then handed to the linker via a temporary file whenever
OpenVAF links a Windows target.

### `ucrt.c` — the shim

`src/ucrt.c` imports exactly one function:

```c
// Polyfill: expose snprintf as a proper export in older UCRT versions.
// __stdio_common_vsprintf is always available; snprintf may not be in import libs.
int snprintf(char *buf, size_t count, const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    int r = __stdio_common_vsprintf(
        _CRT_INTERNAL_PRINTF_STANDARD_SNPRINTF_BEHAVIOR,
        buf, count, fmt, NULL, args);
    va_end(args);
    return r;
}
```

`__stdio_common_vsprintf` is the underlying UCRT entry point that backs
`snprintf`, `sprintf`, and similar functions. Using it directly bypasses the
older SDK import-library gap where `snprintf` was not exported by name from
`ucrtbase.dll`. With this shim, the generated object file defines `snprintf`
locally so that any OSDI plugin referencing it will resolve correctly.

### `build.rs` compilation pipeline

`build.rs` runs a two-step pipeline for each Windows target architecture:

```
ucrt.c
  │
  ├─ clang -c -target <triple> -o ucrt_<arch>.obj
  │       (or cc crate fallback)
  │
  └─ llvm-lib /OUT:ucrt_<arch>.lib ucrt_<arch>.obj
          (MSVC-format import library)
```

For MSYS2 builds (`MSYSTEM` env var present), `build.rs` substitutes `ar`
for `llvm-lib` to produce a GNU-format archive instead.

The `.lib` file is written to `$OUT_DIR/ucrt_{x64,arm64}.lib`. The concrete
target modules then embed it:

```rust
// in x86_64_pc_windows.rs
const UCRT_IMPORTLIB: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/ucrt_x64.lib"));

pub fn target() -> Target {
    Target {
        options: TargetOptions {
            import_lib: UCRT_IMPORTLIB,
            ..windows_msvc_base::opts()
        },
        ..
    }
}
```

`include_bytes!` resolves the path at compile time, so `UCRT_IMPORTLIB` is a
`&'static [u8]` pointing into the binary's `.rodata` section. The bytes are
never written to disk again unless the linker needs them.

### How the linker uses `import_lib`

In the `linker` crate, `link()` checks `target.options.import_lib`:

```rust
if !target.options.import_lib.is_empty() {
    let lib_path = out_dir.join("import_lib.lib");
    fs::write(&lib_path, target.options.import_lib)?;
    cmd.arg(lib_path);
}
```

A temporary `.lib` is materialized from the embedded bytes, its path is appended
to the linker command, and the file is cleaned up after the link step completes.
This keeps the "no prerequisites" guarantee: a fresh Windows machine with only
the OpenVAF binary can link `.osdi` files without any SDK installation.

---

## Worked example: compiling for x86_64-unknown-linux-gnu

The CLI receives `--target x86_64-unknown-linux-gnu` (or defaults to
`host_target()`). The pipeline from target lookup to linked `.osdi`:

**Step 1 — Target lookup.**

```rust
let target = Target::search("x86_64-unknown-linux-gnu").unwrap();
```

`load_specific` matches the string in the `supported_targets!` table and calls
`x86_64_unknown_linux::target()`:

```rust
pub fn target() -> Target {
    let mut base = linux_base::opts();
    base.cpu = "x86-64".into();
    base.pre_link_args
        .entry(LinkerFlavor::Ld)
        .or_default()
        .extend(["-m".into(), "elf_x86_64".into()]);

    Target {
        llvm_target:   "x86_64-unknown-linux-gnu".into(),
        pointer_width: 64,
        arch:          "x86_64".into(),
        data_layout:   "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128".into(),
        options: TargetOptions {
            is_builtin: true,
            ..base
        },
    }
}
```

**Step 2 — LLVM target machine.**

`mir_llvm` calls:

```rust
llvm::TargetMachine::create(
    &target.llvm_target,   // "x86_64-unknown-linux-gnu"
    &target.options.cpu,   // "x86-64"
    &target.options.features, // ""
    &target.data_layout,
)
```

LLVM selects the x86-64 backend, enables SSE2 (implied by `x86-64` CPU
class), and sets up the data layout for 64-bit LP64 Linux. The generated
object file will be ELF64 with System V AMD64 ABI calling conventions.

**Step 3 — Link-argument assembly.**

The `linker` crate reads:

```rust
target.options.pre_link_args[LinkerFlavor::Ld]
// → ["--no-add-needed", "--hash-style=gnu", "-m", "elf_x86_64"]
```

These are prepended to the `ld` command before the input object and output
`-o` flag. `post_link_args` is empty for Linux targets.

**Step 4 — Linker invocation.**

```
ld --no-add-needed --hash-style=gnu -m elf_x86_64
   -shared
   resistor.o
   -o resistor.osdi
```

The result is an ELF shared object exporting the OSDI entry points
(`osdi_descriptors`, `osdi_init`, etc.) and containing all the compiled
analog equations.

---

## Key design decisions

**Fixed target table, not JSON.** Some compilers (notably rustc) support
user-defined target JSON files. OpenVAF restricts itself to the seven built-in
targets. The trade-off is inflexibility in exchange for correctness guarantees:
every `data_layout` string and every link argument combination has been tested
against the LLVM version shipped with OpenVAF. A user-supplied target could
produce silently malformed output if the data layout were wrong.

**`import_lib` is `&'static [u8]`, not `Option<PathBuf>`.** Embedding the
bytes as a `&'static [u8]` ensures the compiler binary is truly self-contained
on Windows. The alternative — pointing at a file on disk — would mean that
moving the binary breaks Windows cross-compilation, and that CI machines need
a Windows SDK installed regardless of the target platform.

**`BTreeMap` for link args.** `LinkArgs = BTreeMap<LinkerFlavor, Vec<String>>`
preserves insertion order within each flavor's argument list (because
`BTreeMap` iterates keys in sorted order and `Vec` preserves push order).
This matters because linkers treat argument order as significant: `-m elf_x86_64`
must precede the input objects, and `msvcrt.lib` must appear after them.

**`host_triple()` normalizes at compile time.** The special cases for
`windows-gnu` and `apple` variants are resolved in a single `if`-chain baked
into the binary. There is no runtime detection, no registry query, and no
environment variable consulted at startup. If the normalization ever needs to
change, recompiling the compiler is the correct response.

**Separate `pre_link_args` and `post_link_args`.** Linker arguments that must
appear before the input objects (emulation mode `-m`, output format flags) are
kept strictly separate from arguments that must appear after (runtime libraries
`msvcrt.lib`). This prevents the base modules from accidentally placing a
post-link library before the object file, which would cause undefined symbol
errors on link.
