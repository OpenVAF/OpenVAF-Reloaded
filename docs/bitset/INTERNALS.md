# `bitset` — Typed bit-set collections

**Location:** `lib/bitset/`
**Role:** A family of bit-set types parameterized over typed index newtypes.
Every type in this crate stores sets of values drawn from a domain of `T`
where `T: Into<usize>`. The crate provides dense, sparse, hybrid, and matrix
variants, all sharing a common word type (`u64`) and iteration strategy.

Cross-links: [stdx INTERNALS](../stdx/INTERNALS.md) ·
[arena INTERNALS](../arena/INTERNALS.md) ·
[mir\_opt INTERNALS](../mir_opt/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
bitset  (lib/bitset/)
  deps: arrayvec, stdx
  └─► mir_build  (SSABuilder uses BitSet<Block>)
  └─► mir        (BitSet used in liveness data structures)
  └─► mir_opt    (BitSet, HybridBitSet, SparseBitMatrix in SCCP, DCE, GVN, ADCE)
```

`bitset` depends on `stdx` for `zip` and the `SliceExntesions`/`VecExtensions`
traits (used in matrix row-manipulation). It depends on `arrayvec` for the
stack-allocated backing of `SparseBitSet`.

---

## Module map

| File | Types |
|------|-------|
| `src/lib.rs` | `BitSet<T>`, `GrowableBitSet<T>`, `BitIter<T>`; `bitwise` helper; operation traits |
| `src/sparse.rs` | `SparseBitSet<T>` (module-private); `SPARSE_MAX = 8` |
| `src/hybrid.rs` | `HybridBitSet<T>`, `HybridIter<T>` |
| `src/matrix.rs` | `BitMatrix<R,C>`, `SparseBitMatrix<R,C>`, `GrowableSparseBitMatrix<R,C>` |

---

## Shared foundations

### Word type

```rust
pub type Word = u64;
pub const WORD_BYTES: usize = 8;
pub const WORD_BITS:  usize = 64;
```

All bitsets store bits packed into `u64` words. Using 64-bit words means the
compiler can emit AVX2 vectorized loops when processing multiple words at once,
and `u64::trailing_zeros()` gives the position of the lowest set bit in a
single instruction.

### `word_index_and_mask`

```rust
fn word_index_and_mask<T: Into<usize>>(elem: T) -> (usize, Word) {
    let elem = elem.into();
    (elem / WORD_BITS, 1 << (elem % WORD_BITS))
}
```

Every insert, remove, and contains call bottoms out here. The division by 64
and the bit mask are both powers-of-two operations, so the compiler emits a
right-shift and a single-bit-set with no division instruction.

### `bitwise` — vectorizable word-level operation

```rust
fn bitwise<Op>(out_vec: &mut [Word], in_vec: &[Word], op: Op) -> bool
where Op: Fn(Word, Word) -> Word
{
    let mut changed = 0;
    for (out, in_) in out_vec.iter_mut().zip(in_vec) {
        let old = *out;
        let new = op(old, *in_);
        *out = new;
        changed |= old ^ new;   // accumulate changed bits
    }
    changed != 0
}
```

The return value (whether any bit changed) is computed via `|= old ^ new`
rather than `changed |= (old != new)`. The comment in the source explains why:
the `!=` form forces the compiler to materialize a boolean on each iteration,
preventing vectorization. Accumulating the XOR of all changed words into a
single `u64` allows the auto-vectorizer to process multiple words in parallel
with SIMD instructions and check the result once at the end.

This function powers `BitSet::union`, `intersect`, and `subtract`.

---

## `BitSet<T>` — dense fixed-size bitset

```rust
pub struct BitSet<T> {
    domain_size: usize,
    words: Vec<Word>,
    marker: PhantomData<T>,
}
```

`BitSet<T>` is the primary type. It represents a subset of `{0, …, domain_size-1}`
with one bit per element, packed into `ceil(domain_size / 64)` words. The
`domain_size` is fixed at construction; use `GrowableBitSet` if you need
runtime growth.

### Construction

```rust
BitSet::new_empty(domain_size)   // all zeros
BitSet::new_filled(domain_size)  // all ones, then clear_excess_bits()
```

`new_filled` allocates all-ones words and then calls `clear_excess_bits` to
zero the unused high bits of the last word. Without this step, operations like
`superset` and `count` would read garbage bits past the domain boundary.

### Core operations

| Method | Semantics | Returns |
|--------|-----------|---------|
| `insert(elem)` | sets the bit | whether it changed |
| `remove(elem)` | clears the bit | whether it changed |
| `contains(elem)` | tests the bit | `bool` |
| `union(other)` | `self \|= other` | whether it changed |
| `subtract(other)` | `self &= !other` | whether it changed |
| `intersect(other)` | `self &= other` | whether it changed |
| `insert_all()` | fill then `clear_excess_bits` | — |
| `inverse()` | `!self` then `clear_excess_bits` | — |
| `superset(other)` | `(self & other) == other` | `bool` |
| `is_empty()` | all words zero | `bool` |
| `count()` | popcount sum | `usize` |
| `ensure(min)` | grow if needed, new words = 0 | — |

`union`, `subtract`, and `intersect` are defined against the operation traits
(`UnionIntoBitSet<T>`, `SubtractFromBitSet<T>`) rather than directly on
`&BitSet<T>`. This means `bitset.union(&hybrid)` works equally well, because
`HybridBitSet` and `SparseBitSet` also implement `UnionIntoBitSet`.

### `BitIter<T>` — trailing-zeros iteration

```rust
pub struct BitIter<'a, T> {
    word:   Word,    // current word with visited bits cleared
    offset: usize,   // bit offset of current word
    iter:   slice::Iter<'a, Word>,
    marker: PhantomData<T>,
}
```

The iterator uses `u64::trailing_zeros()` to find the next set bit in the
current word in O(1), then clears it with `word ^= 1 << bit_pos`. When `word`
reaches zero, it advances to the next word.

The initial state uses a degenerate offset trick:

```rust
word:   0,
offset: usize::MAX - (WORD_BITS - 1),
```

On the first `next()` call, `word == 0` so the iterator immediately advances
to the first real word, setting `offset = offset.wrapping_add(WORD_BITS)`.
Because `usize::MAX - 63 + 64 == 0` (wrapping), `offset` becomes 0 correctly
without needing a separate "started" flag.

Elements are yielded in ascending order, which is a natural consequence of
iterating words in order and using `trailing_zeros` within each word.

---

## `GrowableBitSet<T>` — auto-growing dense bitset

```rust
pub struct GrowableBitSet<T> {
    bit_set: BitSet<T>,
}
```

A thin wrapper around `BitSet<T>` that calls `bit_set.ensure(elem.into() + 1)`
before every `insert` and `remove`. `contains` silently returns `false` for
indices beyond the current domain rather than panicking. Used when the final
domain size is not known at construction time.

---

## `SparseBitSet<T>` — small-element sparse set

```rust
pub(super) const SPARSE_MAX: usize = 8;

pub struct SparseBitSet<T> {
    pub(super) elems: ArrayVec<T, SPARSE_MAX>,
}
```

`SparseBitSet` stores up to 8 elements as a sorted `ArrayVec` on the stack —
no heap allocation. Elements are kept in ascending order; `insert` performs
a linear scan to find the insertion point and shifts the remaining elements
right. Because `SPARSE_MAX` is 8, this scan touches at most 7 comparisons.

`SparseBitSet` is deliberately module-private: callers only use it through
`HybridBitSet`. It exists as a named type only so the matrix types can
reference `SPARSE_MAX` and so that `SparseBitSet` can implement the operation
traits for cross-type operations.

---

## `HybridBitSet<T>` — adaptive sparse/dense bitset

```rust
pub enum HybridBitSet<T> {
    Sparse(SparseBitSet<T>),
    Dense(BitSet<T>),
}
```

`HybridBitSet` starts as `Sparse` (the default constructor returns
`Sparse(SparseBitSet::new_empty())`, which is a constant function and requires
no allocation). When the 9th distinct element would be inserted, it converts
to `Dense`:

```rust
HybridBitSet::Sparse(sparse) => {
    // full and element is not already present → promote
    let mut dense = sparse.to_dense(domain_size);
    let changed = dense.insert(elem);
    *self = HybridBitSet::Dense(dense);
    changed
}
```

`Dense` never demotes back to `Sparse`, even if elements are removed. This
one-way transition avoids the complexity of tracking when to shrink and keeps
the remove path trivial.

The `domain_size` is passed at insert time rather than stored in the
`HybridBitSet` itself. This is deliberate: in contexts like `SparseBitMatrix`
where rows share a column count, storing `domain_size` per row would double
the overhead of uninstantiated rows.

### `clone_from` optimization

`HybridBitSet::clone_from` avoids re-allocating the dense `Vec<Word>` when
cloning a dense set into another dense set:

```rust
if let HybridBitSet::Dense(dst) = self {
    match source {
        HybridBitSet::Sparse(src) => { dst.clear(); dst.reverse_union_sparse(src); }
        HybridBitSet::Dense(src)  => dst.clone_from(src),
    }
} else {
    *self = source.clone()
}
```

The `Dense(dst) ← Dense(src)` path calls `Vec::clone_from` which reuses the
heap allocation if capacities are compatible, avoiding a `malloc`/`free` pair
per dataflow iteration.

### `reverse_union_sparse`

When unioning a `Dense` set with a `Sparse` set, the result must be `Dense`.
Rather than re-checking every bit of both sets, `reverse_union_sparse` walks
the sorted sparse elements, groups them by word, and ORs each group into the
corresponding dense word in a single pass. It simultaneously detects whether
any bits existed in the dense set that were not in the sparse set — this is
the "reverse" part — which is needed to report whether the union changed
anything without doing a second pass.

---

## Operation traits

The crate defines four operation traits so that operations can be dispatched
across the set types without monomorphizing every combination by hand:

```rust
trait UnionIntoBitSet<T>        { fn union_into(&self, other: &mut BitSet<T>) -> bool; }
trait SubtractFromBitSet<T>     { fn subtract_from(&self, other: &mut BitSet<T>) -> bool; }
trait UnionIntoHybridBitSet<T>  { fn union_into(&self, other: &mut HybridBitSet<T>, domain_size: usize) -> bool; }
trait SubtractFromHybridBitSet<T> { fn subtract_from_h(&self, other: &mut HybridBitSet<T>) -> bool; }
```

`FullBitSetOperations<T>` is a blanket supertrait that collects all four;
types that implement all of them (currently `BitSet` and `HybridBitSet`)
satisfy it automatically.

The implementation matrix for `union_into`:

| `self` type | `other` type | strategy |
|-------------|-------------|----------|
| `BitSet` | `BitSet` | `bitwise(|)` |
| `SparseBitSet` | `BitSet` | insert each sparse elem |
| `HybridBitSet::Sparse` | `BitSet` | insert each sparse elem |
| `HybridBitSet::Dense` | `BitSet` | `bitwise(|)` |
| `HybridBitSet` | `HybridBitSet::Sparse` | element-by-element, may promote |
| `HybridBitSet::Dense` | `HybridBitSet::Sparse` | `reverse_union_sparse` |
| `BitSet` | `HybridBitSet::Sparse` | clone dense + `reverse_union_sparse` |
| anything | `HybridBitSet::Dense` | `bitwise(|)` |

---

## `BitMatrix<R, C>` — dense 2D bit matrix

```rust
pub struct BitMatrix<R, C> {
    num_rows:    usize,
    num_columns: usize,
    words:       Vec<Word>,
    marker:      PhantomData<(R, C)>,
}
```

`BitMatrix` lays all rows contiguously in a single flat `Vec<Word>`. Row `r`
occupies words `[r * words_per_row, (r+1) * words_per_row)` where
`words_per_row = ceil(num_columns / 64)`.

Key operations:

- **`insert(row, col)`** / **`contains(row, col)`** — single word access via
  `range(row)` + `word_index_and_mask(col)`.
- **`union_rows(read, write)`** — `words[write] |= words[read]` wordwise,
  using `pick2_mut` to obtain two mutable slices from the same backing `Vec`.
- **`union_row_with(with: &BitSet<C>, write: R)`** — OR a standalone bitset
  into one row, used when seeding a dataflow analysis with initial live-out sets.
- **`intersect_rows(r1, r2)`** — returns the `Vec<C>` of columns set in both rows.
- **`iter(row)`** — yields columns set in a row via `BitIter`.

`BitMatrix` is used in the `mir_opt` ADCE pass to represent the post-dominance
frontier relation: `matrix[block]` is the set of blocks on whose
post-dominance frontier `block` lies.

---

## `SparseBitMatrix<R, C>` — per-row hybrid matrix

```rust
pub struct SparseBitMatrix<R, C> {
    num_columns: usize,
    num_rows:    usize,
    rows:        Vec<HybridBitSet<C>>,
    _row_ty:     PhantomData<fn() -> R>,
}
```

Unlike `BitMatrix`, `SparseBitMatrix` does not pre-allocate storage for all
rows. The `rows` vector is grown lazily: `ensure_row(r)` calls
`ensure_contains_elem(r.into(), HybridBitSet::new_empty)` from `stdx::vec`,
filling skipped rows with empty sets. Rows that are never written cost
nothing beyond the `HybridBitSet::new_empty()` constant (a `Sparse` variant
with an empty `ArrayVec` — no heap allocation).

Each row is a `HybridBitSet<C>`, so sparsely populated rows remain as
`ArrayVec` and only dense rows upgrade to a `Vec<Word>`.

Additional operations over `BitMatrix`:

- **`union_rows(read, write)`** — uses `pick2_mut` on `self.rows` to union two
  row `HybridBitSet` values without cloning.
- **`inverse()`** — produces a transposed `SparseBitMatrix<C, R>` by iterating
  all set bits and inserting the (column, row) pair into the result.
- **`row(r)`** → `Option<&HybridBitSet<C>>` — `None` for uninstantiated rows.

### `GrowableSparseBitMatrix<R, C>`

A newtype over `SparseBitMatrix` that additionally grows `num_columns` when an
inserted column index exceeds the current bound, and calls `dense.ensure(…)` on
any dense rows before inserting so they don't panic on out-of-range bits.
Used in the `mir_opt` taint propagation pass where the domain can grow as new
values are discovered.

---

## Worked example: SCCP feasible-edge tracking

The SCCP pass in `mir_opt` maintains a `TiVec<Block, Successors>` of
feasible successor sets, where `Successors` is effectively a small `BitSet`.
For the live-block check it uses `BitSet<Block>`:

```
domain_size = number of basic blocks in the function (e.g. 12)
words = [0u64; 1]   // one word covers 64 blocks
```

When the SCCP solver decides block 5 is reachable, it calls:

```rust
feasible.insert(Block(5));
// word_index = 5/64 = 0, mask = 1 << 5 = 0b100000
// words[0] |= 0b100000  → changed = true
```

The worklist then iterates over `feasible.iter()` using `BitIter`:

```
initial: word=0, offset=usize::MAX-63
next(): word==0 → advance; word=0b100000, offset=0
trailing_zeros(0b100000)=5 → yield Block(5); word ^= 0b100000 → word=0
next(): word==0 → advance; no more words → None
```

Block 5 is the only element yielded, in O(words_count) time regardless of
domain size.

---

## Key design decisions

**`u64` word type throughout.** Using `u64` rather than `usize` or `u32`
fixes the word width to 64 bits on all platforms. This makes the data layout
and `WORD_BITS` constant predictable across 32-bit and 64-bit targets and lets
the compiler use 64-bit SIMD lanes without platform-specific code.

**Separate `sparse.rs` from `hybrid.rs`.** `SparseBitSet` is module-private
(`pub(super)`); it exists to give `HybridBitSet` a named inner type and to
centralize the sorted-`ArrayVec` logic. Callers that want a small-set
representation use `HybridBitSet` directly. This keeps the public API simple
while allowing the implementation to be split across files.

**`SPARSE_MAX = 8`.** Eight is enough to represent a basic block's typical
number of live definitions at any given program point without heap allocation.
For the GVN equivalence classes stored as `HybridBitSet<DFSId>`, most classes
stay sparse; only large equivalence groups (e.g. all definitions of a common
subexpression in a loop) become dense.

**One-way Sparse → Dense transition.** Once a `HybridBitSet` promotes to
`Dense`, it never demotes. The savings from avoiding an unnecessary dense
representation are small compared to the complexity of tracking when
to shrink. The optimization instead focuses on the common case: sets that stay
sparse through their entire lifetime.

**`changed |= old ^ new` over `changed |= old != new`.** This is the critical
micro-optimization that enables auto-vectorization of `union`, `subtract`, and
`intersect`. A boolean check (`!=`) materializes a 1-byte value per iteration;
the bitwise XOR accumulation produces a word-width value that the SIMD loop
can reduce at the end with a single comparison.

**`SparseBitMatrix` stores `num_rows` separately from `rows.len()`.** The
`rows` vector is shorter than `num_rows` when trailing rows have never been
set. `num_rows` is the declared bound; `rows.len()` is the high-water mark of
rows that have been instantiated. This allows the matrix to be declared with
its full logical size without paying for uninstantiated rows.
