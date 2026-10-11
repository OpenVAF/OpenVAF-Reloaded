# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

OpenVAF-reloaded is a Verilog-A compiler written in Rust that outputs dynamic libraries compatible with the OSDI API (currently version 0.4). The compiler transforms Verilog-A compact device models into optimized native code for circuit simulators like ngspice, SPICE OPUS, and VACASK.

The project is a fork of Pascal Kuthe's original OpenVAF compiler, maintained by Árpád Bűrmen since 2024, with LLVM 18 support contributed by Kreijstal.

## Build Commands

### Prerequisites
- **Rust toolchain**: Install with profile "complete"
- **LLVM 18 to 21**: A distribution package is enough — no source build required.
  Ubuntu 24.04 and later ship LLVM 18 (`apt-get install llvm-18 llvm-18-dev
  libclang-18-dev clang-18`); see README.md for other versions.
- **Environment**: Set `LLVM_SYS_1XX_PREFIX` for the version you build against and
  put its `bin` on `PATH` — `openvaf/target`'s build script invokes `clang` by name:
  ```bash
  export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
  export PATH=/usr/lib/llvm-18/bin:$PATH
  ```

**There is no default LLVM version**: every build and test command that touches
`mir_llvm`, `osdi` or `openvaf` needs `--features llvmXX` (or run `./configure` to
auto-detect and use `./build.sh`). Without it the build fails with several hundred
`cannot find module or crate llvm_sys` errors.

### Building the Compiler
```bash
# Build release version (recommended)
cargo build --release --features llvm18 --bin openvaf-r

# Build debug version
cargo build --features llvm18 --bin openvaf-r

# The binaries are output to:
# - target/release/openvaf-r (release)
# - target/debug/openvaf-r (debug)
```

### Testing
```bash
# Run fast tests only (default)
cargo test --features llvm18
cargo test --release --features llvm18  # On release build

# Run all tests including slow ones
RUN_SLOW_TESTS=1 cargo test --features llvm18

# Update test snapshots (when you've intentionally changed MIR/IR generation)
UPDATE_EXPECT=1 cargo test --features llvm18
```

The front-end crates (`parser`, `syntax`, `hir*`, `mir*`, `basedb`, `sourcegen`)
need neither LLVM nor the feature flag, but `openvaf/target`'s build script still
wants `clang`. `RUST_CHECK=1` makes it emit stubs instead, so front-end work can be
built and tested without any LLVM present:
```bash
RUST_CHECK=1 cargo test -p hir_ty -p hir_lower -p basedb
```

`cargo test -p openvaf --test integration` compiles models to OSDI and runs them
against a mock simulator (`openvaf/openvaf/tests/mock_sim/`), which is the only
harness that lowers *with* equations — the place to test analog operators
numerically. It enumerates `external/vacask/devices`, so create that directory
(empty is fine) if you do not have the VACASK model library checked out.

**Note**: Some expected test results are in `.snap` files in `openvaf/test_data/`, while others are hard-coded in test source files (e.g., `openvaf/mir_autodiff/src/builder/tests.rs`) and must be updated manually.

### Python Bindings
```bash
# Build verilogae Python package
cargo xtask verilogae build

# Run verilogae tests
cargo xtask verilogae test

# Publish verilogae (with platform flags)
cargo xtask verilogae publish --windows
```

## Architecture

### Compilation Pipeline

The compiler follows a multi-stage pipeline similar to rustc:

1. **Lexing & Parsing** (`openvaf/lexer`, `openvaf/parser`)
   - Tokenizes Verilog-A source files
   - Builds concrete syntax trees using the `rowan` library

2. **Preprocessing** (`openvaf/preprocessor`)
   - Handles `include directives and macro expansion
   - Controlled by macro flags passed via CLI

3. **HIR (High-Level Intermediate Representation)** (`openvaf/hir_def`, `openvaf/hir`, `openvaf/hir_lower`, `openvaf/hir_ty`)
   - `hir_def`: Desugars syntax into HIR definitions
   - `hir_ty`: Type inference and checking
   - `hir_lower`: Lowers HIR to MIR

4. **MIR (Mid-Level Intermediate Representation)** (`openvaf/mir`, `openvaf/mir_build`, `openvaf/mir_opt`, `openvaf/mir_autodiff`)
   - `mir_build`: Constructs MIR from HIR
   - `mir_autodiff`: Automatic differentiation for Jacobian computation
   - `mir_opt`: MIR-level optimizations
   - `mir_reader`: Utilities for reading/debugging MIR

5. **Code Generation** (`openvaf/mir_llvm`, `openvaf/linker`)
   - `mir_llvm`: LLVM IR generation from MIR
   - `linker`: Links generated code into `.osdi` dynamic libraries

6. **OSDI Interface** (`openvaf/osdi`)
   - Defines the OSDI 0.4 API structures
   - Header files in `openvaf/osdi/header/`
   - Handles module descriptors, parameters, Jacobian entries

### Database Architecture

Uses `salsa` for incremental compilation:
- **BaseDB** (`openvaf/basedb`): File system, VFS, parsing, linting
- **CompilationDB** (`openvaf/hir`): Extends BaseDB with semantic analysis

### Key Components

- **VFS** (`openvaf/vfs`): Virtual file system for source management
- **Syntax** (`openvaf/syntax`): Rowan-based CST with source mapping
- **Target** (`openvaf/target`): Platform-specific codegen configurations
- **Sim Backend** (`openvaf/sim_back`): Simulation-specific transformations
- **Supporting Libraries** (`lib/`):
  - `arena`: Arena allocator for HIR/MIR nodes
  - `bforest`: B-tree forest data structure
  - `typed_indexmap`: Type-safe indexed collections
  - `stdx`: Standard library extensions
  - `paths`: Path handling utilities

### Python/FFI Bindings

- **verilogae** (`verilogae/`): Python package exposing OpenVAF functionality
  - `verilogae_ffi`: C/C++ FFI layer (cbindgen/cppbindgen)
  - `verilogae_py`: PyO3-based Python bindings

## Development Workflow

### Debugging in VS Code
- Copy `.vscode/launch-openvaf-r.json` to `.vscode/launch.json`
- Two configurations available: Linux (CodeLLDB) and Windows (C++ tools)
- Debug builds disable rayon parallelization (`RAYON_NUM_THREADS=1`) for easier debugging

### Compiler Output Options
```bash
# Dump unoptimized MIR
openvaf-r --dump-unopt-mir model.va

# Dump optimized MIR
openvaf-r --dump-mir model.va

# Dump unoptimized LLVM IR
openvaf-r --dump-unopt-ir model.va

# Dump optimized LLVM IR
openvaf-r --dump-ir model.va
```

### Integration Tests
Located in `integration_tests/` with real-world device models (BSIM, MEXTRAM, EKV, etc.). Some models have restrictive licenses and don't affect the OpenVAF binary license.

Test data snapshots are in `openvaf/test_data/` organized by category:
- `ast/`: Abstract syntax tree snapshots
- `mir/`: MIR snapshots
- `osdi/`: OSDI descriptor snapshots
- `ui/`, `syn_ui/`: User-facing diagnostic snapshots

## OSDI 0.4 API

OpenVAF-reloaded generates models with OSDI 0.4 API, which extends 0.3 with:
- Parameter given flags for model/instance parameters
- Jacobian array extraction functions
- Model input lists (node pairs)
- Offset-based Jacobian loading (for harmonic balance)
- Nature/discipline/unit descriptors
- `OSDI_DESCRIPTOR_SIZE` symbol for binary compatibility

The descriptor structure is backward-compatible with OSDI 0.3 when cast appropriately.

## Code Organization

- **openvaf/**: Main compiler crates
- **verilogae/**: Python bindings
- **lib/**: Shared utility libraries
- **integration_tests/**: Real-world test models
- **xtask/**: Build automation tasks
- **melange/**: (Secondary component, minimal documentation)
- **sourcegen/**: Code generation utilities

## Important Notes

- The project uses workspace resolver version 2
- LLVM version is pinned to 18.1.8 (`llvm-sys = "181.1.1"`)
- Custom salsa fork: `git = 'https://github.com/pascalkuthe/salsa'`
- Rust edition: 2021, minimum version varies by crate
- Release builds use `lto = "off"` and `incremental = true` for faster iteration
- Optimized profile (`opt`) available with full LTO for production builds
