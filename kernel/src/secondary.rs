//! Secondary-hart bring-up: the Rust side of `_secondary_start`.
//!
//! Boot-hart side (in kmain): stash SATP + KERNEL_OFFSET into the
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
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::mmu;
use crate::percpu;
use crate::tracing;

global_asm!(include_str!("secondary.S"));

/// 16 KiB stack for the secondary hart. Aligned to 16 bytes for the
/// RISC-V ABI. `#[unsafe(no_mangle)]` so the asm can `la` it.
const SECONDARY_STACK_SIZE: usize = 16 * 1024;
#[repr(C, align(16))]
struct SecondaryStack([u8; SECONDARY_STACK_SIZE]);

#[unsafe(no_mangle)]
static mut SECONDARY_STACK: SecondaryStack = SecondaryStack([0; SECONDARY_STACK_SIZE]);

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

/// KERNEL_OFFSET as a static so the asm can load it (the value
/// doesn't fit in an immediate). Initialised at boot.
#[unsafe(no_mangle)]
#[used]
static mut KERNEL_OFFSET_VALUE: u64 = 0;

/// Set by `secondary_main` after `HartRegister` emission. Hart 0
/// spin-waits on this before `unmap_identity` so it doesn't tear
/// down the mapping hart 1 is mid-trampoline through.
pub static SECONDARY_READY: AtomicBool = AtomicBool::new(false);

/// Cumulative count of `wfi`s on the secondary hart. Bumped each
/// time the idle loop completes a sleep. Useful as a "hart 1 is
/// alive" signal in dashboards. `Relaxed`: per-hart counter.
pub static SECONDARY_WFI_COUNT: AtomicU64 = AtomicU64::new(0);

/// Stash boot-time values the secondary's asm + Rust need to read.
/// **Call from hart 0 before `sbi::hart_start`.**
///
/// # Safety
///
/// Single-call from hart 0 boot path. Writes to `static mut`s; the
/// data hazard is "secondary hart starts reading before we finish
/// writing." SBI hart_start has a Release-style barrier in OpenSBI
/// (it touches CLINT MSIP, which is an MMIO write — fully ordered
/// w.r.t. prior loads/stores on the issuing hart). So as long as
/// `prepare_for_secondary` runs before `sbi::hart_start`, hart 1's
/// reads see the published values.
pub unsafe fn prepare_for_secondary() {
    // Snapshot current SATP — same root PT that hart 0 is running on.
    let satp: u64;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp); }

    // Address of one-past-end of the secondary stack. Hart 1's sp
    // starts here and grows down through SECONDARY_STACK.
    //
    // SAFETY: SECONDARY_STACK is a static; computing its end address
    // does not deref the memory. We write the symbol — the asm
    // reads `la SECONDARY_STACK_TOP`, which yields the address of
    // the static (not its value); so we need the *value at that
    // address* to be the stack top.
    let stack_top = unsafe {
        (&raw const SECONDARY_STACK as usize) + core::mem::size_of::<SecondaryStack>()
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
/// PC + sp are at higher-half VAs. `tp` is still whatever OpenSBI
/// left it as — `percpu::init` is the first thing we do.
///
/// `a0 = mhartid` (SBI handoff), `a1 = opaque` (logical hartid we
/// passed to sbi_hart_start).
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(_mhartid: usize, hartid: usize) -> ! {
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
        _mhartid as u64,
        protocol::HartRole::Worker,
    );

    // Tell hart 0 it's safe to call unmap_identity now: we've
    // trampolined, we're running purely from higher-half, the
    // identity gigapage can go away without taking us down.
    //
    // Release: any state we wrote that hart 0 needs to see (the
    // HartRegister frame's intern-table entries) is published
    // before hart 0's Acquire load below.
    SECONDARY_READY.store(true, Ordering::Release);

    // v0.6 step 8 scope: just idle. Step 10 adds per-hart runqueue
    // and an idle task that wfi's between yields; for now,
    // wfi-and-loop is the worker.
    loop {
        unsafe { core::arch::asm!("wfi", options(nostack, preserves_flags)); }
        SECONDARY_WFI_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}
