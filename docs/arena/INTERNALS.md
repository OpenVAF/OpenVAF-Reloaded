# `arena` — Typed arena allocation

**Location:** `lib/arena/`
**Role:** Provides `Idx<T>`, a typed `u32` handle into a contiguous allocation
arena, plus `IdxRange<T>` for contiguous slices of handles, and type aliases
`Arena<T>` and `ArenaMap<I, T>` that wrap `typed_index_collections::TiVec`.
The entire crate is one file and has no dependencies beyond `typed-index-collections`.

Cross-links: [stdx INTERNALS](../stdx/INTERNALS.md) ·
[hir\_def INTERNALS](../hir_def/INTERNALS.md) ·
[basedb INTERNALS](../basedb/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
arena   (lib/arena/)
  └─► hir_def  (ExprId, StmtId, ItemTreeId, ErasedAstId, …)
  └─► basedb   (AstIdMap)
  └─► mir_autodiff
```

`arena` is a leaf library. It is not Salsa-aware and has no runtime logic
beyond allocation. `typed-index-collections` provides the underlying
`TiVec<I, T>` which enforces that `arena[idx]` only compiles when `idx`
is of the correct `Idx<T>` type.

---

## `Idx<T>` — typed arena handle

```rust
pub type RawIdx = u32;

pub struct Idx<T> {
    raw: RawIdx,
    _ty: PhantomData<fn() -> T>,
}
```

`Idx<T>` is a 4-byte handle that indexes into an `Arena<T>`. The
`PhantomData<fn() -> T>` phantom makes `Idx<Expr>` and `Idx<Stmt>`
distinct types at compile time: you cannot accidentally use an expression
index to index a statement arena.

The phantom uses the function-pointer form `fn() -> T` (rather than `*const T`
or `T` directly) so that `Idx<T>` is:

- **covariant** in `T` (like `fn() -> T`; a `fn() -> &'a T` is a subtype of
  `fn() -> &'static T` is not required here, but the variance is benign)
- **`Send + Sync`** regardless of whether `T` is, because no `T` is actually
  stored inside `Idx<T>`

### Derived traits

All standard traits are implemented manually to avoid requiring `T: Clone`,
`T: Eq`, etc. — since `Idx<T>` does not contain a `T`, it can always be
`Copy`, `Clone`, `Eq`, `Hash`, `Ord`, and `PartialOrd` without any bound on
`T`:

```rust
impl<T> Copy for Idx<T> {}
impl<T> PartialEq for Idx<T> { /* compares raw */ }
impl<T> Hash for Idx<T> { /* hashes raw */ }
impl<T> Ord for Idx<T> { /* orders by raw */ }
```

Ordering is by insertion index, which is also allocation order. This means
sorting a set of `Idx<T>` values puts them in the same order they were
allocated, which corresponds to their syntactic order in the source file for
HIR arenas built during lowering.

### Debug format

```rust
impl<T> fmt::Debug for Idx<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // strips the module path, leaving only the type name
        let type_name = /* last component of std::any::type_name::<T>() */;
        write!(f, "Idx::<{}>({})", type_name, self.raw)
    }
}
```

A debug-printed `Idx<Expr>` looks like `Idx::<Expr>(3)`. The type name is
stripped to its last `:`-separated component so that fully qualified names
like `hir_def::expr::Expr` become just `Expr`.

### Conversions

```rust
impl<T> From<RawIdx> for Idx<T>  // wraps a raw u32
impl<T> From<Idx<T>> for RawIdx  // unwraps
impl<T> From<usize>  for Idx<T>  // debug_assert: usize < u32::MAX
impl<T> From<Idx<T>> for usize
```

The `usize` conversion truncates on 64-bit platforms (debug-asserted). In
practice no arena grows past `u32::MAX` entries — the debug assertion catches
any overflow during development.

---

## `IdxRange<T>` — contiguous range of handles

```rust
pub struct IdxRange<T> {
    range: Range<u32>,
    _p: PhantomData<T>,
}
```

`IdxRange<T>` represents a half-open range `[start, end)` of `Idx<T>` values
in insertion order. It is the standard way to record that a parent node owns
a contiguous sequence of children — for example, a `Module` in `hir_def` stores
its items as an `IdxRange<Item>` into the `ItemTree` arena.

### Constructors

```rust
IdxRange::new(a..b)             // half-open: [a, b)
IdxRange::new_inclusive(a..=b)  // closed: [a, b]  → stored as [a, b+1)
```

The inclusive form simply adds 1 to the end index before storing it, keeping
the internal representation always half-open.

### `cover`

```rust
pub fn cover(&self, other: &Self) -> Self {
    Self { range: self.range.start..other.range.end, _p: PhantomData }
}
```

Merges two ranges into one that spans from the start of `self` to the end of
`other`, including any gap between them. Used when a parent node's span is
determined by the union of its children's ranges.

### Iteration

`IdxRange<T>` implements `Iterator<Item = Idx<T>>` and
`DoubleEndedIterator` by delegating to the underlying `Range<u32>`. This means
you can write:

```rust
for expr_id in body.exprs_range {
    process(&body.exprs[expr_id]);
}
```

without converting to `usize` by hand.

---

## `Arena<T>` and `ArenaMap<I, T>`

```rust
pub type Arena<T>       = TiVec<Idx<T>, T>;
pub type ArenaMap<I, T> = TiVec<Idx<I>, T>;
```

Both are thin type aliases over `TiVec` from the `typed-index-collections`
crate. `TiVec<I, T>` is a `Vec<T>` newtype that only accepts `I` as an index
type — `arena[Idx::<Stmt>(3)]` compiles but `arena[Idx::<Expr>(3)]` does not
even if both are `u32` at runtime.

**`Arena<T>`** is used when the value type and the index type are the same
concept: allocating `Expr` values produces `Idx<Expr>` handles.

**`ArenaMap<I, T>`** is used for secondary tables keyed by the same index type
but storing a different value. For example, `hir_def::Body` uses:

```rust
pub exprs:           Arena<Expr>            // primary: stores Expr nodes
pub expr_map_back:   ArenaMap<Expr, Option<AstPtr<ast::Expr>>>
                                            // secondary: maps each ExprId back to its AST node
```

Both tables are indexed by `Idx<Expr>` (`ExprId`). `Arena<Expr>` holds the
HIR `Expr` values; `ArenaMap<Expr, …>` holds per-expression metadata without
needing a separate hash map.

### Allocation

`TiVec::push` returns the new index:

```rust
let expr_id: Idx<Expr> = arena.push(expr_value);
```

Indices are stable: `TiVec` never re-orders its elements, so an `Idx<T>`
obtained at allocation time remains valid for the lifetime of the arena.

---

## Worked example: HIR body lowering

`hir_def::Body` is the data structure that holds the HIR for a single analog
block. During lowering from AST to HIR, expressions and statements are
allocated into two arenas:

```rust
pub struct Body {
    pub exprs:   Arena<Expr>,   // all Expr nodes
    pub stmts:   Arena<Stmt>,   // all Stmt nodes
    pub expr_map_back: ArenaMap<Expr, Option<AstPtr<ast::Expr>>>,
    pub stmt_map_back: ArenaMap<Stmt, Option<AstPtr<ast::Stmt>>>,
    // …
}
```

When the lowering pass encounters `V(p, n) / R` in an analog block, it
allocates three `Expr` nodes:

1. `Expr::BranchAccess(V, Branch(p,n))` → `ExprId(0)` (`Idx::<Expr>(0)`)
2. `Expr::Var(R)` → `ExprId(1)`
3. `Expr::BinaryOp { op: Div, lhs: ExprId(0), rhs: ExprId(1) }` → `ExprId(2)`

Each allocation is a single `push` onto `body.exprs`. The children of node 3
are stored as `ExprId` values — not pointers — so the entire tree is a flat
`Vec<Expr>` with pointer-free cross-references. The `expr_map_back` arena
is grown in lockstep, so `body.expr_map_back[ExprId(2)]` points back to the
`/` AST node.

The contribution statement `I(p,n) <+ expr` is similarly a `Stmt` containing
the `ExprId` of the right-hand side. Because both arenas grow contiguously and
indices are stable, the `Body` can be serialized, compared, and cached by
Salsa without copying or pointer-chasing.

---

## Key design decisions

**`PhantomData<fn() -> T>` over `PhantomData<T>`.** Using `fn() -> T` instead
of `*const T` or `T` means `Idx<T>` is `Send + Sync` unconditionally — no
marker impl needed, no `unsafe` — because the phantom carries no actual data
and imposes no threading restrictions. It also avoids implying that `Idx<T>`
owns a `T` (which would make `Drop` checking relevant).

**Flat arena, not pointer graph.** Storing `ExprId` values instead of `Box<Expr>`
or `&Expr` inside each node gives:
- Cache-friendly linear access when iterating all expressions
- Trivial serialization (a `Vec<Expr>` is just bytes + length)
- Salsa-compatible equality (`Vec<Expr>` implements `Eq` without unsafe)
- No heap allocation per node (one `Vec` for the whole tree)

The trade-off is that random access by `ExprId` is `O(1)` but the arena
cannot shrink or free individual elements — it is append-only. For HIR, which
is built once per query and then read-only, this is exactly the right trade-off.

**Type aliases over a newtype.** `Arena<T>` and `ArenaMap<I, T>` are type
aliases rather than newtypes. This means all `TiVec` methods (`push`, `iter`,
`len`, `Index`, `IndexMut`, …) are available directly without boilerplate
delegation, while the type-level index check from `TiVec` still prevents
cross-arena indexing errors.

**`IdxRange` stores `Range<u32>`, not `(Idx<T>, Idx<T>)`.** Using the raw
`u32` range avoids storing two phantom-data fields and makes `IdxRange`
trivially `Copy` without an explicit impl. The `Iterator` impl delegates
directly to `Range<u32>::next`, which the compiler optimizes to a single
increment instruction.
