//! Userspace program embedding and loading (v0.7a).
//!
//! Two programs are baked into the kernel image at build time: `user/hello`
//! (the `workload=userspace` demo — emits one telemetry syscall) and
//! `faulter` (the `workload=userspace-fault` isolation probe — reads a
//! kernel VA, which must fault). `build.rs` resolves each path: the
//! freshly-built artifact when building via `cargo xtask build`, else the
//! committed fixture under `kernel-core/fixtures/`.
//!
//! [`load`] parses an embedded ELF with [`kernel_core::elf`] and maps its
//! segments into a fresh per-process root page table (kernel high-half
//! shared in) with the `U` bit; [`enter`] switches `satp` and drops to
//! U-mode at the entry point.

use alloc::collections::BTreeMap;

use kernel_core::elf::{self, LoadSegment, SegmentPerms};
use kernel_core::mmu::{MapError, PtePerms};
use protocol::StringId;

use crate::frame::{self, FRAME_SIZE};
use crate::sync::Once;
use crate::{mmu, tracing};

/// The `workload=userspace` program: emits one telemetry syscall, then spins.
pub static HELLO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_ELF"));

/// The `workload=userspace-fault` program: emits a marker, then reads a
/// kernel VA to prove the `U`-bit firewall faults it.
pub static FAULTER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FAULTER_ELF"));

/// The counter the `EmitMetric` syscall bumps. Registered once on hart 0
/// (`init_metric`) so the `MetricRegister` frame isn't emitted from inside
/// the trap handler; the handler (on hart 1) reads it via [`user_metric_id`].
static USER_METRIC: Once<StringId> = Once::new();

/// The counter a U-mode page fault bumps — the isolation firewall doing its
/// job. Registered alongside [`USER_METRIC`]; read by the fault handler.
static USER_FAULT_METRIC: Once<StringId> = Once::new();

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

/// Register the userspace counters. Call once at boot, before entering
/// U-mode, so the syscall/fault handlers can emit without interning in trap
/// context.
pub fn init_metric() {
    USER_METRIC.call_once(|| tracing::register_counter("snitchos.user.telemetry_total"));
    USER_FAULT_METRIC.call_once(|| tracing::register_counter("snitchos.user.faults_total"));
}

/// The `StringId` for the userspace telemetry counter (or `None` pre-init).
pub fn user_metric_id() -> Option<StringId> {
    USER_METRIC.get().copied()
}

/// The `StringId` for the U-mode fault counter (or `None` pre-init).
pub fn user_fault_metric_id() -> Option<StringId> {
    USER_FAULT_METRIC.get().copied()
}

/// Hart-1 entry for `workload=userspace`: run the `hello` program.
pub extern "C" fn user_main_entry() -> ! {
    run(HELLO_ELF)
}

/// Hart-1 entry for `workload=userspace-fault`: run the isolation probe.
pub extern "C" fn faulter_main_entry() -> ! {
    run(FAULTER_ELF)
}

/// Build a fresh address space, load `image` into it, and drop to U-mode.
/// Never returns — the hart runs userspace from here.
fn run(image: &'static [u8]) -> ! {
    // Each process gets its own root page table (kernel high-half shared in).
    let root_pa = mmu::new_user_root().expect("userspace: no frame for user root page table");
    match load(root_pa, image) {
        Ok(loaded) => enter(loaded, root_pa),
        Err(e) => panic!("userspace load failed: {e:?}"),
    }
}

/// Translate ELF segment R/W/X flags into page-table perms, always with the
/// `U` bit so U-mode may access the page.
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

/// The page-aligned VAs a segment occupies in memory.
fn pages_of(seg: &LoadSegment) -> impl Iterator<Item = usize> {
    let start = seg.vaddr & !(FRAME_SIZE - 1);
    let end = (seg.vaddr + seg.mem_size + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
    (start..end).step_by(FRAME_SIZE)
}

/// Parse `image` and map its `PT_LOAD` segments into the page table rooted
/// at `root_pa`. Two segments may share a page (e.g. R-X code + R rodata in
/// the first page), so perms are unioned per page and each page is mapped
/// once; file bytes are then copied in and the bss tail left zero. Returns
/// the entry point.
pub fn load(root_pa: usize, image: &[u8]) -> Result<Loaded, LoadError> {
    let plan = elf::parse(image).map_err(LoadError::Parse)?;

    // Union perms over every page each segment touches.
    let mut perms_by_page: BTreeMap<usize, PtePerms> = BTreeMap::new();
    for seg in &plan.segments {
        let perms = perms_for(seg.perms);
        for page_va in pages_of(seg) {
            perms_by_page
                .entry(page_va)
                .and_modify(|p| *p = p.union(perms))
                .or_insert(perms);
        }
    }

    // Allocate a zeroed frame per page and map it; remember its linear-map VA
    // so the copy pass can reach it.
    let mut dst_by_page: BTreeMap<usize, usize> = BTreeMap::new();
    for (&page_va, &perms) in &perms_by_page {
        let f = frame::alloc_zeroed().ok_or(LoadError::OutOfFrames)?;
        mmu::map_in(root_pa, page_va, f.addr(), perms).map_err(LoadError::Map)?;
        dst_by_page.insert(page_va, f.kernel_va());
    }

    // Copy each segment's file bytes into the mapped frames.
    for seg in &plan.segments {
        let file_lo = seg.vaddr;
        let file_hi = seg.vaddr + seg.file_size;
        for page_va in pages_of(seg) {
            let lo = file_lo.max(page_va);
            let hi = file_hi.min(page_va + FRAME_SIZE);
            if lo >= hi {
                continue;
            }
            let dst = dst_by_page[&page_va] + (lo - page_va);
            let src = seg.file_offset + (lo - file_lo);
            // SAFETY: `dst` is a fresh frame's linear-map VA (writable, covers
            // all RAM); the copy length is at most one page; `src` is in-bounds
            // of `image` (the parser validated the segment file range).
            unsafe {
                core::ptr::copy_nonoverlapping(image.as_ptr().add(src), dst as *mut u8, hi - lo);
            }
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

/// Switch to the process's address space (`root_pa`) and drop to U-mode at
/// `loaded.entry`. Never returns.
///
/// `satp` is switched first: the kernel high-half is shared into `root_pa`,
/// so this function's own code/stack (and the trap path it's about to enter)
/// stay mapped across the switch. Order is then load-bearing: clear `SIE`
/// (mask interrupts) *before* arming `sscratch`, so a stray timer IRQ can't
/// see a nonzero `sscratch` in S-mode and mis-take the from-user path in
/// `trap_entry`. `sret` then drops to U *and* restores `SIE` from `SPIE`.
pub fn enter(loaded: Loaded, root_pa: usize) -> ! {
    let satp = mmu::satp_for(root_pa);
    // SAFETY: switches the active address space to the user root (kernel
    // high-half shared, so we keep executing), then forges a trap-return into
    // U-mode. `sscratch` is armed with this hart's kernel sp so the eventual
    // ecall trap switches onto it; sstatus is set for U-mode entry with
    // interrupts on, SUM off, FP off.
    unsafe {
        core::arch::asm!(
        "csrw satp, {satp}",
        "sfence.vma",
        "csrc sstatus, {clear}",
        "csrs sstatus, {set}",
        "csrw sscratch, sp",
        "csrw sepc, {entry}",
        "sret",
        satp = in(reg) satp,
        clear = in(reg) (SPP | SUM | FS | SIE),
        set = in(reg) (SPIE),
        entry = in(reg) loaded.entry,
        options(noreturn));
    }
}
