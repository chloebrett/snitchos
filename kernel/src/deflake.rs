//! Cross-hart memory-ordering investigation scenarios. Three storm
//! workloads, each gated by its own feature, that replace the default
//! boot demo tasks with a tight loop probing a specific kernel
//! cross-hart code path:
//!
//!   - [`spawn_storm`] (`deflake-spawn-storm`): hart 0 calls
//!     `spawn_on(1, ...)` N times, serialised on per-task acks.
//!   - [`ipi_pong`] (`deflake-ipi-pong`): hart 0 sends tight
//!     `IPI_WAKEUP`s to hart 1 with no payload; probes the post-sret
//!     resume window.
//!   - [`shootdown`] (`deflake-shootdown-storm`): hart 0 calls
//!     `mmu::shootdown(KERNEL_OFFSET)` in a loop; probes the IPI
//!     payload-read path.
//!
//! See `plans/residual-race-investigation.md` for hypothesis tree,
//! experiment ladder, and falsified-by-trial-count tables.

#[cfg(feature = "deflake-spawn-storm")]
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
    /// memory), bumps the ack counter, then cycles through `yield_now`
    /// forever because v0.5 tasks can't exit.
    pub extern "C" fn body() -> ! {
        let marker: u64 = 0xdead_beef_cafe_f00d;
        core::hint::black_box(marker);
        ACK_COUNTER.fetch_add(1, Ordering::Release);
        loop {
            crate::sched::yield_now();
        }
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

#[cfg(feature = "deflake-ipi-pong")]
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

#[cfg(feature = "deflake-shootdown-storm")]
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
