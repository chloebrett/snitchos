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
//!   - **TLB shootdown ack.** Step 9 wires this. Sequence:
//!     1. Initiator writes `target.shootdown_va = va` (Relaxed —
//!        about to be published by the next step).
//!     2. Initiator snapshots `target.shootdown_ack` as `pre`.
//!     3. Initiator does `target.ipi_pending |= IPI_TLB_SHOOTDOWN`
//!        with `Release` (this is what publishes `shootdown_va`).
//!     4. Initiator sends the SBI IPI.
//!     5. Target's handler does `ipi_pending.swap(0, Acquire)`,
//!        sees the bit, reads `shootdown_va` (now visible), runs
//!        `sfence.vma vaddr`.
//!     6. Target's handler bumps `shootdown_ack` with `Release`.
//!     7. Initiator spin-waits `target.shootdown_ack.load(Acquire)
//!        > pre`. Once true, the sfence happened-before this load —
//!        > the initiator now knows it's safe to assume no stale TLB
//!        > entries on the target.
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
use core::sync::atomic::{AtomicU32, AtomicU64};

/// Maximum harts supported. Bumped to 2 in v0.6 step 8 for the
/// cooperative-SMP demo (one boot hart + one worker). Each hart
/// gets its own `PER_HART_DATA` slot, its own `PerCpu<T>` cell, and
/// (step 10) its own runqueue + idle task.
pub const MAX_HARTS: usize = 2;

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
    /// IPI pending bitflags. Sender does
    /// `ipi_pending.fetch_or(msg_bit, Release)` (publishes any
    /// payload it wrote first); receiver does
    /// `ipi_pending.swap(0, Acquire)` (sees the payload). See the
    /// module-level memory-ordering discipline.
    pub ipi_pending: AtomicU32,
    /// TLB shootdown payload: the VA whose mapping the initiator
    /// just changed. Set by `mmu::shootdown` *before* the
    /// `IPI_TLB_SHOOTDOWN` bit is published in `ipi_pending`. The
    /// receive-side handler reads this after the Acquire swap on
    /// `ipi_pending` lifts it out of staleness, runs
    /// `sfence.vma vaddr`, then bumps `shootdown_ack`.
    ///
    /// v0.6 step 9 is single-slot — one in-flight shootdown per
    /// target at a time. Under multi-initiator contention (step 11
    /// migrates workload across harts) this becomes a hazard; the
    /// collision-safe variant is per-(target, initiator) slots,
    /// deferred until a real race surfaces.
    pub shootdown_va: AtomicU64,
    /// Monotonic ack counter. Bumped by the receive-side shootdown
    /// handler after `sfence.vma` completes. Initiators snapshot
    /// this before sending the IPI, then spin-wait for it to
    /// advance — proves the target's flush happened-before our spin
    /// observed the new value (Release on bump pairs with Acquire
    /// on initiator's wait).
    pub shootdown_ack: AtomicU64,
}

/// One slot per hart. Statically initialised to `hart_id = i` so a
/// secondary hart starting cold (before its `init()` runs) at least
/// sees a stable value at its slot.
/// Bitmap of harts that have run `init()` and are live for cross-hart
/// signalling (IPIs, TLB shootdowns). Bit `i` set ⇒ hart `i` is online
/// and will respond to IPIs. `mmu::shootdown` consults this so it
/// doesn't try to handshake with a target that's still parked in
/// OpenSBI (which would spin-wait forever for an ack).
///
/// `AtomicU64` so MAX_HARTS up to 64 fits naturally. `Relaxed` on
/// reads/writes: the actual cross-hart synchronisation a shootdown
/// needs is provided by the `ipi_pending`/`shootdown_ack` handshake
/// — this bitmap only gates *whether* to attempt that handshake.
pub static SMP_ONLINE_HARTS: AtomicU64 = AtomicU64::new(0);

/// Logical hart id (`0..MAX_HARTS`) → platform `mhartid`. Written by
/// `kmain` once OpenSBI's boot hart selection is known; read by
/// `ipi::send` to translate the logical target to the mhartid the
/// SBI `send_ipi` call expects.
///
/// Initialised to the identity mapping so single-hart and "boot hart
/// is mhartid 0" cases work without any explicit setup. Boot path
/// overwrites with the actual mapping derived from `_hart_id`.
pub static LOGICAL_TO_MHARTID: [core::sync::atomic::AtomicU64; MAX_HARTS] = [
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(1),
];

pub static PER_HART_DATA: [PerHartData; MAX_HARTS] = [
    PerHartData {
        hart_id: 0,
        ipi_pending: AtomicU32::new(0),
        shootdown_va: AtomicU64::new(0),
        shootdown_ack: AtomicU64::new(0),
    },
    PerHartData {
        hart_id: 1,
        ipi_pending: AtomicU32::new(0),
        shootdown_va: AtomicU64::new(0),
        shootdown_ack: AtomicU64::new(0),
    },
];

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
    // Announce we're online. Any initiator that observes our bit set
    // will start expecting shootdown acks from us.
    SMP_ONLINE_HARTS.fetch_or(1u64 << hartid, core::sync::atomic::Ordering::Relaxed);
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

/// Borrow this hart's `PerHartData` slot for direct field access
/// (e.g., `ipi_pending`). Reads `tp`; same fallback as
/// `current_hartid()` if `tp` is out of range.
#[inline]
pub fn this_cpu() -> &'static PerHartData {
    let tp: usize;
    unsafe { asm!("mv {}, tp", out(reg) tp, options(nostack, preserves_flags)) };

    let base = (&raw const PER_HART_DATA[0]) as usize;
    let end = base + core::mem::size_of::<[PerHartData; MAX_HARTS]>();
    if tp < base || tp >= end {
        return &PER_HART_DATA[0];
    }
    // SAFETY: tp is in range, points at a valid PerHartData with
    // 'static lifetime (the array is a static).
    unsafe { &*(tp as *const PerHartData) }
}
