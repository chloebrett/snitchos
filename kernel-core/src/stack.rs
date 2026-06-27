//! Kernel-stack overflow detection (guard-pages Tier A — cheap, no MMU).
//!
//! Each kernel stack is filled with a [`SENTINEL`] byte at creation. Two pure
//! checks read it back:
//! - [`canary_intact`] reads the bottom [`CANARY_BYTES`] (the lowest addresses) —
//!   the stack grows *down* toward them, so an overflow clobbers them first. The
//!   scheduler checks this on every context switch and **panics naming the task**
//!   if breached: a stack overflow becomes a named fault, not a mysterious
//!   corruption surfacing at an unrelated victim.
//! - [`high_water_bytes`] scans the whole stack for the deepest address ever
//!   written (the lowest-index non-sentinel byte) → bytes used. The heartbeat
//!   emits it as a per-task gauge so a stack creeping toward its limit is visible
//!   *before* it blows.
//!
//! Detection only — Tier B (guard pages, fault-on-overflow) is the real fix.
//! Pure logic, host-tested here; the `kernel` side owns the `Stack` bytes.

/// The fill byte. `0xC3` is `ret` on RISC-V-as-bytes coincidence-aside just a
/// recognizable non-zero pattern unlikely to dominate live stack data; the
/// high-water scan stops at the *first* non-sentinel from the bottom, so a
/// coincidental `0xC3` in the used region above the watermark is harmless.
pub const SENTINEL: u8 = 0xC3;

/// How many bottom bytes form the canary checked on every switch. 16 keeps the
/// per-switch check a single cache line while still catching an overflow that
/// reaches the stack's lowest words.
pub const CANARY_BYTES: usize = 16;

/// Whether the canary at the stack bottom is still all-[`SENTINEL`] — `false`
/// means the stack grew down into (or past) its lowest bytes, i.e. it overflowed.
/// `bottom` is the lowest [`CANARY_BYTES`] of the stack region.
#[must_use]
pub fn canary_intact(bottom: &[u8]) -> bool {
    bottom.iter().all(|&b| b == SENTINEL)
}

/// Bytes ever used by the stack: the distance from the deepest written address
/// (the lowest-index non-[`SENTINEL`] byte) up to the top. `stack[0]` is the
/// lowest address (bottom); the stack grows down from the top, so the untouched
/// region is a sentinel prefix and the first non-sentinel byte is the watermark.
/// `stack.len()` means the bottom byte itself was written — a full overflow.
#[must_use]
pub fn high_water_bytes(stack: &[u8]) -> usize {
    let watermark = stack.iter().position(|&b| b != SENTINEL).unwrap_or(stack.len());
    stack.len() - watermark
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_all_sentinel_canary_is_intact() {
        assert!(canary_intact(&[SENTINEL; CANARY_BYTES]));
    }

    #[test]
    fn a_single_clobbered_canary_byte_breaks_it() {
        let mut bottom = [SENTINEL; CANARY_BYTES];
        bottom[CANARY_BYTES - 1] = 0x00; // a store reached the lowest words
        assert!(!canary_intact(&bottom));
    }

    #[test]
    fn an_untouched_stack_reports_zero_bytes_used() {
        let stack = [SENTINEL; 64];
        assert_eq!(high_water_bytes(&stack), 0);
    }

    #[test]
    fn high_water_is_the_distance_from_the_deepest_write_to_the_top() {
        // Bottom 40 bytes untouched (sentinel), top 24 written → 24 used.
        let mut stack = [SENTINEL; 64];
        for b in &mut stack[40..] {
            *b = 0xAB;
        }
        assert_eq!(high_water_bytes(&stack), 24);
    }

    #[test]
    fn a_clobbered_bottom_byte_reports_a_full_overflow() {
        let mut stack = [SENTINEL; 64];
        stack[0] = 0x00;
        assert_eq!(high_water_bytes(&stack), 64);
    }

    #[test]
    fn the_scan_takes_the_first_non_sentinel_ignoring_coincidental_ones_above() {
        // A `SENTINEL`-valued byte sits in the *used* region (index 2), but the
        // watermark is the first non-sentinel from the bottom (index 1).
        let stack = [SENTINEL, 0xAB, SENTINEL, 0xAB];
        assert_eq!(high_water_bytes(&stack), 4 - 1);
    }
}
