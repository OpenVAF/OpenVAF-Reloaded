# `base_n` — Integer-to-string encoding in arbitrary bases

**Location:** `lib/base_n/`
**Role:** Converts a `u128` integer into its string representation in any base
from 2 to 64. The only public API is two functions — `encode` and `push_str` —
and three base-constant exports. No dependencies; pure `std`.

Cross-links: [stdx INTERNALS](../stdx/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
base_n   (lib/base_n/)
  ├─► openvaf  (cache file naming)
  ├─► mir_llvm (local LLVM symbol generation)
  └─► osdi     (module UUID → symbol name, temp file extensions)
```

`base_n` has no dependencies of its own. It is a leaf utility used in three
places where a compact, filesystem-safe string representation of an integer is
needed.

---

## The alphabet

```rust
const BASE_64: &[u8; 64] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ@$";
```

Digits are assigned in this order:

| Position | Characters | Notes |
|----------|-----------|-------|
| 0–9 | `0`–`9` | decimal digits |
| 10–35 | `a`–`z` | lowercase |
| 36–61 | `A`–`Z` | uppercase |
| 62 | `@` | |
| 63 | `$` | |

The three named base constants select subsets of this alphabet:

| Constant | Value | Alphabet used |
|----------|-------|--------------|
| `CASE_INSENSITIVE` | 36 | digits + lowercase only (safe for case-insensitive filesystems and identifiers) |
| `ALPHANUMERIC_ONLY` | 62 | digits + lowercase + uppercase (no `@`/`$`) |
| `MAX_BASE` | 64 | full alphabet |

Any base between 2 and 64 (inclusive) is accepted. The `debug_assert` in
`push_str` catches out-of-range bases in debug builds.

---

## API

### `push_str(n: u128, base: usize, output: &mut String)`

Appends the base-`base` representation of `n` to `output` without allocating
a separate string. This is the core function; `encode` is a thin wrapper.

The implementation uses a fixed stack buffer of 128 bytes:

```rust
let mut s = [0u8; 128];
let mut index = 0;

loop {
    s[index] = BASE_64[(n % base) as usize];
    index += 1;
    n /= base;
    if n == 0 { break; }
}
s[0..index].reverse();
output.push_str(str::from_utf8(&s[0..index]).unwrap());
```

Digits are produced least-significant-first (each iteration takes `n % base`),
then the slice is reversed in place to get most-significant-first order. The
128-byte buffer is large enough for any `u128` in any base ≥ 2: the longest
representation is `u128::MAX` in base 2, which is 128 bits and fits exactly.

### `encode(n: u128, base: usize) -> String`

Allocates a fresh `String`, calls `push_str`, and returns it. Use `push_str`
when appending to an existing string to avoid an extra allocation.

---

## Where it is used in OpenVAF

### `openvaf/src/cache.rs` — cache file naming

The compiler caches compiled `.osdi` files to avoid recompilation when neither
the source nor the compiler options have changed. The cache key is an MD5 hash
of the source file contents (token-by-token), compiler version, defines, and
lint settings. The 128-bit MD5 digest is converted to a compact filename:

```rust
let hash = u128::from_ne_bytes(*hash(db, &opts.defines));
let hash = base_n::encode(hash, base_n::CASE_INSENSITIVE);
format!("{}.osdi", hash)
```

`CASE_INSENSITIVE` (base 36) is used so the filename is valid on
case-insensitive filesystems (Windows, macOS HFS+). A `u128` in base 36
produces at most 25 characters, which is compact and collision-resistant.

### `mir_llvm/src/context.rs` — local LLVM symbol names

The LLVM code-generation context needs to generate unique names for internal
(private-linkage) symbols. A monotonically incrementing counter is converted
to a short alphanumeric suffix:

```rust
pub fn generate_local_symbol_name(&self, prefix: &str) -> String {
    let idx = self.local_gen_sym_counter.get();
    self.local_gen_sym_counter.set(idx + 1);
    let mut name = String::with_capacity(prefix.len() + 6);
    name.push_str(prefix);
    name.push('.');
    base_n::push_str(idx as u128, base_n::ALPHANUMERIC_ONLY, &mut name);
    name
}
```

`ALPHANUMERIC_ONLY` (base 62) is used because LLVM symbol names allow
alphanumeric characters but `@` and `$` have special meaning in some LLVM IR
contexts. The `.` separator before the numeric suffix ensures no collision with
user-defined names (Verilog-A identifiers cannot contain `.`).

### `osdi/src/compilation_unit.rs` — module UUID → symbol prefix

Each compiled Verilog-A module has a UUID. The UUID is encoded in base 36
and used as the OSDI symbol prefix that linkers and simulators use to find
the module's entry points:

```rust
let sym = base_n::encode(module.info.module.uuid(db) as u128, base_n::CASE_INSENSITIVE);
```

### `osdi/src/lib.rs` — temporary object file extensions

When compiling multiple modules in parallel, temporary object files are named
with unique extensions derived from a counter:

```rust
let num = base_n::encode((i + 1) as u128, CASE_INSENSITIVE);
let extension = format!("o{num}");
dst.with_extension(extension)
```

This avoids collision between the temporary `.o` files for each module/pass
combination without requiring a separate temp-directory.

---

## Worked example

```rust
base_n::encode(255u128, 16)         // "ff"
base_n::encode(255u128, 36)         // "73"
base_n::encode(u128::MAX, 36)       // 25-character string
base_n::encode(12345u128, 62)       // "3D7"
```

For the cache file name use case, an MD5 digest of a typical resistor model
produces a `u128` such as `0x9f3c8a1d…`. In base 36 this becomes something
like `"2k7mxp4jqr9b0n3vd"` — short enough to be a filename, long enough that
collisions are negligible.

---

## Key design decisions

**Stack-allocated output buffer.** The 128-byte buffer on the stack avoids any
heap allocation inside `push_str`. Since a `u128` in base 2 has exactly 128
digits, the buffer can never overflow. The function appends to a caller-supplied
`String` rather than returning a new one, so the caller controls allocation.

**`push_str` as the primary function.** Callers that are building a longer
string (like `generate_local_symbol_name`, which prepends a prefix) avoid
an intermediate allocation by using `push_str` directly. `encode` is a
one-line convenience wrapper for the common "I need a standalone string" case.

**`u128` as the input type.** MD5 produces 128 bits; UUID-style integers also
fit in 128 bits. Using `u128` as the universal input type means no truncation
is needed at any call site, even for the largest hash values OpenVAF produces.

**Fixed `BASE_64` alphabet with named constants.** Rather than letting callers
supply their own alphabet, the crate defines one canonical 64-character
alphabet and three named base constants for the three use cases OpenVAF
actually needs. This prevents subtle bugs from custom alphabets (e.g., using
`+`/`/` from base64 in a filesystem context) while keeping the API minimal.
