# `stdx` — Standard Extensions

**Location:** `lib/stdx/`
**Role:** A grab-bag of small utilities that are used across almost every crate
in OpenVAF but are too small to warrant their own library and too
OpenVAF-specific (or upstream-unavailable) to pull from a third-party crate.
It has no runtime dependencies. Every other crate in the workspace that needs
one of these utilities adds `stdx` to its `Cargo.toml`.

Cross-links: [mir INTERNALS](../mir/INTERNALS.md) ·
[mir\_build INTERNALS](../mir_build/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Module map

| File | Contents |
|------|----------|
| `src/lib.rs` | Top-level re-exports; CI flags; test helpers; `Upcast<T>` trait |
| `src/ieee64.rs` | `Ieee64` — bit-exact f64 with C99 hex format |
| `src/packed_option.rs` | `ReservedValue` trait; `PackedOption<T>` |
| `src/macros.rs` | Code-generation macros for index types, enums, and formatters |
| `src/iter.rs` | `zip` free function; `multiunzip` / `MultiUnzip` up to 12-tuples |
| `src/vec.rs` | `ensure_contains_elem`; `VecExtensions`; `SliceExntesions` |
| `src/pretty.rs` | `List<C>` — configurable separator/prefix/postfix for error messages |

---

## `Ieee64` — bit-exact floating-point

```rust
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct Ieee64(u64);
```

`Ieee64` stores an IEEE 754 binary64 value as its raw bit pattern. The key
design choice is that it derives `Eq` and `Hash` directly on the `u64` bits,
not on the floating-point value. This makes `Ieee64` safe to use as a map key
and in `#[derive(Eq)]` structs — something `f64` cannot do because NaN ≠ NaN
under IEEE semantics.

The MIR uses `Ieee64` as the payload for `F64` constant values in the `Value`
enum. Any time a `real` literal appears in Verilog-A source, it eventually
becomes an `Ieee64` constant in the MIR instruction stream.

### Construction

```rust
Ieee64::with_float(1.5)   // from f64: calls f64::to_bits()
Ieee64::with_bits(0x3FF8000000000000)  // from raw u64
"0x1.8000000000000p0".parse::<Ieee64>() // from hex string
```

### Display format

`Ieee64` implements `Display` using `format_float(bits, w=11, t=52, …)`, which
produces the C99 `printf "%a"` hexadecimal floating-point format. The format
is:

| Class | Example |
|-------|---------|
| Zero | `0.0` / `-0.0` |
| Normal | `0x1.8000000000000p0` (1.5) |
| Subnormal | `0x0.8000000000000p-1022` |
| Infinity | `+Inf` / `-Inf` |
| Quiet NaN | `+NaN` / `+NaN:0x1` (with payload) |
| Signaling NaN | `+sNaN:0x1` |

The format is lossless: `parse_float` is the exact inverse of `format_float`.
Any `f64` bit pattern survives a round-trip through `Display` → `FromStr`
without changing a single bit. This is required for MIR serialization — the
compiler's test suite compares MIR text dumps byte-for-byte.

### `parse_float` internals

`parse_float` reads the hex significand and decimal exponent, normalizes the
significand so the implicit leading `1` bit is at position `t` (bit 52), then
reconstructs the biased exponent field. It rejects:

- Decimal fractions (only `0.0` is allowed in decimal; everything else must be
  `0x…`)
- Too many significand bits (would require rounding, which would be lossy)
- Exponents out of range (overflow → `"Magnitude too large"`, underflow →
  `"Magnitude too small"`)
- Subnormal values where the shift would discard set bits (`"Subnormal
  underflow"`)

The tight rejection policy ensures the format remains a bijection.

---

## `PackedOption<T>` — zero-overhead optional index

```rust
pub trait ReservedValue {
    fn reserved_value() -> Self;
    fn is_reserved_value(&self) -> bool;
}

#[derive(Clone, Copy, …)]
#[repr(transparent)]
pub struct PackedOption<T: ReservedValue>(T);
```

`PackedOption<T>` stores an optional `T` in the same space as `T` itself by
repurposing one value of `T` as the sentinel for `None`. For `u32`-backed
index types, the sentinel is `u32::MAX` — a value that can never be a valid
index into any realistic array.

**Why not `Option<T>`?** On a 64-bit machine, `Option<u32>` is 8 bytes because
the compiler needs a discriminant byte (padded to alignment). `PackedOption<u32>`
is 4 bytes. In data structures with millions of entries — like the MIR's
instruction and block tables — the difference is meaningful. The MIR's
`Phi` nodes use `PackedOption<Value>` to represent optional predecessor values,
and the SSA builder uses it for optional block predecessors.

### The `impl_idx_from!` macro and `ReservedValue`

The `impl_idx_from!` macro (in `macros.rs`) automatically implements
`ReservedValue` for any `u32`-backed index newtype:

```rust
impl_idx_from!(Block(u32));
// expands to, among other things:
impl ReservedValue for Block {
    fn reserved_value() -> Self { Block(u32::MAX) }
    fn is_reserved_value(&self) -> bool { self.0 == u32::MAX }
}
```

This means `PackedOption<Block>` works out of the box for any type declared
with `impl_idx_from!`.

### API

`PackedOption<T>` mirrors the `Option<T>` API closely:

```rust
packed.is_some()          // true if not the reserved value
packed.is_none()
packed.expand()           // → Option<T>
packed.map(|t| …)         // → Option<U>
packed.unwrap()           // panics if None
packed.unwrap_unchecked() // noop in release, panics in debug
packed.take()             // → Option<T>, leaves None behind
```

Conversion in both directions is via `From`:

```rust
let p: PackedOption<Block> = Some(b).into();  // From<Option<T>>
let p: PackedOption<Block> = b.into();        // From<T>; debug-asserts not reserved
let o: Option<Block> = p.into();             // From<PackedOption<T>>
```

---

## Macros

### Index type macros

These four macros are the most widely used items in `stdx`. Nearly every
entity type in the compiler — `Block`, `Value`, `Inst`, `Place`, `FileId`,
`LocalScopeId`, etc. — is declared as a `pub struct Foo(u32)` newtype and then
wired up with one of these macros.

**`impl_idx_from!(Foo(u32))`** — generates:

- `From<u32> for Foo` and `From<Foo> for u32`
- `From<usize> for Foo` (with a debug bounds check) and `From<Foo> for usize`
- `impl ReservedValue for Foo { reserved = Foo(u32::MAX) }`

This is the most common macro call in the codebase. It gives the type full
numeric interoperability and plugs it into `PackedOption`.

**`impl_idx_from_readonly!(Foo(u32))`** — generates only the `Foo → u32` and
`Foo → usize` conversions, not the reverse. Used for types where constructing
from a raw integer would be unsafe (e.g. handles into a validated table).

**`impl_idx_math!(Foo(u32))`** — generates `Add`, `Sub`, `AddAssign`,
`SubAssign` for combinations of `Foo`, `u32`, and `usize`. Used for index
types that need arithmetic (e.g. advancing a cursor or computing an offset).

**`impl_idx_math_from!(Foo(u32))`** — shorthand for
`impl_idx_from!` + `impl_idx_math!`.

### Enum conversion macros

**`impl_from!(A, B, C for MyEnum)`** — generates `From<A> for MyEnum`,
`TryFrom<MyEnum> for A`, and so on for each variant. The variants must be
tuple variants `MyEnum::A(A)`. Avoids writing the same boilerplate repeatedly
for sum types like HIR nodes.

**`impl_from_typed!(Foo(FooType), Bar(BarType) for MyEnum)`** — same but for
variants where the inner type differs from the variant name.

### Formatter macros

**`impl_display!`**, **`impl_debug!`**, **`impl_debug_display!`** all delegate
to `impl_fmt!`, which generates a `fmt::Display` or `fmt::Debug` impl from a
match expression:

```rust
impl_display! {
    match MyError {
        MyError::NotFound(name) => "symbol '{}' not found", name;
        MyError::TypeMismatch   => "type mismatch";
    }
}
```

This pattern is used throughout the diagnostics layer to keep error message
strings next to the variant they describe.

### Utility macros

**`format_to!($buf, "fmt {}", arg)`** — appends a formatted string to an
existing `String` using `fmt::Write`, avoiding a heap allocation compared to
`format!` followed by `push_str`. Used in the pretty-printer and in diagnostic
formatting where a message is built incrementally.

**`eprintln!`** — wraps `std::eprintln!` but panics on CI (`IS_CI = true`) if
called. This ensures that debug `eprintln!` calls are never accidentally left
in the codebase and reach a CI run, where they would silently pass but
contaminate output.

---

## `iter` — iterator utilities

### `zip`

```rust
pub fn zip<A: IntoIterator, B: IntoIterator>(a: A, b: B) -> Zip<…>
```

A free function that calls `a.into_iter().zip(b)`. Exists because the method
form requires the left side to already be an iterator; the free-function form
accepts any `IntoIterator` on both sides, which is more ergonomic when zipping
slices or ranges.

### `multiunzip`

```rust
pub fn multiunzip<FromI, I: IntoIterator>(i: I) -> FromI
where I::IntoIter: MultiUnzip<FromI>
```

`MultiUnzip` is implemented for iterators of 1- to 12-tuples. It consumes the
iterator and distributes each column into a separate `Default + Extend`
collection. This is the n-ary generalization of `Iterator::unzip`:

```rust
let items = vec![(1u32, "a", true), (2, "b", false)];
let (nums, strs, bools): (Vec<u32>, Vec<&str>, Vec<bool>) = multiunzip(items);
// nums = [1, 2], strs = ["a", "b"], bools = [true, false]
```

Used in the HIR and MIR where a single pass over a list needs to produce
multiple parallel output vectors simultaneously.

---

## `vec` — slice and vector extensions

### `ensure_contains_elem`

```rust
pub fn ensure_contains_elem<T>(vec: &mut Vec<T>, elem: usize, fill_value: impl FnMut() -> T)
```

Grows `vec` until index `elem` is valid, filling new slots with `fill_value`.
Used when building a sparse mapping from indices to values: rather than
pre-allocating with a known bound, the builder calls `ensure_contains_elem`
on each insert and the vector grows on demand.

### `SliceExntesions`

The trait (note the typo in the source — `Exntesions` — preserved here
verbatim) extends `[T]` with methods for obtaining mutable references to
multiple distinct elements simultaneously:

```rust
pub trait SliceExntesions<T> {
    fn pick2_mut(&mut self, a: usize, b: usize) -> (&mut T, &mut T);
    fn pick3_mut(&mut self, a: usize, b: usize, c: usize) -> (&mut T, &mut T, &mut T);
    fn pick_n_mut<const N: usize>(&mut self, indices: [usize; N]) -> [&mut T; N];
}
```

Rust's borrow checker rejects `(&mut v[a], &mut v[b])` because it cannot prove
`a ≠ b` at compile time. These methods assert uniqueness at runtime and then
use `unsafe` raw-pointer arithmetic to produce the independent mutable
references. The pattern is needed in the MIR builder and optimizer when two
blocks or two instruction slots must be mutated in the same operation.

`pick_n_mut` is the general form: it takes a const-generic array of `N`
indices, validates all-distinct and in-bounds, then casts the raw pointer array
to a reference array via `ptr::read`.

---

## `pretty` — list formatting for diagnostics

```rust
pub struct List<C> {
    pub data:               C,
    pub separator:          &'static str,   // default: ", "
    pub final_separator:    &'static str,   // default: " or "
    pub prefix:             &'static str,   // default: ""
    pub postfix:            &'static str,   // default: ""
    pub break_after:        u32,            // default: 10
    pub first_break_after:  u32,            // default: 5
}
```

`List<C>` wraps any collection and formats it as a human-readable list in
`Display`. The `final_separator` is used between the last two items, so a list
of three elements `[x, y, z]` formats as `"x, y or z"` — the Oxford-comma-free
form used in most of OpenVAF's error messages.

`break_after` and `first_break_after` insert newlines when the list is long.
The first line breaks after `first_break_after` items; subsequent lines break
every `break_after` items. This prevents a single run-on line when reporting
"expected one of: keyword1, keyword2, …, keywordN."

The builder methods allow construction without writing struct literals:

```rust
List::new(&["module", "discipline", "nature"])
    .with_final_separator(" or ")
    .surround("`")
// → "`module`, `discipline` or `nature`"

List::path(&["std", "constants"])
// separator = ".", final_separator = "."
// → "std.constants"
```

`List::path` is a convenience constructor for dot-separated identifier paths
used in diagnostic messages that reference qualified names.

---

## Top-level utilities (`lib.rs`)

### CI and test-gating constants

```rust
pub const IS_CI: bool = option_env!("CI").is_some();
pub const SKIP_HOST_TESTS: bool = option_env!("CI").is_some() && cfg!(windows);
```

Both are compile-time constants baked from environment variables. `IS_CI` is
`true` when the `CI` environment variable is set (standard for GitHub Actions,
CircleCI, etc.). `SKIP_HOST_TESTS` is `true` on CI Windows builds, where
host-specific tests that require the local toolchain installed are suppressed.

### Test helper functions

```rust
pub fn skip_slow_tests() -> bool
pub fn ignore_dev_tests<T: ?Sized>(_: &T) -> bool
pub fn ignore_slow_tests<T: ?Sized>(_: &T) -> bool
pub fn ignore_never<T: ?Sized>(_: &T) -> bool
```

These are passed as the `ignore` argument in `#[rstest]` or similar test
harnesses. `skip_slow_tests` also creates a sentinel file at
`target/.slow_tests_cookie` when slow tests do run, which CI scripts can check
to confirm the full test suite was exercised.

`project_root()` walks up the directory tree from `CARGO_MANIFEST_DIR` until it
finds a directory containing `README.md`, which it treats as the workspace root.
This makes `openvaf_test_data("resistor.va")` and `integration_test_dir("osdi")`
work regardless of which crate's tests call them.

### `Upcast<T>`

```rust
pub trait Upcast<T: ?Sized> {
    fn upcast(&self) -> &T;
}
```

A manual coercion trait for upcasting a concrete database type to one of its
supertrait objects. Salsa databases implement several query group traits; code
that holds a `&dyn DatabaseA` sometimes needs a `&dyn DatabaseB` without going
through the concrete type. `Upcast<dyn DatabaseB>` on the database struct
provides that bridge. This pattern appears in `basedb` and `hir_def` where the
Salsa database is split into layered query groups.

---

## Key design decisions

**`Ieee64` uses `u64` equality, not `f64` equality.** Two `Ieee64` values are
equal iff their bit patterns are identical. This means `+NaN ≠ -NaN` (which
have different sign bits) and `+0.0 ≠ -0.0` (different sign bits). These
distinctions matter in the MIR constant table, where two bit-identical
constants should share the same `Value` slot but two bit-different constants
should not, regardless of IEEE numeric equivalence.

**`PackedOption` uses `MAX` as the sentinel.** Using the all-ones bit pattern
(`u32::MAX = 0xFFFF_FFFF`) is safe because no realistic data structure has
4 billion entries. The sentinel is statically known, so `is_reserved_value`
compiles to a single comparison instruction with no branching.

**`SliceExntesions` uses unsafe raw pointers.** There is no safe way to obtain
two `&mut T` from the same slice in stable Rust without using
`split_at_mut` (which only works for disjoint prefix/suffix). The
`pick2_mut`/`pick3_mut`/`pick_n_mut` implementations use `assert_ne!` to
establish the disjointness invariant at runtime, then perform the cast. The
bounds check (`assert!(idx < len)`) ensures the pointers are valid; the
distinctness check ensures they are non-aliasing.

**`List` separates `separator` from `final_separator`.** Using `" or "` only
before the last element and `", "` before all others produces grammatically
correct English lists ("x, y or z") without always-Oxford-comma or
always-plain-comma variants. This is a deliberate UX decision: OpenVAF's error
messages should read as natural English prose, not as machine-formatted lists.

**No runtime dependencies.** `stdx` has an empty `[dependencies]` section in
`Cargo.toml`. This keeps compile times for all downstream crates low and
ensures `stdx` can be used as a foundation without pulling in any transitive
dependency graph.
