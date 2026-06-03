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

#[cfg(test)]
mod tests {
    use super::*;

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
