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

pub use kernel_core::mmu::{KERNEL_OFFSET, va_to_pa};

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

/// The boot page table. One root plus three mid-level tables:
/// - `BOOT_PT_MID_MMIO` for the identity gigapage covering MMIO
///   (around `0x10000000`).
/// - `BOOT_PT_MID_KERNEL` for the identity gigapage covering the
///   kernel image and DTB (around `0x80200000`).
/// - `BOOT_PT_MID_HIGHER_KERNEL` for the higher-half gigapage covering
///   the kernel image at `KERNEL_OFFSET + 0x80200000`.
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

/// Build the boot page table and turn the MMU on. Installs:
/// - Identity + higher-half mappings for the kernel image (covers
///   `.text`, `.rodata`, `.data`, `.bss`, stack).
/// - Identity mappings for `mmio_regions` (UART + virtio-mmio slots).
/// - Identity mapping for the 2 MiB page containing `dtb_phys` so
///   `&Fdt`-based DTB access still works after this returns.
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

        // MMIO: identity-map each pre-collected 2 MiB-aligned base.
        // Caller builds `mmio_regions` (currently hardcoded in `kmain`
        // for QEMU `virt`; `collect_mmio_regions` would do it from the
        // DTB but is parked — see findings).
        for &base in mmio_regions.as_slice() {
            map_id_mmio(base, base);
        }

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

/// Tear down the identity mapping for the kernel-image gigapage
/// (`[0x80000000, 0xC0000000)` = root entry 2). After this returns,
/// any access to an identity-VA kernel-image address (`0x80200000+`)
/// faults — the kernel must use higher-half VAs exclusively for its
/// code and statics.
///
/// **Keeps identity MMIO mapped** (root entry 0). `CONSOLE` and `UART`
/// statics still hold physical MMIO bases; the panic handler and the
/// `_pre_init_uart()` fallback still poke physical UART. Removing
/// identity-MMIO is a future checkpoint that requires patching those
/// + adding higher-half MMIO mappings.
///
/// # Safety
///
/// - MMU must be on with the dual-map installed by `enable`.
/// - Kernel must currently be running at higher-half PC + sp
///   (trampoline already executed). Calling this while at identity
///   PC would yank the rug out from under the running instruction
///   stream.
/// - DTB region (which lived in the identity kernel gigapage) becomes
///   unreachable after this. Caller must not read through `&Fdt`
///   afterwards.
pub unsafe fn unmap_identity_kernel() {
    unsafe {
        let root = &mut *(&raw mut BOOT_PT_ROOT);
        // Root entry 2 covers identity [0x80000000, 0xC0000000) — the
        // kernel image, stack, DTB, and any other identity-half data
        // in that gigapage.
        root.set_entry(2, 0);
        asm!("sfence.vma", options(nostack, nomem));
    }
}

