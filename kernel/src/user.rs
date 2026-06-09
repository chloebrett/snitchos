//! Userspace program embedding and loading (v0.7a).
//!
//! The first userspace program, `user/hello`, is baked into the kernel
//! image at build time. `build.rs` resolves the path: the freshly-built
//! artifact when building via `cargo xtask build` (which compiles `hello`
//! first and passes `SNITCHOS_USER_ELF`), otherwise the committed fixture
//! `kernel-core/fixtures/hello.elf`.
//!
//! [`load`] parses the embedded ELF with [`kernel_core::elf`] and maps its
//! segments into the boot page table's low half with the `U` bit set;
//! [`enter`] drops to U-mode at the entry point. v0.7a Step A loads into the
//! shared boot table; Step B will load into a per-process root instead.

use kernel_core::elf::{self, SegmentPerms};
use kernel_core::mmu::{MapError, PtePerms};
use protocol::StringId;

use crate::frame::{self, FRAME_SIZE};
use crate::sync::Once;
use crate::{mmu, tracing};

/// The embedded `user/hello` ELF image (a static, position-dependent
/// RISC-V executable linked at `0x1000_0000`).
pub static HELLO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_ELF"));

/// The counter the `EmitMetric` syscall bumps. Registered once on hart 0
/// (`init_metric`) so the `MetricRegister` frame isn't emitted from inside
/// the trap handler; the handler (on hart 1) reads it via [`user_metric_id`].
static USER_METRIC: Once<StringId> = Once::new();

/// A loaded program, ready to enter.
pub struct Loaded {
    /// The entry-point VA (`e_entry`) to put in `sepc`.
    pub entry: usize,
}

/// Why loading the embedded program failed.
#[derive(Debug)]
#[allow(dead_code, reason = "fields are surfaced via Debug in the load-failure panic")]
pub enum LoadError {
    /// The embedded image is not a valid ELF we can load.
    Parse(elf::ElfError),
    /// The frame allocator is exhausted.
    OutOfFrames,
    /// Installing a page-table entry failed.
    Map(MapError),
}

/// Register the userspace telemetry counter. Call once at boot, before
/// entering U-mode, so the syscall handler can emit without interning in
/// trap context.
pub fn init_metric() {
    USER_METRIC.call_once(|| tracing::register_counter("snitchos.user.telemetry_total"));
}

/// The `StringId` for the userspace telemetry counter, or `None` if
/// [`init_metric`] hasn't run. Read by the `EmitMetric` syscall handler.
pub fn user_metric_id() -> Option<StringId> {
    USER_METRIC.get().copied()
}

/// Hart-1 entry for the `workload=userspace` demo: load the embedded
/// program into the boot table's low half and drop to U-mode. Never
/// returns — the hart runs userspace from here (the program ecalls once,
/// then spins; the timer IRQ keeps trapping through but returns to it).
pub extern "C" fn user_main_entry() -> ! {
    match load() {
        Ok(loaded) => enter(loaded),
        Err(e) => panic!("userspace load failed: {e:?}"),
    }
}

/// Translate ELF segment R/W/X flags into page-table perms, always with
/// the `U` bit so U-mode may access the page.
fn perms_for(p: SegmentPerms) -> PtePerms {
    let mut perms = PtePerms::U;
    if p.read {
        perms = perms.union(PtePerms::R);
    }
    if p.write {
        perms = perms.union(PtePerms::W);
    }
    if p.exec {
        perms = perms.union(PtePerms::X);
    }
    perms
}

/// Parse [`HELLO_ELF`] and map every `PT_LOAD` segment into the boot page
/// table's low half: one fresh zeroed frame per page, mapped `U` + the
/// segment's perms, with the segment's file bytes copied in (the rest left
/// zero — that's the bss). Returns the entry point.
pub fn load() -> Result<Loaded, LoadError> {
    let plan = elf::parse(HELLO_ELF).map_err(LoadError::Parse)?;

    for seg in &plan.segments {
        let perms = perms_for(seg.perms);
        let page_start = seg.vaddr & !(FRAME_SIZE - 1);
        let page_end = (seg.vaddr + seg.mem_size + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);

        let mut page_va = page_start;
        while page_va < page_end {
            let f = frame::alloc_zeroed().ok_or(LoadError::OutOfFrames)?;
            mmu::map(page_va, f.addr(), perms).map_err(LoadError::Map)?;

            // Copy the slice of the segment's file bytes that lands in this
            // page. The frame is fresh-zeroed, so any tail beyond file_size
            // (the bss) is already 0.
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.file_size;
            let lo = file_lo.max(page_va);
            let hi = file_hi.min(page_va + FRAME_SIZE);
            if lo < hi {
                let src = seg.file_offset + (lo - file_lo);
                let dst = f.kernel_va() + (lo - page_va);
                // SAFETY: `dst` is this fresh frame's linear-map VA (writable,
                // covers all RAM); the copy length is at most one page; `src`
                // is in-bounds of HELLO_ELF (the parser validated the segment
                // file range against the image length).
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        HELLO_ELF.as_ptr().add(src),
                        dst as *mut u8,
                        hi - lo,
                    );
                }
            }
            page_va += FRAME_SIZE;
        }
    }

    Ok(Loaded { entry: plan.entry })
}

// sstatus field masks for the enter sequence.
const SPP: usize = 1 << 8; // Previous Privilege: clear -> return to U
const SPIE: usize = 1 << 5; // Previous Interrupt Enable: set -> SIE=1 after sret
const SUM: usize = 1 << 18; // Supervisor User Memory access: clear -> S can't touch U pages
const FS: usize = 0b11 << 13; // FP state: clear -> Off (kernel + program are integer-only)
const SIE: usize = 1 << 1; // Interrupt Enable (live): clear before arming sscratch

/// Drop to U-mode at `loaded.entry`. Never returns.
///
/// Order is load-bearing: clear `SIE` (mask interrupts) *before* arming
/// `sscratch`, so a stray timer IRQ can't see a nonzero `sscratch` in
/// S-mode and mis-take the from-user path in `trap_entry`. `sret` then
/// atomically drops to U *and* restores `SIE` from `SPIE`.
pub fn enter(loaded: Loaded) -> ! {
    // SAFETY: forges a trap-return into U-mode. `sscratch` is armed with
    // this hart's kernel sp so the eventual ecall trap switches onto it;
    // sstatus is set for U-mode entry with interrupts on, SUM off, FP off.
    unsafe {
        core::arch::asm!(
        "csrc sstatus, {clear}",
        "csrs sstatus, {set}",
        "csrw sscratch, sp",
        "csrw sepc, {entry}",
        "sret",
        clear = in(reg) (SPP | SUM | FS | SIE),
        set = in(reg) (SPIE),
        entry = in(reg) loaded.entry,
        options(noreturn));
    }
}
