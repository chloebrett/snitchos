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
        todo!("EXERCISE 1: first-fit alloc + split — see comment above")
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
        todo!("EXERCISE 2: free + coalesce — see comment above")
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
}
