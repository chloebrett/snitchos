//! Kernel-stack overflow protection + high-water gauge.
//!
//! **Overflow protection is guard pages (the real fix).** This module owns the
//! kernel-stack **window** layout + a slot allocator (pure bookkeeping). The kernel
//! maps each slot's stack pages, leaves the page below unmapped, and frees the slot
//! on task exit; an overflow store hits the guard hole and faults at the exact PC.
//! The trap handler uses [`guard_slot_for`] to name the fault, and the per-hart
//! exception stack lets it report cleanly even for a deep overflow.
//!
//! **High-water gauge (proactive telemetry).** Each stack is filled with a
//! [`SENTINEL`] byte at creation; [`high_water_bytes`] scans for the deepest
//! address ever written (the lowest-index non-sentinel byte) → bytes used. The
//! heartbeat emits it per task so a stack creeping toward its limit is visible
//! *before* it blows — independent of the binary guard page.
//!
//! Pure logic, host-tested here; the `kernel` side owns the stack bytes + MMU.
//! (The earlier Tier-A bottom-canary panic was retired once guard pages report
//! cleanly — see `plans/legacy/kernel-stack-hardening.md`.)

use alloc::vec::Vec;

use kernel_mem::mmu::PAGE_SIZE;

/// Bytes per kernel stack (the mapped region of a slot). 16 KiB.
pub const STACK_BYTES: usize = 16384;

/// Base VA of the kernel-stack window: root PTE 257, immediately above the heap's
/// full 1 GiB slot (`HEAP_VA_BASE`, root 256). One dedicated 1 GiB window, so a
/// stack VA can never collide with the heap or linear map.
pub const KSTACK_VA_BASE: usize = 0xffff_ffc0_4000_0000;

/// Window size — one root-PTE slot = 1 GiB.
pub const KSTACK_WINDOW_BYTES: usize = 1 << 30;

/// Guard pages below each stack (left unmapped). One 4 KiB hole is enough: a
/// downward overflow store crosses the stack's lowest page boundary into it.
pub const GUARD_PAGES: usize = 1;

/// Mapped stack pages per slot.
pub const STACK_PAGES: usize = STACK_BYTES / PAGE_SIZE;

/// VA stride between consecutive slots: guard page + mapped stack, no padding.
pub const SLOT_STRIDE: usize = (GUARD_PAGES + STACK_PAGES) * PAGE_SIZE;

/// Stacks the window holds — its capacity.
pub const MAX_SLOTS: usize = KSTACK_WINDOW_BYTES / SLOT_STRIDE;

/// Base VA of slot `slot` — the first byte of its (unmapped) guard page.
#[must_use]
pub const fn slot_base_va(slot: usize) -> usize {
    KSTACK_VA_BASE + slot * SLOT_STRIDE
}

/// Lowest **mapped** byte of slot `slot`'s stack — the page just above the guard.
/// The kernel maps `STACK_PAGES` pages starting here.
#[must_use]
pub const fn slot_stack_base_va(slot: usize) -> usize {
    slot_base_va(slot) + GUARD_PAGES * PAGE_SIZE
}

/// Initial `sp` for slot `slot`: one past its highest stack byte (stacks grow
/// down from here, 16-byte aligned since `STACK_BYTES` is a page multiple).
#[must_use]
pub const fn slot_stack_top_va(slot: usize) -> usize {
    slot_stack_base_va(slot) + STACK_BYTES
}

/// If `va` falls in some slot's guard page, that slot index — the trap handler
/// uses it to name a kernel-stack overflow. `None` if `va` is outside the window
/// or in a mapped stack region (a real fault elsewhere, not a guard hit).
#[must_use]
pub fn guard_slot_for(va: usize) -> Option<usize> {
    if !(KSTACK_VA_BASE..KSTACK_VA_BASE + KSTACK_WINDOW_BYTES).contains(&va) {
        return None;
    }
    let off = va - KSTACK_VA_BASE;
    ((off % SLOT_STRIDE) < GUARD_PAGES * PAGE_SIZE).then_some(off / SLOT_STRIDE)
}

/// Allocator for kernel-stack slots in the window: hands out fresh indices up to
/// [`MAX_SLOTS`] and **recycles** freed ones first, so a long-running spawner (the
/// shell) reuses slots instead of exhausting the window. Pure bookkeeping; the
/// kernel pairs each `alloc`/`free` with the map/unmap of the slot's pages.
#[derive(Debug, Default)]
pub struct SlotAllocator {
    next: usize,
    freed: Vec<usize>,
}

impl SlotAllocator {
    #[must_use]
    pub const fn new() -> Self {
        Self { next: 0, freed: Vec::new() }
    }

    /// A free slot index — a recycled one if any, else the next fresh index.
    /// `None` once the window is exhausted.
    pub fn alloc(&mut self) -> Option<usize> {
        if let Some(slot) = self.freed.pop() {
            return Some(slot);
        }
        (self.next < MAX_SLOTS).then(|| {
            let slot = self.next;
            self.next += 1;
            slot
        })
    }

    /// Return `slot` for reuse. The caller guarantees its pages are already
    /// unmapped and nothing references the stack.
    pub fn free(&mut self, slot: usize) {
        self.freed.push(slot);
    }
}

/// The fill byte. `0xC3` is `ret` on RISC-V-as-bytes coincidence-aside just a
/// recognizable non-zero pattern unlikely to dominate live stack data; the
/// high-water scan stops at the *first* non-sentinel from the bottom, so a
/// coincidental `0xC3` in the used region above the watermark is harmless.
pub const SENTINEL: u8 = 0xC3;

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
    fn slot_allocator_hands_out_distinct_rising_indices() {
        let mut slots = SlotAllocator::new();
        assert_eq!(slots.alloc(), Some(0));
        assert_eq!(slots.alloc(), Some(1));
        assert_eq!(slots.alloc(), Some(2));
    }

    #[test]
    fn a_freed_slot_is_recycled_before_a_fresh_one() {
        // The shell spawns repeatedly; without recycling the 1 GiB window would
        // exhaust. A freed slot is reused before the high-water advances.
        let mut slots = SlotAllocator::new();
        let _zero = slots.alloc();
        let one = slots.alloc().expect("under capacity");
        slots.free(one);
        assert_eq!(slots.alloc(), Some(one), "the freed slot is reused");
        assert_eq!(slots.alloc(), Some(2), "then the high-water resumes");
    }

    #[test]
    fn allocation_fails_once_the_window_is_exhausted() {
        let mut slots = SlotAllocator::new();
        for i in 0..MAX_SLOTS {
            assert_eq!(slots.alloc(), Some(i), "slot {i} is within the window");
        }
        assert_eq!(slots.alloc(), None, "one past the window is refused");
    }

    #[test]
    fn a_slots_stack_sits_one_guard_page_above_its_base() {
        // Layout: [guard page][stack STACK_BYTES]. sp starts one past the top.
        assert_eq!(slot_stack_base_va(0), KSTACK_VA_BASE + PAGE_SIZE);
        assert_eq!(slot_stack_top_va(0), KSTACK_VA_BASE + PAGE_SIZE + STACK_BYTES);
        // Slot 1 begins exactly one stride up.
        assert_eq!(slot_base_va(1), KSTACK_VA_BASE + SLOT_STRIDE);
    }

    #[test]
    fn an_address_in_a_guard_page_maps_to_its_slot() {
        // The trap handler asks "is this fault VA a guard page?" — the first byte
        // of slot 0 and slot 3's guard pages both resolve to their slot.
        assert_eq!(guard_slot_for(KSTACK_VA_BASE), Some(0));
        assert_eq!(guard_slot_for(KSTACK_VA_BASE + PAGE_SIZE - 1), Some(0));
        assert_eq!(guard_slot_for(slot_base_va(3)), Some(3));
    }

    #[test]
    fn an_address_in_a_mapped_stack_region_is_not_a_guard_hit() {
        // The lowest mapped stack byte (just above the guard) is a normal page,
        // not a guard — a fault there is a real bug, not an overflow sentinel.
        assert_eq!(guard_slot_for(slot_stack_base_va(0)), None);
        assert_eq!(guard_slot_for(slot_stack_top_va(0) - 1), None);
    }

    #[test]
    fn addresses_outside_the_window_are_never_guard_hits() {
        assert_eq!(guard_slot_for(KSTACK_VA_BASE - 1), None);
        assert_eq!(guard_slot_for(KSTACK_VA_BASE + KSTACK_WINDOW_BYTES), None);
        assert_eq!(guard_slot_for(0), None);
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
