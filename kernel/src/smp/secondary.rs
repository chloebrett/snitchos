//! Secondary-hart bring-up: the Rust side of `_secondary_start`.
//!
//! Boot-hart side (in kmain): stash SATP + `KERNEL_OFFSET` into the
//! statics that the secondary's asm reads, then call
//! `sbi::hart_start(1, va_to_pa(_secondary_start), 1)`. After the
//! call returns, hart 0 spin-waits on `SECONDARY_READY` so it
//! doesn't `unmap_identity` before hart 1 has finished the
//! trampoline.
//!
//! Secondary side: `_secondary_start` (asm) lands here at
//! `secondary_main` with `a0 = mhartid`, `a1 = logical hartid`.
//! From here on we're running at higher-half PC + sp; `tp` is not
//! yet set, so the first thing is `percpu::init`.
//!
//! v0.6 step 8 secondary work: emit `HartRegister`, signal
//! `SECONDARY_READY`, idle in `wfi`. Real per-hart runqueue +
//! spawning lands in step 10.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::counter::DeferredCounter;

use crate::mmu;
use crate::percpu;
use crate::tracing;

global_asm!(include_str!("secondary.S"));

/// 16 KiB stack for the secondary hart. Aligned to 16 bytes for the
/// RISC-V ABI. `#[unsafe(no_mangle)]` so the asm can `la` it.
const SECONDARY_STACK_SIZE: usize = 16 * 1024;
#[repr(C, align(16))]
struct SecondaryStack([u8; SECONDARY_STACK_SIZE]);

/// One bring-up stack per hart, indexed by *logical* id. Slot 0 (the boot hart,
/// which runs on its `entry.S` stack) is unused — index-is-hartid clarity is worth
/// the 16 KiB. Each secondary runs on its slot for its whole life, so they can't
/// share one stack; hence an array sized by `MAX_HARTS`, not a single stack.
static mut SECONDARY_STACKS: [SecondaryStack; percpu::MAX_HARTS] =
    [const { SecondaryStack([0; SECONDARY_STACK_SIZE]) }; percpu::MAX_HARTS];

/// Stack-top address (`&SECONDARY_STACK + STACK_SIZE`) — written by
/// `prepare_for_secondary` from hart 0 *before* `sbi_hart_start`.
/// The asm at `_secondary_start` does `la t0, X; ld sp, 0(t0)` to
/// pick this up. `static mut` so the write doesn't depend on the
/// boot page table being RWX everywhere.
#[unsafe(no_mangle)]
#[used]
static mut SECONDARY_STACK_TOP: u64 = 0;

/// SATP value (mode + root PPN) hart 0 uses; hart 1 reuses it.
/// Written by `prepare_for_secondary` from kmain *before* calling
/// `sbi::hart_start`.
#[unsafe(no_mangle)]
#[used]
static mut SECONDARY_SATP: u64 = 0;

/// `KERNEL_OFFSET` as a static so the asm can load it (the value
/// doesn't fit in an immediate). Initialised at boot.
#[unsafe(no_mangle)]
#[used]
static mut KERNEL_OFFSET_VALUE: u64 = 0;

/// Set by `secondary_main` after `HartRegister` emission. Hart 0
/// spin-waits on this before `unmap_identity` so it doesn't tear
/// down the mapping hart 1 is mid-trampoline through.
/// Bitmap of secondary harts that have finished bring-up (init + trap vector +
/// timer), by logical id — bit `i` set ⇒ hart `i` is fully up. Hart 0 waits for
/// each secondary's bit before starting the next (and before `unmap_identity`). A
/// bitmap, not a single `bool`, so N secondaries each signal independently.
/// Distinct from `SMP_ONLINE_HARTS` (set earlier in `percpu::init`, before trap
/// setup): the cross-hart probe needs the *fully-up* point, not just "online."
pub static SECONDARY_READY: AtomicU64 = AtomicU64::new(0);

/// Cumulative count of `wfi`s on the secondary hart. Bumped each
/// time the idle loop completes a sleep. Useful as a "hart 1 is
/// alive" signal in dashboards. `Relaxed`: per-hart counter.
pub static SECONDARY_WFI_COUNT: DeferredCounter = DeferredCounter::new("snitchos.smp.secondary_wfi_total");

/// Bumped by `probe_entry` each loop. Used by the
/// `smp-spawn-on-hart-1-runs` integration scenario to prove that a
/// task spawned cross-hart actually executes on the target.
/// `Relaxed`: counter.
pub static PROBE_TICKS: DeferredCounter = DeferredCounter::new("snitchos.smp.hart_1_probe_ticks_total");

/// Demo task spawned via `spawn_on(1, "hart_1_probe", probe_entry)`
/// from kmain. Increments `PROBE_TICKS` and yields. Existence on
/// hart 1's runqueue is what `spawn_on` produces; the increments are
/// what `yield_now` on hart 1 executes after picking it.
pub extern "C" fn probe_entry() -> ! {
    loop {
        // A span tagged with this hart's id (via `span_start_id`'s
        // `current_hartid()`). It is the wire-format proof that a
        // cross-hart-spawned task both *runs on* and is *attributed to*
        // hart 1 — drives `smp-spans-carry-hart-id` and
        // `smp-ipi-wakes-idle-hart`. RAII: closes at the end of the
        // loop body.
        crate::span!("hart1.probe");
        PROBE_TICKS.inc();
        crate::sched::yield_now();
    }
}

/// Cumulative `smp4.tick`s across every hart running the `smp4` worker.
/// `Relaxed`: a plain counter, drained by the heartbeat.
pub static SMP4_WORKER_TICKS: DeferredCounter =
    DeferredCounter::new("snitchos.smp4.worker_ticks_total");

/// Worker for the `smp4` four-hart demo, spawned on every secondary hart. Each
/// iteration opens a `smp4.tick` span — tagged with this hart's id via
/// `current_hartid()` at span open — bumps [`SMP4_WORKER_TICKS`], and yields, so
/// the wire carries `smp4.tick` spans attributed to every hart that ran one. That
/// per-hart attribution is what `smp-four-harts-all-run` asserts.
pub extern "C" fn smp4_worker_entry() -> ! {
    loop {
        crate::span!("smp4.tick");
        SMP4_WORKER_TICKS.inc();
        crate::sched::yield_now();
    }
}

/// Stash boot-time values the secondary's asm + Rust need to read.
/// **Call from hart 0 before `sbi::hart_start`.**
///
/// # Safety
///
/// Single-call from hart 0 boot path. Writes to `static mut`s; the
/// data hazard is "secondary hart starts reading before we finish
/// writing." SBI `hart_start` has a Release-style barrier in `OpenSBI`
/// (it touches CLINT MSIP, which is an MMIO write — fully ordered
/// w.r.t. prior loads/stores on the issuing hart). So as long as
/// `prepare_for_secondary` runs before `sbi::hart_start`, hart 1's
/// reads see the published values.
pub unsafe fn prepare_for_secondary(logical_id: usize) {
    // Snapshot current SATP — same root PT that hart 0 is running on.
    let satp: u64;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp); }

    // Address of one-past-end of this hart's stack slot. `sp` starts here and
    // grows down through `SECONDARY_STACKS[logical_id]`. (The asm reads `la
    // SECONDARY_STACK_TOP` for this value, so the slot must hold the stack top.)
    //
    // SAFETY: `&raw const` takes the slot's address without forming a reference or
    // reading the static; the rest is plain usize math. `logical_id < num_harts <=
    // MAX_HARTS` (the array length), so the index is in bounds. (Indexing a
    // `static mut` is what needs the `unsafe`, unlike a direct `&raw const STATIC`.)
    let stack_top = unsafe {
        (&raw const SECONDARY_STACKS[logical_id] as usize)
            + core::mem::size_of::<SecondaryStack>()
    };

    unsafe {
        // Stack-top address. The asm `la t0, X; ld sp, 0(t0)`
        // expects this slot to hold the higher-half VA of the top
        // of SECONDARY_STACK.
        let top_slot = &raw mut SECONDARY_STACK_TOP;
        top_slot.write(stack_top as u64);

        // SATP for the secondary to install.
        let satp_slot = &raw mut SECONDARY_SATP;
        satp_slot.write(satp);

        // KERNEL_OFFSET for the secondary's trampoline.
        let off_slot = &raw mut KERNEL_OFFSET_VALUE;
        off_slot.write(mmu::KERNEL_OFFSET as u64);
    }
}

/// Entry point from `_secondary_start` after SATP + trampoline.
/// PC + sp are at higher-half VAs. `tp` is still whatever `OpenSBI`
/// left it as — `percpu::init` is the first thing we do.
///
/// `a0 = mhartid` (SBI handoff), `a1 = opaque` (logical hartid we
/// passed to `sbi_hart_start`).
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(mhartid: usize, hartid: usize) -> ! {
    // Set tp = &PER_HART_DATA[hartid]. From here on
    // current_hartid() returns the right value.
    //
    // SAFETY: post-trampoline (higher-half VAs resolve); called
    // exactly once per hart at bring-up.
    unsafe { percpu::init(hartid) };

    // Announce on the wire. The collector resolves the hart_id on
    // subsequent frames to this register.
    tracing::emit_hart_register(
        hartid as u8,
        mhartid as u64,
        protocol::HartRole::Worker,
    );

    // Tell hart 0 it's safe to call unmap_identity now: we've
    // trampolined, we're running purely from higher-half, the
    // identity gigapage can go away without taking us down.
    //
    // Release: any state we wrote that hart 0 needs to see (the
    // HartRegister frame's intern-table entries) is published
    // before hart 0's Acquire load below.
    // Install the trap vector (same `trap_entry` hart 0 uses, lives
    // at a higher-half VA). Then enable software interrupts (IPIs)
    // and timer interrupts. Hart 1 needs its own timer cadence: each
    // hart has independent `stimecmp`, and without a wakeup source
    // hart 1's idle-style loop would `wfi` forever after the first
    // task yields back.
    //
    // SAFETY: post-MMU, trap_entry's higher-half VA resolves;
    // `init_timer` arms *this hart's* stimecmp and enables STIE +
    // SIE; `enable_software_interrupts` enables SSIE.
    unsafe {
        crate::trap::set_trap_vector();
        let interval = crate::trap::TIMER_INTERVAL_TICKS
            .load(Ordering::Relaxed);
        crate::trap::init_timer(interval);
        crate::trap::enable_software_interrupts();
    }

    SECONDARY_READY.fetch_or(1 << hartid, Ordering::Release);

    // v0.6 step 10: enroll in the scheduler as this hart's "main"
    // task. From here on `current_task_id()` returns our id and
    // `yield_now()` cycles through this hart's runqueue. Any task
    // someone has `spawn_on(1, ...)`d will land here.
    let _ = crate::sched::register_bare_task(
        "hart_1_main",
        kernel_proc::sched::TaskState::Running,
    );

    // SMP-cooperative idle loop: yield first (picks up any queued task), then
    // sleep until the next timer IRQ or an IPI breaks wfi. Only `wfi` when the
    // runqueue is genuinely empty — `yield_now` can return to us while another
    // task is still `Ready` (round-robin/aging rotated back here), and sleeping
    // then would strand that task (e.g. a just-woken IPC receiver) until the
    // next timer tick. Each real sleep bumps SECONDARY_WFI_COUNT for the
    // `snitchos.smp.secondary_wfi_total` heartbeat metric.
    loop {
        crate::sched::yield_now();
        if !crate::sched::has_ready_tasks() {
            unsafe { core::arch::asm!("wfi", options(nostack, preserves_flags)); }
            SECONDARY_WFI_COUNT.inc();
        }
    }
}
