//! A fixed-capacity FIFO of signed-PCM (`i16`) samples — the async audio ring the
//! `glitch` server fills and the timer-driven DAC drain empties. Pure bookkeeping,
//! host-tested like `console::ConsoleRing` and `kernel_mem::frame::Bitmap`; the
//! kernel owns the live instance. (Plain code spans, not intra-doc links: this crate
//! deliberately depends on neither.)
//!
//! **Back-pressure, not drop-on-full.** Unlike `ConsoleRing` (which drops the newest
//! byte when full so a slow *consumer* never corrupts input), the producer here is a
//! userspace server that can be told to wait: [`push_slice`](SampleRing::push_slice)
//! stores as many leading samples as fit and returns the accepted count, so the
//! caller re-submits the tail on the next turn. A full ring accepts 0 — the signal to
//! back off. This is what decouples glitch's chunky refills from the steady per-sample
//! drain (and is where an underrun becomes observable: the drain finding this empty
//! mid-stream is the `XRun`).
//!
//! **Why `&mut self` is enough (no atomics).** Same discipline as `ConsoleRing`: the
//! kernel wraps this in a `kernel::sync::Mutex` taken by both the timer drain and the
//! enqueue syscall, both running with `sstatus.SIE == 0`, so they are mutually
//! exclusive in time on one hart and briefly arbitrated by the spinlock on SMP.
//!
//! See `plans/glitch-v2-async-ring.md`.

/// A FIFO of at most `N` signed-PCM samples. `head` is the next sample to drain,
/// `tail` the next slot to fill; `len` tracks occupancy so a full ring (`len == N`)
/// is unambiguous from an empty one (`len == 0`) even when `head == tail`.
pub struct SampleRing<const N: usize> {
    buf: [i16; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<const N: usize> SampleRing<N> {
    /// A fresh, empty ring.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// Number of samples currently buffered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Total capacity in samples.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// No samples buffered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// No free slots — the next [`push_slice`](Self::push_slice) accepts 0.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Store as many leading samples of `samples` as fit; return the count accepted.
    /// A full ring accepts 0 — the caller re-submits the unaccepted tail (back-pressure).
    pub fn push_slice(&mut self, samples: &[i16]) -> usize {
        let accepted = samples.len().min(N - self.len);
        for &s in &samples[..accepted] {
            self.buf[self.tail] = s;
            self.tail = (self.tail + 1) % N;
        }
        self.len += accepted;
        accepted
    }

    /// Remove and return the oldest sample, or `None` if the ring is empty.
    pub fn pop(&mut self) -> Option<i16> {
        if self.is_empty() {
            return None;
        }
        let out = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(out)
    }
}

impl<const N: usize> Default for SampleRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_ring_is_empty() {
        let r = SampleRing::<4>::new();
        assert!(r.is_empty());
        assert!(!r.is_full());
        assert_eq!(r.len(), 0);
        assert_eq!(r.capacity(), 4);
    }

    #[test]
    fn push_slice_accepts_all_when_there_is_room() {
        let mut r = SampleRing::<4>::new();
        assert_eq!(r.push_slice(&[10, 20, 30]), 3);
        assert_eq!(r.len(), 3);
        assert!(!r.is_full());
    }

    #[test]
    fn push_slice_accepts_only_the_remaining_capacity() {
        let mut r = SampleRing::<4>::new();
        assert_eq!(r.push_slice(&[1, 2, 3]), 3);
        // only one slot left, so only the first of the next three is taken
        assert_eq!(r.push_slice(&[4, 5, 6]), 1);
        assert_eq!(r.len(), 4);
        assert!(r.is_full());
    }

    #[test]
    fn push_slice_into_a_full_ring_accepts_zero_and_preserves_data() {
        let mut r = SampleRing::<2>::new();
        assert_eq!(r.push_slice(&[7, 8]), 2);
        assert!(r.is_full());
        assert_eq!(r.push_slice(&[9]), 0); // back-pressure: nothing taken
        // the rejected sample never displaces existing data
        assert_eq!(r.pop(), Some(7));
        assert_eq!(r.pop(), Some(8));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn samples_drain_in_fifo_order() {
        let mut r = SampleRing::<4>::new();
        assert_eq!(r.push_slice(&[-1, 0, 32767]), 3);
        assert_eq!(r.pop(), Some(-1));
        assert_eq!(r.pop(), Some(0));
        assert_eq!(r.pop(), Some(32767));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn indices_wrap_around_the_buffer() {
        // Fill, drain partway, then refill across the `N` boundary.
        let mut r = SampleRing::<4>::new();
        assert_eq!(r.push_slice(&[1, 2, 3, 4]), 4);
        assert_eq!(r.pop(), Some(1));
        assert_eq!(r.pop(), Some(2));
        assert_eq!(r.push_slice(&[5, 6]), 2); // tail wraps past the end
        assert_eq!(r.pop(), Some(3));
        assert_eq!(r.pop(), Some(4));
        assert_eq!(r.pop(), Some(5));
        assert_eq!(r.pop(), Some(6));
        assert!(r.is_empty());
    }

    #[test]
    fn pop_from_empty_is_none() {
        let mut r = SampleRing::<4>::new();
        assert_eq!(r.pop(), None);
    }
}
