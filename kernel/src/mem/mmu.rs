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
use kernel_core::mmu::{self as core_mmu, MapError, PageTable, PtMem, Pte, PtePerms};

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
        Self {
            bases: [0; Self::CAP],
            len: 0,
        }
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
#[expect(
    dead_code,
    reason = "DTB iter pre-MMU crashes under higher-half link — see findings"
)]
pub fn collect_mmio_regions(dtb: &Fdt) -> MmioRegions {
    let mut regions = MmioRegions::new();
    for node in dtb.all_nodes() {
        let is_mmio = node
            .compatible()
            .map(|c| c.all().any(|s| s == "ns16550a" || s == "virtio,mmio"))
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
                mid_kernel_pa,
                va,
                pa,
                perms,
            );
        };
        let map_higher_kernel = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_HIGHER_KERNEL),
                mid_higher_pa,
                va,
                pa,
                perms,
            );
        };
        let map_id_mmio = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_MMIO),
                mid_mmio_pa,
                va,
                pa,
                perms,
            );
        };
        let map_higher_mmio = |va, pa| {
            (&mut *(&raw mut BOOT_PT_ROOT)).map_2mib(
                &mut *(&raw mut BOOT_PT_MID_HIGHER_MMIO),
                mid_higher_mmio_pa,
                va,
                pa,
                perms,
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
        let linear_leaf = Pte::leaf(0x80000000, perms);
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
        root.set_entry(0, Pte::INVALID);
        // Root entry 2: identity [0x80000000, 0xC0000000) — kernel
        // image, stack, DTB.
        root.set_entry(2, Pte::INVALID);
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

    fn read_entry(&self, table_pa: usize, idx: usize) -> Pte {
        let ptr = kernel_core::mmu::pa_to_kernel_va(table_pa) as *const u64;
        // SAFETY: `table_pa` was either returned by `alloc_zeroed_table`
        // (a frame the allocator handed us, reachable via the linear
        // map) or is the root table's PA. `idx` is in 0..512 — caller
        // contract; `map`'s walk only ever uses `vpn[]` indices. Single
        // hart, single-threaded use during a `map` call.
        Pte::from_raw(unsafe { ptr.add(idx).read_volatile() })
    }

    fn write_entry(&mut self, table_pa: usize, idx: usize, value: Pte) {
        let ptr = kernel_core::mmu::pa_to_kernel_va(table_pa) as *mut u64;
        // SAFETY: same as `read_entry`. `&mut self` ensures no
        // concurrent reader on this impl during the write; the MMU
        // walker is the only other reader and we sfence in the
        // wrapper after the whole `map` call.
        unsafe { ptr.add(idx).write_volatile(value.raw()) };
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
    let result = map_in(root_pa, va, pa, perms);
    if result.is_ok() {
        // SAFETY: single instruction, register operand. Invalidates
        // the TLB entry for `va` on this hart only.
        unsafe { asm!("sfence.vma {0}, zero", in(reg) va, options(nostack, nomem)) };
    }
    result
}

/// Install a 4 KiB leaf PTE in an **arbitrary** root page table (given by
/// its physical address), allocating intermediate tables as needed. Unlike
/// [`map`], does **no** `sfence` — the intended use is building an
/// *inactive* address space (e.g. a fresh user root before its `satp`
/// switch), where no TLB entries exist yet. The walk is the host-tested
/// `kernel_core::mmu::map`.
pub fn map_in(root_pa: usize, va: usize, pa: usize, perms: PtePerms) -> Result<(), MapError> {
    let mut mem = KernelPtMem;
    core_mmu::map(root_pa, va, pa, perms, &mut mem)
}

/// Copy `len` bytes from `src_va` in the address space rooted at `src_root` to
/// `dst_va` in `dst_root`, through the kernel's linear map — the cross-AS copy
/// behind the option-D `CopyFromCaller`/`CopyToCaller` syscalls. The walk +
/// per-page validation (`R|U` source / `W|U` dest) + chunking is the
/// host-tested `kernel_core::mmu::copy_across`; this wraps it with the kernel's
/// `KernelPtMem` (table reads) and the actual byte move via `pa_to_kernel_va`.
/// Returns bytes copied on success. No `satp` switch: both page tables are read
/// — and both resolved frames touched — through the linear map, which is mapped
/// into every address space.
pub fn copy_across(
    src_root: usize,
    src_va: usize,
    dst_root: usize,
    dst_va: usize,
    len: usize,
) -> Result<usize, core_mmu::CopyError> {
    let mem = KernelPtMem;
    core_mmu::copy_across(src_root, src_va, dst_root, dst_va, len, &mem, &mut |src_pa, dst_pa, n| {
        // SAFETY: both PAs come from the walker, resolved from validated, mapped
        // user leaves; the linear map covers all physical RAM, so each kernel VA
        // is valid for `n` bytes. Source and destination are distinct user
        // frames in two different address spaces — non-overlapping.
        unsafe {
            core::ptr::copy_nonoverlapping(
                core_mmu::pa_to_kernel_va(src_pa) as *const u8,
                core_mmu::pa_to_kernel_va(dst_pa) as *mut u8,
                n,
            );
        }
    })
    .map(|()| len)
}

/// Allocate a fresh root page table for a new (user) address space and
/// share the kernel's high half into it, returning the root's physical
/// address. Sv39 root slots 256..512 are the high half (kernel image,
/// linear map, heap); copying those root entries shares the whole kernel
/// mapping, so a trap/syscall needs no page-table switch and the kernel
/// stays reachable while userspace runs (the Q27a decision). The low half
/// (slots 0..256) is left unmapped — the loader fills it with `U` pages.
///
/// Returns `None` if the frame allocator is empty.
pub fn new_user_root() -> Option<usize> {
    let root_pa = crate::frame::alloc_zeroed()?.addr();
    let boot_root_pa = va_to_pa((&raw const BOOT_PT_ROOT) as usize);
    let mut mem = KernelPtMem;
    for idx in 256..512 {
        let entry = mem.read_entry(boot_root_pa, idx);
        mem.write_entry(root_pa, idx, entry);
    }
    Some(root_pa)
}

/// Reclaim every frame owned by the **user half** of the address space rooted at
/// `root_pa` — all mapped 4 KiB pages, the L0/L1 page tables beneath user root
/// slots, and the root table itself — returning each to the frame allocator. The
/// shared kernel high half (root slots `256..512`, the entries [`new_user_root`]
/// copied in) is left untouched, since those tables are aliased by every process.
///
/// The caller **must not** have `root_pa` active in `satp` (it's being freed):
/// reclaim runs in the *reaper's* address space, never the exiting child's. Walk
/// logic is the host-tested [`core_mmu::free_user_tree`]; this binds it to
/// `KernelPtMem` (table reads through the linear map) and `frame::free`.
pub fn free_user_root(root_pa: usize) {
    let mem = KernelPtMem;
    core_mmu::free_user_tree(root_pa, &mem, &mut |pa| {
        crate::frame::free(crate::frame::PhysFrame::from_addr(pa));
    });
}

/// The `satp` value (Sv39 mode + root PPN) that activates the address
/// space rooted at `root_pa`. Written with `csrw satp` + `sfence.vma`.
pub fn satp_for(root_pa: usize) -> u64 {
    (SATP_MODE_SV39 << 60) | ((root_pa as u64) >> 12)
}

/// Physical address of the root page table currently active in `satp` on
/// this hart. Reads the live CSR (PPN is bits 43:0 in Sv39) and shifts it
/// back to a PA — the single source of truth for "which address space is
/// loaded," so the scheduler needn't track it separately.
#[must_use]
pub fn current_satp_root() -> usize {
    let satp: usize;
    // SAFETY: reads a CSR; no memory access, no side effects.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack)) };
    const PPN_MASK: usize = (1 << 44) - 1;
    (satp & PPN_MASK) << 12
}

/// Whether the user range `[ptr, ptr+len)` is mapped readable (`R|U`) in the
/// **current** address space — the pre-check `copy_from_user` runs so an
/// in-range-but-unmapped pointer is refused rather than faulting the kernel on
/// the `SUM` deref. Walks the active page table (`current_satp_root`) via
/// `KernelPtMem` using the host-tested [`core_mmu::range_mapped`]; subsumes the
/// user-half bounds check.
#[must_use]
pub fn user_range_readable(ptr: usize, len: usize) -> bool {
    let mem = KernelPtMem;
    core_mmu::range_mapped(
        current_satp_root(),
        ptr,
        len,
        core_mmu::PtePerms::R.union(core_mmu::PtePerms::U),
        &mem,
    )
}

/// Whether the user range `[ptr, ptr+len)` is mapped writable (`W|U`) in the
/// **current** address space — the write mirror of [`user_range_readable`]. The
/// pre-check `copy_to_user` runs (and the `ConsoleRead` handler runs *before*
/// draining its ring, so a bad pointer doesn't consume buffered input).
#[must_use]
pub fn user_range_writable(ptr: usize, len: usize) -> bool {
    let mem = KernelPtMem;
    core_mmu::range_mapped(
        current_satp_root(),
        ptr,
        len,
        core_mmu::PtePerms::W.union(core_mmu::PtePerms::U),
        &mem,
    )
}

/// Activate the address space rooted at `root_pa` on this hart: write
/// `satp` (Sv39 mode + PPN) and `sfence.vma` to flush stale translations.
/// Used by the scheduler to switch address spaces when it switches into a
/// task that lives in a different user root than the one currently loaded.
pub fn activate(root_pa: usize) {
    let satp = satp_for(root_pa);
    // SAFETY: switches the active address space. Every address space shares
    // the kernel high-half (`new_user_root` copies slots 256..512 from the
    // boot root), so the currently-executing kernel code/stack stay mapped
    // across the switch. The `sfence.vma` flushes translations cached under
    // the old root.
    unsafe {
        asm!(
            "csrw satp, {satp}",
            "sfence.vma",
            satp = in(reg) satp,
            options(nostack),
        );
    }
}

/// Repoint an already-mapped 4 KiB VA `va` at a new PA `new_pa` with
/// `perms`, then shoot down the stale translation on every hart.
///
/// This is the first — and currently only — real mmu path that fires a
/// cross-hart `shootdown`. `map` deliberately does a *local* sfence
/// only (a fresh mapping can't be cached stale anywhere). A remap is
/// different: the old VA→PA translation may sit in any hart's TLB, so
/// after overwriting the leaf we must `shootdown(va)` so no hart keeps
/// reading the old frame. `shootdown` does the local `sfence.vma` too,
/// so this covers the calling hart as well.
///
/// Returns `Err(MapError::NotMapped)` if `va` has no 4 KiB leaf to
/// overwrite (unmapped, missing intermediate, or huge-page-covered).
/// Allocates nothing.
///
/// # Safety
///
/// - Must run with MMU on (tables are reached through the linear map).
/// - Caller must ensure repointing `va` is intended — any hart
///   currently dereferencing `va` will, after the shootdown, observe
///   `new_pa`'s contents.
#[cfg_attr(
    not(feature = "itest-workloads"),
    expect(
        dead_code,
        reason = "remap+shootdown path for SMP; exercised by the tlb-shootdown workload (itest-workloads); not yet wired into production multi-hart paths"
    )
)]
pub fn remap(va: usize, new_pa: usize, perms: PtePerms) -> Result<(), MapError> {
    let root_pa = va_to_pa((&raw const BOOT_PT_ROOT) as usize);
    let mut mem = KernelPtMem;
    let result = core_mmu::remap(root_pa, va, new_pa, perms, &mut mem);
    if result.is_ok() {
        shootdown(va);
    }
    result
}

/// Clear the 4 KiB leaf for `va` in the kernel root, returning the PA it mapped
/// (for the caller to `frame::free`). Mirrors [`remap`]: walks only existing
/// tables and, on success, does a cross-hart [`shootdown`] — the leaf was valid,
/// so other harts may have it cached (and a kernel-stack VA is reachable in every
/// address space via the shared high half). The unmapping primitive behind
/// kernel-stack guard-page teardown; `Err(NotMapped)` if `va` isn't a 4 KiB leaf.
pub fn unmap(va: usize) -> Result<usize, MapError> {
    let root_pa = va_to_pa((&raw const BOOT_PT_ROOT) as usize);
    let mut mem = KernelPtMem;
    let result = core_mmu::unmap(root_pa, va, &mut mem);
    if let Ok(freed_pa) = result {
        shootdown(va);
        return Ok(freed_pa);
    }
    result
}

/// Put a guard page below the boot stack (task 0's stack, which lives in the
/// kernel image and so is covered by a 2 MiB leaf). Splits that leaf into 4 KiB
/// leaves, then unmaps the `__boot_stack_guard` page — so a boot-stack overflow
/// faults on it (the trap handler names it) instead of silently corrupting `.bss`.
///
/// Call once early in `kmain`, after the higher-half trampoline (the guard symbol
/// is a higher-half VA) and on the boot hart only (single-hart: no shootdown peer
/// yet). A split/unmap failure is non-fatal — the boot stack just stays unguarded,
/// so it logs nothing and returns.
pub fn guard_boot_stack() {
    unsafe extern "C" {
        static __boot_stack_guard: u8;
    }
    let guard_va = (&raw const __boot_stack_guard) as usize;
    // Refine the 2 MiB kernel-image leaf to 4 KiB, then punch out the guard page.
    if split_huge_leaf(guard_va).is_ok() {
        let _ = unmap(guard_va);
    }
}

/// Split the 2 MiB kernel-image leaf covering `va` into 4 KiB leaves (so a single
/// page in it can be [`unmap`]ped). Walks the kernel root; the mapping is
/// unchanged (same PA + perms per page), so only a local `sfence` is needed —
/// existing translations stay valid. The walk is the host-tested
/// [`core_mmu::split_huge_leaf`].
pub fn split_huge_leaf(va: usize) -> Result<(), MapError> {
    let root_pa = va_to_pa((&raw const BOOT_PT_ROOT) as usize);
    let mut mem = KernelPtMem;
    let result = core_mmu::split_huge_leaf(root_pa, va, &mut mem);
    if result.is_ok() {
        // SAFETY: single instruction, register operand; flushes `va` on this hart.
        unsafe { asm!("sfence.vma {0}, zero", in(reg) va, options(nostack, nomem)) };
    }
    result
}

/// Cumulative count of TLB shootdowns this hart has initiated as a
/// sender (i.e. how many `mmu::map`/`unmap` calls actually fired).
/// Drained by the heartbeat as
/// `snitchos.mmu.shootdowns_sent_total`. `Relaxed`: counter.
pub static SHOOTDOWNS_SENT_TOTAL: crate::counter::DeferredCounter =
    crate::counter::DeferredCounter::new("snitchos.mmu.shootdowns_sent_total");

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
    let online = crate::percpu::SMP_ONLINE_HARTS.load(core::sync::atomic::Ordering::Relaxed);

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

    SHOOTDOWNS_SENT_TOTAL.inc();
}
