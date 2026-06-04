//! Sv39 page-table types and PTE encoding. Pure bit-twiddling — no
//! CSR access, no asm. The kernel binary owns the static instances
//! and the `csrw satp` / `sfence.vma` bridge.
//!
//! See `plans/v0.4-memory-concepts.md` § 2-3 for the Sv39 reference.

/// VA = PA + KERNEL_OFFSET for kernel-space mappings. Matches Linux
/// RISC-V's `PAGE_OFFSET - PHYS_BASE` with PHYS_BASE = 0x80000000.
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

    pub const fn union(self, other: PtePerms) -> Self { PtePerms(self.0 | other.0) }
}

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

/// Encode a leaf PTE for a page mapping. Sets V, A, D, plus any
/// permissions the caller passes.
pub const fn leaf_pte(pa: usize, perms: PtePerms) -> u64 {
    pa_to_pte_ppn(pa) | perms.bits() | PTE_V | PTE_A | PTE_D
}

/// Encode a non-leaf PTE pointing at a child page table. Per the
/// spec, R=W=X=0 with V=1 is the non-leaf marker.
pub const fn branch_pte(child_pa: usize) -> u64 {
    pa_to_pte_ppn(child_pa) | PTE_V
}

/// Decompose an Sv39 virtual address into its three VPN indices and
/// the page offset. VPN[i] indexes the i-th level of the page table
/// walk, root = VPN[2].
pub const fn split_va(va: usize) -> (usize, usize, usize, usize) {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    let offset = va & 0xfff;
    (vpn2, vpn1, vpn0, offset)
}

/// 4 KiB-aligned page table holding 512 Sv39 PTEs. The layout is
/// dictated by hardware; do not reorder fields.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
pub struct PageTable {
    entries: [u64; 512],
}

impl PageTable {
    pub const fn new() -> Self {
        Self { entries: [0; 512] }
    }

    pub fn entry(&self, idx: usize) -> u64 {
        self.entries[idx]
    }

    /// Set entry `idx` to `value` raw. Used by the kernel to clear an
    /// identity-half root entry as part of step 2d (identity unmap).
    pub fn set_entry(&mut self, idx: usize, value: u64) {
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
        let (vpn2, vpn1, _, _) = split_va(va);

        let existing_root = self.entries[vpn2];
        if existing_root == 0 {
            self.entries[vpn2] = branch_pte(mid_pa);
        } else if existing_root != branch_pte(mid_pa) {
            return false;
        }

        let new_leaf = leaf_pte(pa, perms);
        let existing_leaf = mid.entries[vpn1];
        if existing_leaf == new_leaf {
            return true;
        }
        if existing_leaf != 0 {
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
    fn read_entry(&self, table_pa: usize, idx: usize) -> u64;

    /// Write entry `idx` of the table at `table_pa`. `idx` is in `0..512`.
    fn write_entry(&mut self, table_pa: usize, idx: usize, value: u64);
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
}

/// PTE has V=1, R=W=X=0 → non-leaf (points at child table).
const fn pte_is_branch(pte: u64) -> bool {
    let v = pte & PTE_V != 0;
    let rwx = pte & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits());
    v && rwx == 0
}

/// PTE has V=1 and at least one of R/W/X — leaf.
const fn pte_is_leaf(pte: u64) -> bool {
    let v = pte & PTE_V != 0;
    let rwx = pte & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits());
    v && rwx != 0
}

/// Recover a child table's PA from a non-leaf PTE. Inverse of
/// `pa_to_pte_ppn`: PPN at bits 53:10 → PA = PPN << 12 = pte >> 10 << 12.
const fn branch_pte_child_pa(pte: u64) -> usize {
    ((pte >> 10) << 12) as usize
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
    let (vpn2, vpn1, vpn0, _) = split_va(va);
    let mid_pa = walk_or_install(root_pa, vpn2, mem)?;
    let leaf_table_pa = walk_or_install(mid_pa, vpn1, mem)?;
    let existing = mem.read_entry(leaf_table_pa, vpn0);
    if existing & PTE_V != 0 {
        return Err(MapError::AlreadyMapped);
    }
    mem.write_entry(leaf_table_pa, vpn0, leaf_pte(pa, perms));
    Ok(())
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
    if pte_is_leaf(pte) {
        return Err(MapError::AlreadyMapped);
    }
    if pte_is_branch(pte) {
        return Ok(branch_pte_child_pa(pte));
    }
    let new_pa = mem.alloc_zeroed_table().ok_or(MapError::OutOfFrames)?;
    mem.write_entry(table_pa, idx, branch_pte(new_pa));
    Ok(new_pa)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// Host-side `PtMem` backed by a Vec of PageTables. PA encoding:
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

        fn read_entry(&self, table_pa: usize, idx: usize) -> u64 {
            let t_idx = (table_pa >> 12) - 1;
            self.tables[t_idx].entries[idx]
        }

        fn write_entry(&mut self, table_pa: usize, idx: usize, value: u64) {
            let t_idx = (table_pa >> 12) - 1;
            self.tables[t_idx].entries[idx] = value;
        }
    }

    /// Walk root → mid → leaf table given the freshly-built root, returning
    /// the leaf PageTable that holds the final PTE. Avoids repeating the
    /// branch-chasing boilerplate in every assertion.
    fn leaf_table_of<'a>(mem: &'a MockPtMem, va: usize) -> &'a PageTable {
        let (vpn2, vpn1, _, _) = split_va(va);
        let root = mem.table(mem.root_pa());
        let mid_pa = branch_pte_child_pa(root.entry(vpn2));
        let mid = mem.table(mid_pa);
        let leaf_pa = branch_pte_child_pa(mid.entries[vpn1]);
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
        assert!(pte_is_branch(root.entry(1)), "root[1] should be a branch");
        let mid_pa = branch_pte_child_pa(root.entry(1));
        let mid = mem.table(mid_pa);
        assert!(pte_is_branch(mid.entries[1]), "mid[1] should be a branch");
        let leaf = leaf_table_of(&mem, va);
        assert_eq!(leaf.entries[1], leaf_pte(pa, perms));
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
        mem.write_entry(root_pa, 1, leaf_pte(0x80000000, PtePerms::rwxg()));
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
        let mid_pa = branch_pte_child_pa(mem.read_entry(root_pa, 0));
        mem.write_entry(mid_pa, 2, leaf_pte(0x80200000, PtePerms::rwxg()));
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
        assert!(pte_is_branch(mem.read_entry(root_pa, 0)));
    }

    #[test]
    fn map_propagates_perms_to_leaf_pte() {
        let mut mem = MockPtMem::new(8);
        let perms = PtePerms::R.union(PtePerms::W).union(PtePerms::G);
        map(mem.root_pa(), 0x1000, 0x80100000, perms, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        assert_eq!(pte & PtePerms::R.bits(), PtePerms::R.bits());
        assert_eq!(pte & PtePerms::W.bits(), PtePerms::W.bits());
        assert_eq!(pte & PtePerms::G.bits(), PtePerms::G.bits());
        assert_eq!(pte & PtePerms::X.bits(), 0);
    }

    #[test]
    fn map_sets_a_and_d_on_leaf() {
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80100000, PtePerms::R, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        assert_eq!(pte & PTE_A, PTE_A);
        assert_eq!(pte & PTE_D, PTE_D);
    }

    #[test]
    fn map_encodes_pa_at_correct_pte_bits() {
        // PA 0x80200000 → PPN 0x80200 → PTE field at bits 53:10 =
        // 0x80200 << 10 = 0x20080000.
        let mut mem = MockPtMem::new(8);
        map(mem.root_pa(), 0x1000, 0x80200000, PtePerms::R, &mut mem).unwrap();
        let pte = leaf_table_of(&mem, 0x1000).entries[1];
        let ppn_field = pte & !0x3ff;
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
    fn leaf_pte_encodes_ppn_and_flags() {
        // PA 0x80200000 → PPN 0x80200 → field bits = 0x80200 << 10 = 0x20080000.
        // Permissions: R+W+X+G = bits 1|2|3|5 = 0b101110 = 0x2E.
        // Plus V (bit 0), A (bit 6), D (bit 7).
        // Total: 0x20080000 | 0x2E | 0x01 | 0x40 | 0x80 = 0x200800EF.
        let pte = leaf_pte(0x80200000, PtePerms::rwxg());
        assert_eq!(pte, 0x200800EF);
    }

    #[test]
    fn leaf_pte_with_no_perms_still_has_v_a_d() {
        // V=1, A=1, D=1 unconditionally — caller can still produce
        // a no-access leaf if they want (rare; documents the rule).
        let pte = leaf_pte(0, PtePerms::empty());
        assert_eq!(pte & PTE_V, PTE_V);
        assert_eq!(pte & PTE_A, PTE_A);
        assert_eq!(pte & PTE_D, PTE_D);
        assert_eq!(pte & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits()), 0);
    }

    #[test]
    fn branch_pte_has_no_perm_bits() {
        // R=W=X=0 with V=1 is the non-leaf marker. PPN encodes the
        // child table's PA. No A/D since hardware never sets those
        // on a non-leaf walk.
        let pte = branch_pte(0x80300000);
        assert_eq!(pte & PTE_V, PTE_V);
        assert_eq!(pte & (PtePerms::R.bits() | PtePerms::W.bits() | PtePerms::X.bits()), 0);
        assert_eq!(pte & PTE_A, 0);
        assert_eq!(pte & PTE_D, 0);
        // PPN = 0x80300 → at bits 53:10 = 0x80300 << 10 = 0x200C0000.
        assert_eq!(pte & !0x3ff, 0x200C0000);
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
        pt.set_entry(2, 0xdeadbeef);
        pt.set_entry(5, 0xcafebabe);
        assert_eq!(pt.entry(2), 0xdeadbeef);
        assert_eq!(pt.entry(5), 0xcafebabe);
        // Clear entry 2; entry 5 untouched.
        pt.set_entry(2, 0);
        assert_eq!(pt.entry(2), 0);
        assert_eq!(pt.entry(5), 0xcafebabe);
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

        let (id_vpn2, _, _, _) = split_va(id_va);
        let (high_vpn2, _, _, _) = split_va(high_va);

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
        assert_eq!(split_va(0x80200000), (2, 1, 0, 0));
    }

    #[test]
    fn split_va_extracts_nonzero_vpn0() {
        // 0x80201000 = 0x80200000 + one 4 KiB page → vpn0 = 1, not 0.
        // Catches mutations of the vpn0 extraction (>> 12) that survive
        // when all test VAs are 2 MiB-aligned and vpn0 is coincidentally 0.
        assert_eq!(split_va(0x80201000), (2, 1, 1, 0));
    }

    #[test]
    fn split_va_handles_mmio_region() {
        // 0x10000000 → vpn2 = 0, vpn1 = 128, vpn0 = 0.
        assert_eq!(split_va(0x10000000), (0, 128, 0, 0));
    }

    #[test]
    fn map_2mib_installs_branch_in_root_and_leaf_in_mid() {
        let mut root = PageTable::new();
        let mut mid = PageTable::new();
        let mid_pa = 0x80300000;

        assert!(root.map_2mib(&mut mid, mid_pa, 0x80200000, 0x80200000, PtePerms::rwxg()));

        // VA 0x80200000 → vpn2=2, vpn1=1.
        assert_eq!(root.entry(2), branch_pte(mid_pa));
        assert_eq!(mid.entry(1), leaf_pte(0x80200000, PtePerms::rwxg()));
        // All other entries untouched.
        for i in 0..512 {
            if i != 2 {
                assert_eq!(root.entry(i), 0, "root[{i}] should be zero");
            }
            if i != 1 {
                assert_eq!(mid.entry(i), 0, "mid[{i}] should be zero");
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
        assert_eq!(mid.entry(1), leaf_pte(0x80200000, PtePerms::rwxg()));
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
        assert_eq!(root.entry(2), branch_pte(mid_pa));
        assert_eq!(mid.entry(1), leaf_pte(0x80200000, PtePerms::rwxg()));
        assert_eq!(mid.entry(2), leaf_pte(0x80400000, PtePerms::rwxg()));
    }
}
