//! Per-CPU storage stub. SMP-shaped today, single-hart in practice.
//!
//! When v0.5 spawns its scheduler, "current task" / "current
//! runqueue" / "preempt-disable counter" all want per-CPU semantics:
//! one logical slot per hart, read by whatever code happens to be
//! running on that hart. SMP isn't here yet, but the *call-site
//! syntax* is. Wrapping per-CPU access in this type from day one
//! makes the eventual SMP migration mechanical — bump `MAX_HARTS`,
//! implement `current_hartid()`, done.
//!
//! See `plans/v0.5-pre-smp-sync-prefactor.md`.

/// Maximum harts supported. Single-hart for v0.5; bumped when SMP
/// bring-up lands. The bound is compile-time so per-CPU arrays are
/// stack/`.bss`-allocatable.
pub const MAX_HARTS: usize = 1;

/// Container for "one value per hart." `T` should be `Sync` if any
/// hart can observe another hart's slot (which today never happens
/// because we're single-hart; document and revisit at SMP).
pub struct PerCpu<T> {
    cells: [T; MAX_HARTS],
}

impl<T> PerCpu<T> {
    /// Construct with one value per hart provided explicitly.
    pub const fn new(cells: [T; MAX_HARTS]) -> Self {
        Self { cells }
    }

    /// Read this hart's slot.
    pub fn this_cpu(&self) -> &T {
        &self.cells[current_hartid()]
    }

    /// Mutably access this hart's slot. Caller is responsible for
    /// not aliasing another hart's slot — today vacuous (single-hart).
    pub fn this_cpu_mut(&mut self) -> &mut T {
        &mut self.cells[current_hartid()]
    }
}

/// Current hart's ID.
///
/// v0.5: always 0. The single hart QEMU `virt` brings up has hartid 0
/// (assigned by SBI handoff). When SMP arrives this becomes a load
/// from `sscratch` (populated by trap entry to point at per-hart
/// state) or a direct `mhartid` read via SBI delegate — decision
/// deferred to the actual SMP bring-up.
#[inline]
pub fn current_hartid() -> usize {
    0
}
