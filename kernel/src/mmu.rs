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
use kernel_core::mmu::{PageTable, PtePerms};

/// 2 MiB — the page size for every leaf in our boot table.
const PAGE_2MIB: usize = 2 * 1024 * 1024;

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

/// VA = PA + KERNEL_OFFSET for kernel-space mappings. Matches Linux
/// RISC-V's `PAGE_OFFSET - PHYS_BASE` with PHYS_BASE = 0x80000000.
/// The kernel image at PA 0x80200000 maps to higher-half VA
/// 0xffffffff_80200000.
///
/// **v0.4 step 2a state.** The higher-half mapping built below is
/// unused — the linker still places the kernel at identity VAs and
/// the kernel runs at those addresses. The higher-half entries exist
/// only to prove the table-building code works and to set up for
/// step 2c (linker change + early satp + move kernel to higher-half).
pub const KERNEL_OFFSET: usize = 0xffffffff_00000000;

/// Convert a kernel virtual address to its physical address. Strips
/// `KERNEL_OFFSET` if the VA is in the higher-half range; passes
/// identity-range VAs through unchanged.
///
/// Used at the boundary where the kernel hands an address to a device
/// (virtio queue addresses, DMA buffer pointers). Devices have no MMU
/// and treat the value as physical, so anywhere we'd otherwise pass
/// `&static as u64` or `slice.as_ptr() as u64` we route through this.
///
/// Pre-trampoline (PC at identity), `&static as usize` is PC-relative
/// and gives the physical address, so this function is a no-op.
/// Post-trampoline (PC at higher-half), `&static as usize` gives a
/// higher-half VA, and this strips `KERNEL_OFFSET`.
pub const fn va_to_pa(va: usize) -> usize {
    if va >= KERNEL_OFFSET {
        va - KERNEL_OFFSET
    } else {
        va
    }
}

unsafe extern "C" {
    /// Start of the kernel image (linker symbol, see linker.ld).
    static __kernel_start: u8;
    /// One past the end of the kernel image, including stack.
    static __kernel_end: u8;
}

/// The boot identity-mapping page table. One root + two mid-level
/// tables — one for the gigapage containing MMIO at 0x10000000, one
/// for the gigapage containing the kernel image around 0x80200000.
///
/// `static mut` so that `enable()` can populate them once at boot and
/// the satp PPN field can name their physical addresses. After the
/// satp write they're immutable in practice — a future "boot-only
/// memory reclaimed" pass will move these into a discardable linker
/// section.
static mut BOOT_PT_ROOT: PageTable = PageTable::new();
static mut BOOT_PT_MID_KERNEL: PageTable = PageTable::new();
static mut BOOT_PT_MID_MMIO: PageTable = PageTable::new();
/// Higher-half mid table for the gigapage containing the kernel image
/// at `KERNEL_OFFSET + 0x80200000` (= root index 510). Populated in
/// step 2a; unused until a future step relinks the kernel at
/// higher-half.
static mut BOOT_PT_MID_HIGHER_KERNEL: PageTable = PageTable::new();

/// Build the boot identity-mapping table from the kernel image bounds
/// (linker symbols) plus MMIO regions discovered in the DTB, then
/// write satp + sfence.vma. Kernel keeps running at the same virtual
/// addresses (identity).
///
/// # Safety
///
/// - Must be called exactly once.
/// - MMU must be off at entry (mode = Bare in `satp`).
/// - Every code address, stack address, and data pointer in use at
///   the call site must be inside one of the regions we identity-map
///   (kernel image + MMIO). The position-1 boot order (call just
///   before the heartbeat loop) satisfies this — see plan.
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
        // Kernel image: round both ends to 2 MiB so we cover the
        // whole image plus a bit of slop. Dual-mapped: identity (the
        // kernel runs there) AND higher-half (unused in step 2a;
        // proving the table-building code works).
        let mid_kernel_pa = (&raw const BOOT_PT_MID_KERNEL) as usize;
        let mid_higher_kernel_pa = (&raw const BOOT_PT_MID_HIGHER_KERNEL) as usize;
        let kstart_aligned = kernel_start & !(PAGE_2MIB - 1);
        let kend_aligned = (kernel_end + PAGE_2MIB - 1) & !(PAGE_2MIB - 1);
        let mut addr = kstart_aligned;
        while addr < kend_aligned {
            // Identity.
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_KERNEL),
                mid_kernel_pa,
                addr,
                addr,
                perms,
            );
            // Higher-half.
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_HIGHER_KERNEL),
                mid_higher_kernel_pa,
                addr + KERNEL_OFFSET,
                addr,
                perms,
            );
            addr += PAGE_2MIB;
        }

        // MMIO: identity-map each pre-collected 2 MiB-aligned base.
        // Pre-collection happens in `collect_mmio_regions` from the
        // DTB; we don't borrow `&Fdt` here.
        let mid_mmio_pa = (&raw const BOOT_PT_MID_MMIO) as usize;
        for &base in mmio_regions.as_slice() {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_MMIO),
                mid_mmio_pa,
                base,
                base,
                perms,
            );
        }

        // DTB region. The kernel keeps using `&Fdt` after this
        // function returns (timebase_hz, uart_addr, virtio_console::init),
        // so the DTB pages must be mapped. One 2 MiB page covers any
        // sane DTB (typically < 64 KiB).
        let dtb_aligned = dtb_phys & !(PAGE_2MIB - 1);
        let dtb_gig = dtb_aligned >> 30;
        let kernel_gig = kernel_start >> 30;
        if dtb_gig == kernel_gig {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_KERNEL),
                mid_kernel_pa,
                dtb_aligned,
                dtb_aligned,
                perms,
            );
        } else {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_MMIO),
                mid_mmio_pa,
                dtb_aligned,
                dtb_aligned,
                perms,
            );
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

