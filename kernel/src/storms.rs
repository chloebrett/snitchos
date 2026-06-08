//! Cross-hart stress / regression workloads. Each replaces the default
//! boot demo with a tight loop hammering a specific kernel cross-hart
//! code path, selected at runtime via `workload=<name>` (the whole
//! module compiles only into `itest-workloads` builds):
//!
//!   - [`spawn_storm`] (`workload=spawn-storm`): hart 0 calls
//!     `spawn_on(1, ...)` N times, serialised on per-task acks.
//!   - [`ipi_pong`] (`workload=ipi-pong`): hart 0 sends tight
//!     `IPI_WAKEUP`s to hart 1 with no payload; probes the post-sret
//!     resume window.
//!   - [`shootdown`] (`workload=shootdown-storm`): hart 0 calls
//!     `mmu::shootdown(KERNEL_OFFSET)` in a loop; probes the IPI
//!     payload-read path.
//!   - [`mutex_storm`] (`workload=mutex-storm`): two tasks hammer a
//!     shared `Mutex` across harts.
//!   - [`virtio_storm`] (`workload=virtio-storm`): hart 0 emit-storm
//!     over the virtio TX path, hart 1 atomic spin.
//!
//! These originally characterised an unfixed cross-hart race (a dropped
//! `MutexGuard` in `virtio_console::send`, since fixed); they are kept
//! as regression guards. See `plans/residual-race-investigation.md`.

pub mod virtio_storm {
    //! Validation experiment for H11-refined: is the cross-hart bug
    //! specifically in the virtio-console emission path
    //! (`virtio_console::send` + `TX_STAGING` + MMIO notify)?
    //!
    //! Hart 0 runs a task that calls `tracing::emit_metric` in a
    //! tight loop — each call interns + serializes a frame + pushes
    //! it through `TX_STAGING` + virtio descriptor ring + MMIO
    //! notify. Hart 1 runs a task that does pure Relaxed
    //! `fetch_add` on a shared atomic.
    //!
    //! No cross-hart mutex contention. Hart 0 owns `TX_STAGING` and
    //! `INTERN_TABLE` end-to-end; hart 1 only touches its own
    //! atomic. The only shared state is the atomic and (implicitly)
    //! the cache lines holding the virtio descriptor ring / staging
    //! buffer.
    //!
    //! If H11-refined is right, the bug fires when hart 0 is in the
    //! middle of the virtio emit path while hart 1 mutates memory.
    //! Predict ≥30% per-boot flake with fix off.

    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    use crate::tracing;

    pub const N: u64 = 5_000;

    /// `StringId` of the metric used by the storm body. Registered
    /// once by `init()` before the storm task starts. Stored as a
    /// raw `u32` so it can sit in a `pub static` without needing a
    /// const constructor for `StringId`.
    pub static EMIT_METRIC: AtomicU32 = AtomicU32::new(0);

    /// Count of `emit_metric` calls completed by hart 0. Bumped
    /// after every successful emit. `Relaxed` — single writer.
    pub static HART0_EMITS: AtomicU64 = AtomicU64::new(0);

    /// Count of atomic fetch_adds completed by hart 1. Bumped per
    /// iteration. `Relaxed` — single writer.
    pub static HART1_ITERATIONS: AtomicU64 = AtomicU64::new(0);

    /// Hart 0 sets this when its emit loop finishes; hart 1 polls
    /// it to know when to stop. Without this, hart 1 would race
    /// ahead and exit before hart 0 made any progress, and we'd
    /// have no overlap.
    pub static HART1_STOP: AtomicBool = AtomicBool::new(false);

    /// The shared atomic hart 1 hammers. Hart 0 never touches it
    /// inside the storm body — but the cache line is implicitly
    /// shared between vCPUs via QEMU's coherency emulation.
    pub static SHARED: AtomicU64 = AtomicU64::new(0);

    /// Register the storm's emission metric. Called from kmain
    /// after the heartbeat metrics are registered (so the order on
    /// the wire is heartbeat-set first, then this).
    pub fn init() {
        let id = tracing::register_counter("snitchos.deflake.virtio_storm_emits");
        EMIT_METRIC.store(id.0, Ordering::Relaxed);
    }

    pub extern "C" fn body_hart0() -> ! {
        let metric = protocol::StringId(EMIT_METRIC.load(Ordering::Relaxed));
        for i in 0..N {
            tracing::emit_metric(metric, i as i64);
            HART0_EMITS.fetch_add(1, Ordering::Relaxed);
        }
        HART1_STOP.store(true, Ordering::Relaxed);
        crate::sched::exit_now()
    }

    pub extern "C" fn body_hart1() -> ! {
        while !HART1_STOP.load(Ordering::Relaxed) {
            SHARED.fetch_add(1, Ordering::Relaxed);
            HART1_ITERATIONS.fetch_add(1, Ordering::Relaxed);
        }
        crate::sched::exit_now()
    }
}

pub mod mutex_storm {
    //! Validation experiment for revised-H7. Both harts run a
    //! long-running task that takes and releases a shared
    //! `kernel::sync::Mutex<()>` `N` times in a tight loop. No payload
    //! in the critical section — the only operation under the lock is
    //! a Relaxed atomic bump (to prevent dead-code elimination from
    //! collapsing the loop body).
    //!
    //! Each task bumps a per-hart `ACQUIRES_HART{0,1}` counter at
    //! the start of every iteration; the heartbeat re-emits both as
    //! metrics. Scenario asserts both counters reach `N`. With fix on,
    //! BQL fences at every trap return should keep this clean. With
    //! fix off, if revised-H7 is right (Acquire/Release on the
    //! `spin::Mutex` CAS dropped by multi-thread TCG), one or both
    //! tasks should wedge mid-loop and the counter stalls.
    //!
    //! Why both harts are *running tasks* (not main + spawned task):
    //! main is the source of heartbeat emissions, so it needs to keep
    //! ticking even when the storm is running. Both storm tasks
    //! cooperatively yield... actually no — they don't yield. Each
    //! task hammers its loop straight through, then calls `exit_now`.
    //! With N=100k and ~100 ns per uncontended acquire, the loop is
    //! ~10 ms wall — main starves for that brief window then
    //! resumes. Acceptable.

    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::sync::Mutex;

    pub const N: u64 = 100_000;

    /// The contended mutex. `()` payload so no work happens under the
    /// lock — only the lock/unlock atomic sequence is exercised.
    pub static MUTEX: Mutex<()> = Mutex::new(());

    /// Per-hart acquire counts. Bumped at the START of each iteration
    /// (before the lock acquire) — so a stall mid-acquire leaves the
    /// counter below N, distinguishable from a stall mid-release.
    /// `Relaxed` — single writer per cell.
    pub static ACQUIRES_HART0: AtomicU64 = AtomicU64::new(0);
    pub static ACQUIRES_HART1: AtomicU64 = AtomicU64::new(0);

    /// Atomic touched inside the critical section. Prevents the loop
    /// body from being optimised to "lock/unlock with no observable
    /// effect." `Relaxed` — the mutex is what we're testing, not
    /// this counter.
    pub static IN_CRITICAL_BUMP: AtomicU64 = AtomicU64::new(0);

    pub extern "C" fn body_hart0() -> ! {
        for _ in 0..N {
            ACQUIRES_HART0.fetch_add(1, Ordering::Relaxed);
            let _guard = MUTEX.lock();
            IN_CRITICAL_BUMP.fetch_add(1, Ordering::Relaxed);
            drop(_guard);
        }
        crate::sched::exit_now()
    }

    pub extern "C" fn body_hart1() -> ! {
        for _ in 0..N {
            ACQUIRES_HART1.fetch_add(1, Ordering::Relaxed);
            let _guard = MUTEX.lock();
            IN_CRITICAL_BUMP.fetch_add(1, Ordering::Relaxed);
            drop(_guard);
        }
        crate::sched::exit_now()
    }
}

pub mod spawn_storm {
    //! Cross-hart spawn storm. Drives N serialised
    //! `spawn_on(1, body)` iterations from hart 0 with an MMIO-fenced
    //! ack wait. Each iteration is one trial of the residual
    //! cross-hart memory-ordering race on hart 1's IPI pickup path.

    use core::sync::atomic::{AtomicU64, Ordering};

    /// Number of spawn iterations. With per-trial flake rate ≈ 1%
    /// (eyeballed from suite-wide 8% across ~8 cross-hart trials),
    /// 200 iterations gives ~87% per-run flake without the fix —
    /// enough signal for `--repeat 5`.
    pub const N: u64 = 200;

    /// Cumulative count of spawned tasks that reached their body and
    /// bumped this counter. Hart 0's storm loop polls it after each
    /// spawn; the heartbeat re-emits its value every tick as the
    /// `snitchos.deflake.spawn_storm_acks` metric.
    /// `Release` on the bump pairs with `Acquire` on hart 0's poll —
    /// but multi-thread TCG is the entire reason we don't trust that
    /// pairing, hence `fence_via_uart_lsr` below.
    pub static ACK_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Body run by every storm task on hart 1. Touches a stack-local
    /// (H2 probe — proves the new task's `sp` resolves to writable
    /// memory), bumps the ack counter, then exits. With v0.5.x's
    /// minimal task-exit, the task is removed from the runqueue and
    /// hart 1 returns to `wfi` (via `hart_1_main`'s yield-then-wfi
    /// loop) before the next spawn arrives — restoring the
    /// `hart 1 in wfi → IPI → trap → switch` trial pattern the
    /// storm exists to test.
    pub extern "C" fn body() -> ! {
        let marker: u64 = 0xdead_beef_cafe_f00d;
        core::hint::black_box(marker);
        ACK_COUNTER.fetch_add(1, Ordering::Release);
        crate::sched::exit_now()
    }

    /// Single-MMIO-read BQL fence used by hart 0's ack-wait spin. The
    /// UART LSR at base+5 has no side effects on read. QEMU's MMIO
    /// path acquires the Big QEMU Lock, which serialises against
    /// other vCPUs and incidentally provides the cross-hart memory
    /// fence that multi-thread TCG drops on plain `Acquire`. Hart 0
    /// is the observer, not the hart under test — fencing it does
    /// not interfere with the race window on hart 1's pickup path.
    fn fence_via_uart_lsr() {
        let lsr = crate::console::emergency_uart_base() + 5;
        // SAFETY: lsr is the 16550 line-status register; reading it
        // is a non-destructive observation. The address is mapped
        // (the emergency UART base is always reachable post-MMU).
        unsafe { core::ptr::read_volatile(lsr as *const u8) };
    }

    /// Drive the storm. Iteration `i` spawns one minimal task on
    /// hart 1 and waits for the ack counter to exceed `i` before
    /// advancing. Returns when all N tasks have acked.
    pub fn run() {
        for i in 0..N {
            crate::sched::spawn_on(1, "deflake", body);
            loop {
                fence_via_uart_lsr();
                if ACK_COUNTER.load(Ordering::Acquire) > i {
                    break;
                }
                core::hint::spin_loop();
            }
        }
    }
}

pub mod ipi_pong {
    //! Tight IPI_WAKEUP loop from hart 0 to hart 1. Each iteration is
    //! one `hart 1 in wfi → IPI → trap → swap-Acquire → sret → resume`
    //! trial — directly the post-sret memory-ordering window the
    //! deflake residual was suspected to live on. No spawning, no
    //! heap growth: scales to ~10k trials per boot.
    //!
    //! Pacing: hart 0 sends one IPI, then spins on `rdtime` for
    //! `DELAY_TICKS` before the next send. At timebase 10 MHz,
    //! 1 000 ticks ≈ 100 µs — long enough that hart 1
    //! (`hart_1_main`'s `yield → wfi → wake → yield`) re-enters
    //! `wfi` before the next IPI lands. Without pacing the IPIs
    //! coalesce into one trap on hart 1 and the trial count
    //! collapses.
    //!
    //! No MMIO touches on hart 0 inside the loop — the storm's whole
    //! point is to leave the kernel's race window unfenced. `rdtime`
    //! is a CSR read, not MMIO.

    use core::sync::atomic::{AtomicU64, Ordering};

    pub const N: u64 = 10_000;

    /// Inter-IPI delay on hart 0, in `rdtime` ticks. Tuned so hart 1
    /// has time to `sret → loop iter → yield → wfi` before the next
    /// IPI lands. Too low: IPIs coalesce (RECEIVED_TOTAL << N). Too
    /// high: storm takes forever.
    const DELAY_TICKS: u64 = 1_000;

    /// Count of IPI sends issued by hart 0. Bumped after each send;
    /// the heartbeat re-emits as `snitchos.deflake.ipi_pong_sends`.
    /// Survival signal: if hart 0 wedged or the kernel hung mid-loop,
    /// this counter stays below N. `Relaxed` — single writer.
    pub static SENDS: AtomicU64 = AtomicU64::new(0);

    fn now() -> u64 {
        let t: u64;
        // SAFETY: rdtime is a S-mode-readable CSR; reading it has no
        // side effects.
        unsafe { core::arch::asm!("rdtime {}", out(reg) t) };
        t
    }

    /// Drive the storm. Sends N IPIs paced by DELAY_TICKS each.
    pub fn run() {
        for _ in 0..N {
            crate::ipi::send(1, crate::ipi::IPI_WAKEUP);
            SENDS.fetch_add(1, Ordering::Relaxed);
            let deadline = now() + DELAY_TICKS;
            while now() < deadline {
                core::hint::spin_loop();
            }
        }
    }
}

pub mod shootdown {
    //! Tight `mmu::shootdown(va)` loop from hart 0. Each iteration:
    //!
    //!   - hart 0 writes `shootdown_va` into hart 1's PerHartData,
    //!     snapshots `shootdown_ack`, sends `IPI_TLB_SHOOTDOWN`,
    //!     spin-waits (Acquire) on `shootdown_ack` to advance.
    //!   - hart 1 takes the IPI; `ipi::handle_pending` does
    //!     `ipi_pending.swap(0, Acquire)`, reads `shootdown_va`,
    //!     runs `sfence.vma`, bumps `shootdown_ack` with Release.
    //!
    //! Tests the IPI payload-read path — distinct from `ipi_pong`
    //! which has no payload. The race window we're probing: does
    //! hart 1 actually see the `shootdown_va` value hart 0 wrote
    //! before the IPI, on multi-thread TCG? If not, hart 1 sfences
    //! the wrong address; the test still completes (sfence with any
    //! VA is harmless on a fresh-mapping kernel) but the
    //! `shootdowns_received_total` counter still climbs.
    //!
    //! **Built-in confounder.** Hart 0's spin-wait inside
    //! `mmu::shootdown` is itself an Acquire load on
    //! `shootdown_ack`. If multi-thread TCG drops it, hart 0 wedges
    //! before hart 1 can be blamed. We accept this trade — the
    //! kernel's existing API is the API we're testing. A wedge here
    //! is informative either way.
    //!
    //! Choice of VA: `KERNEL_OFFSET` — a real higher-half VA that is
    //! always mapped, so sfence on it on hart 1 is a real TLB op,
    //! not a no-op.

    use core::sync::atomic::{AtomicU64, Ordering};

    pub const N: u64 = 5_000;
    const DELAY_TICKS: u64 = 2_000;

    /// Count of shootdowns initiated by hart 0. The heartbeat
    /// re-emits as `snitchos.deflake.shootdown_storm_sends`.
    /// `Relaxed` — single writer.
    pub static SENDS: AtomicU64 = AtomicU64::new(0);

    fn now() -> u64 {
        let t: u64;
        // SAFETY: rdtime is S-mode-readable; no side effects.
        unsafe { core::arch::asm!("rdtime {}", out(reg) t) };
        t
    }

    pub fn run() {
        let va = crate::mmu::KERNEL_OFFSET;
        for _ in 0..N {
            crate::mmu::shootdown(va);
            SENDS.fetch_add(1, Ordering::Relaxed);
            let deadline = now() + DELAY_TICKS;
            while now() < deadline {
                core::hint::spin_loop();
            }
        }
    }
}
