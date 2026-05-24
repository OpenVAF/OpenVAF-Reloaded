# `list_pool` — Pooled variable-length lists

**Location:** `lib/list_pool/`
**Origin:** Vendored from `cranelift_entity`, adapted for OpenVAF.
**Role:** Provides `ListHandle<T>` — a 4-byte handle to a variable-length list
of `T` values — backed by a shared `ListPool<T>`. The design targets the same
niche as `bforest`: many small lists that share a pool and can be cleared as a
group in O(1).

Cross-links: [bforest INTERNALS](../bforest/INTERNALS.md) ·
[stdx INTERNALS](../stdx/INTERNALS.md) ·
[mir INTERNALS](../mir/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Where it is used in OpenVAF

The MIR defines two type aliases in `mir/src/instructions.rs`:

```rust
pub type ValueList    = list_pool::ListHandle<Value>;
pub type ValueListPool = list_pool::ListPool<Value>;

pub type UseList    = list_pool::ListHandle<Use>;
pub type UseListPool = list_pool::ListPool<Use>;
```

`ValueList` is embedded directly inside `InstructionData` variants that take a
variable number of operands (e.g. calls, phi nodes). Each `InstructionData` is
stored in a flat `TiVec<Inst, InstructionData>` inside the `DataFlowGraph`.
Because `ListHandle<Value>` is 4 bytes, storing a variable operand list adds
the same cost as storing a single fixed operand — no pointer indirection, no
`Vec` header.

`UseList` tracks the use-def chain for each `Value`: every time a value is
referenced as an operand, a `Use` record is added to the value's use list.
Both pools live on the `DataFlowGraph`:

```rust
pub struct DataFlowGraph {
    pub value_lists: ValueListPool,
    pub use_lists:   UseListPool,
    // …
}
```

---

## Memory layout

The pool is a single `Vec<T>`:

```
data: [ … | len | e0 | e1 | … | eN-1 | (pad) | … ]
              ↑    ↑
              |    └─ ListHandle.index points here (element 0)
              └─ length field is one slot before the elements
```

Each allocated block occupies a power-of-two number of slots:
- 1 slot for the length field
- up to `block_size - 1` slots for elements

The length field stores the current count; unused trailing slots hold
`T::reserved_value()`.

### Size classes

```rust
fn sclass_size(sclass: SizeClass) -> usize { 4 << sclass }
// sclass 0 → 4 slots  (1 length + up to 3 elements)
// sclass 1 → 8 slots  (1 length + up to 7 elements)
// sclass 2 → 16 slots (1 length + up to 15 elements)
// …
```

`sclass_for_length(len)` computes the smallest size class that holds `len`
elements plus the length slot. The implementation uses a leading-zeros trick:

```rust
fn sclass_for_length(len: usize) -> SizeClass {
    30 - (len as u32 | 3).leading_zeros() as SizeClass
}
```

`| 3` ensures that lengths 0–3 all map to size class 0 (block of 4). For
lengths 4–7 the result is size class 1, and so on, doubling with each class.

---

## `ListHandle<T>`

```rust
pub struct ListHandle<T: ReservedValue> {
    index: u32,        // offset into pool.data of the first element; 0 = empty
    unused: PhantomData<T>,
}
```

`index == 0` is the sentinel for the empty list. It is safe because the pool
never allocates at offset 0: the first slot of any block is always the length
field, and `index` always points one past the length field (to element 0 of
the list). Therefore no valid list can have `index == 0`.

`ListHandle<T>` is 4 bytes. `Default` returns the empty list (index = 0)
without touching the pool.

### Key operations

| Method | What it does |
|--------|-------------|
| `is_empty()` | check `index == 0` — no pool access |
| `len(&pool)` | read `pool.data[index - 1]` (the length slot) |
| `as_slice(&pool)` | `&pool.data[index .. index + len]` — a direct slice into the pool |
| `as_mut_slice(&mut pool)` | same, mutable |
| `get(i, &pool)` | `as_slice(pool).get(i).cloned()` |
| `first(&pool)` | `pool.data[index]` — one array access |
| `push(elem, &mut pool)` | append; reallocate to next size class if `new_len` is a power of two ≥ 4 |
| `extend(iter, &mut pool)` | bulk append; uses `grow` for exact-size iterators |
| `insert(i, elem, &mut pool)` | `push` then shift tail right |
| `remove(i, &mut pool)` | shift tail left, then `remove_last` |
| `swap_remove(i, &mut pool)` | swap with last, then `remove_last` |
| `truncate(new_len, &mut pool)` | may reallocate to a smaller class |
| `clear(&mut pool)` | free block to pool's free list; set `index = 0` |
| `take()` | `mem::take(self)` — leaves an empty handle, returns the old one |
| `deep_clone(&mut pool)` | allocate a fresh block and copy contents — does not alias |
| `to_pool(src, dst)` | copy list from one pool into another |

### Cloning without deep-cloning

`Clone` is derived for `ListHandle<T>`. The clone has the same `index` as the
original — it is an alias. The comment in the source is explicit: *"Cloning an
entity list does not allocate new memory for the clone. It creates an alias of
the same memory."* Mutating one clone through `as_mut_slice` silently mutates
the other. This is intentional: in the MIR, `InstructionData` is cloned when
duplicating instructions, but the operand list is treated as copy-on-write by
the surrounding pass logic.

Use `deep_clone` when an independent copy is needed.

---

## `ListPool<T>`

```rust
pub struct ListPool<T: ReservedValue> {
    data: Vec<T>,
    free: Vec<usize>,  // free-list heads, one per size class
}
```

### Allocation

`alloc(sclass)` checks `free[sclass]` first. The free list for each size class
is an intrusive singly-linked list embedded in `data`. A free block looks like:

```
data[block]     = T::from(0)        // length = 0 signals "free"
data[block + 1] = T::from(next)     // next free block + 1, or 0 for end-of-list
free[sclass]    = block + 1         // head points at the "next" field
```

The `+ 1` offset means the free-list head is always the index of the `next`
field, not the block start. `0` terminates the list (a value of 0 at the
`next` field means no further free blocks). On allocation, the head is
replaced by the value it points to:

```rust
self.free[sclass] = self.data[head].into();  // pop head
```

If the free list is empty, `data` is extended by `sclass_size(sclass)` slots
filled with `T::reserved_value()`.

### Reallocation

`realloc(block, from_sclass, to_sclass, elems_to_copy)` allocates a new block,
copies the first `elems_to_copy` elements (including the length slot at
position 0), and frees the old block. `mut_slices(block0, block1)` splits
`data` to get two non-overlapping mutable slices, allowing a direct
`copy_from_slice` without an intermediate buffer.

### Growth trigger

A list grows to the next size class exactly when the new length would be a
power of two ≥ 4, i.e. when `is_sclass_min_length(new_len)` is true:

```rust
fn is_sclass_min_length(len: usize) -> bool {
    len > 3 && len.is_power_of_two()
}
```

At that point, the block is too small for even one more element. Conversely,
when removing the last element that would bring the length down to such a
boundary, the block shrinks to the next smaller size class.

### Clearing

```rust
pub fn clear(&mut self) {
    self.data.clear();
    self.free.clear();
}
```

A single `Vec::clear()` invalidates every list in the pool simultaneously.
The pool keeps its heap allocation for reuse (the capacity remains). This is
the same O(1) global clear pattern as `bforest::MapForest::clear()`.

---

## Worked example: MIR phi node operands

A phi node in the MIR collects one incoming value per predecessor block.
For a block with three predecessors the phi's value list has three entries.
The `DataFlowGraph` stores it as:

```
dfg.value_lists.data:
  index:  0    1    2    3    4    5    6    7
  data: [ …  | 3  | V5 | V2 | V9 | •  | …  | … ]
                ↑    ↑
                |    └─ ListHandle.index = 2
                └─ length field (len = 3)
```

`phi.args.as_slice(&dfg.value_lists)` returns `&data[2..5]` = `[V5, V2, V9]`
directly — no allocation, no indirection beyond the one array index computation.

When the CFG is simplified and a predecessor is eliminated, the pass calls
`phi.args.remove(1, &mut dfg.value_lists)`, which shifts `V9` left and calls
`remove_last(3, pool)`. Since `len - 1 = 2` does not trigger a size-class
shrink (`is_sclass_min_length(3)` is false), the length field is simply
decremented:

```
after remove(1):
  data: [ …  | 2  | V5 | V9 | •  | •  | … ]
```

The block stays in size class 0 (4 slots). No reallocation.

---

## Key design decisions

**`index == 0` as the empty sentinel.** The pool never occupies slot 0 (the
first slot of any allocated block is the length field, and `index` points one
past it). This gives the empty list a natural representation with no pool
access required for `is_empty()`.

**Power-of-two size classes starting at 4.** The minimum block size of 4
(1 length + 3 elements) means that even a one-element list wastes only 2
slots, and the doubling strategy keeps the average waste below 50% while
bounding the number of reallocations over a sequence of pushes to O(log N).

**Cloning aliases the pool storage.** Since the caller controls the pool
lifetime, sharing storage between clones is safe as long as the pool is not
cleared while both handles are live. The design explicitly accepts this
trade-off to keep `ListHandle<T>` `Copy`-compatible and 4 bytes wide.

**`T: ReservedValue + Copy + Into<usize> + From<usize>`.** The `Into<usize>`
and `From<usize>` bounds allow the length field and free-list pointers to be
stored as `T` values in the same `Vec<T>`, eliminating the need for a separate
metadata array. The `ReservedValue` bound provides the fill value for
uninitialized slots (`T::reserved_value()`). For `Value(u32)` and `Use(u32)`,
all four bounds are satisfied by the `impl_idx_from!` macro from `stdx`.

**`T::from(0)` is used for the free-list terminator.** Index 0 is both the
"empty list" sentinel in `ListHandle` and the "end of free list" value in the
pool's `free` vector. Because the pool's `data[0]` is never a valid `next`
pointer (the pool begins allocations from `data.len()` which is at least 4),
this dual use of 0 is safe.
