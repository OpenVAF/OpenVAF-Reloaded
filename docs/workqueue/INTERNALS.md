# `workqueue` — De-duplicating worklist structures

**Location:** `lib/workqueue/`
**Role:** Provides two de-duplicating worklist types — `WorkQueue<T>` (FIFO)
and `WorkStack<T>` (LIFO) — for iterative dataflow algorithms over dense
integer indices. Both pair a `VecDeque`/`Vec` for ordering with a `BitSet<T>`
for O(1) membership, so inserting an element that is already in the queue is
a no-op. `WorkStack<T>` is defined in the source but not used in the current
codebase; all production call sites use `WorkQueue<T>`.

Cross-links: [bitset INTERNALS](../bitset/INTERNALS.md) ·
[mir INTERNALS](../mir/INTERNALS.md) ·
[ARCHITECTURE](../../ARCHITECTURE.md)

---

## Crate relationships

```
workqueue   (lib/workqueue/)
  ├─► mir_opt      (dead_code_elimination)
  └─► mir_autodiff (live_derivatives fixpoint)
```

The only dependency is `bitset`. There is no dependency on `stdx`, `arena`,
or the MIR — `workqueue` is a pure data-structure crate and knows nothing
about compiler-specific types.

---

## Type bound

Both types share the same trait bound on `T`:

```rust
T: From<usize> + Into<usize> + Copy + PartialEq + Debug
```

- `From<usize> + Into<usize>` — needed to construct elements from a range
  (`0..size`) and to index into the `BitSet`.
- `Copy` — elements are stored by value in both the deque/vec and (implicitly)
  the bitset.
- `PartialEq + Debug` — for standard utilities; `PartialEq` is not used inside
  the worklist logic itself.

Any `T` produced by `impl_idx_from!` (from `stdx`) satisfies these bounds.
In practice `T` is always `Inst` (a `u32` newtype).

---

## `WorkQueue<T>` — FIFO de-duplicating queue

```rust
pub struct WorkQueue<T: …> {
    pub deque: VecDeque<T>,
    pub set:   BitSet<T>,
}
```

### Construction

| Constructor | What it creates |
|-------------|----------------|
| `with_all(size)` | All elements `0..size` pre-inserted (deque filled, bitset fully set) |
| `with_none(size)` | Empty queue; deque pre-allocated to `size`, bitset empty |

### Key methods

| Method | Behaviour |
|--------|-----------|
| `insert(element) -> bool` | Calls `set.insert`. If the bit was not already set, pushes to the **back** of the deque and returns `true`. Otherwise no-op and returns `false`. |
| `pop() -> Option<T>` | Pops from the **front** of the deque, clears the bit, returns the element. FIFO order. The element can be re-inserted after a `pop`. |
| `take() -> Option<T>` | Pops from the **front** without clearing the bit. The element **cannot** be re-inserted; any future `insert` call will find the bit still set and silently discard it. |
| `is_empty() -> bool` | Checks `deque.is_empty()` (not the bitset). |
| `clear()` | Clears both deque and bitset. |
| `extend(iter)` | Filters the iterator through `set.insert`, then extends the deque with the accepted elements. One pass; does not double-insert. |

### `pop` vs `take`

The distinction is subtle but intentional:

- **`pop`** clears the bit after dequeuing. The element is marked "not in queue"
  and can be re-inserted by a later `insert` call. This is the standard fixpoint
  loop: process an element, potentially re-enqueue it (or its dependents) when
  their state changes.

- **`take`** leaves the bit set after dequeuing. The element will never pass the
  `set.insert` guard again, so it is processed exactly once. This is the
  "visit each element once" pattern — a topological traversal rather than a
  fixpoint.

`dead_code.rs` uses `take` for its initial reverse-order sweep (visiting every
instruction once), then switches to `insert`/`take` for propagation (each
dead instruction adds its operand-defining instructions back to the queue, but
since the bit is never cleared, each instruction that gets re-added is
processed once more and then permanently excluded).

### `From<BitSet<T>>`

```rust
impl<I: …> From<BitSet<I>> for WorkQueue<I> {
    fn from(set: BitSet<I>) -> Self {
        Self { deque: set.iter().collect(), set }
    }
}
```

Converts a pre-populated `BitSet` into a work queue. The deque is filled in
bit-iteration order (ascending index). Used in `mir_autodiff` to seed the
initial worklist from a post-order traversal result.

---

## `WorkStack<T>` — LIFO de-duplicating stack

`WorkStack<T>` is structurally identical to `WorkQueue<T>` except that the
`VecDeque` is replaced by a plain `Vec`:

- `insert` pushes to the **back** of the vec.
- `pop` pops from the **back** (LIFO).
- `take` pops from the **back** without clearing the bit.

Everything else — the bitset membership check, `with_all`/`with_none`,
`extend`, `From<BitSet>` — is identical. The LIFO order means the most
recently inserted element is processed first, which matches depth-first
traversal patterns.

`WorkStack` is not currently used anywhere in the OpenVAF codebase (no files
import it); it exists as an alternative for DFS-based worklist algorithms.

---

## Memory layout

For a function with `N` instructions:

```
WorkQueue<Inst>:
  deque: VecDeque<u32>  — heap allocation, up to N elements
  set:   BitSet<Inst>   — ceil(N/64) × 8 bytes of heap
```

Both structures are pre-allocated at construction time via `with_none(N)`, so
there are no incremental reallocations during the fixpoint loop — the deque
starts with capacity `N` and the bitset is sized to `N` bits.

---

## Worked example: dead code elimination

`mir_opt::dead_code_elimination` (`mir_opt/src/dead_code.rs`) removes MIR
instructions whose results are not used and are not in the `output_values` set.

**Setup.** The queue is constructed with the bitset fully set (`new_filled`),
meaning every instruction starts as a candidate:

```rust
let mut work_list = WorkQueue {
    deque: VecDeque::new(),          // empty — seeded by the initial sweep
    set:   BitSet::new_filled(N),    // all N instructions marked
};
```

**Initial sweep.** The pass walks basic blocks in reverse (from the last block
to the first, and within each block from the last instruction to the first).
For each instruction it calls `process(work_list, inst, …)`:

```rust
fn process(workque: &mut WorkQueue<Inst>, inst: Inst, func: &mut Function, …) {
    if func.dfg.inst_dead(inst, true)
        && !func.dfg.inst_results(inst).iter().any(|r| output_values.contains(*r))
    {
        func.dfg.zap_inst(inst);
        func.layout.remove_inst(inst);
        // operand-defining instructions might now be dead
        for arg in func.dfg.instr_args(inst) {
            if let ValueDef::Result(def_inst, _) = func.dfg.value_def(*arg) {
                workque.insert(def_inst);
            }
        }
    } else {
        // still live — permanently exclude from future processing
        workque.set.remove(inst);
    }
}
```

When an instruction is found dead, it is removed and its operand-defining
instructions are enqueued (`insert` adds them to the deque if not already
present). When an instruction is found live, its bit is manually cleared so
it will never be processed again.

**Fixpoint.** After the initial sweep, the main loop drains the queue with
`take` (not `pop`) — since bits are never cleared by `take`, each instruction
is visited at most once in this phase, but can be re-inserted once if a later
elimination makes it newly dead. The loop terminates when the deque is empty.

This two-phase structure (reverse sweep seeding the queue, then `take`-based
propagation) avoids revisiting instructions that are proven live while allowing
cascading elimination of newly dead operand producers.

---

## Key design decisions

**`BitSet` as the membership oracle, not `HashSet`.** For dense integer
indices like `Inst`, a `BitSet` is both faster and smaller than a hash set.
`set.insert` is a bit-test-and-set (two memory accesses into the bitset's
backing `Vec<u64>`). `HashSet::insert` requires hashing and a hash table
probe. For a function with 1000 instructions the bitset is 16 words (128
bytes); a `HashSet` would be kilobytes.

**Public fields `deque` and `set`.** Both fields are `pub`. This is deliberate:
`dead_code.rs` constructs the `WorkQueue` directly using struct literal syntax
(`WorkQueue { deque: VecDeque::new(), set: BitSet::new_filled(N) }`) rather
than a constructor, because it wants a fully-set bitset but an empty deque —
a combination `with_all` would not provide. The public fields also allow the
`set.remove(inst)` call in the live-instruction branch without going through
the queue API.

**`pop` preserves re-insertion; `take` does not.** The two dequeue methods
support the two dominant worklist patterns in one type without needing two
separate abstractions. A fixpoint algorithm re-inserts work as state changes
(`pop` is correct); a single-pass traversal should visit each element exactly
once (`take` is correct). Providing both in the same type avoids the need for
a wrapper or a flag.

**`WorkStack` mirrors `WorkQueue` exactly.** The only difference is `Vec`
vs `VecDeque` and push/pop direction. Both share the same invariants and API
surface, so switching between BFS and DFS traversal order is a one-word change
at the call site.
