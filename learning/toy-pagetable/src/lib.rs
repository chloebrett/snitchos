//! toy-pagetable — a standalone Sv39 page-table playground.
//!
//! Sv39 = 39-bit virtual addresses, translated through a **3-level**
//! page table. A VA splits into three 9-bit indices + a 12-bit page
//! offset:
//!
//! ```text
//!  38      30 29      21 20      12 11         0
//! +----------+----------+----------+------------+
//! |  VPN[2]  |  VPN[1]  |  VPN[0]  |   offset   |
//! +----------+----------+----------+------------+
//!   9 bits     9 bits     9 bits      12 bits
//!  (root idx) (mid idx)  (leaf idx)
//! ```
//!
//! Each level is a 512-entry table of 64-bit PTEs (512 = 2^9, one per
//! index). A PTE either **points at the next table** (a *branch*: V=1,
//! R=W=X=0) or **is a leaf** (V=1, at least one of R/W/X) that ends the
//! walk and yields a physical page. A leaf can appear at *any* level:
//!
//! - leaf at level 0 → 4 KiB page   (offset = low 12 bits of VA)
//! - leaf at level 1 → 2 MiB page   (offset = low 21 bits) ← "megapage"
//! - leaf at level 2 → 1 GiB page   (offset = low 30 bits) ← "gigapage"
//!
//! The 1 GiB leaf is how the kernel's linear map covers all of RAM with
//! a single root PTE (see `LINEAR_OFFSET` in kernel-core/src/mmu.rs).
//!
//! Everything here mirrors `kernel-core/src/mmu.rs`. The three `todo!()`
//! exercises are: [`split_va`], [`translate`], and [`Mem::map_4kib`].
//! See `EXERCISES.md`.

/// PTE bit positions, straight from the RISC-V privileged spec.
pub const PTE_V: u64 = 1 << 0; // Valid
pub const PTE_R: u64 = 1 << 1; // Readable
pub const PTE_W: u64 = 1 << 2; // Writable
pub const PTE_X: u64 = 1 << 3; // eXecutable
pub const PTE_A: u64 = 1 << 6; // Accessed
pub const PTE_D: u64 = 1 << 7; // Dirty

/// The R|W|X mask — a PTE with V=1 is a leaf iff any of these are set,
/// and a branch iff none are.
pub const PTE_RWX: u64 = PTE_R | PTE_W | PTE_X;

/// Permission bits a caller can request on a leaf. V/A/D are added
/// automatically by [`leaf_pte`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Perms(u64);

impl Perms {
    pub const R: Perms = Perms(PTE_R);
    pub const W: Perms = Perms(PTE_W);
    pub const X: Perms = Perms(PTE_X);

    pub const fn rwx() -> Self {
        Perms(PTE_RWX)
    }
    pub const fn bits(self) -> u64 {
        self.0
    }
    pub const fn union(self, other: Perms) -> Self {
        Perms(self.0 | other.0)
    }
}

// --- PTE encode / decode (provided — these mirror kernel-core exactly) ---

/// The PPN (physical page number) field lives at PTE bits 53:10, while a
/// PA's page number is at bits 55:12. So encoding is `pa >> 12 << 10`,
/// i.e. `pa >> 2`. Getting this shift wrong is the classic PTE bug.
const fn pa_to_ppn_field(pa: usize) -> u64 {
    (pa as u64) >> 2
}

/// Recover the PA held in a PTE's PPN field. For a *branch* this is the
/// child table's PA; for a *leaf* it's the mapped page's base PA.
/// Inverse of the above: `pte >> 10 << 12`.
pub const fn pte_addr(pte: u64) -> usize {
    ((pte >> 10) << 12) as usize
}

/// Encode a leaf PTE: PPN(pa) + perms + V + A + D.
pub const fn leaf_pte(pa: usize, perms: Perms) -> u64 {
    pa_to_ppn_field(pa) | perms.bits() | PTE_V | PTE_A | PTE_D
}

/// Encode a branch (non-leaf) PTE pointing at a child table. V=1,
/// R=W=X=0 is the branch marker per the spec.
pub const fn branch_pte(child_pa: usize) -> u64 {
    pa_to_ppn_field(child_pa) | PTE_V
}

pub const fn pte_is_valid(pte: u64) -> bool {
    pte & PTE_V != 0
}

/// V=1 and at least one of R/W/X → leaf (ends the walk).
pub const fn pte_is_leaf(pte: u64) -> bool {
    pte_is_valid(pte) && (pte & PTE_RWX) != 0
}

/// V=1 and R=W=X=0 → branch (points at a child table).
pub const fn pte_is_branch(pte: u64) -> bool {
    pte_is_valid(pte) && (pte & PTE_RWX) == 0
}

/// Why a [`Mem::map_4kib`] walk couldn't install a mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// A valid PTE already occupies a slot the walk needed (a leaf in
    /// the way, or the exact VA already mapped).
    AlreadyMapped,
    /// Ran out of frames while trying to allocate an intermediate table.
    OutOfFrames,
}

/// Backing store for page tables. Stands in for physical memory: each
/// "table" is 512 u64 PTEs, addressed by a fake PA of `(index + 1) << 12`
/// so that PA 0 never collides with a real table and the `>>12`/`<<12`
/// round-trips cleanly. The root table is pre-installed at `root_pa()`.
///
/// `cap` bounds how many *intermediate* tables a walk may allocate, so
/// tests can force the `OutOfFrames` path.
pub struct Mem {
    tables: Vec<[u64; 512]>,
    cap: usize,
    allocs: usize,
}

impl Mem {
    /// `cap` = max intermediate-table allocations (root is free).
    pub fn new(cap: usize) -> Self {
        Self {
            tables: vec![[0u64; 512]],
            cap,
            allocs: 0,
        }
    }

    pub fn root_pa(&self) -> usize {
        1 << 12
    }

    pub fn alloc_count(&self) -> usize {
        self.allocs
    }

    /// Allocate a fresh zeroed table, returning its PA, or `None` if the
    /// cap is hit.
    pub fn alloc_table(&mut self) -> Option<usize> {
        if self.allocs >= self.cap {
            return None;
        }
        self.tables.push([0u64; 512]);
        self.allocs += 1;
        Some(self.tables.len() << 12)
    }

    pub fn read(&self, table_pa: usize, idx: usize) -> u64 {
        self.tables[(table_pa >> 12) - 1][idx]
    }

    pub fn write(&mut self, table_pa: usize, idx: usize, value: u64) {
        self.tables[(table_pa >> 12) - 1][idx] = value;
    }

    // ===================================================================
    // EXERCISE 3 — install a 4 KiB leaf (the WRITE walk).
    //
    // Mirror kernel-core's `map` + `walk_or_install`. Walk from the root
    // using vpn2, then vpn1, allocating an intermediate table whenever a
    // slot is empty (write a `branch_pte` to it). At level 0, write the
    // `leaf_pte(pa, perms)` for vpn0.
    //
    // Rules:
    //   * At each of the two upper levels: read the slot.
    //       - branch  → descend into `pte_addr(pte)`.
    //       - leaf    → a huge page is in the way → Err(AlreadyMapped).
    //       - invalid → `alloc_table()` (Err(OutOfFrames) if None), write
    //                   a `branch_pte` to the slot, descend into the new table.
    //   * At level 0: if the slot is already valid → Err(AlreadyMapped);
    //     otherwise write the leaf and Ok(()).
    //
    // A failed allocation does NOT unwind partially-installed tables
    // (matches the real kernel; see the step-5 plan).
    // ===================================================================
    pub fn map_4kib(&mut self, va: usize, target_pa: usize, perms: Perms) -> Result<(), MapError> {
        let (vpn2, vpn1, vpn0, _offset) = split_va(va);
        let vpn = [vpn0, vpn1, vpn2];

        let mut pa = self.root_pa();
        for level in [2, 1] {
            let pte = self.read(pa, vpn[level]);
            if pte_is_leaf(pte) {
                return Err(MapError::AlreadyMapped);
            } else if pte_is_branch(pte) {
                pa = pte_addr(pte);
            } else if !pte_is_valid(pte) {
                let Some(new_table) = self.alloc_table() else {
                    return Err(MapError::OutOfFrames);
                };
                self.write(pa, vpn[level], branch_pte(new_table));
                pa = new_table;
            }
        }

        let pte = self.read(pa, vpn[0]);
        if pte_is_valid(pte) {
            return Err(MapError::AlreadyMapped);
        }
        self.write(pa, vpn[0], leaf_pte(target_pa, perms));

        Ok(())
    }
}

/// Decompose an Sv39 VA into (VPN[2], VPN[1], VPN[0], offset).
///
/// EXERCISE 1 — the 9/9/9/12 carve. Each VPN is 9 bits (mask `0x1ff`):
///   * VPN[2] = bits 38:30   (shift right 30)
///   * VPN[1] = bits 29:21   (shift right 21)
///   * VPN[0] = bits 20:12   (shift right 12)
///   * offset = bits 11:0    (mask `0xfff`)
pub fn split_va(va: usize) -> (usize, usize, usize, usize) {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    let offset = va & 0xfff;
    (vpn2, vpn1, vpn0, offset)
}

/// Translate a VA to a PA by walking the page table — i.e. do by hand
/// what the hardware MMU does on every memory access. Returns `None` on
/// a page fault (an invalid PTE before reaching a leaf).
///
/// EXERCISE 2 — the READ walk, including huge-page leaves. Start at
/// `root_pa`, index with VPN[2], VPN[1], VPN[0] in turn:
///
/// - invalid PTE → return None (page fault).
/// - branch PTE → descend into `pte_addr(pte)` and continue.
/// - leaf PTE → STOP. The page base is `pte_addr(pte)`; OR in the VA's
///   low bits, where the number of low bits depends on the level the leaf
///   sat at: level 0 → 12 bits (4 KiB), level 1 → 21 bits (2 MiB), level 2
///   → 30 bits (1 GiB). In one expression: `offset_mask = (1 << (12 + 9 *
///   level)) - 1`, and the answer is `pte_addr(pte) | (va & offset_mask)`.
///
/// Walking past level 0 without hitting a leaf is also a fault.
pub fn translate(mem: &Mem, root_pa: usize, va: usize) -> Option<usize> {
    let (vpn2, vpn1, vpn0, _offset) = split_va(va);
    let vpn = [vpn0, vpn1, vpn2];

    let mut pa = root_pa;
    for level in [2, 1, 0] {
        let pte = mem.read(pa, vpn[level]);
        if !pte_is_valid(pte) {
            return None;
        }
        pa = pte_addr(pte);
        if pte_is_leaf(pte) {
            let offset_mask = (1 << (12 + 9 * level)) - 1;
            return Some(pa | (va & offset_mask));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Exercise 1: split_va -----------------------------------------

    #[test]
    fn split_va_extracts_indices() {
        // 0x80200000 → vpn2=2, vpn1=1, vpn0=0, offset=0 (mirrors the
        // real kernel-core test).
        assert_eq!(split_va(0x80200000), (2, 1, 0, 0));
    }

    #[test]
    fn split_va_extracts_nonzero_vpn0_and_offset() {
        // One 4 KiB page up, plus a sub-page offset.
        assert_eq!(split_va(0x80201abc), (2, 1, 1, 0xabc));
    }

    #[test]
    fn split_va_each_index_is_nine_bits() {
        // All-ones in each field → every VPN should be 0x1ff, offset 0xfff.
        let va = (0x1ff << 30) | (0x1ff << 21) | (0x1ff << 12) | 0xfff;
        assert_eq!(split_va(va), (0x1ff, 0x1ff, 0x1ff, 0xfff));
    }

    #[test]
    fn split_va_higher_half_kernel_va_lands_in_root_slot_510() {
        // The kernel image's higher-half VA. The mask must drop the
        // sign-extension bits 63:39, but bits 38:32 ARE real address bits:
        //   0xffffffff_80200000 → bits 38:30 = 0b111111110 = 510.
        // So vpn2 = 510 (NOT 2 — the identity VA 0x80200000 is what gives 2).
        // Dual-mapping the kernel at root[2] (identity) and root[510]
        // (higher-half) works precisely because these don't collide.
        assert_eq!(split_va(0xffffffff_80200000), (510, 1, 0, 0));
    }

    // ---- Test helper: hand-build a mapping at a chosen leaf level ------
    //
    // Provided (not an exercise) so the translate tests can construct
    // tables without depending on map_4kib. `leaf_level` 0/1/2 builds a
    // 4 KiB / 2 MiB / 1 GiB leaf.
    fn install_leaf(mem: &mut Mem, va: usize, pa: usize, leaf_level: usize, perms: Perms) {
        let (vpn2, vpn1, vpn0, _) = split_va(va);
        let idxs = [vpn2, vpn1, vpn0];
        let mut table_pa = mem.root_pa();
        for (step, &idx) in idxs.iter().enumerate() {
            let level = 2 - step;
            if level == leaf_level {
                mem.write(table_pa, idx, leaf_pte(pa, perms));
                return;
            }
            let pte = mem.read(table_pa, idx);
            table_pa = if pte_is_valid(pte) {
                pte_addr(pte)
            } else {
                let child = mem.alloc_table().expect("test Mem cap too small");
                mem.write(table_pa, idx, branch_pte(child));
                child
            };
        }
    }

    // ---- Exercise 2: translate ----------------------------------------

    #[test]
    fn translate_4kib_page_combines_base_and_offset() {
        let mut mem = Mem::new(8);
        install_leaf(&mut mem, 0x80201000, 0x90000000, 0, Perms::rwx());
        assert_eq!(translate(&mem, mem.root_pa(), 0x80201abc), Some(0x90000abc));
    }

    #[test]
    fn translate_megapage_uses_21_bit_offset() {
        // Leaf at level 1 maps a whole 2 MiB region. A VA anywhere in it
        // resolves to base + (VA & 0x1fffff).
        let mut mem = Mem::new(8);
        install_leaf(&mut mem, 0x80200000, 0x40000000, 1, Perms::rwx());
        // 0x80200000 + 0x1abcd is within the same 2 MiB page.
        assert_eq!(
            translate(&mem, mem.root_pa(), 0x80200000 + 0x1abcd),
            Some(0x40000000 + 0x1abcd),
        );
    }

    #[test]
    fn translate_gigapage_uses_30_bit_offset() {
        // Leaf at level 2 maps a whole 1 GiB region (the linear-map shape).
        let mut mem = Mem::new(8);
        install_leaf(&mut mem, 0x40000000, 0x80000000, 2, Perms::rwx());
        // Deep inside the gigapage.
        assert_eq!(
            translate(&mem, mem.root_pa(), 0x40000000 + 0x3abcdef),
            Some(0x80000000 + 0x3abcdef),
        );
    }

    #[test]
    fn translate_unmapped_va_faults() {
        let mem = Mem::new(8); // empty root → nothing mapped
        assert_eq!(translate(&mem, mem.root_pa(), 0x80201000), None);
    }

    #[test]
    fn translate_faults_when_walk_dead_ends_before_leaf() {
        // Branch installed at the root, but the mid slot is empty.
        let mut mem = Mem::new(8);
        let child = mem.alloc_table().unwrap();
        let (vpn2, _, _, _) = split_va(0x80201000);
        mem.write(mem.root_pa(), vpn2, branch_pte(child));
        assert_eq!(translate(&mem, mem.root_pa(), 0x80201000), None);
    }

    // ---- Exercise 3: map_4kib -----------------------------------------

    #[test]
    fn map_installs_leaf_at_expected_indices() {
        let mut mem = Mem::new(8);
        let va = 0x40201000; // vpn2=1, vpn1=1, vpn0=1
        let pa = 0x90000000;
        mem.map_4kib(va, pa, Perms::R.union(Perms::W)).unwrap();

        let root = mem.read(mem.root_pa(), 1);
        assert!(pte_is_branch(root), "root[1] should be a branch");
        let mid = mem.read(pte_addr(root), 1);
        assert!(pte_is_branch(mid), "mid[1] should be a branch");
        let leaf = mem.read(pte_addr(mid), 1);
        assert_eq!(leaf, leaf_pte(pa, Perms::R.union(Perms::W)));
    }

    #[test]
    fn map_allocates_two_intermediate_tables_in_empty_root() {
        let mut mem = Mem::new(8);
        mem.map_4kib(0x1000, 0x80100000, Perms::R).unwrap();
        assert_eq!(mem.alloc_count(), 2);
    }

    #[test]
    fn map_reuses_existing_intermediate_tables() {
        let mut mem = Mem::new(8);
        mem.map_4kib(0x1000, 0x80100000, Perms::R).unwrap();
        mem.map_4kib(0x2000, 0x80101000, Perms::R).unwrap(); // same mid+leaf table
        assert_eq!(mem.alloc_count(), 2);
    }

    #[test]
    fn map_double_map_same_va_is_already_mapped() {
        let mut mem = Mem::new(8);
        mem.map_4kib(0x1000, 0x80100000, Perms::R).unwrap();
        assert_eq!(
            mem.map_4kib(0x1000, 0x80200000, Perms::R),
            Err(MapError::AlreadyMapped),
        );
    }

    #[test]
    fn map_out_of_frames_when_allocator_empty() {
        let mut mem = Mem::new(0);
        assert_eq!(
            mem.map_4kib(0x1000, 0x80100000, Perms::R),
            Err(MapError::OutOfFrames),
        );
    }

    // ---- Integration: map then translate round-trips ------------------

    #[test]
    fn map_then_translate_round_trips() {
        let mut mem = Mem::new(8);
        mem.map_4kib(0x80201000, 0x90000000, Perms::rwx()).unwrap();
        assert_eq!(translate(&mem, mem.root_pa(), 0x80201abc), Some(0x90000abc));
    }

    // ---- Property: map/translate round-trip over the whole VA space ----
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn map_then_translate_round_trips_for_any_page_and_offset(
            va_page in 0usize..(1 << 27), // 27-bit page number = full 39-bit VA
            pa_page in 0usize..(1 << 27),
            offset in 0usize..4096,
        ) {
            let va = va_page << 12;
            let pa = pa_page << 12;
            let mut mem = Mem::new(8);
            mem.map_4kib(va, pa, Perms::rwx()).unwrap();
            prop_assert_eq!(translate(&mem, mem.root_pa(), va + offset), Some(pa + offset));
        }
    }
}
