//! A power-of-two **buddy allocator** — the third allocation strategy.
//!
//! The arena is `2^max_order` *units* (a unit = the smallest allocatable
//! block, "order 0"). Every block has an *order*: an order-`k` block spans
//! `2^k` units. Free blocks are kept in **per-order free lists** — one list
//! per size class — so finding a free block of a given size is O(1).
//!
//! The magic is **coalescing in O(1)** via the *buddy* relationship. A block
//! of order `k` has exactly one buddy: the other half of the `2^(k+1)` block
//! they pair into. Because order-`k` blocks are aligned to `2^k`, the two
//! buddies differ in exactly **one bit** — bit `k` of the offset:
//!
//! ```text
//!   order-(k+1) block, size 2^(k+1)
//!   ┌───────────────┬───────────────┐
//!   │  buddy A       │  buddy B      │    A.offset = ....0.... (bit k = 0)
//!   │  offset X      │  offset X^2^k │    B.offset = ....1.... (bit k = 1)
//!   └───────────────┴───────────────┘    differ only in bit k
//! ```
//!
//! So `buddy = offset ^ (1 << order)` — flip bit `order` and you're at the
//! buddy; flip again, back. On free, you check "is my buddy also free at the
//! same order?" — one XOR + one lookup, no neighbour scan. If yes, merge into
//! an order-`k+1` block and repeat upward. That O(1)-everywhere coalescing is
//! why Linux uses buddy for physical pages (contrast SnitchOS's bitmap, which
//! can't coalesce into bigger contiguous runs cheaply at all).
//!
//! The cost: **internal fragmentation** — a 33-unit request rounds up to a
//! 64-unit (order-6) block, wasting 31 units.

/// The buddy of an order-`order` block at `offset`: the *other* half of the
/// `2^(order+1)` pair. Differ-by-one-bit ⇒ a single XOR. (Provided — the
/// concept to internalise is *why* it's an XOR; the exercises are the alloc
/// split and the free coalesce that use it.)
pub fn buddy_offset(offset: usize, order: usize) -> usize {
    offset ^ (1 << order)
}

pub struct Buddy {
    max_order: usize,
    /// `free[k]` = offsets of free order-`k` blocks.
    free: Vec<Vec<usize>>,
}

impl Buddy {
    /// A fresh arena of `2^max_order` units: one free block of the top order.
    pub fn new(max_order: usize) -> Self {
        let mut free = vec![Vec::new(); max_order + 1];
        free[max_order].push(0);
        Self { max_order, free }
    }

    /// Total units in the arena (`2^max_order`).
    pub fn capacity_units(&self) -> usize {
        1 << self.max_order
    }

    /// Sum of all free units across every order.
    pub fn total_free_units(&self) -> usize {
        self.free
            .iter()
            .enumerate()
            .map(|(order, list)| list.len() * (1 << order))
            .sum()
    }

    /// How many free blocks sit at a given order (introspection / tests).
    pub fn free_list(&self, order: usize) -> &[usize] {
        &self.free[order]
    }

    // -------------------------------------------------------------------
    // EXERCISE A — allocate an order-`order` block (with splitting).
    //
    // Find the SMALLEST available order `j >= order` that has a free block;
    // pop one. If `j > order`, split it down: each split of an order-`o`
    // block yields two order-`(o-1)` buddies — keep the lower half, push the
    // upper half (`block + 2^(o-1)`) onto `free[o-1]` — until you reach
    // `order`. Return the offset of the block you kept.
    //
    // Return None if no order `>= order` has a free block.
    //
    // Real-world twin: Linux's `__rmqueue_smallest` / `expand`.
    // -------------------------------------------------------------------
    pub fn alloc(&mut self, order: usize) -> Option<usize> {
        let _ = order;
        todo!("EXERCISE A: find-smallest + split down — see the comment above")
    }

    // -------------------------------------------------------------------
    // EXERCISE B — free an order-`order` block at `offset` (with coalescing).
    //
    // Loop while `order < max_order`:
    //   * compute the buddy with `buddy_offset(offset, order)`.
    //   * if the buddy is currently free at this order (search `free[order]`),
    //     remove it, merge: the parent starts at `min(offset, buddy)`, bump
    //     `order`, and continue the loop.
    //   * otherwise stop.
    // Then push the (possibly merged-up) block onto `free[order]`.
    //
    // Real-world twin: Linux's `__free_one_page` coalescing loop.
    // -------------------------------------------------------------------
    pub fn free(&mut self, offset: usize, order: usize) {
        let _ = (offset, order);
        todo!("EXERCISE B: XOR-buddy coalesce loop — see the comment above")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buddy_offset_flips_exactly_one_bit() {
        // Order 0 → flip bit 0; order 3 → flip bit 3. Involution: twice = identity.
        assert_eq!(buddy_offset(0, 0), 1);
        assert_eq!(buddy_offset(1, 0), 0);
        assert_eq!(buddy_offset(0, 3), 8);
        assert_eq!(buddy_offset(8, 3), 0);
        assert_eq!(buddy_offset(buddy_offset(5, 2), 2), 5);
    }

    #[test]
    fn new_is_one_full_block() {
        let b = Buddy::new(4); // 16 units
        assert_eq!(b.capacity_units(), 16);
        assert_eq!(b.total_free_units(), 16);
        assert_eq!(b.free_list(4).len(), 1);
    }

    #[test]
    fn alloc_whole_arena_takes_the_top_block() {
        let mut b = Buddy::new(4);
        assert_eq!(b.alloc(4), Some(0));
        assert_eq!(b.total_free_units(), 0);
    }

    #[test]
    fn alloc_smallest_splits_all_the_way_down() {
        // Allocating one order-0 block from a fresh order-4 arena splits the
        // top block down, leaving exactly one free buddy at each lower order.
        let mut b = Buddy::new(4);
        assert_eq!(b.alloc(0), Some(0));
        assert_eq!(b.free_list(0).len(), 1); // buddy at offset 1
        assert_eq!(b.free_list(1).len(), 1); // buddy at offset 2
        assert_eq!(b.free_list(2).len(), 1); // buddy at offset 4
        assert_eq!(b.free_list(3).len(), 1); // buddy at offset 8
        assert_eq!(b.total_free_units(), 15); // 16 - 1
    }

    #[test]
    fn two_order0_allocs_are_buddies() {
        let mut b = Buddy::new(4);
        assert_eq!(b.alloc(0), Some(0));
        assert_eq!(b.alloc(0), Some(1)); // the order-0 buddy of block 0
    }

    #[test]
    fn alloc_returns_none_when_no_block_large_enough() {
        let mut b = Buddy::new(2); // 4 units
        b.alloc(2).unwrap(); // takes everything
        assert_eq!(b.alloc(0), None);
    }

    #[test]
    fn free_coalesces_buddies_back_to_the_full_block() {
        let mut b = Buddy::new(4);
        let a0 = b.alloc(0).unwrap(); // 0
        let a1 = b.alloc(0).unwrap(); // 1 (its buddy)
        b.free(a0, 0); // buddy (1) still allocated → no merge yet
        assert_eq!(b.free_list(0).len(), 1);
        b.free(a1, 0); // now buddies reunite and cascade all the way up
        assert_eq!(b.free_list(4).len(), 1, "should coalesce back to one top block");
        assert_eq!(b.total_free_units(), 16);
    }

    #[test]
    fn free_with_allocated_buddy_does_not_coalesce() {
        let mut b = Buddy::new(4);
        let a0 = b.alloc(0).unwrap(); // 0
        let _a1 = b.alloc(0).unwrap(); // 1, kept allocated
        b.free(a0, 0);
        // Buddy of 0 (=1) is still in use, so 0 stays a lone order-0 free block.
        assert_eq!(b.free_list(0), &[0]);
        assert_eq!(b.total_free_units(), 15);
    }

    // ---- Property: alloc/free preserve the buddy invariants ------------
    use proptest::prelude::*;
    use proptest::test_runner::TestCaseError;

    const MAX_ORDER: usize = 6; // 64-unit arena

    fn check_invariants(b: &Buddy, live_units: usize) -> Result<(), TestCaseError> {
        for order in 0..=MAX_ORDER {
            for &off in b.free_list(order) {
                // Every free block is aligned to its own size.
                prop_assert_eq!(
                    off % (1 << order),
                    0,
                    "order-{} block at {} is misaligned",
                    order,
                    off
                );
                prop_assert!(off + (1 << order) <= b.capacity_units(), "block past arena end");
                // No free block's buddy is ALSO free at the same order —
                // they would have had to coalesce. (Only meaningful below
                // the top order, where a buddy exists.)
                if order < MAX_ORDER {
                    let buddy = buddy_offset(off, order);
                    prop_assert!(
                        !b.free_list(order).contains(&buddy),
                        "order-{} buddies {} and {} both free — should have merged",
                        order,
                        off,
                        buddy
                    );
                }
            }
        }
        // Conservation: free + handed-out == capacity.
        prop_assert_eq!(
            b.total_free_units() + live_units,
            b.capacity_units(),
            "unit conservation broken"
        );
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        #[test]
        fn random_alloc_free_preserves_buddy_invariants(
            ops in prop::collection::vec((0usize..=3, any::<bool>()), 0..400),
        ) {
            let mut b = Buddy::new(MAX_ORDER);
            let mut live: Vec<(usize, usize)> = Vec::new(); // (offset, order)
            let mut live_units = 0usize;

            check_invariants(&b, live_units)?;
            for (order, do_alloc) in ops {
                if do_alloc || live.is_empty() {
                    if let Some(off) = b.alloc(order) {
                        live.push((off, order));
                        live_units += 1 << order;
                    }
                } else {
                    // free a pseudo-arbitrary live block (index derived from `order`)
                    let idx = order % live.len();
                    let (off, o) = live.swap_remove(idx);
                    b.free(off, o);
                    live_units -= 1 << o;
                }
                check_invariants(&b, live_units)?;
            }
        }
    }
}
