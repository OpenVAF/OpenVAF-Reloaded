# `verilogae_py` — Python extension module

**Location:** `verilogae/verilogae_py/`
**Role:** A CPython extension module (`verilogae.so` / `verilogae.pyd`) that
exposes VerilogAE to Python. It is written directly against the CPython C API
via `pyo3-ffi` (raw bindings, not the high-level PyO3 interface), with NumPy
integration for zero-copy vectorized model evaluation.

Cross-links: [verilogae INTERNALS](../verilogae/INTERNALS.md) ·
[verilogae_ffi INTERNALS](../verilogae_ffi/INTERNALS.md)

---

## Crate layout

```
verilogae/verilogae_py/src/
  lib.rs      — PyInit_verilogae entry point; FUNCTIONS table; module-level setup
  load.rs     — load_py(), load_info_py(), load_vfs() Python function implementations
  model.rs    — VaeModel, VaeFunction, VaeParam Python types; call implementation
  numpy.rs    — NumpyArray, ItemType; NumPy C-API integration
  typeref.rs  — Global PyObject* refs: NUMPY_API, NUMPY_ARR_TYPE, etc.
  ffi.rs      — new_type() helper; zero!() macro; PyTypeObject boilerplate
  offsets.rs  — offset_to!() macro for computing struct field offsets
  unicode.rs  — UTF-8 ↔ Python str helpers
  util.rs     — likely() / unlikely() branch prediction hints
  build.rs    — pyo3-build-config: detects Python installation and sets link flags
```

---

## Module entry point (`lib.rs`)

```rust
#[no_mangle]
pub unsafe extern "C" fn PyInit_verilogae() -> *mut PyObject
```

`PyInit_verilogae` is the standard CPython extension initialisation function.
It is called by the Python interpreter when `import verilogae` is executed.

The function:
1. Calls `PyType_Ready` for all three Python types (`VAE_MODEL_TY`,
   `VAE_FUNCTION_TY`, `VAE_PARAM_TY`).
2. Creates the module with `PyModule_Create`.
3. Calls `init_typerefs()` to populate global `PyObject*` caches
   (see [Global type refs](#global-type-refs)).
4. Adds `__version__` (from `env!("CARGO_PKG_VERSION")`).
5. Adds `__all__`.

---

## Python API

Three module-level functions:

| Python name | Rust handler | Description |
|-------------|-------------|-------------|
| `verilogae.load(path, ...)` | `load_py` | Compile the `.va` file (if not cached) and return a `VaeModel` with callable functions |
| `verilogae.load_info(path, ...)` | `load_info_py` | Compile only the modelcard (no callable functions); much faster |
| `verilogae.export_vfs(path, ...)` | `load_vfs` | Run the preprocessor and return a dict of `{virtual_path: file_contents}` |

All three accept keyword arguments for compilation options (include dirs,
defines, lint levels, cache dir, target, opt level, VFS dict).

### `load_py` / `load_info_py`

Both handlers call `verilogae_ffi::Opts::write()` to populate a C `Opts`
struct from the Python keyword arguments, then call `verilogae_load` (or
`verilogae_load` with `full_compile = false` for `load_info`). On success,
the raw library handle (a `*const c_void`) is wrapped in a `VaeModel`
Python object.

---

## Python types

### `VaeModel`

```
VaeModel attributes (read-only):
  functions   — tuple of VaeFunction objects (one per (*retrieve*) variable)
  modelcard   — tuple of VaeParam objects (one per parameter)
  op_vars     — tuple of str (operating-point variable names)
  module_name — str (the Verilog-A module name)
  nodes       — tuple of str (port names)
```

`VaeModel` stores the raw library handle (`*const c_void`) obtained from
`verilogae_load`. All attribute values are built once at construction time by
reading the exported globals (`verilogae_functions`, `verilogae_real_params`,
etc.) via `verilogae_ffi` accessors.

On deallocation (`tp_dealloc`), the library handle is passed to
`Library::from_raw` and dropped, which calls `dlclose`.

### `VaeFunction`

```
VaeFunction attributes (read-only):
  voltages    — tuple of str: voltage input names
  currents    — tuple of str: current input names
  real_params — tuple of str: real parameter names relevant to this function
  int_params  — tuple of str: integer parameter names
  str_params  — tuple of str: string parameter names
  dep_break   — dict: dependency-breaking variable names and their types

VaeFunction.__call__(voltages=..., currents=..., params=..., temp=...) -> ndarray
```

Calling a `VaeFunction` dispatches `verilogae_call_fun_parallel` (which
uses `rayon_core` internally). The call handler:
1. Extracts each named input from the kwargs as a NumPy array or scalar.
2. Builds a `FatPtr<f64>` for each input:
   - If scalar: `set_scalar(val)`.
   - If NumPy array: `set_ptr(data_ptr, stride_bytes)` using the NumPy
     array's strides.
3. Allocates an output array with `PyArray_SimpleNew`.
4. Calls `verilogae_call_fun_parallel(fun_ptr, cnt, voltages, currents, ...)`.
5. Returns the output ndarray.

### `VaeParam`

```
VaeParam attributes (read-only):
  name        — str
  units       — str
  description — str
  group       — str
  type        — str ("real", "integer", or "string")
  default     — float, int, or str (the default value)
  flags       — int (ParamFlags bitmask)
```

`VaeParam` is constructed from the modelcard data (the output of
`init_modelcard` plus the name/unit/description arrays).

---

## NumPy integration (`numpy.rs`)

`NumpyArray` wraps a raw `*mut PyObject` that is guaranteed to be a NumPy
array. It is constructed via `PyArray_SimpleNew` (for output arrays) or by
checking `PyArray_Check` on caller-supplied arguments.

```rust
pub enum ItemType { Float64, Int32, Complex128 }
```

`NumpyArray::data_ptr()` returns the raw data pointer and `strides()` returns
the byte strides. These are passed directly into `FatPtr::set_ptr`, enabling
zero-copy access to NumPy array data regardless of memory layout (C-order,
Fortran-order, or strided slices).

Complex-valued outputs use `NUMPY_CDOUBLE_DESCR` to create arrays of
`np.complex128`.

---

## Global type refs (`typeref.rs`)

Several global `*mut PyObject` pointers are cached after module init:

| Symbol | Value |
|--------|-------|
| `NUMPY_API` | NumPy C API function table pointer |
| `NUMPY_ARR_TYPE` | `np.ndarray` type object |
| `NUMPY_CDOUBLE_DESCR` | `np.dtype(complex)` descriptor |
| `TEMPERATURE_STR` | interned `"temp"` Python string |
| `VOLTAGES_STR` | interned `"voltages"` Python string |
| `CURRENTS_STR` | interned `"currents"` Python string |

`init_typerefs()` populates these globals once during `PyInit_verilogae`.
Using cached interned strings for hot-path keyword argument lookups avoids
repeated `PyUnicode_FromString` calls per function invocation.

---

## `offset_to!` macro and struct field offsets

Because `VaeModel`, `VaeFunction`, and `VaeParam` are `#[repr(C)]` structs
with `PyObject_HEAD` at the start, member offsets for `PyMemberDef` must be
computed at compile time. The `offset_to!` macro expands to
`std::mem::offset_of!(StructName, field)` (or an equivalent `addr_of!`-based
implementation for older compilers).

---

## Build configuration (`build.rs`)

`pyo3-build-config` detects the Python installation:
- The Python include path (`-I/usr/include/pythonX.Y`).
- The link library and flags (`-lpython3.X` or `python3X.lib`).
- The `Py_3_8` cfg flag (enabling `METH_FASTCALL` for the function table).

On the `static` feature path, the extension links `libverilogae.a` directly.
On the dynamic path, `libverilogae.so` must be available at runtime.

---

## Key design decisions

**Raw `pyo3-ffi` instead of high-level PyO3.** The high-level PyO3 API adds
significant overhead through GIL management, error conversion, and trait
dispatch. For a performance-oriented library that dispatches tens of thousands
of model evaluations per second, the raw CPython C API gives full control.
It also avoids PyO3's version compatibility shims.

**`FatPtr` for zero-copy NumPy access.** The `ptr + stride` design means the
Python caller can pass a column from a 2-D NumPy array (non-contiguous) or a
scalar without any copying. VerilogAE reads through the stride directly in the
generated machine code.

**`rayon_core::scope` inside `verilogae_call_fun_parallel`.** The
parallelism lives in the Rust layer, invisible to Python. The GIL is not
released during the call (the GIL release would require the high-level PyO3
API); instead, the generated model function is assumed to be GIL-independent
(it is: it does only arithmetic on the provided data pointers).

**Lazy NumPy import.** `import_array()` (NumPy's C-API initializer macro) is
deferred to the first call that needs NumPy, rather than at module init. This
avoids a hard dependency on NumPy being installed even if the user only uses
`load_info()`.

> **TODO(verify):** The `import_array()` call location and whether NumPy is
> actually a lazy dependency or is required at module init — the code was not
> fully read due to macro complexity in `typeref.rs`.
