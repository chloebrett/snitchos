//! Sv39 page-table types and PTE encoding. Pure bit-twiddling — no
//! CSR access, no asm. The kernel binary owns the static instances
//! and the `csrw satp` / `sfence.vma` bridge.
//!
//! See `plans/v0.4-memory-concepts.md` § 2-3 for the Sv39 reference.

/// VA = PA + `KERNEL_OFFSET` for kernel-space mappings. Matches Linux
/// RISC-V's `PAGE_OFFSET - PHYS_BASE` with `PHYS_BASE` = 0x80000000.
/// The kernel image at PA 0x80200000 maps to higher-half VA
/// `0xffffffff_80200000`.
pub const KERNEL_OFFSET: usize = 0xffffffff_00000000;

/// Base offset for the kernel's linear map of physical memory.
/// PA `p` is reachable at VA `p + LINEAR_OFFSET` for the range covered
/// by `mmu::enable`'s linear-map leaf (currently a single 1 GiB Sv39
/// huge page covering RAM at PA `0x80000000`).
///
/// Picked to satisfy Sv39's canonical-high rule (bits 63:39 must equal
/// bit 38) for all in-range physical addresses, and to land in a root
/// PTE index distinct from the kernel-image and MMIO higher-half
/// mappings.
pub const LINEAR_OFFSET: usize = 0xffffffd0_00000000;

/// Base of the kernel-heap virtual-address range. The heap grows into
/// the 1 GiB slot starting here (root PTE 256). Picked to satisfy
/// Sv39 canonical-high (bits 63:39 == bit 38) and to not collide with
/// the kernel-image (510), linear-map (322), or MMIO (508) root slots.
pub const HEAP_VA_BASE: usize = 0xffffffc0_00000000;

/// Convert a physical address to the kernel's linear-map VA. Inverse
/// of `va_to_pa` for the linear-map range (not for kernel-image VAs
/// in the higher-half mapping at `KERNEL_OFFSET`).
///
/// Used by the frame allocator and its callers: anything that needs
/// to dereference the contents of an allocated frame (zero it, write
/// a fresh page table into it, etc.) does so at `pa_to_kernel_va(pa)`.
pub const fn pa_to_kernel_va(pa: usize) -> usize {
    pa + LINEAR_OFFSET
}

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

/// Top of the Sv39 user half. Valid user VAs live in `[0, USER_VA_END)`;
/// everything at or above is the non-canonical hole or the kernel high-half
/// (`HEAP_VA_BASE`, `LINEAR_OFFSET`, `KERNEL_OFFSET` all sit far above this).
pub const USER_VA_END: usize = 1 << 38;

/// Largest buffer the kernel will copy in from a user process in one syscall.
/// Bounds the work a single `copy_from_user` can demand and caps span-name /
/// log-string lengths.
pub const MAX_USER_STR_LEN: usize = 256;

/// True iff `[ptr, ptr + len)` is a buffer the kernel may safely read from a
/// user process: non-null, length within `MAX_USER_STR_LEN`, no address
/// wraparound, and wholly inside the user half. The last guard is the
/// security boundary — without it a process could smuggle a kernel-high-half
/// pointer and turn a "copy this string" syscall into a read-oracle for
/// kernel memory.
///
/// This is a bounds check, not a page-table walk: whether the range is
/// actually *mapped* is a separate concern, caught at access time (a
/// fault-graceful copy is a deferred refinement).
#[must_use]
pub const fn user_range_ok(ptr: usize, len: usize) -> bool {
    if ptr == 0 || len > MAX_USER_STR_LEN {
        return false;
    }
    match ptr.checked_add(len) {
        Some(end) => end <= USER_VA_END,
        None => false,
    }
}

/// PTE permission and attribute bits. V (valid) is always set on any
/// PTE this module produces. A (accessed) and D (dirty) are pre-set
/// to 1 on every leaf — eliminates the hardware-update trap path.
/// G (global) is set on kernel-shared mappings; the kernel passes it
/// in via `PtePerms` since `kernel-core` doesn't know which side of
/// the canonical-half divide we're on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PtePerms(u64);

impl PtePerms {
    pub const R: PtePerms = PtePerms(1 << 1);
    pub const W: PtePerms = PtePerms(1 << 2);
    pub const X: PtePerms = PtePerms(1 << 3);
    pub const U: PtePerms = PtePerms(1 << 4);
    pub const G: PtePerms = PtePerms(1 << 5);

    pub const fn empty() -> Self { PtePerms(0) }
    pub const fn rwxg() -> Self { PtePerms(Self::R.0 | Self::W.0 | Self::X.0 | Self::G.0) }

    pub const fn bits(self) -> u64 { self.0 }

    #[must_use]
    pub const fn union(self, other: PtePerms) -> Self { PtePerms(self.0 | other.0) }

    /// Whether `self` grants every bit in `other`.
    #[must_use]
    pub const fn contains(self, other: PtePerms) -> bool { self.0 & other.0 == other.0 }
}

/// Bytes per 4 KiB page — the granularity a cross-AS copy walks at.
pub const PAGE_SIZE: usize = 4096;

/// PTE bit positions defined by the privileged spec.
const PTE_V: u64 = 1 << 0;
const PTE_A: u64 = 1 << 6;
const PTE_D: u64 = 1 << 7;

/// Convert a physical address to the PPN field's encoded position
/// inside a PTE. The PPN field is at bits 53:10, so `pa >> 12 << 10`
/// = `pa >> 2`. Easy to get wrong by writing `pa | flags`.
const fn pa_to_pte_ppn(pa: usize) -> u64 {
    (pa as u64) >> 2
}

/// A single Sv39 page-table entry. `#[repr(transparent)]` over `u64` so a
/// `[Pte; 512]` is bit-identical to the hardware table layout. Constructors
/// and predicates live here so the raw bit-twiddling (and its spec invariants)
/// is in one place instead of scattered across free functions taking bare
/// `u64` — and so a PTE can't be silently swapped with a PA or perms `u64`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Pte(u64);

impl Pte {
    /// The empty (V=0) entry — an unmapped slot.
    pub const INVALID: Pte = Pte(0);

    /// Encode a leaf PTE for a page mapping. Sets V, A, D, plus any
    /// permissions the caller passes.
    pub const fn leaf(pa: usize, perms: PtePerms) -> Self {
        Pte(pa_to_pte_ppn(pa) | perms.bits() | PTE_V | PTE_A | PTE_D)
    }

    /// Encode a non-leaf PTE pointing at a child page table. Per the
    /// spec, R=W=X=0 with V=1 is the non-leaf marker.
    pub(crate) const fn branch(child_pa: usize) -> Self {
        Pte(pa_to_pte_ppn(child_pa) | PTE_V)
    }

    /// Reconstruct a `Pte` from a raw `u64` read out of hardware page-table
    /// memory. Inverse of [`Pte::raw`]; the bit pattern is trusted as-is.
    pub const fn from_raw(bits: u64) -> Self {
        Pte(bits)
    }

    /// The raw `u64` for storing into hardware page-table memory.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// V=1.
    pub(crate) const fn is_valid(self) -> bool {
        self.0 & PTE_V != 0
    }

    /// V=1 and R=W=X=0 — points at a child table.
    pub(crate) const fn is_branch(self) -> bool {
        self.is_valid() && self.rwx() == 0
    }

    /// V=1 and at least one of R/W/X — a leaf mapping.
    pub(crate) const fn is_leaf(self) -> bool {
        self.is_valid() && self.rwx() != 0
    }

    /// Recover a child table's PA from a non-leaf PTE. Inverse of
    /// `pa_to_pte_ppn`: PPN at bits 53:10 → PA = `pte >> 10 << 12`.
    pub(crate) const fn child_pa(self) -> usize {
        ((self.0 >> 10) << 12) as usize
    }

    const fn rwx(self) -> u64 {
        self.0 & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits())
    }

    /// The mapped page's PA from a leaf PTE: PPN at bits 53:10 → `pte >> 10 << 12`.
    pub(crate) const fn leaf_pa(self) -> usize {
        ((self.0 >> 10) << 12) as usize
    }

    /// The R/W/X/U/G permission bits carried by this PTE. The mask is the
    /// literal `0b11_1110` — bits 1–5 (R,W,X,U,G), which are disjoint, so a
    /// built-up `|` mask would be a nest of equivalent mutants; the literal
    /// has none.
    pub(crate) const fn perms(self) -> PtePerms {
        PtePerms(self.0 & 0b11_1110)
    }
}

/// An Sv39 virtual address split into its three VPN indices and the page
/// offset. `vpn2` indexes the root table, `vpn1` the mid, `vpn0` the leaf —
/// naming the levels so a `vpn1`/`vpn0` swap is a field-name error, not a
/// silent positional bug the old `(usize, usize, usize, usize)` tuple allowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Sv39Va {
    pub vpn2: usize,
    pub vpn1: usize,
    pub vpn0: usize,
    pub offset: usize,
}

/// Decompose an Sv39 virtual address into its VPN indices and page offset.
pub(crate) const fn split_va(va: usize) -> Sv39Va {
    Sv39Va {
        vpn2: (va >> 30) & 0x1ff,
        vpn1: (va >> 21) & 0x1ff,
        vpn0: (va >> 12) & 0x1ff,
        offset: va & 0xfff,
    }
}

/// 4 KiB-aligned page table holding 512 Sv39 PTEs. The layout is
/// dictated by hardware; do not reorder fields.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
pub struct PageTable {
    entries: [Pte; 512],
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PageTable {
    pub const fn new() -> Self {
        Self { entries: [Pte::INVALID; 512] }
    }

    pub fn entry(&self, idx: usize) -> Pte {
        self.entries[idx]
    }

    /// Set entry `idx` to `value`. Used by the kernel to clear an
    /// identity-half root entry (`Pte::INVALID`) as part of step 2d
    /// (identity unmap).
    pub fn set_entry(&mut self, idx: usize, value: Pte) {
        self.entries[idx] = value;
    }

    /// Install a 2 MiB identity-ish leaf in this root table at
    /// VA `va`, pointing at PA `pa`, with `perms`. `mid` is the
    /// level-1 page table to use for `va`'s gigapage range; if the
    /// root entry is empty, we set it to a branch pointing at `mid`
    /// and use `mid_pa` as `mid`'s physical address. If the root
    /// entry already points at a mid table (this call's not the
    /// first 2 MiB in this gigapage), we expect the caller to pass
    /// the same `mid` — installing the leaf there.
    ///
    /// Returns `false` if the requested entry would overwrite a
    /// different existing mapping (sanity check, not a hard fault
    /// — caller decides what to do).
    pub fn map_2mib(
        &mut self,
        mid: &mut PageTable,
        mid_pa: usize,
        va: usize,
        pa: usize,
        perms: PtePerms,
    ) -> bool {
        let Sv39Va { vpn2, vpn1, .. } = split_va(va);

        let existing_root = self.entries[vpn2];
        if !existing_root.is_valid() {
            self.entries[vpn2] = Pte::branch(mid_pa);
        } else if existing_root != Pte::branch(mid_pa) {
            return false;
        }

        let new_leaf = Pte::leaf(pa, perms);
        let existing_leaf = mid.entries[vpn1];
        if existing_leaf == new_leaf {
            return true;
        }
        if existing_leaf.is_valid() {
            return false;
        }
        mid.entries[vpn1] = new_leaf;
        true
    }
}

/// Backing store for page tables under a `map` walk. The walk
/// never holds a raw pointer to a table — instead, it reads and
/// writes entries through this trait, given a table's physical
/// address and an index. The implementation owns the deref
/// (`pa_to_kernel_va` + raw load/store in the kernel; safe Vec
/// indexing in tests), so the walk itself stays free of `unsafe`
/// and is fully host-testable.
pub trait PtMem {
    /// Allocate a fresh zeroed 4 KiB page table. Returns its
    /// physical address (or the test-side moral equivalent), or
    /// `None` if no frame is available.
    fn alloc_zeroed_table(&mut self) -> Option<usize>;

    /// Read entry `idx` of the table at `table_pa`. `idx` is in `0..512`.
    fn read_entry(&self, table_pa: usize, idx: usize) -> Pte;

    /// Write entry `idx` of the table at `table_pa`. `idx` is in `0..512`.
    fn write_entry(&mut self, table_pa: usize, idx: usize, value: Pte);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The walk hit a valid PTE (leaf at any level, or a leaf that
    /// would overlap the requested 4 KiB) at a position that would
    /// require overwriting it.
    AlreadyMapped,
    /// `PtMem::alloc_zeroed_table` returned `None` while the walk
    /// was trying to install an intermediate table.
    OutOfFrames,
    /// `remap` found no 4 KiB leaf to overwrite at this VA — the VA
    /// is unmapped, an intermediate table is missing, or a huge-page
    /// leaf covers it (so there is no 4 KiB granularity to remap).
    NotMapped,
}

/// Install a 4 KiB leaf PTE mapping VA `va` → PA `pa` with `perms`.
/// Walks the table tree rooted at `root_pa` from VPN[2] to VPN[0],
/// allocating intermediate tables from `mem` on demand. The caller
/// is responsible for `sfence.vma` after a successful return.
///
/// Returns `Err(AlreadyMapped)` if any walked PTE already has V=1
/// in a way that conflicts (huge-page leaf at level 2 or 1, or any
/// leaf at level 0). Returns `Err(OutOfFrames)` if intermediate
/// allocation fails — the walk does *not* clean up partially
/// installed intermediate tables (see `plans/v0.4-memory-step-5-page-table-mutation.md`).
pub fn map(
    root_pa: usize,
    va: usize,
    pa: usize,
    perms: PtePerms,
    mem: &mut dyn PtMem,
) -> Result<(), MapError> {
    let Sv39Va { vpn2, vpn1, vpn0, .. } = split_va(va);
    let mid_pa = walk_or_install(root_pa, vpn2, mem)?;
    let leaf_table_pa = walk_or_install(mid_pa, vpn1, mem)?;
    let existing = mem.read_entry(leaf_table_pa, vpn0);
    if existing.is_valid() {
        return Err(MapError::AlreadyMapped);
    }
    mem.write_entry(leaf_table_pa, vpn0, Pte::leaf(pa, perms));
    Ok(())
}

/// Overwrite the existing 4 KiB leaf PTE for `va` with one mapping to
/// `new_pa` with `perms`. Walks only *existing* tables (never
/// allocates) and requires a valid 4 KiB leaf already present; the
/// caller is responsible for the TLB shootdown after a successful
/// return (this is what makes `remap` distinct from `map` — the old
/// translation may be cached on other harts).
///
/// Returns `Err(NotMapped)` if the VA is unmapped, an intermediate
/// table is missing, or a huge-page leaf covers the VA. Unlike `map`,
/// `remap` never returns `OutOfFrames` (it allocates nothing) and
/// never returns `AlreadyMapped` (overwriting is the whole point).
pub fn remap(
    root_pa: usize,
    va: usize,
    new_pa: usize,
    perms: PtePerms,
    mem: &mut dyn PtMem,
) -> Result<(), MapError> {
    let Sv39Va { vpn2, vpn1, vpn0, .. } = split_va(va);
    let mid_pa = walk_existing(root_pa, vpn2, mem)?;
    let leaf_table_pa = walk_existing(mid_pa, vpn1, mem)?;
    let existing = mem.read_entry(leaf_table_pa, vpn0);
    if !existing.is_leaf() {
        return Err(MapError::NotMapped);
    }
    mem.write_entry(leaf_table_pa, vpn0, Pte::leaf(new_pa, perms));
    Ok(())
}

/// Descend one level through an *existing* branch PTE. Returns the
/// child table PA. Errors `NotMapped` if the slot is empty (no table)
/// or holds a leaf (huge page — no child table to descend into).
fn walk_existing(
    table_pa: usize,
    idx: usize,
    mem: &dyn PtMem,
) -> Result<usize, MapError> {
    let pte = mem.read_entry(table_pa, idx);
    if pte.is_branch() {
        return Ok(pte.child_pa());
    }
    Err(MapError::NotMapped)
}

/// At `table_pa[idx]`: if V=0, allocate + install a non-leaf PTE
/// pointing at the new table and return its PA. If V=1 and the PTE
/// is non-leaf, recover and return the child PA. If V=1 and the PTE
/// is a leaf (a huge page in the way), return `AlreadyMapped`.
fn walk_or_install(
    table_pa: usize,
    idx: usize,
    mem: &mut dyn PtMem,
) -> Result<usize, MapError> {
    let pte = mem.read_entry(table_pa, idx);
    if pte.is_leaf() {
        return Err(MapError::AlreadyMapped);
    }
    if pte.is_branch() {
        return Ok(pte.child_pa());
    }
    let new_pa = mem.alloc_zeroed_table().ok_or(MapError::OutOfFrames)?;
    mem.write_entry(table_pa, idx, Pte::branch(new_pa));
    Ok(new_pa)
}

/// A translated 4 KiB leaf: the mapped page's base PA and its permissions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Leaf {
    pub pa: usize,
    pub perms: PtePerms,
}

/// Resolve `va` to its 4 KiB leaf in the address space rooted at `root_pa`,
/// returning the page-base PA + perms. `None` if the VA is unmapped, an
/// intermediate table is missing, or a huge-page leaf covers it (no 4 KiB
/// granularity). The read-only walk underneath a cross-AS copy — it can target
/// *any* root, unlike a `SUM`-bit deref which only reaches the active space.
#[must_use]
pub fn translate(root_pa: usize, va: usize, mem: &dyn PtMem) -> Option<Leaf> {
    let Sv39Va { vpn2, vpn1, vpn0, .. } = split_va(va);
    let mid_pa = walk_existing(root_pa, vpn2, mem).ok()?;
    let leaf_table_pa = walk_existing(mid_pa, vpn1, mem).ok()?;
    let pte = mem.read_entry(leaf_table_pa, vpn0);
    pte.is_leaf().then(|| Leaf { pa: pte.leaf_pa(), perms: pte.perms() })
}

/// Why a cross-address-space copy was refused — surfaced before any bytes move
/// (the copy is validated end-to-end first, so a refusal never half-copies).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyError {
    /// `(va, len)` failed the user-buffer bounds check (null, over the
    /// per-copy cap, address overflow, or outside the user half).
    BadRange,
    /// A page in the range is unmapped (or covered by a huge page).
    Unmapped,
    /// A page lacks a required permission (`R`/`W` or `U`).
    Perms,
}

/// Copy `len` bytes from `src_va` (in `src_root`) to `dst_va` (in `dst_root`),
/// invoking `copy(src_pa, dst_pa, n)` for each maximal run that stays within a
/// single page on *both* sides. The two address spaces share one physical
/// memory (`mem` reads both roots' tables); the caller's `copy` callback does
/// the actual byte move through the resolved physical addresses (the kernel's
/// linear map; a mock in tests) — keeping this logic pure and host-testable.
///
/// Both ranges are bounds-checked (`user_range_ok`); each page is checked as it
/// is reached — source must grant `R|U`, destination `W|U` — and the copy
/// proceeds page-by-page. A refusal on a later page may leave **leading bytes
/// already copied**, so callers treat a failed copy as a discarded transfer
/// (the FS's scratch / client buffers are thrown away on an error reply).
/// `len == 0` is a no-op success.
pub fn copy_across(
    src_root: usize,
    src_va: usize,
    dst_root: usize,
    dst_va: usize,
    len: usize,
    mem: &dyn PtMem,
    copy: &mut dyn FnMut(usize, usize, usize),
) -> Result<(), CopyError> {
    if !user_range_ok(src_va, len) || !user_range_ok(dst_va, len) {
        return Err(CopyError::BadRange);
    }

    let mut done = 0;
    while done < len {
        let src = translate(src_root, src_va + done, mem).ok_or(CopyError::Unmapped)?;
        let dst = translate(dst_root, dst_va + done, mem).ok_or(CopyError::Unmapped)?;
        if !src.perms.contains(PtePerms::R.union(PtePerms::U))
            || !dst.perms.contains(PtePerms::W.union(PtePerms::U))
        {
            return Err(CopyError::Perms);
        }
        let src_off = (src_va + done) & (PAGE_SIZE - 1);
        let dst_off = (dst_va + done) & (PAGE_SIZE - 1);
        let chunk = core::cmp::min(
            core::cmp::min(PAGE_SIZE - src_off, PAGE_SIZE - dst_off),
            len - done,
        );
        copy(src.pa + src_off, dst.pa + dst_off, chunk);
        done += chunk;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    #[test]
    fn user_range_accepts_low_half_buffer() {
        assert!(user_range_ok(0x1000, 16));
    }

    #[test]
    fn user_range_accepts_buffer_ending_exactly_at_user_top() {
        // Half-open: last byte is USER_VA_END - 1, so end == USER_VA_END is fine.
        assert!(user_range_ok(USER_VA_END - 16, 16));
    }

    #[test]
    fn user_range_rejects_null() {
        assert!(!user_range_ok(0, 16));
    }

    #[test]
    fn user_range_rejects_kernel_high_half() {
        assert!(!user_range_ok(KERNEL_OFFSET, 16));
        assert!(!user_range_ok(HEAP_VA_BASE, 16));
        assert!(!user_range_ok(LINEAR_OFFSET, 16));
    }

    #[test]
    fn user_range_rejects_crossing_the_user_top() {
        // ptr is in the user half but ptr + len spills past it.
        assert!(!user_range_ok(USER_VA_END - 8, 16));
    }

    #[test]
    fn user_range_rejects_wraparound() {
        assert!(!user_range_ok(usize::MAX - 4, 16));
    }

    #[test]
    fn user_range_rejects_over_long() {
        assert!(!user_range_ok(0x1000, MAX_USER_STR_LEN + 1));
        assert!(user_range_ok(0x1000, MAX_USER_STR_LEN));
    }

    /// Host-side `PtMem` backed by a Vec of `PageTables`. PA encoding:
    /// `(index + 1) << 12` so PA=0 stays distinguishable from "unset"
    /// and the encoding survives the `>>12 <<10` round-trip through
    /// PTE PPN bits. Slot 0 is reserved for the root.
    struct MockPtMem {
        tables: Vec<PageTable>,
        cap: usize,
        intermediate_allocs: usize,
    }

    impl MockPtMem {
        /// `cap` is the max number of *intermediate* tables the walk
        /// can allocate. The root is pre-installed and doesn't count.
        fn new(cap: usize) -> Self {
            Self {
                tables: std::vec![PageTable::new()],
                cap,
                intermediate_allocs: 0,
            }
        }

        #[allow(
            clippy::unused_self,
            reason = "mirrors the real PtMem accessor shape; the mock root is always slot 0"
        )]
        fn root_pa(&self) -> usize {
            (1) << 12
        }

        fn intermediate_alloc_count(&self) -> usize {
            self.intermediate_allocs
        }

        fn table(&self, pa: usize) -> &PageTable {
            let idx = (pa >> 12) - 1;
            &self.tables[idx]
        }
    }

    impl PtMem for MockPtMem {
        fn alloc_zeroed_table(&mut self) -> Option<usize> {
            if self.intermediate_allocs >= self.cap {
                return None;
            }
            self.tables.push(PageTable::new());
            self.intermediate_allocs += 1;
            Some(self.tables.len() << 12)
        }

        fn read_entry(&self, table_pa: usize, idx: usize) -> Pte {
            let t_idx = (table_pa >> 12) - 1;
            self.tables[t_idx].entries[idx]
        }

        fn write_entry(&mut self, table_pa: usize, idx: usize, value: Pte) {
            let t_idx = (table_pa >> 12) - 1;
            self.tables[t_idx].entries[idx] = value;
        }
    }

    /// Walk root → mid → leaf table given the freshly-built root, returning
    /// the leaf `PageTable` that holds the final PTE. Avoids repeating the
    /// branch-chasing boilerplate in every assertion.
    fn leaf_table_of(mem: &MockPtMem, va: usize) -> &PageTable {
        let Sv39Va { vpn2, vpn1, .. } = split_va(va);
        let root = mem.table(mem.root_pa());
        let mid_pa = root.entry(vpn2).child_pa();
        let mid = mem.table(mid_pa);
        let leaf_pa = mid.entries[vpn1].child_pa();
        mem.table(leaf_pa)
    }

    #[test]
    fn map_into_empty_root_installs_leaf_at_expected_indices() {
        let mut mem = MockPtMem::new(8);
        // VA 0x40201000 → vpn2=1, vpn1=1, vpn0=1.
        let va = 0x40201000;
        let pa = 0x90000000;
        let perms = PtePerms::R.union(PtePerms::W);
        map(mem.root_pa(), va, pa, perms, &mut mem).unwrap();

        let root = mem.table(mem.root_pa());
        assert!(root.entry(1).is_branch(), "root[1] should be a branch");
        let mid_pa = root.entry(1).child_pa();
        let mid = mem.table(mid_pa);
        assert!(mid.entries[1].is_branch(), "mid[1] should be a branch");
        let leaf = leaf_table_of(&mem, va);
        assert_eq!(leaf.entries[1], Pte::leaf(pa, perms));
    }

    #[test]
    fn map_allocates_two_intermediate_tables_in_empty_root() {
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(mem.intermediate_alloc_count(), 2);
    }

    #[test]
    fn map_reuses_existing_intermediate_tables() {
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        // Second map in the same level-0 table (same vpn2/vpn1) should
        // allocate nothing — both intermediates already exist.
        map(mem.root_pa(), 0x2000, 0x80101000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(mem.intermediate_alloc_count(), 2);
    }

    #[test]
    fn map_reuses_mid_but_allocates_new_leaf_table_for_different_2mib_range() {
        let mut mem = MockPtMem::new(8);
        // 0x1000 → vpn2=0, vpn1=0, vpn0=1.
        // 0x201000 → vpn2=0, vpn1=1, vpn0=1. Same mid, different leaf.
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        map(mem.root_pa(), 0x201000, 0x80101000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(mem.intermediate_alloc_count(), 3);
    }

    #[test]
    fn map_returns_already_mapped_on_double_map_same_va() {
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(
            map(mem.root_pa(), 0x1000, 0x80200000, PtePerms::R, &mut mem),
            Err(MapError::AlreadyMapped),
        );
    }

    #[test]
    fn map_returns_already_mapped_when_root_has_huge_leaf() {
        // Pre-install a 1 GiB huge-page leaf at root[1] (linear-map shape).
        let mut mem = MockPtMem::new(8);
        let root_pa = mem.root_pa();
        mem.write_entry(root_pa, 1, Pte::leaf(0x80000000, PtePerms::rwxg()));
        // VA 0x40000000 → vpn2=1; collides with the huge leaf.
        assert_eq!(
            map(root_pa, 0x40000000, 0x90000000, PtePerms::R, &mut mem),
            Err(MapError::AlreadyMapped),
        );
        assert_eq!(
            mem.intermediate_alloc_count(),
            0,
            "no tables allocated on early-fail",
        );
    }

    #[test]
    fn map_returns_already_mapped_when_mid_has_huge_leaf() {
        // 2 MiB huge-leaf in mid table (R/W/X set at level 1).
        let mut mem = MockPtMem::new(8);
        let root_pa = mem.root_pa();
        map(root_pa, 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        // Overwrite mid[2] with a 2 MiB leaf.
        let mid_pa = mem.read_entry(root_pa, 0).child_pa();
        mem.write_entry(mid_pa, 2, Pte::leaf(0x80200000, PtePerms::rwxg()));
        // VA 0x400000 → vpn2=0, vpn1=2 — collides.
        assert_eq!(
            map(root_pa, 0x400000, 0x90000000, PtePerms::R, &mut mem),
            Err(MapError::AlreadyMapped),
        );
    }

    #[test]
    fn map_returns_out_of_frames_when_allocator_exhausted() {
        let mut mem = MockPtMem::new(0);
        assert_eq!(
            map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem),
            Err(MapError::OutOfFrames),
        );
    }

    #[test]
    fn map_returns_out_of_frames_partway_through_walk() {
        // Allocator has capacity for the mid but not the leaf table.
        // The mid is left installed — the walk does not unwind, as
        // documented in the step-5 plan.
        let mut mem = MockPtMem::new(1);
        let root_pa = mem.root_pa();
        assert_eq!(
            map(root_pa, 0x1000, 0x80100000, PtePerms::R, &mut mem),
            Err(MapError::OutOfFrames),
        );
        assert_eq!(mem.intermediate_alloc_count(), 1, "mid was leaked");
        assert!(mem.read_entry(root_pa, 0).is_branch());
    }

    #[test]
    fn map_propagates_perms_to_leaf_pte() {
        let mut mem = MockPtMem::new(8);
        let perms = PtePerms::R.union(PtePerms::W).union(PtePerms::G);
        map(mem.root_pa(), 0x1000, 0x80100000, perms, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        assert_eq!(pte.raw() & PtePerms::R.bits(), PtePerms::R.bits());
        assert_eq!(pte.raw() & PtePerms::W.bits(), PtePerms::W.bits());
        assert_eq!(pte.raw() & PtePerms::G.bits(), PtePerms::G.bits());
        assert_eq!(pte.raw() & PtePerms::X.bits(), 0);
    }

    #[test]
    fn map_sets_a_and_d_on_leaf() {
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        assert_eq!(pte.raw() & PTE_A, PTE_A);
        assert_eq!(pte.raw() & PTE_D, PTE_D);
    }

    #[test]
    fn map_encodes_pa_at_correct_pte_bits() {
        // PA 0x80200000 → PPN 0x80200 → PTE field at bits 53:10 =
        // 0x80200 << 10 = 0x20080000.
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80200000, PtePerms::R, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        let ppn_field = pte.raw() & !0x3ff;
        assert_eq!(ppn_field, 0x20080000);
    }

    #[test]
    fn u_perm_occupies_bit_4() {
        // bit 4 is the U (user-mode) flag per the Sv39 spec.
        assert_eq!(PtePerms::U.bits(), 1 << 4);
    }

    #[test]
    fn union_combines_disjoint_perms() {
        // | and & give the same result for equal inputs, but differ for
        // disjoint ones — kill the | → & mutant.
        let rw = PtePerms::R.union(PtePerms::W);
        assert_eq!(rw.bits(), PtePerms::R.bits() | PtePerms::W.bits());
    }

    #[test]
    fn union_is_idempotent() {
        // R ^ R = 0, so R.union(R) = R only with |. Kills the | → ^ mutant.
        assert_eq!(PtePerms::R.union(PtePerms::R).bits(), PtePerms::R.bits());
    }

    #[test]
    fn pte_leaf_constructor_matches_raw_encoding() {
        let pte = Pte::leaf(0x80200000, PtePerms::rwxg());
        assert_eq!(pte.raw(), 0x200800EF);
        assert!(pte.is_valid());
        assert!(pte.is_leaf());
        assert!(!pte.is_branch());
    }

    #[test]
    fn pte_branch_is_branch_not_leaf_and_recovers_child_pa() {
        let pte = Pte::branch(0x80300000);
        assert!(pte.is_valid());
        assert!(pte.is_branch());
        assert!(!pte.is_leaf());
        assert_eq!(pte.child_pa(), 0x80300000);
    }

    #[test]
    fn pte_invalid_is_neither_leaf_nor_branch() {
        assert_eq!(Pte::INVALID.raw(), 0);
        assert!(!Pte::INVALID.is_valid());
        assert!(!Pte::INVALID.is_leaf());
        assert!(!Pte::INVALID.is_branch());
    }

    #[test]
    fn leaf_pte_with_no_perms_still_has_v_a_d() {
        // V=1, A=1, D=1 unconditionally — caller can still produce
        // a no-access leaf if they want (rare; documents the rule).
        let pte = Pte::leaf(0, PtePerms::empty());
        assert_eq!(pte.raw() & PTE_V, PTE_V);
        assert_eq!(pte.raw() & PTE_A, PTE_A);
        assert_eq!(pte.raw() & PTE_D, PTE_D);
        assert_eq!(pte.raw() & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits()), 0);
    }

    #[test]
    fn branch_pte_has_no_perm_bits() {
        // R=W=X=0 with V=1 is the non-leaf marker. PPN encodes the
        // child table's PA. No A/D since hardware never sets those
        // on a non-leaf walk.
        let pte = Pte::branch(0x80300000);
        assert_eq!(pte.raw() & PTE_V, PTE_V);
        assert_eq!(pte.raw() & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits()), 0);
        assert_eq!(pte.raw() & PTE_A, 0);
        assert_eq!(pte.raw() & PTE_D, 0);
        // PPN = 0x80300 → at bits 53:10 = 0x80300 << 10 = 0x200C0000.
        assert_eq!(pte.raw() & !0x3ff, 0x200C0000);
    }

    #[test]
    fn heap_va_base_lands_in_distinct_root_slot() {
        // Sanity-check the heap's root-PTE slot. Must:
        //   - be canonical-high (bits 63:39 == bit 38 == 1),
        //   - index a root entry distinct from kernel image (510),
        //     linear map (322), and MMIO (508),
        //   - be 1 GiB-aligned (so the whole root slot is the heap).
        let Sv39Va { vpn2, vpn1, vpn0, offset } = split_va(HEAP_VA_BASE);
        assert_eq!(vpn1, 0);
        assert_eq!(vpn0, 0);
        assert_eq!(offset, 0);
        assert_ne!(vpn2, 510, "would collide with higher-half kernel image");
        assert_ne!(vpn2, 322, "would collide with linear map");
        assert_ne!(vpn2, 508, "would collide with higher-half MMIO");
        let bit_38 = (HEAP_VA_BASE >> 38) & 1;
        let bits_63_39 = HEAP_VA_BASE >> 39;
        assert_eq!(bit_38, 1);
        assert_eq!(bits_63_39, 0x1FFFFFF);
    }

    #[test]
    fn pa_to_kernel_va_offsets_into_linear_map() {
        // PA 0 → start of linear-map VA range.
        assert_eq!(pa_to_kernel_va(0), LINEAR_OFFSET);
        // Start of QEMU virt RAM.
        assert_eq!(pa_to_kernel_va(0x80000000), LINEAR_OFFSET + 0x80000000);
        // Kernel image base.
        assert_eq!(pa_to_kernel_va(0x80200000), LINEAR_OFFSET + 0x80200000);
    }

    #[test]
    fn pa_to_kernel_va_results_are_canonical_high() {
        // Every output must satisfy Sv39's sign-extension rule:
        // bits 63:39 all equal bit 38. With LINEAR_OFFSET in the
        // canonical-high range, any PA in [0, 1 GiB) stays there.
        for pa in [0usize, 0x80000000, 0x80200000, 0xBFFFF000] {
            let va = pa_to_kernel_va(pa);
            let bit_38 = (va >> 38) & 1;
            let bits_63_39 = va >> 39;
            assert_eq!(bit_38, 1, "VA {va:#x} not in canonical-high (bit 38 = 0)");
            // bits 63:39 = 25 ones means bits_63_39 == 0x1FFFFFF.
            assert_eq!(bits_63_39, 0x1FFFFFF, "VA {va:#x} fails sign extension");
        }
    }

    #[test]
    fn va_to_pa_identity_input_passes_through() {
        // Identity-range addresses (below KERNEL_OFFSET) are already
        // physical; va_to_pa must not subtract anything.
        assert_eq!(va_to_pa(0x80200000), 0x80200000);
        assert_eq!(va_to_pa(0x10000000), 0x10000000);
        assert_eq!(va_to_pa(0), 0);
    }

    #[test]
    fn va_to_pa_higher_half_input_strips_kernel_offset() {
        // Higher-half VAs (>= KERNEL_OFFSET) get KERNEL_OFFSET
        // subtracted to recover the physical address.
        assert_eq!(va_to_pa(KERNEL_OFFSET + 0x80200000), 0x80200000);
        assert_eq!(va_to_pa(KERNEL_OFFSET + 0x10000000), 0x10000000);
        assert_eq!(va_to_pa(KERNEL_OFFSET), 0);
    }

    #[test]
    fn set_entry_clears_and_replaces_specific_index() {
        let mut pt = PageTable::new();
        let a = Pte::leaf(0x80200000, PtePerms::rwxg());
        let b = Pte::branch(0x80300000);
        pt.set_entry(2, a);
        pt.set_entry(5, b);
        assert_eq!(pt.entry(2), a);
        assert_eq!(pt.entry(5), b);
        // Clear entry 2; entry 5 untouched.
        pt.set_entry(2, Pte::INVALID);
        assert_eq!(pt.entry(2), Pte::INVALID);
        assert_eq!(pt.entry(5), b);
    }

    #[test]
    fn higher_half_va_indexes_different_root_entry_than_identity() {
        // Pins that, with KERNEL_OFFSET = 0xffffffff_00000000, the
        // kernel image's higher-half VA (0xffffffff_80200000) lands in
        // a different root entry than its identity VA (0x80200000).
        // This means dual-mapping kernel image → identity AND →
        // higher-half does not alias inside the same root entry, so
        // the two leaves can coexist via separate mid tables.
        const KERNEL_OFFSET: usize = 0xffffffff_00000000;
        let id_va = 0x80200000usize;
        let high_va = id_va + KERNEL_OFFSET;

        let id_vpn2 = split_va(id_va).vpn2;
        let high_vpn2 = split_va(high_va).vpn2;

        assert_eq!(id_vpn2, 2, "identity kernel should index root[2]");
        assert_eq!(high_vpn2, 510, "higher-half kernel should index root[510]");
        assert_ne!(id_vpn2, high_vpn2);
    }

    #[test]
    fn split_va_extracts_indices() {
        // 0x80200000 = 0b 10_0000_0001 0_0000_0000 0_0000_0000 0000_0000_0000
        //   bits 38..30 = 0b 0000_0000_10 = 2
        //   bits 29..21 = 0b 0000_0000_1  = 1
        //   bits 20..12 = 0
        //   offset      = 0
        assert_eq!(split_va(0x80200000), Sv39Va { vpn2: 2, vpn1: 1, vpn0: 0, offset: 0 });
    }

    #[test]
    fn split_va_extracts_nonzero_vpn0() {
        // 0x80201000 = 0x80200000 + one 4 KiB page → vpn0 = 1, not 0.
        // Catches mutations of the vpn0 extraction (>> 12) that survive
        // when all test VAs are 2 MiB-aligned and vpn0 is coincidentally 0.
        assert_eq!(split_va(0x80201000), Sv39Va { vpn2: 2, vpn1: 1, vpn0: 1, offset: 0 });
    }

    #[test]
    fn split_va_handles_mmio_region() {
        // 0x10000000 → vpn2 = 0, vpn1 = 128, vpn0 = 0.
        assert_eq!(split_va(0x10000000), Sv39Va { vpn2: 0, vpn1: 128, vpn0: 0, offset: 0 });
    }

    #[test]
    fn remap_overwrites_existing_leaf_with_new_pa() {
        // The whole point of remap: a VA already mapped to PA-A is
        // pointed at PA-B with new perms, in place. The leaf PTE must
        // now encode B, not A.
        let mut mem = MockPtMem::new(8);
        let va = 0x1000;
        map(mem.root_pa(), va, 0x80100000, PtePerms::R, &mut mem).unwrap();

        remap(mem.root_pa(), va, 0x80200000, PtePerms::R.union(PtePerms::W), &mut mem).unwrap();

        let pte = leaf_table_of(&mem, va).entries[1];
        assert_eq!(pte, Pte::leaf(0x80200000, PtePerms::R.union(PtePerms::W)));
    }

    #[test]
    fn remap_on_unmapped_va_returns_not_mapped() {
        // Nothing mapped at all — the leaf table doesn't even exist.
        let mut mem = MockPtMem::new(8);
        assert_eq!(
            remap(mem.root_pa(), 0x1000, 0x80200000, PtePerms::R, &mut mem),
            Err(MapError::NotMapped),
        );
    }

    #[test]
    fn remap_when_leaf_slot_empty_returns_not_mapped() {
        // Intermediates exist (a sibling VA in the same leaf table is
        // mapped) but the target VA's own leaf slot is V=0.
        let mut mem = MockPtMem::new(8);
        // 0x1000 and 0x2000 share vpn2/vpn1; vpn0 = 1 vs 2.
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(
            remap(mem.root_pa(), 0x2000, 0x80200000, PtePerms::R, &mut mem),
            Err(MapError::NotMapped),
        );
    }

    #[test]
    fn remap_when_huge_leaf_covers_va_returns_not_mapped() {
        // A 1 GiB huge leaf at root[0] covers VA 0x1000. There is no
        // 4 KiB leaf to remap — descending would mean splitting a huge
        // page, which remap does not do.
        let mut mem = MockPtMem::new(8);
        let root_pa = mem.root_pa();
        mem.write_entry(root_pa, 0, Pte::leaf(0x80000000, PtePerms::rwxg()));
        assert_eq!(
            remap(root_pa, 0x1000, 0x80200000, PtePerms::R, &mut mem),
            Err(MapError::NotMapped),
        );
    }

    #[test]
    fn remap_allocates_no_tables() {
        // remap must walk existing tables only. After a map (which
        // allocated 2 intermediates), a remap allocates nothing more.
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        let before = mem.intermediate_alloc_count();
        remap(mem.root_pa(), 0x1000, 0x80200000, PtePerms::R, &mut mem).unwrap();
        assert_eq!(mem.intermediate_alloc_count(), before);
    }

    #[test]
    fn remap_changes_only_the_target_leaf() {
        // A sibling mapping in the same leaf table must be untouched.
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        map(mem.root_pa(), 0x2000, 0x80300000, PtePerms::R, &mut mem).unwrap();
        let sibling_before = leaf_table_of(&mem, 0x2000).entries[2];
        remap(mem.root_pa(), 0x1000, 0x80200000, PtePerms::W, &mut mem).unwrap();
        assert_eq!(leaf_table_of(&mem, 0x1000).entries[1], Pte::leaf(0x80200000, PtePerms::W));
        assert_eq!(leaf_table_of(&mem, 0x2000).entries[2], sibling_before);
    }

    #[test]
    fn map_2mib_installs_branch_in_root_and_leaf_in_mid() {
        let mut root = PageTable::new();
        let mut mid = PageTable::new();
        let mid_pa = 0x80300000;

        assert!(root.map_2mib(&mut mid, mid_pa, 0x80200000, 0x80200000, PtePerms::rwxg()));

        // VA 0x80200000 → vpn2=2, vpn1=1.
        assert_eq!(root.entry(2), Pte::branch(mid_pa));
        assert_eq!(mid.entry(1), Pte::leaf(0x80200000, PtePerms::rwxg()));
        // All other entries untouched.
        for i in 0..512 {
            if i != 2 {
                assert_eq!(root.entry(i), Pte::INVALID, "root[{i}] should be empty");
            }
            if i != 1 {
                assert_eq!(mid.entry(i), Pte::INVALID, "mid[{i}] should be empty");
            }
        }
    }

    #[test]
    fn map_2mib_idempotent_when_called_twice_with_same_args() {
        let mut root = PageTable::new();
        let mut mid = PageTable::new();
        let mid_pa = 0x80300000;
        assert!(root.map_2mib(&mut mid, mid_pa, 0x80200000, 0x80200000, PtePerms::rwxg()));
        assert!(root.map_2mib(&mut mid, mid_pa, 0x80200000, 0x80200000, PtePerms::rwxg()));
        assert_eq!(mid.entry(1), Pte::leaf(0x80200000, PtePerms::rwxg()));
    }

    #[test]
    fn map_2mib_two_regions_in_same_gigapage_share_mid_table() {
        // 0x80200000 and 0x80400000 are both in the [0x80000000, 0xC0000000)
        // gigapage. They should share the same mid table (vpn1=1, vpn1=2).
        let mut root = PageTable::new();
        let mut mid = PageTable::new();
        let mid_pa = 0x80300000;
        assert!(root.map_2mib(&mut mid, mid_pa, 0x80200000, 0x80200000, PtePerms::rwxg()));
        assert!(root.map_2mib(&mut mid, mid_pa, 0x80400000, 0x80400000, PtePerms::rwxg()));
        assert_eq!(root.entry(2), Pte::branch(mid_pa));
        assert_eq!(mid.entry(1), Pte::leaf(0x80200000, PtePerms::rwxg()));
        assert_eq!(mid.entry(2), Pte::leaf(0x80400000, PtePerms::rwxg()));
    }

    // ---- cross-address-space copy: translate + copy_across ----

    /// Allocate a second address-space root inside the same mock physical
    /// memory (both page tables live in one `PtMem`, as in the real kernel).
    fn fresh_root(mem: &mut MockPtMem) -> usize {
        mem.alloc_zeroed_table().expect("a frame for a second root table")
    }

    fn ru() -> PtePerms {
        PtePerms::R.union(PtePerms::U)
    }
    fn wu() -> PtePerms {
        PtePerms::W.union(PtePerms::U)
    }

    /// Run `copy_across` recording the `(src_pa, dst_pa, len)` chunks it emits.
    fn record(
        mem: &MockPtMem,
        src_root: usize,
        src_va: usize,
        dst_root: usize,
        dst_va: usize,
        len: usize,
    ) -> Result<std::vec::Vec<(usize, usize, usize)>, CopyError> {
        let mut chunks = std::vec::Vec::new();
        copy_across(src_root, src_va, dst_root, dst_va, len, mem, &mut |s, d, n| {
            chunks.push((s, d, n));
        })?;
        Ok(chunks)
    }

    #[test]
    fn translate_returns_the_leaf_pa_and_perms() {
        let mut mem = MockPtMem::new(16);
        let root = mem.root_pa();
        map(root, 0x40201000, 0x8000_0000, ru(), &mut mem).unwrap();

        let leaf = translate(root, 0x40201000, &mem).expect("mapped VA translates");
        assert_eq!(leaf.pa, 0x8000_0000);
        // Exact perms (not just `contains`): a leaf mapped R|U reads back as
        // exactly R|U — pins the `perms()` mask, not just "≥ R|U".
        assert_eq!(leaf.perms, ru());
    }

    #[test]
    fn pteperms_contains_is_superset_membership() {
        let rwu = PtePerms::R.union(PtePerms::W).union(PtePerms::U);
        assert!(rwu.contains(ru())); // superset grants the subset
        assert!(rwu.contains(PtePerms::W));
        assert!(!ru().contains(PtePerms::W)); // R|U does not grant W
        assert!(!ru().contains(rwu)); // subset does not grant the superset
    }

    #[test]
    fn translate_of_an_unmapped_va_is_none() {
        let mem = MockPtMem::new(16);
        assert_eq!(translate(mem.root_pa(), 0x40201000, &mem), None);
    }

    #[test]
    fn a_within_page_copy_is_a_single_chunk() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();

        let chunks = record(&mem, src_root, 0x10000, dst_root, 0x20000, 64).unwrap();
        assert_eq!(chunks, std::vec![(0xA000_0000, 0xB000_0000, 64)]);
    }

    #[test]
    fn a_copy_spanning_a_page_splits_at_the_source_boundary() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        // Source starts 16 bytes before its page end; dest at a page base.
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();
        map(src_root, 0x11000, 0xA000_1000, ru(), &mut mem).unwrap();
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();

        let chunks = record(&mem, src_root, 0x10000 + 0x1000 - 16, dst_root, 0x20000, 64).unwrap();
        assert_eq!(
            chunks,
            std::vec![(0xA000_0000 + 0xff0, 0xB000_0000, 16), (0xA000_1000, 0xB000_0000 + 16, 48)],
        );
    }

    #[test]
    fn a_copy_spanning_a_page_splits_at_the_destination_boundary() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();
        map(dst_root, 0x21000, 0xB000_1000, wu(), &mut mem).unwrap();

        let chunks = record(&mem, src_root, 0x10000, dst_root, 0x20000 + 0x1000 - 16, 64).unwrap();
        assert_eq!(
            chunks,
            std::vec![(0xA000_0000, 0xB000_0000 + 0xff0, 16), (0xA000_0000 + 16, 0xB000_1000, 48)],
        );
    }

    #[test]
    fn copy_refuses_an_unmapped_source_page() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();

        assert_eq!(record(&mem, src_root, 0x10000, dst_root, 0x20000, 32), Err(CopyError::Unmapped));
    }

    #[test]
    fn copy_refuses_an_unmapped_destination_page() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();

        assert_eq!(record(&mem, src_root, 0x10000, dst_root, 0x20000, 32), Err(CopyError::Unmapped));
    }

    #[test]
    fn copy_refuses_a_range_outside_the_user_half() {
        let mem = MockPtMem::new(4);
        let root = mem.root_pa();
        assert_eq!(record(&mem, root, USER_VA_END, root, 0x20000, 16), Err(CopyError::BadRange));
    }

    #[test]
    fn copy_refuses_an_over_long_range() {
        let mem = MockPtMem::new(4);
        let root = mem.root_pa();
        assert_eq!(
            record(&mem, root, 0x10000, root, 0x20000, MAX_USER_STR_LEN + 1),
            Err(CopyError::BadRange),
        );
    }

    #[test]
    fn copy_refuses_a_source_without_read_permission() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        // Write-only-ish page: a valid leaf (W set) that nonetheless lacks R.
        map(src_root, 0x10000, 0xA000_0000, wu(), &mut mem).unwrap(); // no R
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();

        assert_eq!(record(&mem, src_root, 0x10000, dst_root, 0x20000, 32), Err(CopyError::Perms));
    }

    #[test]
    fn copy_refuses_a_destination_without_write_permission() {
        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();
        map(dst_root, 0x20000, 0xB000_0000, ru(), &mut mem).unwrap(); // no W

        assert_eq!(record(&mem, src_root, 0x10000, dst_root, 0x20000, 32), Err(CopyError::Perms));
    }

    #[test]
    fn copy_moves_the_bytes_through_the_resolved_frames() {
        use std::cell::RefCell;
        use std::collections::HashMap;

        let mut mem = MockPtMem::new(32);
        let src_root = mem.root_pa();
        let dst_root = fresh_root(&mut mem);
        map(src_root, 0x10000, 0xA000_0000, ru(), &mut mem).unwrap();
        map(dst_root, 0x20000, 0xB000_0000, wu(), &mut mem).unwrap();

        let store: RefCell<HashMap<usize, u8>> = RefCell::new(HashMap::new());
        for i in 0..8usize {
            store.borrow_mut().insert(0xA000_0000 + i, (i + 1) as u8);
        }
        copy_across(src_root, 0x10000, dst_root, 0x20000, 8, &mem, &mut |s, d, n| {
            let mut m = store.borrow_mut();
            for k in 0..n {
                let b = *m.get(&(s + k)).unwrap_or(&0);
                m.insert(d + k, b);
            }
        })
        .unwrap();

        let m = store.borrow();
        for i in 0..8usize {
            assert_eq!(m.get(&(0xB000_0000 + i)), Some(&((i + 1) as u8)));
        }
    }
}
