//! Boot-time identity page table and the `csrw satp` / `sfence.vma`
//! bridge.
//!
//! The page-table type and PTE encoding live in `kernel_core::mmu` —
//! all bit-twiddling, host-tested. This module owns the singleton
//! statics and the asm that flips translation on.
//!
//! See `plans/v0.4-memory-step-1-satp-on.md` for the design and
//! decisions; `plans/v0.4-memory-concepts.md` for the Sv39 background.

use core::arch::asm;

use fdt::Fdt;
use kernel_core::mmu::{self as core_mmu, MapError, PageTable, PtMem, PtePerms, leaf_pte};

pub use kernel_core::mmu::{KERNEL_OFFSET, LINEAR_OFFSET, va_to_pa};

/// 2 MiB — the page size for every leaf in our boot table.
const PAGE_2MIB: usize = 2 * 1024 * 1024;

/// 2 MiB-aligned base of the MMIO region on QEMU `virt`. Covers the
/// NS16550A UART at `0x10000000` plus the eight virtio-mmio slots at
/// `0x10001000+`. Hardcoded here while DTB-driven discovery is
/// parked (see `collect_mmio_regions`).
pub const QEMU_VIRT_MMIO_BASE: usize = 0x10000000;

/// Sv39 mode field value for the `satp` register.
const SATP_MODE_SV39: u64 = 8;

/// A set of 2 MiB-aligned physical bases to identity-map for MMIO.
/// Collected from the DTB before `mmu::enable` runs so that page-table
/// construction is decoupled from DTB traversal.
pub struct MmioRegions {
    bases: [usize; Self::CAP],
    len: usize,
}

impl MmioRegions {
    const CAP: usize = 16;

    pub const fn new() -> Self {
        Self { bases: [0; Self::CAP], len: 0 }
    }

    /// Insert a 2 MiB-aligned base if not already present. Silently
    /// drops the entry if the buffer is full (QEMU `virt` collapses
    /// to 1-2 distinct 2 MiB regions; 16 is plenty of headroom).
    pub fn insert(&mut self, base: usize) {
        let aligned = base & !(PAGE_2MIB - 1);
        for i in 0..self.len {
            if self.bases[i] == aligned {
                return;
            }
        }
        if self.len < Self::CAP {
            self.bases[self.len] = aligned;
            self.len += 1;
        }
    }

    pub fn as_slice(&self) -> &[usize] {
        &self.bases[..self.len]
    }
}

/// Walk the DTB for `ns16550a` and `virtio,mmio` nodes; return the set
/// of distinct 2 MiB-aligned bases covering them.
///
/// **Currently unused — `kmain` hardcodes the MMIO region instead.**
/// DTB iteration crashes pre-MMU under higher-half link in a way we
/// haven't isolated (see `plans/v0.4-memory-findings.md`). Kept here
/// for the day we figure it out.
#[expect(dead_code, reason = "DTB iter pre-MMU crashes under higher-half link — see findings")]
pub fn collect_mmio_regions(dtb: &Fdt) -> MmioRegions {
    let mut regions = MmioRegions::new();
    for node in dtb.all_nodes() {
        let is_mmio = node
            .compatible()
            .map(|c| {
                c.all()
                    .any(|s| s == "ns16550a" || s == "virtio,mmio")
            })
            .unwrap_or(false);
        if !is_mmio {
            continue;
        }
        let Some(reg) = node.reg().and_then(|mut r| r.next()) else {
            continue;
        };
        regions.insert(reg.starting_address as usize);
    }
    regions
}

unsafe extern "C" {
    /// Start of the kernel image (linker symbol, see linker.ld).
    static __kernel_start: u8;
    /// One past the end of the kernel image, including stack.
    static __kernel_end: u8;
}

/// The boot page table. One root plus four mid-level tables:
/// - `BOOT_PT_MID_MMIO` for the identity gigapage covering MMIO
///   (around `0x10000000`).
/// - `BOOT_PT_MID_KERNEL` for the identity gigapage covering the
///   kernel image and DTB (around `0x80200000`).
/// - `BOOT_PT_MID_HIGHER_KERNEL` for the higher-half gigapage covering
///   the kernel image at `KERNEL_OFFSET + 0x80200000`.
/// - `BOOT_PT_MID_HIGHER_MMIO` for the higher-half gigapage covering
///   MMIO at `KERNEL_OFFSET + 0x10000000`.
///
/// `static mut` so that `enable` can populate them once at boot and
/// the satp PPN field can name their physical addresses. After
/// `enable` writes satp they're functionally immutable — a future
/// "boot-only memory reclaimed" pass will move these into a
/// discardable linker section.
static mut BOOT_PT_ROOT: PageTable = PageTable::new();
static mut BOOT_PT_MID_KERNEL: PageTable = PageTable::new();
static mut BOOT_PT_MID_MMIO: PageTable = PageTable::new();
static mut BOOT_PT_MID_HIGHER_KERNEL: PageTable = PageTable::new();
static mut BOOT_PT_MID_HIGHER_MMIO: PageTable = PageTable::new();

/// Build the boot page table and turn the MMU on. Installs:
/// - Identity + higher-half mappings for the kernel image (covers
///   `.text`, `.rodata`, `.data`, `.bss`, stack).
/// - Identity mappings for `mmio_regions` (UART + virtio-mmio slots).
/// - Identity mapping for the 2 MiB page containing `dtb_phys` so
///   `&Fdt`-based DTB access still works after this returns.
///
/// Writes `satp` with Sv39 mode and the root PPN, then `sfence.vma`.
///
/// After this returns the kernel runs with paging on but still at
/// identity PC. The trampoline in `kmain` is what moves PC/sp to
/// higher-half; this function just builds the world the trampoline
/// needs.
///
/// # Safety
///
/// - Must be called exactly once.
/// - MMU must be off at entry (mode = Bare in `satp`).
/// - Must be called before any code that loads an absolute symbol VA
///   into a register (formatted `println!`, `dyn` dispatch via
///   absolute fn pointers, `trap_entry as *const () as usize`). With
///   higher-half link, those values only resolve once the higher-half
///   mapping is live.
#[allow(
    clippy::deref_addrof,
    reason = "`&mut *(&raw mut BOOT_PT_*)` is the required raw-pointer-to-static reference idiom; clippy's deref_addrof misreads `*(&raw mut X)` as a redundant `*&`"
)]
pub unsafe fn enable(mmio_regions: &MmioRegions, dtb_phys: usize) {
    // SAFETY: linker symbols are addresses, not values. Take pointers,
    // never deref.
    let kernel_start = (&raw const __kernel_start) as usize;
    let kernel_end = (&raw const __kernel_end) as usize;

    let perms = PtePerms::rwxg();

    // SAFETY: `enable` is documented to run exactly once at boot, so
    // no concurrent reads of BOOT_PT_*. Population finishes before
    // satp is written.
    unsafe {
        let mid_kernel_pa = (&raw const BOOT_PT_MID_KERNEL) as usize;
        let mid_higher_pa = (&raw const BOOT_PT_MID_HIGHER_KERNEL) as usize;
        let mid_mmio_pa = (&raw const BOOT_PT_MID_MMIO) as usize;
        let mid_higher_mmio_pa = (&raw const BOOT_PT_MID_HIGHER_MMIO) as usize;

        // Three small helpers, one per mid table, hiding the
        // `&mut *(&raw mut STATIC)` dance that would otherwise repeat
        // at every call site.
        let map_id_kernel = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_KERNEL),
                mid_kernel_pa, va, pa, perms,
            );
        };
        let map_higher_kernel = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_HIGHER_KERNEL),
                mid_higher_pa, va, pa, perms,
            );
        };
        let map_id_mmio = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_MMIO),
                mid_mmio_pa, va, pa, perms,
            );
        };
        let map_higher_mmio = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_HIGHER_MMIO),
                mid_higher_mmio_pa, va, pa, perms,
            );
        };

        // Kernel image: dual-mapped at identity (where the kernel runs
        // at boot) and higher-half (where it runs after the
        // trampoline). Identity gets unmapped in `unmap_identity_kernel`.
        let kstart_aligned = kernel_start & !(PAGE_2MIB - 1);
        let kend_aligned = (kernel_end + PAGE_2MIB - 1) & !(PAGE_2MIB - 1);
        let mut addr = kstart_aligned;
        while addr < kend_aligned {
            map_id_kernel(addr, addr);
            map_higher_kernel(addr + KERNEL_OFFSET, addr);
            addr += PAGE_2MIB;
        }

        // MMIO: dual-mapped at identity (used by `init_handshake`
        // before `CONSOLE` is set, and by the panic handler via
        // `emergency_uart_base` when satp is 0) and higher-half (used
        // by `CONSOLE`/`UART` after init, and after `unmap_identity`
        // tears down the identity copy). Caller builds `mmio_regions`
        // (currently hardcoded in `kmain` for QEMU `virt`).
        for &base in mmio_regions.as_slice() {
            map_id_mmio(base, base);
            map_higher_mmio(base + KERNEL_OFFSET, base);
        }

        // Linear map: one 1 GiB Sv39 huge-page leaf installed directly
        // in the root, mapping
        // `[LINEAR_OFFSET + 0x80000000, LINEAR_OFFSET + 0xC0000000)`
        // to physical `[0x80000000, 0xC0000000)`. Covers all of
        // QEMU `virt`'s RAM up to 1 GiB; platforms with more RAM
        // would need additional leaves.
        //
        // This is the mapping the frame allocator will use to give
        // any allocated frame a kernel-reachable VA via
        // `pa_to_kernel_va`.
        let linear_va = LINEAR_OFFSET + 0x80000000;
        let linear_idx = (linear_va >> 30) & 0x1ff;
        let linear_leaf = leaf_pte(0x80000000, perms);
        (&mut *(&raw mut BOOT_PT_ROOT)).set_entry(linear_idx, linear_leaf);

        // DTB region. Kernel keeps using `&Fdt` after this returns
        // (timebase_hz, uart_addr, virtio_console::init). One 2 MiB
        // page covers any sane DTB (< 64 KiB). Routes through whichever
        // identity mid table covers its gigapage.
        let dtb_aligned = dtb_phys & !(PAGE_2MIB - 1);
        if (dtb_aligned >> 30) == (kernel_start >> 30) {
            map_id_kernel(dtb_aligned, dtb_aligned);
        } else {
            map_id_mmio(dtb_aligned, dtb_aligned);
        }

        // Turn it on. PPN field is bits 43:0; MODE in bits 63:60.
        let root_pa = (&raw const BOOT_PT_ROOT) as usize;
        let satp_value = (SATP_MODE_SV39 << 60) | ((root_pa as u64) >> 12);
        asm!(
            "csrw satp, {satp}",
            "sfence.vma",
            satp = in(reg) satp_value,
            options(nostack),
        );
    }
}

/// Tear down both identity mappings: the kernel-image gigapage
/// (`[0x80000000, 0xC0000000)` = root entry 2) and the MMIO gigapage
/// (`[0x00000000, 0x40000000)` = root entry 0). After this returns,
/// any access to an identity-half VA faults — the kernel must use
/// higher-half VAs exclusively, including for MMIO.
///
/// # Safety
///
/// - MMU must be on with the dual-map installed by `enable`, and the
///   higher-half MMIO mapping must be live.
/// - Kernel must currently be running at higher-half PC + sp
///   (trampoline already executed). Calling this while at identity
///   PC would yank the rug out from under the running instruction
///   stream.
/// - `CONSOLE` and `UART` statics must already hold higher-half VAs,
///   and the panic-handler / `_pre_init_uart` paths must already
///   route through `emergency_uart_base()`. Otherwise the next print
///   faults.
/// - DTB region (which lived in the identity kernel gigapage) becomes
///   unreachable after this. Caller must not read through `&Fdt`
///   afterwards.
#[allow(
    clippy::deref_addrof,
    reason = "`&mut *(&raw mut BOOT_PT_ROOT)` is the required raw-pointer-to-static reference idiom; clippy's deref_addrof misreads `*(&raw mut X)` as a redundant `*&`"
)]
pub unsafe fn unmap_identity() {
    unsafe {
        let root = &mut *(&raw mut BOOT_PT_ROOT);
        // Root entry 0: identity [0x00000000, 0x40000000) — MMIO.
        root.set_entry(0, 0);
        // Root entry 2: identity [0x80000000, 0xC0000000) — kernel
        // image, stack, DTB.
        root.set_entry(2, 0);
        asm!("sfence.vma", options(nostack, nomem));
    }
}

/// `PtMem` impl backed by the kernel's frame allocator and the
/// linear map. Tables live as 4 KiB physical frames; the kernel
/// dereferences them at `pa_to_kernel_va(pa)`.
struct KernelPtMem;

impl PtMem for KernelPtMem {
    fn alloc_zeroed_table(&mut self) -> Option<usize> {
        crate::frame::alloc_zeroed().map(|f| f.addr())
    }

    fn read_entry(&self, table_pa: usize, idx: usize) -> u64 {
        let ptr = kernel_core::mmu::pa_to_kernel_va(table_pa) as *const u64;
        // SAFETY: `table_pa` was either returned by `alloc_zeroed_table`
        // (a frame the allocator handed us, reachable via the linear
        // map) or is the root table's PA. `idx` is in 0..512 — caller
        // contract; `map`'s walk only ever uses `vpn[]` indices. Single
        // hart, single-threaded use during a `map` call.
        unsafe { ptr.add(idx).read_volatile() }
    }

    fn write_entry(&mut self, table_pa: usize, idx: usize, value: u64) {
        let ptr = kernel_core::mmu::pa_to_kernel_va(table_pa) as *mut u64;
        // SAFETY: same as `read_entry`. `&mut self` ensures no
        // concurrent reader on this impl during the write; the MMU
        // walker is the only other reader and we sfence in the
        // wrapper after the whole `map` call.
        unsafe { ptr.add(idx).write_volatile(value) };
    }
}

/// Install a 4 KiB leaf PTE mapping VA `va` → PA `pa` with `perms`
/// in the kernel's live page table. Allocates intermediate tables
/// via `frame::alloc_zeroed` if needed. On success, flushes the TLB
/// entry for `va` on this hart via `sfence.vma`.
///
/// Returns `Err(MapError::AlreadyMapped)` if any walked PTE
/// conflicts (huge-page leaf or existing 4 KiB leaf), or
/// `Err(MapError::OutOfFrames)` if the frame allocator is empty.
///
/// # Safety
///
/// - Must run with MMU on (the wrapper accesses tables through the
///   linear map at `pa_to_kernel_va`).
/// - Caller must ensure the VA range isn't already in use for
///   something else the kernel relies on. The walk catches
///   already-mapped collisions, but doesn't reason about
///   higher-level intent (e.g. "this VA belongs to the heap range").
/// - v0.6 step 9: `map` does a **local** `sfence.vma` only. New
///   mappings (which is what `map` is — `core_mmu::map` errors out if
///   the leaf PTE is already valid) don't need cross-hart broadcast:
///   no other hart can have a stale TLB entry for a VA that was
///   previously unmapped; they'd just take a fault and walk the new
///   PTE. Cross-hart `shootdown(va)` is the primitive for **remap**
///   and **unmap** flows (not yet wired into mmu::map proper).
pub fn map(va: usize, pa: usize, perms: PtePerms) -> Result<(), MapError> {
    let root_pa = va_to_pa((&raw const BOOT_PT_ROOT) as usize);
    let mut mem = KernelPtMem;
    let result = core_mmu::map(root_pa, va, pa, perms, &mut mem);
    if result.is_ok() {
        // SAFETY: single instruction, register operand. Invalidates
        // the TLB entry for `va` on this hart only.
        unsafe { asm!("sfence.vma {0}, zero", in(reg) va, options(nostack, nomem)) };
    }
    result
}

/// Cumulative count of TLB shootdowns this hart has initiated as a
/// sender (i.e. how many `mmu::map`/`unmap` calls actually fired).
/// Drained by the heartbeat as
/// `snitchos.mmu.shootdowns_sent_total`. `Relaxed`: counter.
pub static SHOOTDOWNS_SENT_TOTAL: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Invalidate the TLB entry for `va` on this hart locally, then ensure
/// every other online hart does the same before returning.
///
/// Protocol (the 7-step handshake from `kernel::percpu`'s home doc):
///
///   1. Local `sfence.vma va` — covers the calling hart.
///   2. For each other online hart `t`:
///      - Write `t.shootdown_va = va`
///      - Snapshot `t.shootdown_ack` as `pre`
///      - Send `IPI_TLB_SHOOTDOWN` (the `fetch_or` on
///        `t.ipi_pending` is `Release`, publishing `shootdown_va`)
///      - Raise the SBI IPI
///   3. Spin-wait on each target's `shootdown_ack` (`Acquire`)
///      until it exceeds `pre`. Once true, that target has run its
///      sfence and the new mapping is universally visible.
///
/// Skips harts not in `SMP_ONLINE_HARTS`. v0.6 boot calls
/// `heap::init` before hart 1 is online; the bitmap check makes
/// those calls a no-op for the offline hart.
#[expect(
    dead_code,
    reason = "TLB-shootdown IPI path for SMP; not called until multi-hart bring-up wires it in"
)]
#[allow(
    clippy::needless_range_loop,
    reason = "`target` is also used as a hart-bitmask shift (`1 << target`), so the index is load-bearing, not pure iteration"
)]
pub fn shootdown(va: usize) {
    // (1) local first — covers the calling hart even if there are no
    // other online harts to ack.
    //
    // SAFETY: single instruction, register operand. Flushes the TLB
    // entry for `va` on this hart (any ASID).
    unsafe { asm!("sfence.vma {0}, zero", in(reg) va, options(nostack, nomem)) };

    let me = crate::percpu::current_hartid();
    let online = crate::percpu::SMP_ONLINE_HARTS
        .load(core::sync::atomic::Ordering::Relaxed);

    // (2) publish shootdown_va + snapshot acks for each target. We
    // do this in a loop so all sends complete before any spin-wait,
    // which lets multiple targets fence in parallel.
    let mut pre_acks = [0u64; crate::percpu::MAX_HARTS];
    let mut targeted = 0u64;
    for target in 0..crate::percpu::MAX_HARTS {
        if target == me {
            continue;
        }
        if online & (1u64 << target) == 0 {
            continue; // hart not online yet — sfence on it is moot
        }
        let slot = &crate::percpu::PER_HART_DATA[target];
        slot.shootdown_va
            .store(va as u64, core::sync::atomic::Ordering::Relaxed);
        pre_acks[target] = slot
            .shootdown_ack
            .load(core::sync::atomic::Ordering::Relaxed);
        crate::ipi::send(target, crate::ipi::IPI_TLB_SHOOTDOWN);
        targeted |= 1u64 << target;
    }

    // (3) spin-wait each targeted hart's ack to advance.
    for target in 0..crate::percpu::MAX_HARTS {
        if targeted & (1u64 << target) == 0 {
            continue;
        }
        let slot = &crate::percpu::PER_HART_DATA[target];
        while slot
            .shootdown_ack
            .load(core::sync::atomic::Ordering::Acquire)
            <= pre_acks[target]
        {
            core::hint::spin_loop();
        }
    }

    SHOOTDOWNS_SENT_TOTAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

