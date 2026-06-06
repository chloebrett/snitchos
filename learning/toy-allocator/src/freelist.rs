//! A free-list allocator over a logical byte arena `[0, size)`.
//!
//! We model the arena as a sorted list of *free* spans. There is no
//! real memory here — `alloc` just hands back the **offset** of a span
//! it carved out. That keeps the focus on the algorithm:
//!
//!   * **alloc** = find a free span big enough, carve `size` off its
//!     front, return that offset. (first-fit)
//!   * **free**  = put a span back, merging it with any neighbour it
//!     now touches so the list doesn't fragment forever (coalescing).
//!
//! This is exactly what `vendor/linked_list_allocator` does, except the
//! real one threads the free list *through the freed memory itself*
//! (each free block stores its own size + next-pointer in its first
//! bytes). The algorithm is identical; only the bookkeeping storage
//! differs. We use a `Vec` so you can `println!` the whole free list.

/// A contiguous run of free bytes: `[start, start + size)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeBlock {
    pub start: usize,
    pub size: usize,
}

/// Free-list allocator.
///
/// Invariant the exercises must preserve: `free` is sorted by `start`,
/// the blocks never overlap, and no two blocks are *adjacent* (an
/// adjacent pair must always be merged into one). If you keep that
/// invariant, `fragments()` is a meaningful fragmentation metric.
pub struct Arena {
    pub size: usize,
    free: Vec<FreeBlock>,
}

impl Arena {
    /// A fresh arena is one big free block covering everything.
    pub fn new(size: usize) -> Self {
        let free = if size == 0 {
            Vec::new()
        } else {
            vec![FreeBlock { start: 0, size }]
        };
        Self { size, free }
    }

    /// Read-only view of the free list — handy for `dbg!` while you work.
    pub fn free_list(&self) -> &[FreeBlock] {
        &self.free
    }

    /// Total free bytes (sum of all free spans).
    pub fn free_bytes(&self) -> usize {
        self.free.iter().map(|b| b.size).sum()
    }

    /// Number of distinct free spans. With coalescing correct, this is
    /// your fragmentation count: 1 = fully coalesced, higher = fragmented.
    pub fn fragments(&self) -> usize {
        self.free.len()
    }

    /// Size of the biggest single allocation that could still succeed.
    /// Mirrors `snitchos.heap.largest_free_block_bytes`.
    pub fn largest_free_block(&self) -> usize {
        self.free.iter().map(|b| b.size).max().unwrap_or(0)
    }

    // -------------------------------------------------------------------
    // EXERCISE 1 — first-fit allocation with splitting.
    //
    // Find the *lowest-offset* free block whose size >= `size`. Carve
    // `size` bytes off its front and return the offset you carved from.
    // If carving empties the block, remove it from the list; otherwise
    // shrink it (advance its `start`, reduce its `size`).
    //
    // Return None if `size` is 0 or no block is big enough.
    //
    // Real-world twin: `Heap::allocate_first_fit` in
    // vendor/linked_list_allocator, and `Bitmap::alloc` in
    // kernel-core/src/frame.rs (which is the fixed-size cousin).
    // -------------------------------------------------------------------
    pub fn alloc(&mut self, size: usize) -> Option<usize> {
        if size == 0 {
            return None;
        }
        let (index, block) = self
            .free
            .iter_mut()
            .enumerate()
            .find(|(_, it)| it.size >= size)?;
        let taken = block.start;
        if block.size > size {
            block.start += size;
            block.size -= size;
        } else {
            self.free.remove(index);
        }
        Some(taken)
        // q: this push/remove from vec is inefficient, right?
        // is a linked list better? do we use that in the kernel?
    }

    // -------------------------------------------------------------------
    // EXERCISE 2 — free with coalescing.
    //
    // Insert the span `[start, start + size)` back into the free list,
    // keeping it sorted by `start`. Then merge it with a neighbour if
    // they touch:
    //   * left-adjacent:  prev.start + prev.size == start
    //   * right-adjacent: start + size == next.start
    // A freed span can touch BOTH neighbours (it fills a hole exactly) —
    // handle that case so three blocks become one.
    //
    // After this runs, the invariant from the struct doc must hold: no
    // two free blocks are adjacent.
    //
    // Real-world twin: `Heap::deallocate` coalescing, and the kernel
    // heap's fragmentation metrics depend entirely on this working.
    // -------------------------------------------------------------------
    pub fn free(&mut self, start: usize, size: usize) {
        debug_assert!(
            self.free
                .iter()
                .all(|b| start + size <= b.start || b.start + b.size <= start),
            "free({start}, {size}) overlaps an existing free block — double free or bad span",
        );

        let i = self
            .free
            .iter()
            .position(|b| b.start > start)
            .unwrap_or(self.free.len());
        let merge_prev = i > 0 && self.free[i - 1].start + self.free[i - 1].size == start;
        let merge_next = i < self.free.len() && start + size == self.free[i].start;

        if merge_prev {
            let prev = &mut self.free[i - 1];
            prev.size += size;
        }

        if merge_next {
            if merge_prev {
                let next_size = self.free[i].size;
                let prev = &mut self.free[i - 1];
                prev.size += next_size;
                self.free.remove(i);
            } else {
                let next = &mut self.free[i];
                next.start -= size;
                next.size += size;
            }
        }

        if !merge_prev && !merge_next {
            self.free.insert(i, FreeBlock { start, size });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_arena_is_one_big_free_block() {
        let a = Arena::new(100);
        assert_eq!(a.free_bytes(), 100);
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.largest_free_block(), 100);
    }

    #[test]
    fn alloc_carves_from_the_front() {
        let mut a = Arena::new(100);
        assert_eq!(a.alloc(30), Some(0));
        assert_eq!(a.free_bytes(), 70);
        assert_eq!(a.fragments(), 1);
    }

    #[test]
    fn consecutive_allocs_do_not_overlap() {
        let mut a = Arena::new(100);
        let first = a.alloc(30).unwrap();
        let second = a.alloc(30).unwrap();
        assert_eq!(first, 0);
        assert_eq!(second, 30);
        assert!(second >= first + 30, "second alloc overlaps the first");
    }

    #[test]
    fn alloc_of_exact_remaining_empties_the_list() {
        let mut a = Arena::new(100);
        a.alloc(60).unwrap();
        a.alloc(40).unwrap();
        assert_eq!(a.free_bytes(), 0);
        assert_eq!(a.fragments(), 0);
    }

    #[test]
    fn alloc_too_big_fails_and_leaves_arena_untouched() {
        let mut a = Arena::new(100);
        a.alloc(80).unwrap();
        assert_eq!(a.alloc(50), None);
        assert_eq!(a.free_bytes(), 20);
    }

    #[test]
    fn alloc_zero_is_none() {
        let mut a = Arena::new(100);
        assert_eq!(a.alloc(0), None);
    }

    #[test]
    fn free_returns_bytes_to_the_pool() {
        let mut a = Arena::new(100);
        let p = a.alloc(30).unwrap();
        a.free(p, 30);
        assert_eq!(a.free_bytes(), 100);
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.largest_free_block(), 100);
    }

    #[test]
    fn free_into_empty_list_becomes_the_only_block() {
        let mut a = Arena::new(100);
        a.alloc(100).unwrap(); // drains the arena — free list is now empty
        assert_eq!(a.fragments(), 0);

        a.free(0, 100); // no neighbours to coalesce with; just insert
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.free_bytes(), 100);
        assert_eq!(a.largest_free_block(), 100);
    }

    #[test]
    fn freeing_in_ascending_order_coalesces_at_the_tail() {
        let mut a = Arena::new(30);
        a.alloc(30).unwrap(); // drain to empty

        a.free(0, 10); // empty-list path → [{0,10}]
        a.free(10, 10); // past the end AND right-adjacent to the tail → extends it
        assert_eq!(
            a.fragments(),
            1,
            "10 should merge into the tail, not append"
        );
        assert_eq!(a.free_bytes(), 20);
        assert_eq!(a.largest_free_block(), 20);

        a.free(20, 10); // tail-adjacent again → one block covers everything
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.largest_free_block(), 30);
    }

    #[test]
    fn freeing_a_hole_coalesces_both_neighbours() {
        // Three 10-byte allocations back to back, then free the middle.
        let mut a = Arena::new(30);
        let b0 = a.alloc(10).unwrap();
        let b1 = a.alloc(10).unwrap();
        let b2 = a.alloc(10).unwrap();
        assert_eq!(a.fragments(), 0);

        a.free(b1, 10); // hole in the middle, bounded by two allocations
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.free_bytes(), 10);

        a.free(b0, 10); // now touches the middle hole's left edge
        assert_eq!(a.fragments(), 1, "b0 should merge with the middle hole");
        assert_eq!(a.largest_free_block(), 20);

        a.free(b2, 10); // fills the last gap — everything is one block again
        assert_eq!(a.fragments(), 1);
        assert_eq!(a.largest_free_block(), 30);
    }

    #[test]
    fn non_adjacent_frees_stay_fragmented() {
        let mut a = Arena::new(30);
        let b0 = a.alloc(10).unwrap();
        let _b1 = a.alloc(10).unwrap();
        let b2 = a.alloc(10).unwrap();

        a.free(b0, 10);
        a.free(b2, 10); // b0 and b2 are NOT adjacent — two separate holes
        assert_eq!(a.fragments(), 2);
        assert_eq!(a.free_bytes(), 20);
        assert_eq!(a.largest_free_block(), 10);
    }

    // Build a two-block free list with empty space at the very front, then
    // free into that front. `next_index` is 0 here (no left neighbour), which
    // the `len() == 1` special-case doesn't cover when len > 1.
    fn arena_with_two_blocks_and_a_front_gap() -> Arena {
        let mut a = Arena::new(100);
        a.alloc(100).unwrap(); // drain to empty
        a.free(20, 10);
        a.free(40, 10); // free list: [{20,10}, {40,10}]
        assert_eq!(a.fragments(), 2);
        a
    }

    #[test]
    fn freeing_at_the_front_of_a_multi_block_list_non_adjacent() {
        let mut a = arena_with_two_blocks_and_a_front_gap();
        a.free(0, 10); // before both blocks, touches neither → new front fragment
        assert_eq!(a.fragments(), 3);
        assert_eq!(a.free_bytes(), 30);
        assert_eq!(a.largest_free_block(), 10);
    }

    #[test]
    fn freeing_at_the_front_of_a_multi_block_list_merges_first() {
        let mut a = arena_with_two_blocks_and_a_front_gap();
        a.free(10, 10); // 10+10 == 20 → must merge into the first block
        assert_eq!(a.fragments(), 2);
        assert_eq!(a.free_bytes(), 30);
        assert_eq!(a.largest_free_block(), 20); // {10,20} now the biggest
    }

    #[test]
    fn freeing_a_lone_gap_in_the_middle_adds_a_fragment() {
        let mut a = Arena::new(100);
        a.alloc(100).unwrap();
        a.free(0, 10);
        a.free(80, 10); // [{0,10}, {80,10}]
        a.free(40, 10); // sits between, adjacent to neither → pure middle insert
        assert_eq!(a.fragments(), 3);
        assert_eq!(a.free_bytes(), 30);
    }

    #[test]
    fn filling_an_exact_hole_between_two_blocks_merges_all_three() {
        // Build [{0,10}, {20,10}] with a 10-byte hole at [10,20) between them,
        // then free into that hole. This is the ONLY geometry that routes
        // through the merge_prev && merge_next arm in the middle of the list
        // (not at the front or tail).
        let mut a = Arena::new(30);
        a.alloc(30).unwrap(); // drain to empty
        a.free(0, 10);
        a.free(20, 10); // [{0,10}, {20,10}]
        assert_eq!(a.fragments(), 2);

        a.free(10, 10); // exact hole, flanked on both sides → all three merge
        assert_eq!(a.fragments(), 1);
        assert_eq!(
            a.free_bytes(),
            30,
            "coalescing must conserve bytes, not invent them"
        );
        assert_eq!(a.largest_free_block(), 30);
        assert!(a.free_bytes() <= a.size, "free bytes exceeded the arena");
    }

    // --- Property / model-based test ------------------------------------
    //
    // Instead of hand-picking scenarios, drive the arena with a long,
    // deterministic pseudo-random sequence of allocs and frees, and after
    // EVERY op assert the structural invariants. A model (`live`) tracks the
    // regions currently handed out so we only ever free valid spans and can
    // check byte conservation. This catches whole classes of bugs (like the
    // merge double-count) without anyone predicting the failing geometry.

    /// Deterministic, dependency-free PRNG. Same seed → same sequence, so a
    /// failure is always reproducible. (xorshift requires a non-zero state.)
    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// The free list must always be: in bounds, strictly sorted, non-empty
    /// blocks, and with a real GAP between consecutive blocks (anything
    /// touching should have coalesced). Plus: free + handed-out == capacity.
    fn assert_invariants(a: &Arena, live_bytes: usize) {
        let blocks = a.free_list();
        for (i, b) in blocks.iter().enumerate() {
            assert!(b.size > 0, "degenerate zero-size block at {i}: {blocks:?}");
            assert!(
                b.start + b.size <= a.size,
                "block runs past the arena: {b:?} in {blocks:?}",
            );
            if i > 0 {
                let prev = blocks[i - 1];
                // Strict `<` enforces sorted + non-overlapping + non-adjacent
                // in one shot: `==` would mean "should have merged", `>` means
                // overlap or out-of-order.
                assert!(
                    prev.start + prev.size < b.start,
                    "blocks not coalesced or out of order: {prev:?} then {b:?}",
                );
            }
        }
        assert_eq!(
            a.free_bytes() + live_bytes,
            a.size,
            "byte conservation broken: free={} + live={} != size={}",
            a.free_bytes(),
            live_bytes,
            a.size,
        );
    }

    #[test]
    fn property_random_alloc_free_preserves_invariants() {
        const ARENA: usize = 256;
        const OPS: usize = 400;

        for seed in 0..64u64 {
            let mut a = Arena::new(ARENA);
            let mut live: Vec<(usize, usize)> = Vec::new(); // (offset, size) handed out
            let mut live_bytes = 0usize;
            let mut rng = (seed + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;

            assert_invariants(&a, live_bytes);

            for _ in 0..OPS {
                // Bias slightly toward alloc when nothing is live so we don't
                // spin on the empty branch.
                let do_alloc = live.is_empty() || xorshift64(&mut rng) % 2 == 0;
                if do_alloc {
                    let size = (xorshift64(&mut rng) % 32) as usize + 1; // 1..=32
                    if let Some(off) = a.alloc(size) {
                        live.push((off, size));
                        live_bytes += size;
                    }
                } else {
                    let idx = (xorshift64(&mut rng) % live.len() as u64) as usize;
                    let (off, size) = live.swap_remove(idx);
                    a.free(off, size);
                    live_bytes -= size;
                }
                assert_invariants(&a, live_bytes);
            }
        }
    }
}
