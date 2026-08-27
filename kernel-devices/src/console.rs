//! A fixed-capacity byte FIFO for console (UART) input — the Tier-0 polled-RX
//! ring. Pure bookkeeping, host-tested like `kernel_mem::frame::Bitmap` and
//! `kernel_proc::sched::Runqueue`; the kernel owns the live instance. (Plain
//! code spans, not intra-doc links: this crate deliberately depends on neither.)
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

/// The console line that asks the kernel to reboot itself via SBI SRST.
///
/// Newline-terminated on purpose: it is a *committed line*, not a prefix someone
/// is halfway through typing. The leading run of `~` cannot begin a Stitch
/// expression, so the token cannot be reached by editing real source.
///
/// **Unauthenticated by construction** — anyone holding the UART can reboot the
/// board. Correct for a bench dev board, and recorded here so it is a decision
/// rather than an oversight. See `plans/board-bridge.md` Step 5.
pub const REBOOT_TOKEN: &[u8] = b"~~~reboot\n";

/// Watches a console byte stream for [`REBOOT_TOKEN`], across arbitrary read
/// boundaries.
///
/// **Why a sliding window and not a match cursor.** The obvious implementation
/// keeps "how many bytes of the token match so far" and resets it to zero on a
/// mismatch. That is wrong whenever the token's opening bytes repeat: given
/// `~~~~reboot\n`, the fourth `~` fails to match `r`, the cursor resets, and the
/// `~` that *begins* the real token is consumed by the reset rather than
/// re-examined. The window compares the last `N` bytes seen and so cannot have
/// that bug — no fallback table, and correctness that is obvious by inspection.
///
/// Ten bytes of state; this runs in the RX interrupt path.
pub struct RebootDetector {
    window: [u8; REBOOT_TOKEN.len()],
    /// Bytes seen so far, saturating at the window size — below it the window is
    /// not yet meaningful and must never compare equal.
    seen: usize,
}

impl RebootDetector {
    #[must_use]
    pub const fn new() -> Self {
        Self { window: [0; REBOOT_TOKEN.len()], seen: 0 }
    }

    /// Feed freshly-read console bytes. Returns `true` on the read that completes
    /// the token.
    ///
    /// Scans the whole slice rather than returning early, so the window is left
    /// consistent with every byte the console actually received.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        let mut hit = false;
        for &byte in bytes {
            self.window.rotate_left(1);
            self.window[REBOOT_TOKEN.len() - 1] = byte;
            self.seen = self.seen.saturating_add(1);
            if self.seen >= REBOOT_TOKEN.len() && self.window == REBOOT_TOKEN {
                hit = true;
            }
        }
        hit
    }
}

impl Default for RebootDetector {
    fn default() -> Self {
        Self::new()
    }
}

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

    /// Append every byte, or none of them. Returns `false` (storing nothing) if
    /// they don't all fit.
    ///
    /// **Why all-or-nothing, when [`push`](Self::push) drops freely.** Telemetry
    /// on this ring is a COBS stream delimited by `0x00`. A half-written frame is
    /// not a lost frame but a *corrupt* one: the host decoder cannot tell the
    /// truncation from data, so it discards everything up to the next delimiter
    /// and takes a healthy frame down with it. Refusing whole frames keeps the
    /// stream self-describing under back-pressure, which is what makes a dropped
    /// frame merely absent rather than damaging.
    ///
    /// The free-space test is written as a comparison rather than a subtraction
    /// on purpose — `N - self.len` is safe here (`len <= N` always), but the
    /// mirror-image `bytes.len() - free` form underflows for a run larger than
    /// the ring and silently reports space that does not exist.
    pub fn push_all(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > N - self.len {
            return false;
        }
        for &byte in bytes {
            let stored = self.push(byte);
            debug_assert!(stored, "push_all checked space for every byte up front");
        }
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

    /// The happy path: the whole token arrives in one read.
    #[test]
    fn the_reboot_token_is_recognised_in_a_single_read() {
        let mut d = RebootDetector::new();
        assert!(d.feed(REBOOT_TOKEN));
    }

    /// **The case that matters.** The RX ring hands over whatever arrived, so a
    /// token typed at a terminal almost never lands in one read — it arrives a
    /// keystroke at a time. A detector that only matches a contiguous buffer
    /// passes every casual test and never fires in practice.
    #[test]
    fn the_reboot_token_is_recognised_when_split_across_reads() {
        let mut d = RebootDetector::new();
        let (head, tail) = REBOOT_TOKEN.split_at(4);
        assert!(!d.feed(head), "not yet — only part of the token has arrived");
        assert!(d.feed(tail));
    }

    /// One byte per read is the terminal-typing case, and the extreme of the split.
    #[test]
    fn the_reboot_token_is_recognised_one_byte_at_a_time() {
        let mut d = RebootDetector::new();
        let (last, rest) = REBOOT_TOKEN.split_last().expect("token is non-empty");
        for &byte in rest {
            assert!(!d.feed(&[byte]));
        }
        assert!(d.feed(&[*last]));
    }

    /// Ordinary console traffic must never reboot the board.
    #[test]
    fn ordinary_input_does_not_trigger_a_reboot() {
        let mut d = RebootDetector::new();
        assert!(!d.feed(b"let x = 1\nprint(x)\n"));
        assert!(!d.feed(b"reboot\n"), "the bare word is not the token");
        assert!(!d.feed(b"~~~reboot"), "no trailing newline — not a committed line");
    }

    /// A near-miss must not leave the detector primed. Without this, a partial
    /// match followed by unrelated input could complete a token that was never
    /// typed.
    #[test]
    fn an_aborted_token_does_not_prime_the_detector() {
        let mut d = RebootDetector::new();
        assert!(!d.feed(b"~~~rebo"));
        // Unrelated bytes land in between: the prefix is abandoned, not merely
        // paused. (Contrast the split-read test, where the halves are adjacent in
        // the stream and *must* match.)
        assert!(!d.feed(b"XYZ"));
        assert!(!d.feed(b"ot\n"), "the abandoned prefix must not complete");
    }

    /// Overlap: the token's own opening bytes repeat, so a run of them followed by
    /// a real token must still match. A detector that resets its match position to
    /// zero on mismatch — the obvious implementation — drops the `~` that begins
    /// the true token and misses this.
    #[test]
    fn a_run_of_leading_bytes_before_the_token_still_matches() {
        let mut d = RebootDetector::new();
        assert!(d.feed(b"~~~~~~reboot\n"));
    }

    /// The token buried in a longer read, with traffic on both sides.
    #[test]
    fn the_reboot_token_is_found_amid_surrounding_traffic() {
        let mut d = RebootDetector::new();
        assert!(d.feed(b"noise before ~~~reboot\nand after"));
    }

    /// A whole encoded frame goes in, or none of it does. Telemetry is a COBS
    /// stream delimited by `0x00`: half a frame in the ring is not a lost frame,
    /// it is a *corrupt* one, and the host decoder resynchronises by discarding
    /// whatever follows until the next delimiter. So partial writes cost more
    /// than the frame they truncate.
    #[test]
    fn push_all_stores_every_byte_when_they_fit() {
        let mut r = ConsoleRing::<4>::new();
        assert!(r.push_all(&[b'a', b'b', b'c']));
        assert_eq!(r.len(), 3);
        assert_eq!(r.pop(), Some(b'a'));
        assert_eq!(r.pop(), Some(b'b'));
        assert_eq!(r.pop(), Some(b'c'));
    }

    #[test]
    fn push_all_stores_nothing_when_they_do_not_all_fit() {
        let mut r = ConsoleRing::<4>::new();
        assert!(r.push(b'x'));
        assert!(r.push(b'y'));
        // Two free slots, three bytes offered: refuse, and leave the ring alone.
        assert!(!r.push_all(&[b'a', b'b', b'c']));
        assert_eq!(r.len(), 2);
        assert_eq!(r.pop(), Some(b'x'));
        assert_eq!(r.pop(), Some(b'y'));
        assert_eq!(r.pop(), None);
    }

    /// The boundary case, called out because an off-by-one here is invisible in
    /// the common path: a frame that exactly fills the remaining space must be
    /// accepted, not refused.
    #[test]
    fn push_all_accepts_a_run_that_exactly_fills_the_ring() {
        let mut r = ConsoleRing::<4>::new();
        assert!(r.push_all(&[b'1', b'2', b'3', b'4']));
        assert!(r.is_full());
        assert_eq!(r.len(), 4);
    }

    /// Refusing must not consume, even when the run is longer than the whole
    /// ring — the "too large to ever fit" case, which a naive free-space check
    /// written as a subtraction gets wrong by underflowing.
    #[test]
    fn push_all_refuses_a_run_larger_than_capacity() {
        let mut r = ConsoleRing::<2>::new();
        assert!(!r.push_all(&[b'a', b'b', b'c']));
        assert!(r.is_empty());
    }

    /// Empty input is trivially satisfiable and must not report failure — a
    /// caller counting drops would otherwise count one for a frame that never
    /// existed.
    #[test]
    fn push_all_of_nothing_succeeds() {
        let mut r = ConsoleRing::<2>::new();
        assert!(r.push_all(&[]));
        assert!(r.is_empty());
    }

    /// All-or-nothing must hold across the wrap boundary too: free space is not
    /// contiguous there, so a check that reasons about `tail`'s distance to the
    /// end of the buffer rather than about occupancy will refuse a run that fits.
    #[test]
    fn push_all_fits_across_the_wrap_boundary() {
        let mut r = ConsoleRing::<4>::new();
        assert!(r.push_all(&[b'1', b'2', b'3']));
        assert_eq!(r.pop(), Some(b'1'));
        assert_eq!(r.pop(), Some(b'2'));
        // Two free slots, but they straddle the end of the backing array.
        assert!(r.push_all(&[b'4', b'5']));
        assert_eq!(r.pop(), Some(b'3'));
        assert_eq!(r.pop(), Some(b'4'));
        assert_eq!(r.pop(), Some(b'5'));
        assert!(r.is_empty());
    }
}
