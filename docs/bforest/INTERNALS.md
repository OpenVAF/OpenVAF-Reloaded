# `bforest` — B+-tree forest

**Location:** `lib/bforest/`
**Origin:** A vendored fork of `cranelift_bforest`, modified to add features
needed by OpenVAF (notably `Map::merge` and `Map::insert_sorted`).
**Role:** A family of ordered map and set types that share a single node pool.
The design optimizes for many small trees (one per basic block, one per live
variable, etc.) rather than one large tree, and for keys and values that are
small copyable types (typically `u32` newtypes).

Cross-links: [stdx INTERNALS](../stdx/INTERNALS.md) ·
[mir\_build INTERNALS](../mir_build/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Why not `BTreeMap`?

The crate's own README states this plainly: **these are not faster general-purpose
data structures**. The trade-offs are different:

| Property | `std::BTreeMap` | `bforest::Map` |
|----------|----------------|---------------|
| Empty tree size | 24 bytes (`ptr + len + cap`) | 4 bytes (`PackedOption<Node>`) |
| Clear N trees | O(N × tree_size) | O(1) for the whole forest |
| Key ordering | `Ord` on the key type | external `Comparator` object |
| Keys/values | any `T` | `Copy`, optimized for 32-bit |
| Allocation | per-tree | pooled across the whole forest |

The last row is the main win. When the MIR builder maintains a use-def map for
each of several hundred blocks, clearing the entire set of maps between queries
is O(1): `MapForest::clear()` calls `NodePool::clear()` which is a single
`Vec::clear()`.

---

## Module map

| File | Contents |
|------|----------|
| `src/lib.rs` | Constants, `Comparator` trait, `Forest` trait, `Node`, `SetValue`, helpers |
| `src/node.rs` | `NodeData<F>`, `SplitOff<F>`, `Removed`; all node-level operations |
| `src/pool.rs` | `NodePool<F>` — allocation, free list, tree freeing |
| `src/path.rs` | `Path<F>` — root-to-leaf cursor; find/insert/remove |
| `src/map.rs` | `MapForest<K,V>`, `Map<K,V>`, `MapCursor<K,V>`, `MapIter<K,V>` |
| `src/set.rs` | `SetForest<K>`, `Set<K>`, `SetCursor<K>`, `SetIter<K>`, `RevSetIter<K>` |

---

## Constants and shared types

```rust
const INNER_SIZE: usize = 8;   // branching factor of inner nodes
const MAX_PATH:   usize = 16;  // maximum tree height (never reached)
```

`INNER_SIZE = 8` is chosen so that an inner node occupies exactly one 64-byte
cache line when keys and node references are 32 bits each:

```
Inner node: u8 size  +  [u32; 7] keys  +  [u32; 8] trees  =  1 + 28 + 32 = 61 bytes
```

(Padding rounds to 64 bytes.) A map leaf node likewise fits in 64 bytes:

```
Leaf node (map): u8 size  +  [K; 7]  +  [V; 7]  =  1 + 28 + 28 = 57 bytes
```

A set leaf has no value, so it can store more keys:

```
Leaf node (set): u8 size  +  [K; 15]  +  [(); 15]  =  1 + 60 + 0 = 61 bytes
```

`MAX_PATH = 16` is a worst-case bound. With branching factor 4 (the minimum,
when all inner nodes are half-full), a tree holding 2³² entries would need
log₄(2³²) = 16 levels. In practice, OpenVAF trees have far fewer entries and
at most 3–4 levels.

### `Node(u32)` — node reference

```rust
struct Node(u32);
impl_idx_from!(Node(u32));
```

A 32-bit index into `NodePool::nodes`. `impl_idx_from!` from `stdx` gives
bidirectional conversions with `u32` and `usize`, and implements `ReservedValue`
with `u32::MAX` as the sentinel — enabling `PackedOption<Node>`.

### `Comparator<K>` — context-bearing key comparison

```rust
pub trait Comparator<K: Copy> {
    fn cmp(&self, a: K, b: K) -> Ordering;
    fn search(&self, k: K, s: &[K]) -> Result<usize, usize> {
        s.binary_search_by(|x| self.cmp(*x, k))
    }
}

impl<K: Copy + Ord> Comparator<K> for () { … }  // trivial impl
```

Keys do not need to implement `Ord` themselves. The comparator is an external
object passed to every mutating operation. This allows keys to be small opaque
index types (e.g. `Value(u32)`) that derive their ordering from a separate
table rather than from their numeric value.

### `Forest` trait — associated array types

```rust
trait Forest {
    type Key:         Copy;
    type Value:       Copy;
    type LeafKeys:    Copy + BorrowMut<[Self::Key]>;
    type LeafValues:  Copy + BorrowMut<[Self::Value]>;
    fn splat_key(key: Self::Key)     -> Self::LeafKeys;
    fn splat_value(value: Self::Value) -> Self::LeafValues;
}
```

`Forest` is an internal seam. The two concrete implementations are:

- **`MapTypes<K, V>`**: `LeafKeys = [K; 7]`, `LeafValues = [V; 7]`
- **`SetTypes<K>`**: `LeafKeys = [K; 15]`, `LeafValues = [SetValue; 15]`

`SetValue` is a zero-sized type `struct SetValue()`. Because `[SetValue; 15]`
is zero bytes, the set leaf holds 15 keys in the same space a map leaf uses for
7 key-value pairs.

`splat_key` and `splat_value` initialize a freshly allocated array by
replicating a single value across all slots. This sidesteps the need for a
`Default` bound on `K` or `V` — the first entry is duplicated into every slot
so the array is fully initialized without a sentinel value.

---

## `NodeData<F>` — the B+-tree node

```rust
pub(super) enum NodeData<F: Forest> {
    Inner {
        size: u8,                     // number of keys (sub-trees = size + 1)
        keys: [F::Key; INNER_SIZE - 1],  // [7] discriminating keys
        tree: [Node;   INNER_SIZE],      // [8] child node references
    },
    Leaf {
        size: u8,
        keys: F::LeafKeys,
        vals: F::LeafValues,
    },
    Free { next: Option<Node> },     // free-list link
}
```

`NodeData<F>` is `Copy` — it is stored directly in a `Vec<NodeData<F>>`
without boxing. The manual `Copy` impl (not derived) avoids requiring
`F: Copy`.

### Inner node invariant

In an inner node with `size = s`, there are `s` keys and `s + 1` sub-trees.
Key `keys[i]` separates `tree[i]` from `tree[i+1]`: every key in the sub-tree
rooted at `tree[i]` is strictly less than `keys[i]`, and every key in
`tree[i+1]` is greater than or equal to `keys[i]`.

### Operations

**`split(insert_index)`** — splits a full node in half. The `insert_index`
hint biases the split point so that after the insertion is retried, both
halves are as even as possible:

```rust
fn split_pos(len: usize, ins: usize) -> usize {
    if ins <= len / 2 { len / 2 } else { (len + 1) / 2 }
}
```

Returns a `SplitOff<F>` containing the new right-hand node's data and the
critical key that separates the two halves.

**`balance(crit_key, rhs)`** — after an underflow, attempts to merge with the
right sibling. If the combined entry count fits in one node, everything moves
to the right node and the left node is left empty (returns `None`). Otherwise
entries are redistributed evenly (returns the new critical key for the right
node).

**`Removed`** — the status returned after a removal:

| Variant | Meaning |
|---------|---------|
| `Healthy` | Node still has ≥ half capacity |
| `Rightmost` | Rightmost entry removed; path needs to advance |
| `Underflow` | Below half capacity; must rebalance with sibling |
| `Empty` | No entries left; node must be removed from parent |

---

## `NodePool<F>` — the shared allocator

```rust
pub(super) struct NodePool<F: Forest> {
    nodes:    Vec<NodeData<F>>,
    freelist: Option<Node>,
}
```

`NodePool` is a flat `Vec` with an intrusive free list. Freed nodes are
overwritten with `NodeData::Free { next: freelist }` and the freelist head is
updated. Allocation checks the freelist first; if empty, it pushes a new entry
onto the vector.

```
alloc_node:
  if freelist → pop head, overwrite with new data
  else        → vec.push(data), return len-1 as Node

free_node:
  nodes[node] = Free { next: freelist }
  freelist = Some(node)
```

**`free_tree(node)`** — recursively frees an entire sub-tree. The recursion
depth is bounded by `MAX_PATH = 16`, so stack overflow is not possible.
Freeing an entire tree without touching the parent's free list first would
leave dangling `Node` references; the recursive approach ensures that inner
nodes are freed after their children.

**`clear()`** — calls `Vec::clear()`, which drops all elements in O(N) time
where N is the number of allocated nodes. But because all trees share the pool,
a single `clear()` destroys every tree in the entire forest simultaneously.
This is the key property for clearing all block-local maps between MIR passes.

---

## `Path<F>` — root-to-leaf traversal state

```rust
pub(super) struct Path<F: Forest> {
    size:  usize,
    node:  [Node; MAX_PATH],
    entry: [u8;   MAX_PATH],
    unused: PhantomData<F>,
}
```

`Path<F>` is `Copy` and stack-allocated. It records the path from the root
down to the current leaf: `node[0]` is always the root, `node[size-1]` is the
current leaf node, and `entry[l]` is the child index taken at level `l`.
`size = 0` is the canonical off-the-end position.

### `find(key, root, pool, comp)`

Walks from the root to the leaf:

1. At each inner node, binary-search the key array. If `key` is found at
   position `i`, follow `tree[i+1]` (the `>=` branch). If not found, follow
   `tree[i]` (the `<` branch).
2. At the leaf, binary-search again. If found, record the position and return
   `Some(value)`. If not found, record the insertion position and return `None`.

After `find`, the path points either at the found entry or at the position
where the key would be inserted to maintain sorted order.

### `insert(key, value, pool)`

Attempts `try_leaf_insert` (in-place shift). If the leaf is full, calls
`split_and_insert`:

```
split_and_insert (bottom-up loop):
  for level from leaf to root:
    split current node → lhs (current) + rhs (new)
    determine which half the insert position falls in → update path
    insert into the not-full half
    if parent had room → insert rhs into parent; done
  // reached level 0 without finding room → allocate a new root
  new_root = Inner(orig_root, crit_key, rhs_node)
  path.size += 1; prepend root to path arrays
```

The height of the tree grows by 1 only when the root itself is split.

### `remove(pool)`

Removes the entry at the current position:

1. Call `leaf_remove` → `Removed` status.
2. `Healthy`: done (update critical key if we removed the front entry).
3. `Rightmost`: advance path to next node.
4. `Underflow`: call `balance` with the right sibling.
5. `Empty`: recursively remove the now-empty node from its parent.

After all rebalancing, prune the root if it has shrunk to a single sub-tree
(the single child becomes the new root).

---

## `Map<K, V>` and `MapForest<K, V>`

```rust
pub struct Map<K, V> {
    root: PackedOption<Node>,   // 4 bytes; None → empty
    unused: PhantomData<(K, V)>,
}

pub struct MapForest<K, V> {
    nodes: NodePool<MapTypes<K, V>>,
}
```

`Map<K,V>` is 4 bytes. An empty map is `root = PackedOption::None` (the
`u32::MAX` sentinel), which costs nothing beyond those 4 bytes. No heap
allocation happens until the first `insert`.

All map operations take a `&MapForest` (or `&mut MapForest`) and a
`&dyn Comparator<K>`:

```rust
map.get(key, &forest, &comp)          // → Option<V>
map.get_or_less(key, &forest, &comp)  // → Option<(K, V)>  (closest ≤ key)
map.insert(key, value, &mut forest, &comp)  // → Option<V> (old value)
map.remove(key, &mut forest, &comp)         // → bool
map.iter(&forest, &comp)              // → MapIter (ascending)
map.cursor(&mut forest, &comp)        // → MapCursor (positioned)
```

### `merge` and `insert_sorted`

These are the OpenVAF additions over the upstream cranelift version.

**`map.merge(other, &mut forest, &comp, f)`** — absorbs `other` into `self`
in a single sorted pass. `f(existing, incoming) -> V` resolves conflicts.
Implemented via `insert_sorted` with `other`'s iterator as the source.

**`map.insert_sorted(next_src, &mut forest, &comp, f)`** — takes a closure
that yields `(K, T)` pairs in ascending key order (no duplicates) and merges
them into the map. The cursor advances through the destination map in lockstep,
avoiding redundant `find` calls. This is O(N + M) rather than O(M log N) for
a bulk insert of M entries into a map of size N.

---

## `Set<K>` and `SetForest<K>`

```rust
pub struct Set<K> {
    root: PackedOption<Node>,
    unused: PhantomData<K>,
}
```

`Set<K>` is identical in structure to `Map<K, ()>` but uses `SetTypes<K>`
which gives leaves 15 keys instead of 7. The public API mirrors `Map` minus
the value parameter:

```rust
set.contains(key, &forest, &comp)
set.insert(key, &mut forest, &comp)   // → bool (was absent)
set.remove(key, &mut forest, &comp)   // → bool (was present)
set.clear(&mut forest)                // frees the tree
set.retain(&mut forest, |k| bool)     // filter in-place
set.iter(&forest, &comp)              // → SetIter (ascending)
set.cursor(&mut forest, &comp)        // → SetCursor
```

`RevSetIter<K>` provides reverse (descending) iteration by calling `prev` on
the path repeatedly.

---

## Worked example: block-local use-def map in `mir_build`

The SSA builder in `mir_build` tracks, for each basic block and each `Place`
variable, the `Value` that was last written. This is a map from `Place → Value`.
Because a block can define only a few variables before being sealed, the typical
tree has 1–5 entries and never overflows a single leaf node.

Using `bforest`:

```rust
let mut forest: MapForest<Place, Value> = MapForest::new();
let mut block_map: Map<Place, Value> = Map::new();
// costs 4 bytes; no allocation

// Record a write to Place(3) in this block
block_map.insert(Place(3), Value(7), &mut forest, &());
// first insert: allocates one leaf node in forest.nodes

// Look up the last writer of Place(3)
let val = block_map.get(Place(3), &forest, &());
// → Some(Value(7)), path created on stack, no allocation

// At end of pass, clear all block maps in O(1)
forest.clear();
// all 200 block maps are cleared by a single Vec::clear()
```

If the maps were `BTreeMap<Place, Value>` instead, clearing 200 maps would
touch every allocator-level node individually. With `MapForest`, the entire
pool is discarded in a single operation.

---

## Key design decisions

**`Map<K,V>` is 4 bytes.** Storing only a `PackedOption<Node>` (one `u32`)
means that an array of 1000 empty maps costs 4 KB — the same as a single
`Vec<_>` header. The forest owns the memory; the maps are just handles.

**Free list via `NodeData::Free`.** Rather than maintaining a separate free
list allocation, freed nodes are overwritten with the `Free` variant and linked
through the same `Vec`. This keeps the pool compact and avoids a second
allocation per pool.

**`splat_key`/`splat_value` avoid `Default`.** Filling a newly allocated leaf
array by replicating the first key avoids requiring `K: Default`. This matters
for index types that intentionally have no "zero" value, or whose `Default`
might be semantically inappropriate (e.g. `u32::MAX` is the reserved value for
`PackedOption`).

**`Path<F>` is `Copy` and stack-allocated.** Every tree operation creates a
`Path::default()` on the stack, uses it for the operation, and then drops it.
No heap allocation is needed for traversal state. The fixed-size `[Node; 16]`
and `[u8; 16]` arrays are sized for the theoretical maximum depth; in practice
the compiler will optimize away the unused tail.

**Set leaves hold 15 keys, not 7.** Since `SetValue` is zero bytes, a set
leaf wastes half its space if it uses the same 7-entry layout as a map leaf.
The `SetTypes` implementation doubles the key count, halving the number of
leaf nodes and inner nodes needed for any given set size.

**`INNER_SIZE = 8` targets one cache line.** The branch factor and array sizes
are chosen so that each node — inner or leaf — fits in exactly 64 bytes for
32-bit keys and values. Searching within a node is a cache-friendly linear or
binary scan of at most 7–15 elements, all in a single cache line.
