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
    /// Search cursor: the lowest word index that *may* hold a free bit. Invariant:
    /// every word below it is fully allocated (`0`). `alloc` scans from here rather
    /// than word 0, so filling the pool sequentially is O(n) total (O(1) amortized
    /// per alloc) instead of O(n²) — the frontier only advances. `free`/release
    /// rewind it when they return a frame *below* the cursor.
    next_hint: usize,
    /// Cumulative count of bitmap words `alloc` has examined — a diagnostic on the
    /// allocator's search cost. Its whole point is to make the O(1)-cursor property
    /// *assertable*: a full fill stays O(n) here, so a regression to a linear
    /// scan-from-0 (O(n²)) is caught by a test, not just by an audit measurement.
    scan_words: usize,
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
        Self { bits, capacity, frames_free: 0, next_hint: 0, scan_words: 0 }
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
        // Scan from the frontier, not word 0 — words below `next_hint` are known
        // fully allocated. Advancing it as words fill makes a sequential fill O(n)
        // instead of O(n²).
        for i in self.next_hint..self.bits.len() {
            self.scan_words += 1;
            let w = self.bits[i];
            if w != 0 {
                let bit = w.trailing_zeros() as usize;
                let frame = i * 64 + bit;
                if frame >= self.capacity {
                    return None;
                }
                self.bits[i] = w & !(1u64 << bit);
                self.frames_free -= 1;
                // This word may still hold free bits; if it just emptied, the next
                // alloc advances past it. Either way words below `i` stay 0.
                self.next_hint = i;
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

    /// Cumulative bitmap words examined by `alloc` — the allocator's search cost.
    /// Stays O(n) for a sequential fill thanks to the frontier cursor; asserted by
    /// `filling_the_pool_is_linear_not_quadratic`.
    pub fn scan_words(&self) -> usize {
        self.scan_words
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
            // A frame became free below the search frontier — rewind so `alloc`
            // reconsiders it (keeps "returns the lowest free frame").
            self.next_hint = self.next_hint.min(word_idx);
        }
    }
}

/// Release every frame whose base physical address falls outside all
/// `reserved` ranges, leaving reserved frames in-use. Ranges are
/// half-open `[start, end)` over physical addresses; a frame is judged
/// by its base PA (`ram_base + idx * frame_size`) only, so a reserved
/// range whose `end` is not frame-aligned still reserves the frame it
/// lands inside but not the next one.
///
/// The kernel calls this once at boot with the SBI / kernel-image / DTB
/// regions; the bitmap starts all-in-use (per `Bitmap::new`), so this
/// is the sole step that populates the free pool.
pub fn release_unreserved(
    bitmap: &mut Bitmap,
    ram_base: usize,
    frame_size: usize,
    reserved: &[(usize, usize)],
) {
    for f in 0..bitmap.capacity() {
        let pa = ram_base + f * frame_size;
        let reserved_hit = reserved.iter().any(|&(start, end)| pa >= start && pa < end);
        if !reserved_hit {
            bitmap.release_range(f, 1);
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
    fn filling_the_pool_is_linear_not_quadratic() {
        // Turns the O(1)-cursor property into an assertion. Filling N frames
        // sequentially costs O(N) total word-scans via the frontier cursor; a
        // regression to "scan from word 0 every alloc" is O(N²). No *behavioral*
        // test can catch that (the frames handed out are identical) — only the
        // scan cost differs. This kills the scan-from-0 mutant.
        const N: usize = 8192;
        let (mut storage, cap) = empty(N);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, N);
        for _ in 0..N {
            bm.alloc().unwrap();
        }
        // Cursor fill: ~1-2 word-scans per alloc ≈ N. Scan-from-0 fill: Σ(k/64) ≈
        // N²/128 ≈ 500k for N=8192. 4·N cleanly separates them.
        assert!(
            bm.scan_words() < 4 * N,
            "fill scanned {} words for {N} frames — expected O(N) (~{N}), not O(N²)",
            bm.scan_words(),
        );
    }

    #[test]
    fn alloc_reuses_a_frame_freed_below_the_allocation_frontier() {
        // Exhaust the pool so the search frontier sits at the top, then free a
        // low frame far behind it. The next alloc must return that low frame — an
        // O(1) search cursor must *rewind* on free, never strand freed frames
        // behind the frontier. (This is the correctness guard the hint must keep.)
        let (mut storage, cap) = empty(256);
        let mut bm = Bitmap::new(&mut storage, cap);
        bm.release_range(0, 256);
        for _ in 0..256 {
            bm.alloc().unwrap();
        }
        assert_eq!(bm.alloc(), None); // pool empty, frontier at the top
        bm.free(3);
        assert_eq!(bm.alloc(), Some(3), "a frame freed behind the frontier is found again");
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
    fn release_unreserved_frees_frames_outside_every_reserved_range() {
        // 10 frames of size 4096 from ram_base 0. Reserve frame 0 only.
        let (mut storage, cap) = empty(10);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(&mut bm, 0, 4096, &[(0, 4096)]);
        // Frame 0 (pa 0) stays in-use; frames 1..10 freed.
        assert_eq!(bm.count_free(), 9);
        assert_eq!(bm.count_in_use(), 1);
    }

    #[test]
    fn release_unreserved_treats_range_end_as_exclusive() {
        // Reserve [0, 4096): only frame 0 (base 0). Frame 1's base is
        // exactly 4096 == end, so it is NOT reserved.
        let (mut storage, cap) = empty(4);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(&mut bm, 0, 4096, &[(0, 4096)]);
        assert_eq!(bm.count_in_use(), 1); // only frame 0
        assert_eq!(bm.alloc(), Some(1)); // frame 1 is free
    }

    #[test]
    fn release_unreserved_reserves_by_base_pa_not_overlap() {
        // Non-frame-aligned end at 4097. Frame 0 (base 0) and frame 1
        // (base 4096) are both < 4097 → reserved. Frame 2 (base 8192)
        // is free, even though frame 1 straddles the boundary.
        let (mut storage, cap) = empty(4);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(&mut bm, 0, 4096, &[(0, 4097)]);
        assert_eq!(bm.count_in_use(), 2); // frames 0 and 1
        assert_eq!(bm.alloc(), Some(2)); // frame 2 is the lowest free
    }

    #[test]
    fn release_unreserved_honours_multiple_disjoint_ranges() {
        // Reserve frame 0 and frame 5; the other 8 are freed.
        let (mut storage, cap) = empty(10);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(&mut bm, 0, 4096, &[(0, 4096), (5 * 4096, 6 * 4096)]);
        assert_eq!(bm.count_free(), 8);
        assert_eq!(bm.alloc(), Some(1)); // frame 0 reserved, 1 is lowest free
    }

    #[test]
    fn release_unreserved_handles_sbi_kernel_dtb_shape() {
        // Realistic boot layout. ram_base below the kernel image leaves
        // an SBI hole; the kernel image and the DTB are carved out.
        let ram_base = 0x8000_0000;
        let frame = 4096;
        let kernel_start = ram_base + 2 * frame; // frames 0,1 are SBI
        let kernel_end = kernel_start + 2 * frame; // frames 2,3 are kernel
        let dtb_start = ram_base + 6 * frame; // frame 6 is DTB
        let dtb_end = dtb_start + frame;
        let (mut storage, cap) = empty(8);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(
            &mut bm,
            ram_base,
            frame,
            &[(0, kernel_start), (kernel_start, kernel_end), (dtb_start, dtb_end)],
        );
        // Reserved: frames 0,1 (SBI), 2,3 (kernel), 6 (DTB) = 5 frames.
        // Free: frames 4,5,7 = 3 frames.
        assert_eq!(bm.count_in_use(), 5);
        assert_eq!(bm.count_free(), 3);
        assert_eq!(bm.alloc(), Some(4)); // lowest free is past the kernel
    }

    #[test]
    fn release_unreserved_with_no_reserved_ranges_frees_everything() {
        let (mut storage, cap) = empty(16);
        let mut bm = Bitmap::new(&mut storage, cap);
        release_unreserved(&mut bm, 0x8000_0000, 4096, &[]);
        assert_eq!(bm.count_free(), 16);
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
