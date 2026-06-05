//! Per-CPU storage. SMP-shaped, used single-hart in practice through
//! v0.6 step 10; bumps to real multi-hart in step 8.
//!
//! Two abstractions live here:
//!
//!   - `PerHartData`: a fixed per-hart struct, statically allocated in
//!     `PER_HART_DATA[MAX_HARTS]`. The RISC-V `tp` register points at
//!     this hart's slot; `current_hartid()` reads the `hart_id` field
//!     through `tp`. Future per-CPU state (current task, current span
//!     cursor, IPI pending bits, shootdown ack atomics) joins this
//!     struct in subsequent v0.6 steps.
//!
//!   - `PerCpu<T>`: a generic "one value per hart" wrapper for
//!     statics that don't fit naturally into `PerHartData`. Indexes
//!     into its internal `[T; MAX_HARTS]` using `current_hartid()`.
//!
//! Cacheline alignment on `PerHartData` (64 bytes) prevents adjacent
//! harts' fields from sharing a line under SMP — false sharing would
//! make every write on hart 0 invalidate hart 1's cache copy of an
//! unrelated field.
//!
//! ## Memory ordering discipline (the home doc)
//!
//! Existing kernel atomics fall into four classes:
//!
//!   - **Pure counters** (`fetch_add` on counts, `store` on
//!     last-value snapshots): `Relaxed`. The atomic *is* the
//!     shared data; there is no payload to publish, so no
//!     happens-before is needed.
//!   - **Per-CPU statics** (anything wrapped in `PerCpu<T>`): each
//!     hart only touches its own slot, so there is no cross-hart
//!     observer to order against. `Relaxed`.
//!   - **Same-CPU IRQ handoff** (timer ISR → resumed thread on the
//!     same hart): trap-return synchronises the handler's memory
//!     ops with the trapped thread by hardware. `Relaxed` is
//!     correct *because both ends are on the same hart*.
//!   - **Init-once** (config written at boot, read forever): no
//!     ongoing synchronisation. `Relaxed`.
//!
//! v0.6 steps 7+ introduce three patterns that *are* genuine cross-
//! hart synchronisation and require Release/Acquire:
//!
//!   - **IPI pending bits.** Sender stores payload then sets the
//!     bit with `Release`; target's IPI handler reads the bit with
//!     `Acquire` before reading the payload. Without this, target
//!     can observe the bit set but the payload still stale.
//!   - **TLB shootdown ack.** Target completes `sfence.vma`,
//!     stores `shootdown_ack[hartid]` with `Release`; initiator's
//!     spin-wait loads with `Acquire` to know the sfence happened.
//!   - **Cross-hart task wake** (`spawn_on(hart, ...)`): the target
//!     hart's runqueue mutex provides the synchronisation for the
//!     queue contents; the IPI's `Release` store on
//!     `IPI_PENDING |= Wakeup` is all that publishes "look at your
//!     queue."
//!
//! These don't exist yet. When they land in step 7, the orderings
//! above are the contract.
//!
//! See `plans/v0.6-smp-cooperative.md`.

use core::arch::asm;

/// Maximum harts supported. Single-hart through v0.6 step 10; the
/// constant bumps when secondary-hart bring-up lands.
pub const MAX_HARTS: usize = 1;

/// Per-hart bookkeeping. `tp` register points at this hart's slot in
/// `PER_HART_DATA`. New fields land here as v0.6 steps progress —
/// `current_task`, `current_span_cursor`, `ipi_pending`,
/// `shootdown_ack`. Adding a field doesn't touch any call site
/// because access goes through `&PER_HART_DATA[hartid]`.
///
/// `#[repr(C)]` pins layout so the `tp`-relative read of `hart_id` at
/// offset 0 is stable. `align(64)` keeps two harts' slots on separate
/// cache lines so under SMP a write on hart 0 doesn't invalidate
/// hart 1's cache of an unrelated field.
#[repr(C, align(64))]
pub struct PerHartData {
    /// Dense logical id `0..MAX_HARTS`. Read by `current_hartid()`
    /// via `tp`. Initialised once in `init()`.
    pub hart_id: u32,
}

/// One slot per hart. Statically initialised to `hart_id = i` so a
/// secondary hart starting cold (before its `init()` runs) at least
/// sees a stable value at its slot.
pub static PER_HART_DATA: [PerHartData; MAX_HARTS] = [PerHartData { hart_id: 0 }];

/// Initialise this hart's per-CPU context. Sets `tp` to point at this
/// hart's `PER_HART_DATA` slot so subsequent `current_hartid()` calls
/// read through it.
///
/// **Call once per hart, very early.** On the boot hart this must run
/// after the trampoline (the static's address is a higher-half VA) and
/// before any code that calls `current_hartid()` — which today means
/// before the first `span!` emission, since `tracing::span_start`
/// reads `current_hartid()` to populate `hart_id` on the SpanStart
/// frame.
///
/// # Safety
///
/// Caller must hold the "called once per hart, after MMU on, before
/// any per-hart-aware code" invariant. Calling twice is harmless
/// (tp gets the same value); calling pre-MMU would write the
/// higher-half VA to tp but reads through it would fault on the
/// missing mapping.
pub unsafe fn init(hartid: usize) {
    debug_assert!(hartid < MAX_HARTS, "hartid out of range");
    let ptr = &PER_HART_DATA[hartid] as *const PerHartData as usize;
    unsafe {
        asm!("mv tp, {}", in(reg) ptr, options(nostack, preserves_flags));
    }
}

/// Container for "one value per hart." Indexes into `[T; MAX_HARTS]`
/// using `current_hartid()`. Useful for per-CPU statics whose shape
/// doesn't fit naturally into `PerHartData` (e.g. `PerCpu<Mutex<T>>`).
pub struct PerCpu<T> {
    cells: [T; MAX_HARTS],
}

impl<T> PerCpu<T> {
    pub const fn new(cells: [T; MAX_HARTS]) -> Self {
        Self { cells }
    }

    pub fn this_cpu(&self) -> &T {
        &self.cells[current_hartid()]
    }

    pub fn this_cpu_mut(&mut self) -> &mut T {
        &mut self.cells[current_hartid()]
    }
}

/// Current hart's logical id. Reads through `tp`, which `init()`
/// configured to point at this hart's `PER_HART_DATA` slot.
///
/// Pre-`init()` safety: returns 0 if `tp` points outside the
/// `PER_HART_DATA` static. Today no caller exists in the
/// post-MMU-pre-init window, but the guard avoids reading garbage
/// through a freshly-zeroed `tp` should the boot order ever shuffle.
#[inline]
pub fn current_hartid() -> usize {
    let tp: usize;
    unsafe { asm!("mv {}, tp", out(reg) tp, options(nostack, preserves_flags)) };

    let base = (&raw const PER_HART_DATA[0]) as usize;
    let end = base + core::mem::size_of::<[PerHartData; MAX_HARTS]>();
    if tp < base || tp >= end {
        return 0;
    }
    // SAFETY: tp is in the PER_HART_DATA range, so it points at a
    // valid PerHartData. The `hart_id` field is at offset 0 (per the
    // `#[repr(C)]` layout).
    unsafe { (*(tp as *const PerHartData)).hart_id as usize }
}
