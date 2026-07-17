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

/// Single-MMIO-read BQL fence used by the storms' cross-hart spin-waits.
/// The 16550 LSR at base+5 reads without side effects, and QEMU's MMIO
/// path takes the Big QEMU Lock — which serialises against other vCPUs
/// and incidentally supplies the cross-hart memory fence multi-thread
/// TCG drops on a plain `Acquire`. Fence the *observer*/coordinator
/// hart with this, never the hart whose race window is under test.
fn fence_via_uart_lsr() {
    let lsr = crate::console::emergency_uart_base() + 5;
    // SAFETY: the 16550 LSR is a mapped, side-effect-free register.
    unsafe { core::ptr::read_volatile(lsr as *const u8) };
}

pub mod live_tasks {
    //! Many long-**lived** tasks (`workload=live-tasks`): spawn `N` tasks that each
    //! loop-`yield_now` forever (never exit), so the scheduler's task table holds `N`
    //! *live* entries and every context switch resolves two of them by id. Stresses
    //! the O(1) `TaskDirectory` lookup with a large live table — distinct from
    //! `spawn-storm`, whose tasks exit (and, pre-reaping, linger as zombies). Drives
    //! the `sched-task-lookup-is-o1` scenario, which asserts probes-per-switch stays
    //! constant (~2) as the live count grows; an O(tasks) scan would be ~`N`.

    /// How many live tasks to spawn. Large enough that a linear-scan lookup would be
    /// obvious (probes-per-switch ≈ `N` ≫ the O(1) constant) yet the ~16 KiB stacks
    /// (× `N`) fit the default machine.
    pub const N: usize = 200;

    /// Entry for each live task: yield forever. Never exits, so it stays a live
    /// table entry the scheduler round-robins through.
    pub extern "C" fn body() -> ! {
        loop {
            crate::sched::yield_now();
        }
    }
}

pub mod panic_now {
    //! Minimal crash smoke (`workload=panic-now`): a kernel task calls `panic!()`
    //! immediately on its first run — no guard page, no MMU, no fault. Isolates
    //! whether the stack-guard family's snemu-vs-QEMU divergence (only-snemu
    //! `kernel.heartbeat`) is really about the crash *timing*, not guard pages.

    /// Entry for the panic task. Never returns: the `panic!` halts the kernel.
    pub extern "C" fn body() -> ! {
        panic!("panic-now workload: deliberate immediate panic");
    }
}

pub mod stack_guard {
    //! Kernel-stack guard Tier-B smoke (`workload=stack-guard`): a kernel task
    //! deliberately stores into its *own* unmapped guard page from a context with
    //! full stack headroom, faulting at the exact store. The trap handler
    //! recognizes the guard region, snitches `Log("kernel stack overflow: task …")`,
    //! and panics — proving the fault→name→halt path deterministically, without the
    //! double-fault risk of a deep real overflow (which `stack-overflow-deep`
    //! exercises).

    /// Entry for the smoke task. Never returns: the guard write faults and the
    /// trap handler halts the kernel (the spin is unreachable).
    pub extern "C" fn smoke_body() -> ! {
        crate::sched::touch_current_stack_guard();
        loop {
            crate::sched::yield_now();
        }
    }
}

pub mod boot_stack_guard {
    //! Boot-stack (task 0) guard smoke (`workload=boot-stack-guard`): a kernel task
    //! stores into the boot stack's unmapped guard page (punched by
    //! `mmu::guard_boot_stack`), faulting at the store. The trap handler recognizes
    //! the boot guard region and snitches `Log("kernel stack overflow: boot stack
    //! …")` before panicking — proving the boot guard is actually unmapped and named.

    /// Entry for the smoke task. Never returns: the guard write faults and the
    /// trap handler halts the kernel (the spin is unreachable).
    pub extern "C" fn smoke_body() -> ! {
        crate::sched::touch_boot_stack_guard();
        loop {
            crate::sched::yield_now();
        }
    }
}

pub mod stack_overflow_deep {
    //! Kernel-stack guard Tier-B *deep* smoke (`workload=stack-overflow-deep`): a
    //! kernel task recurses until it genuinely overflows its 16 KiB stack into the
    //! unmapped guard page. The overflowing store faults; because the trap handler
    //! now runs on the **per-hart exception stack**, it builds its frame on clean
    //! memory and snitches `Log("kernel stack overflow: task … guard page …")`
    //! before panicking — where *without* the exception stack a deep overflow would
    //! double-fault on the overflowed stack and hang/reset. The capability proof
    //! for `plans/legacy/kernel-stack-hardening.md` Phase 1.

    /// Recurse with a ~1 KiB frame each call until the stack crosses into the guard
    /// page. `#[inline(never)]` + work *after* the recursive call defeats tail-call
    /// elimination, and `black_box` keeps the per-frame buffer live, so the stack
    /// genuinely grows ~1 KiB per level.
    #[inline(never)]
    fn recurse(depth: usize) -> usize {
        let mut buf = [0u8; 1024];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (depth ^ i) as u8;
        }
        core::hint::black_box(&buf);
        recurse(depth + 1).wrapping_add(buf[depth % buf.len()] as usize)
    }

    /// Entry for the smoke task. Never returns: the recursion overflows into the
    /// guard page and the trap handler halts the kernel.
    pub extern "C" fn smoke_body() -> ! {
        let _ = recurse(0);
        loop {
            crate::sched::yield_now();
        }
    }
}

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

    /// Drive the storm. Iteration `i` spawns one minimal task on
    /// hart 1 and waits for the ack counter to exceed `i` before
    /// advancing. Returns when all N tasks have acked.
    pub fn run() {
        for i in 0..N {
            crate::sched::spawn_on(1, "deflake", body);
            loop {
                super::fence_via_uart_lsr();
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

pub mod tlb_shootdown {
    //! Cross-hart TLB-shootdown **correctness** workload — the oracle
    //! `shootdown-storm` deliberately isn't. The storm proves the IPI
    //! payload-read plumbing (counters climb even if hart 1 sfences the
    //! wrong VA); this proves the *consequence*: after hart 0 repoints a
    //! VA at a new frame, hart 1 stops reading the old one.
    //!
    //! ## The teeth (why a fresh map wouldn't do)
    //!
    //! If hart 1 had no TLB entry for the test VA, its page-fault walker
    //! would resolve the new PTE correctly whether or not shootdown ran
    //! — a vacuous pass. So each round hart 1 reads **through** the VA
    //! *before* the remap, caching the old translation; only the
    //! shootdown's `sfence` can invalidate it. Under QEMU TCG the
    //! softmmu TLB only flushes on an intercepted `sfence.vma`, so a
    //! missing / wrong-VA shootdown genuinely leaves hart 1 reading the
    //! stale frame — `STALE_READS` catches it. (Teeth proven out of band
    //! by a deliberately-broken counterfactual; see the step-13 plan.)
    //!
    //! ## Protocol (one shared VA `V`, two pre-filled frames A/B)
    //!
    //!   - **Setup (hart 0):** alloc frames A, B; write distinct
    //!     sentinels into each via the linear map; `map(V → A)`; publish
    //!     round 0.
    //!   - **hart 0 driver (`run`, heartbeat-driven, one shot):** for
    //!     `i` in `1..=N`: wait until hart 1 has read round `i-1` (so it
    //!     cached the old frame); `remap(V → other frame)` — which fires
    //!     the cross-hart `shootdown(V)`; publish `EXPECTED` then
    //!     `ROUND = i`.
    //!   - **hart 1 reader (`reader_body`, task):** on each new round,
    //!     read `*V`; if it isn't the expected sentinel, bump
    //!     `STALE_READS`; record the round and ack it.
    //!
    //! Ordering: hart 0 stores `EXPECTED` then `ROUND` (Release); hart 1
    //! Acquire-loads `ROUND`, then reads `V`, then `EXPECTED`. The
    //! shootdown completes (hart 1's IPI handler sfenced + acked) inside
    //! `remap` *before* `ROUND` is published, so a correct shootdown
    //! means the round-`i` read re-walks to frame `i`.

    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use kernel_mem::mmu::{PtePerms, pa_to_kernel_va};

    /// Rounds the driver runs. Each is one cross-hart remap+shootdown
    /// round-trip; the scenario only needs to observe enough of them to
    /// rule out a vacuous pass.
    pub const N: u64 = 512;

    const SENTINEL_A: u64 = 0xAAAA_AAAA_AAAA_AAAA;
    const SENTINEL_B: u64 = 0xBBBB_BBBB_BBBB_BBBB;

    /// Dedicated test VA in a fresh root slot one gigapage above the
    /// heap (root slot 257). Nothing else maps here, so the initial
    /// `map` installs clean intermediates and the remap can't collide.
    const TEST_VA: usize = kernel_mem::mmu::HEAP_VA_BASE + (1 << 30);

    /// Highest round hart 1 has read. Heartbeat re-emits as
    /// `snitchos.smp.tlb_remap_rounds` — proves the workload actually
    /// ran (not a vacuous pass). `Relaxed`: single writer (hart 1).
    pub static ROUNDS: AtomicU64 = AtomicU64::new(0);

    /// Count of reads where hart 1 saw the *stale* frame after a remap.
    /// Heartbeat re-emits as `snitchos.smp.tlb_stale_reads`. Must stay
    /// 0 — any nonzero value is a missed shootdown. `Relaxed`: single
    /// writer (hart 1).
    pub static STALE_READS: AtomicU64 = AtomicU64::new(0);

    /// Current round, published by hart 0 (Release) and observed by
    /// hart 1 (Acquire). Carries the happens-before for `EXPECTED`.
    static ROUND: AtomicU64 = AtomicU64::new(0);

    /// Sentinel hart 1 should read this round. Stored before `ROUND`.
    static EXPECTED: AtomicU64 = AtomicU64::new(0);

    /// Count of rounds hart 1 has read (= highest round read + 1; 0 =
    /// none). hart 0 waits on this so it only remaps after hart 1 has
    /// cached the old translation.
    static HART1_READS: AtomicU64 = AtomicU64::new(0);

    /// hart 0 → hart 1: setup is complete, `V` is mapped, start reading.
    static SETUP_DONE: AtomicBool = AtomicBool::new(false);

    /// hart 0 → hart 1: the round loop finished; the reader may exit.
    static STOP: AtomicBool = AtomicBool::new(false);

    /// Physical addresses of the two frames, published by `setup`.
    static FRAME_A_PA: AtomicU64 = AtomicU64::new(0);
    static FRAME_B_PA: AtomicU64 = AtomicU64::new(0);

    /// R+W+G leaf perms for the test page (kernel-global, read/written
    /// as a `u64`; no execute needed).
    fn perms() -> PtePerms {
        PtePerms::R.union(PtePerms::W).union(PtePerms::G)
    }

    /// hart 0: allocate + fill the two frames, install `V → A`, publish
    /// round 0. Runs once, at the top of `run`.
    fn setup() {
        let Some(fa) = crate::frame::alloc_zeroed() else {
            panic!("tlb-shootdown: OOM allocating frame A");
        };
        let Some(fb) = crate::frame::alloc_zeroed() else {
            panic!("tlb-shootdown: OOM allocating frame B");
        };
        let (pa_a, pa_b) = (fa.addr(), fb.addr());
        // SAFETY: both frames are freshly allocated and reachable via
        // the linear map; we write one `u64` sentinel into each.
        unsafe {
            (pa_to_kernel_va(pa_a) as *mut u64).write_volatile(SENTINEL_A);
            (pa_to_kernel_va(pa_b) as *mut u64).write_volatile(SENTINEL_B);
        }
        FRAME_A_PA.store(pa_a as u64, Ordering::Relaxed);
        FRAME_B_PA.store(pa_b as u64, Ordering::Relaxed);
        if crate::mmu::map(TEST_VA, pa_a, perms()).is_err() {
            panic!("tlb-shootdown: initial map of TEST_VA failed");
        }
        EXPECTED.store(SENTINEL_A, Ordering::Relaxed);
        ROUND.store(0, Ordering::Release);
        SETUP_DONE.store(true, Ordering::Release);
    }

    /// hart 0 driver. Called once from the heartbeat (first tick),
    /// blocking main until the N rounds complete.
    pub fn run() {
        setup();
        for i in 1..=N {
            // Wait until hart 1 has read round i-1 (HART1_READS counts
            // rounds read, so == i means rounds 0..=i-1 are done). Only
            // then does hart 1 hold the *old* translation we're about to
            // invalidate.
            loop {
                super::fence_via_uart_lsr();
                if HART1_READS.load(Ordering::Acquire) >= i {
                    break;
                }
                core::hint::spin_loop();
            }
            let (pa, sentinel) = if i & 1 == 1 {
                (FRAME_B_PA.load(Ordering::Relaxed) as usize, SENTINEL_B)
            } else {
                (FRAME_A_PA.load(Ordering::Relaxed) as usize, SENTINEL_A)
            };
            // remap fires the cross-hart shootdown; on success hart 1's
            // stale TLB entry for TEST_VA is invalidated before we
            // publish the new round.
            if crate::mmu::remap(TEST_VA, pa, perms()).is_err() {
                panic!("tlb-shootdown: remap of mapped TEST_VA failed");
            }
            EXPECTED.store(sentinel, Ordering::Relaxed);
            ROUND.store(i, Ordering::Release);
        }
        // Drain the final round, then release the reader.
        loop {
            super::fence_via_uart_lsr();
            if HART1_READS.load(Ordering::Acquire) > N {
                break;
            }
            core::hint::spin_loop();
        }
        STOP.store(true, Ordering::Release);
    }

    /// hart 1 reader task. Reads through `V` once per published round
    /// and records any stale read.
    pub extern "C" fn reader_body() -> ! {
        while !SETUP_DONE.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        let mut last = u64::MAX;
        loop {
            if STOP.load(Ordering::Acquire) {
                crate::sched::exit_now();
            }
            let r = ROUND.load(Ordering::Acquire);
            if r == last {
                core::hint::spin_loop();
                continue;
            }
            last = r;
            // Read THROUGH the test VA. The previous round's read cached
            // frame[r-1] in this hart's TLB; if hart 0's shootdown for
            // round r worked, this re-walk now sees frame[r].
            //
            // SAFETY: TEST_VA is a live kernel-global 4 KiB mapping
            // (installed by `setup`, repointed by `run`); reading a
            // `u64` through it is valid.
            let v = unsafe { (TEST_VA as *const u64).read_volatile() };
            if v != EXPECTED.load(Ordering::Relaxed) {
                STALE_READS.fetch_add(1, Ordering::Relaxed);
            }
            ROUNDS.store(r, Ordering::Relaxed);
            HART1_READS.store(r + 1, Ordering::Release);
        }
    }
}

pub mod ping_pong {
    //! Cross-hart bidirectional strict-alternation cadence oracle. ping
    //! (hart 0) and pong (hart 1) hand a single shared `TURN` flag back
    //! and forth (0 = ping's turn, 1 = pong's): each side busy-waits for
    //! its turn, takes it, and flips the flag to the partner. Both turn
    //! counters reaching `K` proves `K` strict, lockstep alternations
    //! happened across the hart boundary — neither side can advance
    //! until the other has handed off.
    //!
    //! What this uniquely covers (vs the other SMP guards): it is the
    //! only *bidirectional* cross-hart liveness test. If either hart's
    //! write of `TURN` were not visible to the other (the multi-thread
    //! TCG Acquire-drop the deflake saga chased), the flag wedges and
    //! both counters stall — the scenario's budget catches it. The
    //! `fence_via_uart_lsr` in the wait loop is the established
    //! visibility workaround.
    //!
    //! **Why busy-spin, not IPI-wake-from-`wfi`:** an earlier cut had
    //! each side yield to idle and `wfi`, relying on the partner's
    //! `IPI_WAKEUP` to re-wake it. That hit a lost-wakeup — if the IPI
    //! lands between the turn-check and the `wfi`, `handle_pending`
    //! clears `SSIP` and the `wfi` then sleeps until the 1 Hz timer
    //! (race-free IPI condition-wait needs IRQs disabled across
    //! check+`wfi`, which is preemption-era machinery). The
    //! one-directional IPI-wake path is already covered by `ipi-pong` /
    //! `spawn-storm` / `ipi-self-wakeup`; this oracle deliberately
    //! avoids `wfi` so it measures alternation, not the scheduler's
    //! (current) inability to sleep-wait on a memory condition.
    //!
    //! hart 0's ping runs heartbeat-driven (`run`, one shot on the
    //! first tick, blocking main while pong busy-spins on hart 1), so
    //! the heartbeat can drain both counters once it returns — same
    //! pattern as `tlb_shootdown`.

    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// Turns each side takes.
    pub const K: u64 = 200;

    /// Whose turn it is: 0 = ping (hart 0), 1 = pong (hart 1). ping
    /// goes first.
    static TURN: AtomicU32 = AtomicU32::new(0);

    /// Turns completed by each side. Heartbeat re-emits both. `Relaxed`:
    /// one writer per cell.
    pub static PING_TURNS: AtomicU64 = AtomicU64::new(0);
    pub static PONG_TURNS: AtomicU64 = AtomicU64::new(0);

    /// Busy-wait until it is `mine`'s turn. No yield / `wfi` — see the
    /// module doc for why this oracle stays off the sleep path.
    fn await_turn(mine: u32) {
        while TURN.load(Ordering::Acquire) != mine {
            super::fence_via_uart_lsr();
            core::hint::spin_loop();
        }
    }

    /// hart 0 driver — heartbeat-driven, runs once. Takes K turns,
    /// handing off to pong (hart 1) between each.
    pub fn run() {
        for _ in 0..K {
            await_turn(0);
            {
                crate::span!("ping.turn");
                PING_TURNS.fetch_add(1, Ordering::Relaxed);
            }
            TURN.store(1, Ordering::Release);
        }
    }

    /// hart 1 pong task. Mirror of `run`; exits after K turns.
    pub extern "C" fn pong_body() -> ! {
        for _ in 0..K {
            await_turn(1);
            {
                crate::span!("pong.turn");
                PONG_TURNS.fetch_add(1, Ordering::Relaxed);
            }
            TURN.store(0, Ordering::Release);
        }
        crate::sched::exit_now()
    }
}
