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
pub unsafe fn enable(dtb: &Fdt) {
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

        // MMIO: every distinct 2 MiB region containing an ns16550a or
        // virtio,mmio node. The QEMU `virt` layout puts UART at
        // 0x10000000 and the virtio-mmio slots at 0x10001000+ — all in
        // one 2 MiB region — but discovering from the DTB matches the
        // long-term shape and dedupes naturally via `map_2mib`'s
        // idempotency.
        let mid_mmio_pa = (&raw const BOOT_PT_MID_MMIO) as usize;
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
            let base = reg.starting_address as usize;
            let aligned = base & !(PAGE_2MIB - 1);
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_MMIO),
                mid_mmio_pa,
                aligned,
                aligned,
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
