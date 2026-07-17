//! A fixed-capacity byte FIFO for console (UART) input — the Tier-0 polled-RX
//! ring. Pure bookkeeping, host-tested like [`crate::frame::Bitmap`] and
//! [`crate::sched::Runqueue`]; the kernel owns the live instance.
//!
//! **Why `&mut self` is enough (no atomics).** The kernel wraps this in a
//! `kernel::sync::Mutex` taken by *both* the timer-driven RX drain and the
//! `ConsoleRead` syscall. That's safe despite one being an IRQ path because both
//! run with `sstatus.SIE == 0` (traps mask interrupts; see the v0.8 lesson) — so
//! on one hart they're mutually exclusive in time (the timer can't fire while the
//! syscall holds the lock), and on SMP the spinlock briefly arbitrates. No
//! nested-IRQ re-entry, no allocation, no telemetry — so unlike the virtio TX
//! path, this lock is safe to take in `handle_timer`.
//!
//! **Drop-on-full.** A bounded ring never blocks the producer: when full, a new
//! byte is dropped rather than overwriting unread data. A slow consumer loses the
//! newest input, never corrupts the FIFO.
//!
//! See `plans/legacy/console-tier0-polled-rx.md`.

/// A byte FIFO of fixed capacity `N`. `head` is the next byte to read, `tail`
/// the next slot to write; `len` tracks occupancy so a full ring (`len == N`) is
/// unambiguous from an empty one (`len == 0`) even when `head == tail`.
pub struct ConsoleRing<const N: usize> {
    buf: [u8; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<const N: usize> ConsoleRing<N> {
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

    /// Number of bytes currently buffered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// No bytes buffered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// No free slots — the next [`push`](Self::push) will drop.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Append one byte. Returns `true` if stored, `false` if the ring was full
    /// (the byte is dropped — a bounded ring never blocks the producer).
    pub fn push(&mut self, byte: u8) -> bool {
        if self.is_full() {
            return false;
        }
        self.buf[self.tail] = byte;
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        true
    }

    /// Remove and return the oldest byte, or `None` if the ring is empty.
    pub fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let out = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(out)
    }
}

impl<const N: usize> Default for ConsoleRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_ring_is_empty() {
        let r = ConsoleRing::<4>::new();
        assert!(r.is_empty());
        assert!(!r.is_full());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn push_then_pop_returns_the_byte() {
        let mut r = ConsoleRing::<4>::new();
        assert!(r.push(b'x'));
        assert_eq!(r.len(), 1);
        assert_eq!(r.pop(), Some(b'x'));
        assert!(r.is_empty());
    }

    #[test]
    fn bytes_come_out_in_fifo_order() {
        let mut r = ConsoleRing::<4>::new();
        for b in [b'a', b'b', b'c'] {
            assert!(r.push(b));
        }
        assert_eq!(r.pop(), Some(b'a'));
        assert_eq!(r.pop(), Some(b'b'));
        assert_eq!(r.pop(), Some(b'c'));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn indices_wrap_around_the_buffer() {
        // Fill, drain partway, then refill across the `N` boundary.
        let mut r = ConsoleRing::<4>::new();
        for b in [b'1', b'2', b'3', b'4'] {
            assert!(r.push(b));
        }
        assert_eq!(r.pop(), Some(b'1'));
        assert_eq!(r.pop(), Some(b'2'));
        assert!(r.push(b'5')); // tail wraps past the end
        assert!(r.push(b'6'));
        assert_eq!(r.pop(), Some(b'3'));
        assert_eq!(r.pop(), Some(b'4'));
        assert_eq!(r.pop(), Some(b'5'));
        assert_eq!(r.pop(), Some(b'6'));
        assert!(r.is_empty());
    }

    #[test]
    fn push_into_a_full_ring_drops_and_reports_false() {
        let mut r = ConsoleRing::<2>::new();
        assert!(r.push(b'a'));
        assert!(r.push(b'b'));
        assert!(r.is_full());
        assert!(!r.push(b'c')); // dropped, not stored
        assert_eq!(r.len(), 2);
        // the dropped byte never displaces existing data
        assert_eq!(r.pop(), Some(b'a'));
        assert_eq!(r.pop(), Some(b'b'));
        assert_eq!(r.pop(), None);
    }

    #[test]
    fn pop_from_empty_is_none() {
        let mut r = ConsoleRing::<4>::new();
        assert_eq!(r.pop(), None);
    }
}
