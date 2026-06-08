//! Batched single-producer / single-consumer ring — pure, no `unsafe`.
//!
//! The point (v0.6 step 12, third variant): amortize the cross-hart
//! Release/Acquire fence over a whole batch the way the `Mutex` does,
//! but without a lock. `heapless::spsc` fences *per item* (a Release
//! store of `tail` on every `enqueue`); this fences *per batch* — the
//! per-slot writes are `Relaxed` and one `Release` store of `tail`
//! publishes the lot.
//!
//! Buffer is `[AtomicU64; CAP]` rather than `UnsafeCell` so the whole
//! thing stays safe and host-testable. SPSC discipline (single writer
//! of `tail`, single writer of `head`) is the caller's contract.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Power-of-two-capacity SPSC ring. `tail` (producer) and `head`
/// (consumer) are monotonic counters; the live count is
/// `tail - head ≤ CAP`. `CAP` must be a power of two (masked indexing).
pub struct BatchRing<const CAP: usize> {
    buf: [AtomicU64; CAP],
    head: AtomicUsize,
    tail: AtomicUsize,
}

impl<const CAP: usize> Default for BatchRing<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> BatchRing<CAP> {
    pub const fn new() -> Self {
        assert!(CAP.is_power_of_two(), "BatchRing CAP must be a power of two");
        Self {
            buf: [const { AtomicU64::new(0) }; CAP],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Producer side. Copy as many of `items` as fit into the ring;
    /// return how many were enqueued. One `Release` store of `tail`
    /// publishes all of them.
    pub fn enqueue_batch(&self, items: &[u64]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed); // producer owns tail
        let head = self.head.load(Ordering::Acquire);
        let free = CAP - tail.wrapping_sub(head);
        let n = items.len().min(free);
        for (i, &item) in items.iter().take(n).enumerate() {
            self.buf[tail.wrapping_add(i) & (CAP - 1)].store(item, Ordering::Relaxed);
        }
        // One Release publishes all the slot writes above to a consumer
        // that Acquire-loads `tail`.
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// Consumer side. Fill as much of `out` as the ring holds; return
    /// how many were dequeued.
    pub fn dequeue_batch(&self, out: &mut [u64]) -> usize {
        let head = self.head.load(Ordering::Relaxed); // consumer owns head
        let tail = self.tail.load(Ordering::Acquire);
        let avail = tail.wrapping_sub(head);
        let n = out.len().min(avail);
        for (i, slot) in out.iter_mut().take(n).enumerate() {
            *slot = self.buf[head.wrapping_add(i) & (CAP - 1)].load(Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ring_dequeues_nothing() {
        let r = BatchRing::<4>::new();
        let mut out = [0u64; 4];
        assert_eq!(r.dequeue_batch(&mut out), 0);
    }

    #[test]
    fn fifo_order_preserved() {
        let r = BatchRing::<4>::new();
        assert_eq!(r.enqueue_batch(&[10, 20, 30]), 3);
        let mut out = [0u64; 4];
        assert_eq!(r.dequeue_batch(&mut out), 3);
        assert_eq!(&out[..3], &[10, 20, 30]);
    }

    #[test]
    fn enqueue_caps_at_capacity() {
        let r = BatchRing::<4>::new();
        // Only 4 fit; the 5th is rejected (backpressure).
        assert_eq!(r.enqueue_batch(&[1, 2, 3, 4, 5]), 4);
        let mut out = [0u64; 8];
        assert_eq!(r.dequeue_batch(&mut out), 4);
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
    }

    #[test]
    fn enqueue_respects_existing_occupancy() {
        // Kills the `CAP - (tail-head)` free-space mutant: with the ring
        // already half-full, only the *remaining* room accepts items.
        let r = BatchRing::<4>::new();
        assert_eq!(r.enqueue_batch(&[1, 2]), 2);
        // 2 free now; offering 3 enqueues only 2 (not 3).
        assert_eq!(r.enqueue_batch(&[3, 4, 5]), 2);
        let mut out = [0u64; 8];
        assert_eq!(r.dequeue_batch(&mut out), 4);
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
    }

    #[test]
    fn partial_dequeue_then_rest() {
        let r = BatchRing::<4>::new();
        assert_eq!(r.enqueue_batch(&[1, 2, 3]), 3);
        let mut out = [0u64; 2];
        assert_eq!(r.dequeue_batch(&mut out), 2);
        assert_eq!(out, [1, 2]);
        assert_eq!(r.dequeue_batch(&mut out), 1);
        assert_eq!(out[0], 3);
    }

    #[test]
    fn survives_wraparound() {
        // Cycle well past CAP to exercise index wrap; order must hold.
        let r = BatchRing::<4>::new();
        let mut next = 0u64;
        let mut expect = 0u64;
        for _ in 0..1000 {
            let batch = [next, next + 1, next + 2];
            let pushed = r.enqueue_batch(&batch);
            next += pushed as u64;
            let mut out = [0u64; 3];
            let got = r.dequeue_batch(&mut out);
            for &v in &out[..got] {
                assert_eq!(v, expect);
                expect += 1;
            }
        }
        assert!(expect > 100, "should have moved many items, got {expect}");
    }
}
