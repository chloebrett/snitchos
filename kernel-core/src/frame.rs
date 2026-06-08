//! Physical frame bitmap. Pure data — wraps a caller-provided slice
//! of `u64`s plus a frame count. Convention: bit = 1 means free,
//! bit = 0 means in-use. So "find first set bit" is the alloc op,
//! which `trailing_zeros` makes O(words).
//!
//! The kernel side (`kernel::frame`) holds the static backing storage
//! and a `Mutex<Bitmap>` around an instance. Telemetry counters live
//! there too; this module is pure bookkeeping.

/// Frame bitmap. `bits` is the backing storage; `capacity` is the
/// number of frames actually tracked (may be < `bits.len() * 64` if
/// the cap doesn't fall on a word boundary). `frames_free` is
/// maintained internally so `count_free` and the OOM check in `alloc`
/// are O(1).
pub struct Bitmap<'a> {
    bits: &'a mut [u64],
    capacity: usize,
    frames_free: usize,
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
        Self { bits, capacity, frames_free: 0 }
    }

    /// Mark frames `[start, start + count)` as free. Out-of-range
    /// frames (start ≥ capacity, or extending past capacity) are
    /// clamped silently. Idempotent — bits already set stay set and
    /// don't double-count in `frames_free`.
    pub fn release_range(&mut self, start: usize, count: usize) {
        if start >= self.capacity {
            return;
        }
        let end = (start + count).min(self.capacity);
        for f in start..end {
            self.set_bit_tracked(f);
        }
    }

    /// Allocate the lowest-indexed free frame. Returns its index, or
    /// `None` if no frames are free. Returns immediately when the
    /// pool is empty (no scan) — critical for OOM workloads where
    /// many failing allocs may happen per tick.
    pub fn alloc(&mut self) -> Option<usize> {
        if self.frames_free == 0 {
            return None;
        }
        for (i, w) in self.bits.iter_mut().enumerate() {
            if *w != 0 {
                let bit = w.trailing_zeros() as usize;
                let frame = i * 64 + bit;
                if frame >= self.capacity {
                    return None;
                }
                *w &= !(1u64 << bit);
                self.frames_free -= 1;
                return Some(frame);
            }
        }
        None
    }

    /// Allocate `n` consecutive free frames. Returns the starting
    /// frame index, or `None` if no run of `n` exists. Marks all `n`
    /// bits used and decrements `frames_free` by `n` on success.
    /// `n == 0` returns `None` — zero-length allocation is a
    /// programmer error, not a degenerate success.
    pub fn alloc_contiguous(&mut self, n: usize) -> Option<usize> {
        if n == 0 || n > self.frames_free {
            return None;
        }
        let mut run_start: Option<usize> = None;
        let mut run_len: usize = 0;
        for frame in 0..self.capacity {
            let word_idx = frame / 64;
            let mask = 1u64 << (frame % 64);
            if self.bits[word_idx] & mask != 0 {
                if run_start.is_none() {
                    run_start = Some(frame);
                }
                run_len += 1;
                if run_len == n {
                    let start = run_start.unwrap();
                    for f in start..start + n {
                        let w = f / 64;
                        let m = 1u64 << (f % 64);
                        self.bits[w] &= !m;
                    }
                    self.frames_free -= n;
                    return Some(start);
                }
            } else {
                run_start = None;
                run_len = 0;
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
        self.set_bit_tracked(frame);
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// O(1) — reads the maintained counter.
    pub fn count_free(&self) -> usize {
        self.frames_free
    }

    pub fn count_in_use(&self) -> usize {
        self.capacity - self.frames_free
    }

    /// Set the bit and bump `frames_free` only on a 0→1 transition.
    /// Used by both `release_range` and `free` so the counter stays
    /// accurate under idempotent retries.
    fn set_bit_tracked(&mut self, frame: usize) {
        let word_idx = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bits[word_idx] & mask == 0 {
            self.bits[word_idx] |= mask;
            self.frames_free += 1;
        }
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
    fn alloc_contiguous_returns_run_start() {
        // Frames 10..20 free, rest in-use. A run of 5 should start at 10.
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(10, 10);
        assert_eq!(bm.alloc_contiguous(5), Some(10));
    }

    #[test]
    fn alloc_contiguous_spans_word_boundary() {
        // 60..70 free; a run of 8 must straddle the u64 boundary at 64.
        let (mut storage, cap) = empty(128);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(60, 10);
        assert_eq!(bm.alloc_contiguous(8), Some(60));
        // All 8 marked used; remaining 2 (68, 69) still free.
        assert_eq!(bm.count_free(), 2);
        assert_eq!(bm.alloc(), Some(68));
    }

    #[test]
    fn alloc_contiguous_returns_none_when_no_run_fits() {
        // Free frames scattered: 0,1,2 and 10,11. No run of 4.
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 3);
        bm.release_range(10, 2);
        assert_eq!(bm.alloc_contiguous(4), None);
        // State preserved on failure.
        assert_eq!(bm.count_free(), 5);
    }

    #[test]
    fn alloc_contiguous_skips_used_bits_in_middle() {
        // 0..5 free, 5 used, 6..20 free. Run of 10 must start at 6, not 0.
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 5);
        bm.release_range(6, 14);
        assert_eq!(bm.alloc_contiguous(10), Some(6));
    }

    #[test]
    fn alloc_contiguous_decrements_frames_free_by_n() {
        let (mut storage, cap) = empty(256);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 256);
        assert_eq!(bm.count_free(), 256);
        assert_eq!(bm.alloc_contiguous(100), Some(0));
        assert_eq!(bm.count_free(), 156);
    }

    #[test]
    fn alloc_contiguous_zero_returns_none() {
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 64);
        assert_eq!(bm.alloc_contiguous(0), None);
        assert_eq!(bm.count_free(), 64);
    }

    #[test]
    fn alloc_contiguous_larger_than_pool_returns_none() {
        let (mut storage, cap) = empty(64);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 64);
        assert_eq!(bm.alloc_contiguous(65), None);
        assert_eq!(bm.count_free(), 64);
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
