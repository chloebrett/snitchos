//! Physical frame bitmap. Pure data — wraps a caller-provided slice
//! of `u64`s plus a frame count. Convention: bit = 1 means free,
//! bit = 0 means in-use. So "find first set bit" is the alloc op,
//! which trailing_zeros makes O(words).
//!
//! The kernel side (`kernel::frame`) holds the static backing storage
//! and a `Mutex<Bitmap>` around an instance. Telemetry counters live
//! there too; this module is pure bookkeeping.

/// Frame bitmap. `bits` is the backing storage; `capacity` is the
/// number of frames actually tracked (may be < `bits.len() * 64` if
/// the cap doesn't fall on a word boundary).
pub struct Bitmap<'a> {
    bits: &'a mut [u64],
    capacity: usize,
}

impl<'a> Bitmap<'a> {
    /// Wrap a backing buffer + capacity. All frames start in-use; the
    /// caller must explicitly `release_range` the ones that are
    /// actually available.
    pub fn new(bits: &'a mut [u64], capacity: usize) -> Self {
        assert!(
            capacity <= bits.len() * 64,
            "bitmap capacity {capacity} exceeds storage of {} bits",
            bits.len() * 64,
        );
        for w in bits.iter_mut() {
            *w = 0;
        }
        Self { bits, capacity }
    }

    /// Mark frames `[start, start + count)` as free. Out-of-range
    /// frames (start ≥ capacity, or extending past capacity) are
    /// clamped silently.
    pub fn release_range(&mut self, start: usize, count: usize) {
        if start >= self.capacity {
            return;
        }
        let end = (start + count).min(self.capacity);
        for f in start..end {
            self.set_bit(f);
        }
    }

    /// Allocate the lowest-indexed free frame. Returns its index, or
    /// `None` if no frames are free.
    pub fn alloc(&mut self) -> Option<usize> {
        // Scan words for the first non-zero one. `trailing_zeros` then
        // gives the lowest free bit.
        for (i, w) in self.bits.iter_mut().enumerate() {
            if *w != 0 {
                let bit = w.trailing_zeros() as usize;
                let frame = i * 64 + bit;
                if frame >= self.capacity {
                    // Word had bits set past capacity — shouldn't
                    // happen because release_range clamps, but guard.
                    return None;
                }
                *w &= !(1u64 << bit);
                return Some(frame);
            }
        }
        None
    }

    /// Mark `frame` as free. Idempotent — double-free is a no-op
    /// rather than a panic so callers can be lazy in error paths.
    /// Out-of-range frames panic (programmer error, not a graceful
    /// recovery case).
    pub fn free(&mut self, frame: usize) {
        assert!(
            frame < self.capacity,
            "free({frame}) is past capacity {}",
            self.capacity,
        );
        self.set_bit(frame);
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// O(words). Cache externally if it becomes a hot path.
    pub fn count_free(&self) -> usize {
        let mut total = 0usize;
        for w in self.bits.iter() {
            total += w.count_ones() as usize;
        }
        // Clamp in case any bits are set past capacity (shouldn't
        // happen — release_range clamps — but defensive).
        total.min(self.capacity)
    }

    pub fn count_in_use(&self) -> usize {
        self.capacity - self.count_free()
    }

    fn set_bit(&mut self, frame: usize) {
        self.bits[frame / 64] |= 1u64 << (frame % 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    fn empty(capacity: usize) -> (std::vec::Vec<u64>, usize) {
        let words = capacity.div_ceil(64);
        (vec![0u64; words], capacity)
    }

    #[test]
    fn new_starts_with_every_frame_in_use() {
        let (mut storage, cap) = empty(256);
        let bm = Bitmap::new(&mut storage, cap);
        assert_eq!(bm.capacity(), 256);
        assert_eq!(bm.count_free(), 0);
        assert_eq!(bm.count_in_use(), 256);
    }

    #[test]
    fn release_range_marks_frames_free() {
        let (mut storage, cap) = empty(256);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(10, 30);
        assert_eq!(bm.count_free(), 30);
        assert_eq!(bm.count_in_use(), 226);
    }

    #[test]
    fn release_range_clamps_to_capacity() {
        let (mut storage, cap) = empty(100);
        let mut bm = Bitmap::new(&mut storage, cap);
        // Request to free 50 starting at 80 — only 20 fit.
        bm.release_range(80, 50);
        assert_eq!(bm.count_free(), 20);
    }

    #[test]
    fn release_range_crossing_word_boundaries_works() {
        // 64 frames span exactly one u64; releasing 100 frames starting
        // at 30 crosses into the next word.
        let (mut storage, cap) = empty(256);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(30, 100);
        assert_eq!(bm.count_free(), 100);
    }

    #[test]
    fn alloc_returns_lowest_free_frame_and_marks_used() {
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(5, 3);  // frames 5, 6, 7 free
        assert_eq!(bm.alloc(), Some(5));
        assert_eq!(bm.alloc(), Some(6));
        assert_eq!(bm.alloc(), Some(7));
        assert_eq!(bm.alloc(), None);
    }

    #[test]
    fn alloc_returns_none_when_empty() {
        let (mut storage, cap) = empty(128);
        let mut bm = Bitmap::new(&mut storage, cap);
        assert_eq!(bm.alloc(), None);
    }

    #[test]
    fn alloc_skips_used_frames_and_finds_free_in_higher_word() {
        // Capacity 128, free only frame 100 (in second u64 word).
        let (mut storage, cap) = empty(128);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(100, 1);
        assert_eq!(bm.alloc(), Some(100));
        assert_eq!(bm.alloc(), None);
    }

    #[test]
    fn free_returns_frame_to_pool() {
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 64);
        let f = bm.alloc().unwrap();
        assert_eq!(bm.count_free(), 63);
        bm.free(f);
        assert_eq!(bm.count_free(), 64);
    }

    #[test]
    fn alloc_free_round_trip_preserves_counts() {
        let (mut storage, cap) = empty(256);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 256);
        let mut taken = std::vec::Vec::new();
        for _ in 0..256 {
            taken.push(bm.alloc().unwrap());
        }
        assert_eq!(bm.count_free(), 0);
        for f in taken {
            bm.free(f);
        }
        assert_eq!(bm.count_free(), 256);
    }

    #[test]
    fn capacity_can_be_less_than_storage_word_count_times_64() {
        // 70 frames → 2 u64 words (128 bits), but only frames 0..70 exist.
        let (mut storage, cap) = empty(70);
        let mut bm = Bitmap::new(&mut storage, cap);
        assert_eq!(bm.capacity(), 70);
        bm.release_range(0, 100);  // requests beyond cap are clamped
        assert_eq!(bm.count_free(), 70);
    }

    #[test]
    fn double_free_is_idempotent() {
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 1);
        let f = bm.alloc().unwrap();
        bm.free(f);
        bm.free(f);  // already free — should be a no-op
        assert_eq!(bm.count_free(), 1);
    }
}
