//! A one-bit-per-frame allocator — the fixed-size cousin of the
//! free-list. This is a faithful miniature of `kernel-mem/src/frame.rs`.
//!
//! Convention (same as the real one): **bit = 1 means FREE**, bit = 0
//! means in-use. That choice is deliberate — "find a free frame"
//! becomes "find the lowest set bit", which `u64::trailing_zeros` does
//! in one instruction per word.
//!
//! Everything here is provided EXCEPT `alloc`, which is the one
//! conceptual gem (Exercise 3).

pub struct FrameBitmap {
    bits: Vec<u64>,
    capacity: usize,
    free: usize,
}

impl FrameBitmap {
    /// All frames start in-use; the caller releases the ones that are
    /// actually available (mirrors the real allocator, which starts
    /// everything reserved and frees only real RAM).
    pub fn new(capacity: usize) -> Self {
        let words = capacity.div_ceil(64);
        Self {
            bits: vec![0u64; words],
            capacity,
            free: 0,
        }
    }

    /// Mark `[start, start + count)` free (clamped to capacity).
    pub fn release_range(&mut self, start: usize, count: usize) {
        if start >= self.capacity {
            return;
        }
        let end = (start + count).min(self.capacity);
        for frame in start..end {
            self.set_free(frame);
        }
    }

    /// Return a single frame to the pool. Idempotent.
    pub fn free(&mut self, frame: usize) {
        assert!(frame < self.capacity, "free past capacity");
        self.set_free(frame);
    }

    pub fn count_free(&self) -> usize {
        self.free
    }

    pub fn count_in_use(&self) -> usize {
        self.capacity - self.free
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 0 -> 1 transition only, so the `free` counter stays accurate
    /// under idempotent double-frees.
    fn set_free(&mut self, frame: usize) {
        let word = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bits[word] & mask == 0 {
            self.bits[word] |= mask;
            self.free += 1;
        }
    }

    // -------------------------------------------------------------------
    // EXERCISE 3 — allocate the lowest free frame.
    //
    // Return the index of the lowest-numbered free frame and mark it
    // in-use (clear its bit), decrementing the free counter. Return
    // None if nothing is free.
    //
    // Do it the fast way the kernel does:
    //   * if `self.free == 0`, bail immediately (no scan).
    //   * otherwise scan words; the first non-zero word has a free
    //     frame in it. `word.trailing_zeros()` gives the bit index;
    //     frame = word_index * 64 + bit.
    //   * guard against a "free" bit that lies past `capacity` (the
    //     last word can have padding bits) — return None in that case.
    //
    // This is line-for-line `Bitmap::alloc` in kernel-mem/src/frame.rs.
    // -------------------------------------------------------------------
    pub fn alloc(&mut self) -> Option<usize> {
        if self.free == 0 {
            return None;
        }

        for (i, word) in self.bits.iter_mut().enumerate() {
            let bit = word.trailing_zeros() as usize;
            if bit == 64 {
                continue;
            } else {
                self.free -= 1;
                *word &= !(1u64 << bit);
                return Some(bit + i * 64);
            }
        }

        return None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bitmap_is_all_in_use() {
        let bm = FrameBitmap::new(128);
        assert_eq!(bm.capacity(), 128);
        assert_eq!(bm.count_free(), 0);
        assert_eq!(bm.count_in_use(), 128);
    }

    #[test]
    fn release_then_alloc_returns_lowest_free_frame() {
        let mut bm = FrameBitmap::new(128);
        bm.release_range(10, 5); // frames 10..15 free
        assert_eq!(bm.count_free(), 5);
        assert_eq!(bm.alloc(), Some(10));
        assert_eq!(bm.alloc(), Some(11));
        assert_eq!(bm.count_free(), 3);
    }

    #[test]
    fn alloc_crosses_word_boundaries() {
        let mut bm = FrameBitmap::new(128);
        bm.release_range(70, 1); // a single free frame in the *second* u64
        assert_eq!(bm.alloc(), Some(70));
    }

    #[test]
    fn alloc_on_empty_pool_is_none() {
        let mut bm = FrameBitmap::new(128);
        assert_eq!(bm.alloc(), None);
    }

    #[test]
    fn freed_frame_is_reusable() {
        let mut bm = FrameBitmap::new(128);
        bm.release_range(0, 2);
        let f = bm.alloc().unwrap();
        bm.free(f);
        assert_eq!(
            bm.alloc(),
            Some(f),
            "freed frame should be handed out again"
        );
    }
}
