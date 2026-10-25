# `typed_indexmap` — Type-safe ordered maps and sets

**Location:** `lib/typed_indexmap/`
**Role:** Thin wrappers around `indexmap`'s `IndexMap` and `IndexSet` that
enforce a typed position index at compile time. `TiMap<I,K,V>` gives you an
`IndexMap<K,V>` where the integer position returned by insertion has type `I`
rather than `usize`. `TiSet<K,V>` does the same for `IndexSet<V>` with
position type `K`. Both use `ahash::RandomState` as the hasher.

Cross-links: [arena INTERNALS](../arena/INTERNALS.md) ·
[stdx INTERNALS](../stdx/INTERNALS.md) ·
[mir INTERNALS](../mir/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
typed_indexmap   (lib/typed_indexmap/)
  ├─► hir_lower  (string interning, node-to-position maps)
  ├─► mir        (value/block position maps)
  ├─► mir_autodiff
  ├─► sim_back   (signal and parameter tables)
  └─► osdi       (ABI parameter index tables)
```

The crate has two external dependencies: `indexmap 2.x` (insertion-order
hash map/set with O(1) position lookup) and `ahash 0.8` (non-cryptographic
hasher). It does not depend on `stdx` or `arena`.

---

## `TiMap<I, K, V>`

```rust
#[repr(transparent)]
pub struct TiMap<I, K, V> {
    pub raw: IndexMap<K, V, ahash::RandomState>,
    _marker: PhantomData<fn(I) -> I>,
}
```

`TiMap<I,K,V>` is a zero-cost newtype over `IndexMap<K,V,ahash::RandomState>`.
The `#[repr(transparent)]` guarantees identical memory layout, which allows a
safe `AsRef` conversion implemented via an unsafe pointer cast:

```rust
impl<I, K, V> AsRef<TiMap<I, K, V>> for IndexMap<K, V, ahash::RandomState> {
    fn as_ref(&self) -> &TiMap<I, K, V> {
        // SAFETY: repr(transparent) — same layout
        unsafe { &*(self as *const _ as *const TiMap<I, K, V>) }
    }
}
```

The phantom `fn(I) -> I` (rather than just `I`) makes the marker invariant in
`I` and keeps `TiMap` `Send + Sync` regardless of whether `I` is, because no
`I` value is actually stored.

### Key methods

| Method | Signature | What it does |
|--------|-----------|-------------|
| `insert_full` | `(&mut self, K, V) -> (I, Option<V>)` | Insert and return the typed position + displaced value |
| `next_index` | `(&self) -> I` | `self.raw.len().into()` — the index the next insert would get |
| `get_index` | `(&self, I) -> Option<(&K, &V)>` | Position → entry (delegates to `IndexMap::get_index`) |
| `index` | `<Q>(&self, &Q) -> Option<I>` | Key → position (delegates to `IndexMap::get_index_of`) |
| `iter_enumerated` | `(&self) -> Iter<'_, I, K, V>` | Yields `(I, (&K, &V))` tuples |
| `keys` | `(&self) -> impl Iterator<Item = &K>` | Delegates to `IndexMap::keys` |
| `Index<I>` | `index(&self, I) -> &V` | Panicking position lookup |
| `IndexMut<I>` | `index_mut(&mut self, I) -> &mut V` | Panicking mutable position lookup |

`insert_full` is the primary constructor: it returns the typed index `I` for
the inserted entry so the caller can store it without an extra `index()` call.
If the key was already present, the old value is returned as `Some(old)` and
the existing position is returned — the map is not re-ordered.

### Default and construction

`TiMap::new()` creates an empty map. `TiMap::default()` delegates to
`IndexMap::default()` which uses `ahash::RandomState`. The public `raw` field
gives direct access to the underlying `IndexMap` for operations not wrapped by
`TiMap`.

---

## `TiSet<K, V>`

```rust
pub struct TiSet<K, V> {
    pub raw: IndexSet<V, ahash::RandomState>,
    _marker: PhantomData<fn(K) -> K>,
}
```

`TiSet<K,V>` wraps `IndexSet<V>` where `K` is the typed position index and `V`
is the element. Despite the name, `K` is not the key — `V` is both the stored
element and the lookup key. `K` is only a phantom type for the position integer.

This differs from `TiMap` in one important way: there is no `#[repr(transparent)]`
on `TiSet`. The `AsRef` shortcut is not provided; callers use `tiset.raw`
directly when they need `IndexSet` methods.

### Key methods

| Method | Signature | What it does |
|--------|-----------|-------------|
| `ensure` | `(&mut self, V) -> (K, bool)` | Insert if absent; return `(position, was_new)` |
| `insert` | `(&mut self, V) -> bool` | Insert; return `true` if new |
| `replace` | `(&mut self, K, V) -> V` | Insert `new_val` at tail, then `swap_indices` to put it at `index` |
| `index` | `<Q>(&self, &Q) -> Option<K>` | Value → position |
| `indices` | `(&self, &[V]) -> impl Iterator<Item=K>` | Batch value→position lookup |
| `unwrap_index` | `(&self, &V) -> K` | Panicking value → position |
| `contains` | `<Q>(&self, &Q) -> bool` | Delegates to `IndexSet::contains` |
| `iter_enumerated` | `(&self) -> Iter<K, V>` | Yields `(K, &V)` tuples |
| `retain` | `(&mut self, FnMut(K, &V) -> bool)` | Retain with typed position exposed |
| `Index<K>` | `index(&self, K) -> &V` | Panicking position lookup |

`ensure` is the idiom for interning-style tables: insert the value if not
already present, then return its stable position regardless. The boolean lets
the caller know if this was the first occurrence.

`replace(index, new_val)` is implemented by inserting `new_val` at the tail
(getting a fresh index), then calling `swap_indices` to move it to the desired
position. This in-place replacement keeps the position stable for all other
entries.

---

## The `ahash` hasher choice

Both `TiMap` and `TiSet` hard-code `ahash::RandomState` as the hasher rather
than `std::collections::hash_map::RandomState` (SipHash 1-3). `ahash` is
non-cryptographic but significantly faster for short keys (integers, short
strings) on modern hardware. The random state is seeded at runtime, so
hash-flooding denial-of-service attacks are still prevented — the trade-off
versus SipHash is purely performance.

For OpenVAF's use cases (compiler internal tables keyed by integers, interned
strings, or compact model parameter names), the non-cryptographic property is
acceptable and the speed advantage is real.

---

## Where it is used in OpenVAF

### `hir_lower` — name→node tables

`hir_lower` uses `TiMap` to build tables that map identifiers to their
lowered HIR nodes. The typed index returned by `insert_full` is then stored
inside the IR to reference entries without repeated hash lookups.

### `mir` — value and block position maps

The MIR uses `TiSet` for interning small integer-keyed tables (e.g., mapping
`Value`s to positions in a result set). `iter_enumerated` makes it easy to
emit numbered entries from a set during code generation.

### `sim_back` and `osdi` — parameter tables

The simulator back-end and OSDI ABI layer use `TiMap<ParamId, String, ParamInfo>`
style tables to assign stable numeric positions to compact model parameters.
The OSDI ABI requires parameters to appear at fixed integer offsets in the
emitted struct; `TiMap` provides both the lookup (`index(name)`) and the stable
integer position (`insert_full` → `ParamId`) in one structure.

### `mir_autodiff` — AD variable tables

`mir_autodiff` uses `TiSet` to collect the set of values that require
derivative computation. `ensure` maps each `Value` to a derivative index
without duplicates; `iter_enumerated` then drives the emission of derivative
instructions.

---

## Worked example: OSDI parameter table

Consider a compact model with three parameters:

```verilog-a
parameter real tnom = 27.0;
parameter real is   = 1e-14;
parameter real n    = 1.0;
```

`sim_back` builds:

```rust
let mut params: TiMap<ParamId, String, ParamInfo> = TiMap::new();

let (tnom_id, _) = params.insert_full("tnom".to_string(), ParamInfo { default: 27.0, .. });
// tnom_id = ParamId(0)

let (is_id, _) = params.insert_full("is".to_string(),   ParamInfo { default: 1e-14, .. });
// is_id = ParamId(1)

let (n_id, _) = params.insert_full("n".to_string(),     ParamInfo { default: 1.0,  .. });
// n_id = ParamId(2)
```

Later, when generating the OSDI struct, the code iterates:

```rust
for (id, (name, info)) in params.iter_enumerated() {
    // id: ParamId, name: &String, info: &ParamInfo
    emit_osdi_param(id.into(), name, info.default);
}
```

This emits parameter 0 (`tnom`), 1 (`is`), 2 (`n`) in insertion order —
matching the order in which the model's `paramset` is defined, which is the
order OSDI expects. The `id: ParamId` comes directly from the typed position,
not from a separate counter.

If a later pass needs to look up the offset for `is` by name:

```rust
let offset: Option<ParamId> = params.index("is");
// Some(ParamId(1))
```

No re-scanning; `IndexMap::get_index_of` is O(1).

---

## Key design decisions

**`#[repr(transparent)]` on `TiMap` enables a free `AsRef` cast.** Because
`TiMap<I,K,V>` and `IndexMap<K,V,ahash::RandomState>` have identical layout,
the conversion is a pointer cast with no runtime cost. This lets code that
receives a `&IndexMap` from an external source treat it as a `&TiMap` without
copying or wrapping. `TiSet` does not have this guarantee (no `repr(transparent)`)
so no equivalent cast is provided.

**Typed phantom index, not a newtype integer.** `I` is never stored — it
only appears in `PhantomData`. The underlying storage remains `usize` (as
`IndexMap` uses internally). This means there is no runtime overhead from
the typed index; the compile-time error you get when mixing up `ParamId` and
`NodeId` is free.

**`fn(I) -> I` phantom for invariance.** Using `PhantomData<fn(I) -> I>`
rather than `PhantomData<I>` makes both `TiMap` and `TiSet` invariant in `I`.
This is the conservative choice: covariant phantoms can allow unsound lifetime
substitutions for types that don't actually store `I`. Since `I` is always a
plain integer type in practice (e.g., `u32` wrapped by `impl_idx_from!`), the
variance is invisible at the use site.

**Hard-coded `ahash` rather than a generic hasher parameter.** The `indexmap`
crate supports `BuildHasher` as a generic parameter; `typed_indexmap` fixes it
to `ahash::RandomState`. This simplifies all the type signatures — no `S: BuildHasher`
bound propagating through every caller — and matches OpenVAF's preference for
concrete fast defaults over maximally generic abstractions.

**`ensure` over `insert_or_ignore`.** The `ensure(val) -> (K, bool)` API
returns both the position and a flag, making it a single call for the common
interning pattern ("get the position of this value, inserting it if new").
Splitting this into `contains` + conditional `insert_full` would require two
hash lookups; `ensure` uses `IndexSet::insert_full` which does one.
