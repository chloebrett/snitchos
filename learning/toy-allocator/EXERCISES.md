# toy-allocator — exercises

Three allocation strategies. Free-list + bitmap model the real kernel; buddy
models what *Linux* does instead. Implement the `todo!()`s so the tests pass.

```bash
cd learning
cargo test -p toy-allocator        # all of it
cargo test -p toy-allocator freelist::   # just the free-list tests
cargo test -p toy-allocator bitmap::     # just the bitmap tests
cargo test -p toy-allocator buddy::      # just the buddy tests
```

Each test failing is a `todo!()` panic until you implement it. Implement,
re-run, watch them go green one area at a time.

---

## Exercise 1 — first-fit alloc + split (`src/freelist.rs`)

**Concept.** A heap hands out *variable-size* blocks. It keeps a list of free
spans; to allocate, it walks the list for the first span big enough and carves
the request off the front. If the span is bigger than the request, the leftover
stays free (a *split*).

**Why first-fit?** It's O(n) and simple. The real `linked_list_allocator` is
first-fit too. Alternatives (best-fit, buddy) trade complexity for less
fragmentation — a great rabbit hole once this works.

**Maps to:** `Heap::allocate_first_fit` in `vendor/linked_list_allocator`,
driven by `kernel/src/heap.rs`’s `#[global_allocator]`.

**Done when:** `freelist::tests` up to `alloc_zero_is_none` pass.

---

## Exercise 2 — free + coalesce (`src/freelist.rs`)

**Concept.** Freeing without merging is a trap: alloc/free the same size in a
loop and the free list shatters into ever-smaller unmergeable shards even
though total free bytes are huge. *Coalescing* — merging a freed span with any
free neighbour it now touches — is what keeps `largest_free_block` healthy.

The subtle case: a freed span that fills a hole *exactly* touches a free
neighbour on **both** sides. Three blocks must collapse into one.

**Maps to:** `Heap::deallocate` coalescing. The kernel’s
`snitchos.heap.free_blocks` / `largest_free_block_bytes` Grafana gauges are
literally measuring whether this works at runtime.

**Done when:** `freeing_a_hole_coalesces_both_neighbours` and
`non_adjacent_frees_stay_fragmented` pass.

---

## Exercise 3 — lowest-free-frame via `trailing_zeros` (`src/bitmap.rs`)

**Concept.** Physical RAM is handed out in *fixed-size* 4 KiB frames, so you
don’t need variable spans — one bit per frame is enough. With the
"1 = free" convention, finding a free frame is "find the lowest set bit",
which is a single `trailing_zeros` per 64-bit word. No fragmentation possible
(every frame is interchangeable), but you can’t do variable sizes.

**The performance gotcha (already baked into the scaffold):** keep a running
`free` counter so the empty-pool check is O(1). The real kernel learned this
the hard way — popcount-scanning every word on each failed alloc stalled
heartbeats during the OOM stress test. See the note in
`kernel-mem/src/frame.rs` and CLAUDE.md’s frame-allocator gotcha.

**Maps to:** `Bitmap::alloc` in `kernel-mem/src/frame.rs` — your version
should end up nearly identical.

**Done when:** all `bitmap::tests` pass.

---

## Exercise A — buddy alloc + split (`src/buddy.rs`)

**Concept.** Blocks come in power-of-two sizes ("orders"), one free list per
order. To allocate order `k`, find the smallest free order `j >= k`; if `j > k`,
**split** repeatedly — each split of an order-`o` block makes two order-`(o-1)`
buddies; keep one, free the other — until you reach `k`. O(log n), and sizes
always being powers of two is what makes the buddy trick possible.

**Maps to:** Linux's `__rmqueue_smallest` + `expand`.

**Done when:** the `alloc_*` and `two_order0_allocs_are_buddies` tests pass.

---

## Exercise B — buddy free + XOR coalesce (`src/buddy.rs`) ★

**Concept.** The gem. A block's buddy is `offset ^ (1 << order)` — they differ
in exactly one bit because order-`k` blocks are aligned to `2^k`. On free, check
if the buddy is *also* free at this order: if so, remove it, merge into an
order-`(k+1)` block (start = `min(offset, buddy)`), and repeat upward. One XOR +
one lookup per level — **O(1) coalescing, no neighbour scan** (contrast the
free-list, which walks to find neighbours). That's why Linux uses buddy for
physical pages, and why it can hand back large contiguous runs that a bitmap
can't cheaply reconstruct.

The invariant to preserve (and what the proptest checks): **no two free buddies
ever both sit in the same free list** — they must have merged. That's the buddy
analogue of the free-list's "no two adjacent free blocks."

**Maps to:** Linux's `__free_one_page` coalescing loop.

**Done when:** `free_coalesces_buddies_back_to_the_full_block`,
`free_with_allocated_buddy_does_not_coalesce`, and the proptest
`random_alloc_free_preserves_buddy_invariants` pass.

---

## Stretch goals (no tests — for understanding)

1. **Alignment.** Real allocators must return aligned addresses. Add
   `alloc_aligned(&mut self, size, align)` to `Arena`: round the chosen block’s
   start up to a multiple of `align`, which may leave a small free sliver
   *before* the allocation. This is why heaps waste a few bytes per alloc.
2. **Best-fit vs first-fit.** Make `alloc` pick the *smallest* sufficient
   block instead of the first. Write a workload that fragments first-fit badly
   and watch `fragments()` differ.
3. **Embedded free list.** The real heap stores each free block’s
   `{size, next}` *inside the free memory itself* — zero side bookkeeping.
   Sketch how `FreeBlock` would become a header written into a `Vec<u8>`
   backing store, with `next` as an offset. This is the leap from "toy" to
   `linked_list_allocator`.
