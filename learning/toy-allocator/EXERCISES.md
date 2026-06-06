# toy-allocator — exercises

Two allocation strategies, both used by the real kernel. Implement the
`todo!()`s so the tests pass.

```bash
cd learning
cargo test -p toy-allocator        # all of it
cargo test -p toy-allocator freelist::   # just the free-list tests
cargo test -p toy-allocator bitmap::     # just the bitmap tests
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
`kernel-core/src/frame.rs` and CLAUDE.md’s frame-allocator gotcha.

**Maps to:** `Bitmap::alloc` in `kernel-core/src/frame.rs` — your version
should end up nearly identical.

**Done when:** all `bitmap::tests` pass.

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
