//! Sv39 virtual-address translation: the 3-level page-table walk the kernel's
//! higher-half and linear-map mappings rely on. Pure over `Memory` — the CPU
//! calls it for every fetch / load / store once `satp` selects Sv39.

use crate::mem::Memory;

/// `satp` mode field lives in bits 63:60; 8 selects Sv39 (0 is bare).
const SATP_MODE_SHIFT: u64 = 60;
const MODE_SV39: u64 = 8;

/// Physical page number width (bits 53:10 of a PTE, 43:0 of `satp`).
const PPN_MASK: u64 = (1 << 44) - 1;

/// PTE flag bits.
pub(crate) mod pte {
    pub const V: u64 = 1 << 0; // valid
    pub const R: u64 = 1 << 1; // readable
    pub const W: u64 = 1 << 2; // writable
    pub const X: u64 = 1 << 3; // executable
    pub const U: u64 = 1 << 4; // user-accessible
}

/// The kind of access being translated (selects the permission bit to check).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Access {
    Fetch,
    Load,
    Store,
}

/// Translation failed — a page fault for the attempted access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageFault;

/// Translate `va` to a physical address under `satp`. Bare mode is the
/// identity; Sv39 walks the 3-level table, honoring superpage leaves and
/// per-access permissions. `user` is whether the access is at U-mode privilege,
/// and `sum` whether `sstatus.SUM` is set — together they gate the U (user) bit
/// so userspace can't reach kernel pages and S-mode can't accidentally reach
/// user pages.
pub(crate) fn translate(
    satp: u64,
    va: u64,
    access: Access,
    mem: &Memory,
    user: bool,
    sum: bool,
) -> Result<u64, PageFault> {
    if satp >> SATP_MODE_SHIFT != MODE_SV39 {
        return Ok(va);
    }
    let (pte, pa) = walk_leaf(satp, va, mem)?;
    check_perms(pte, access, user, sum)?;
    Ok(pa)
}

/// Whether `satp` selects Sv39 (vs bare/identity mapping). The TLB only caches Sv39
/// translations; bare mode is a free identity and needs no lookup.
pub(crate) fn is_sv39(satp: u64) -> bool {
    satp >> SATP_MODE_SHIFT == MODE_SV39
}

/// Walk the Sv39 table to the **leaf PTE** for `va`, returning `(leaf_pte, paddr)`
/// with **no permission check** — the caller (or the TLB, per access) runs
/// [`check_perms`], since permissions depend on privilege/`SUM` which the *walk*
/// doesn't. `PageFault` if a level's PTE is invalid or the walk runs off the bottom.
/// Assumes Sv39 (callers gate bare mode with [`is_sv39`]).
pub(crate) fn walk_leaf(satp: u64, va: u64, mem: &Memory) -> Result<(u64, u64), PageFault> {
    let mut table = (satp & PPN_MASK) << 12;
    for level in (0u64..3).rev() {
        let vpn = (va >> (12 + 9 * level)) & 0x1ff;
        let pte = mem.read_u64(table + vpn * 8).map_err(|_| PageFault)?;
        if pte & pte::V == 0 {
            return Err(PageFault);
        }
        let ppn = (pte >> 10) & PPN_MASK;
        if pte & (pte::R | pte::X) != 0 {
            // Leaf PTE. Take the low (12 + 9*level) bits from the VA so
            // superpages (leaves above level 0) map their whole range.
            let mask = (1u64 << (12 + 9 * level)) - 1;
            return Ok((pte, ((ppn << 12) & !mask) | (va & mask)));
        }
        table = ppn << 12;
    }
    Err(PageFault)
}

pub(crate) fn check_perms(pte: u64, access: Access, user: bool, sum: bool) -> Result<(), PageFault> {
    // Privilege gate on the U bit, before the R/W/X bits.
    let user_page = pte & pte::U != 0;
    let privilege_ok = if user {
        // U-mode may only touch user pages.
        user_page
    } else if user_page {
        // S-mode reaching a user page: never for a fetch; for load/store only
        // when SUM permits it.
        !matches!(access, Access::Fetch) && sum
    } else {
        // S-mode reaching a supervisor page: always fine.
        true
    };
    if !privilege_ok {
        return Err(PageFault);
    }
    let ok = match access {
        Access::Fetch => pte & pte::X != 0,
        Access::Load => pte & pte::R != 0,
        Access::Store => pte & pte::W != 0,
    };
    if ok { Ok(()) } else { Err(PageFault) }
}

/// Number of direct-mapped TLB entries (power of two). 1024 comfortably covers a
/// scenario's hot working set of pages without a large clone cost.
const TLB_ENTRIES: usize = 1024;

/// One cached Sv39 translation: the leaf `pte` (for the per-access permission
/// re-check) and the 4 KiB physical page number `ppn`, tagged by virtual page number
/// and the cache `epoch` that validates it (a mismatched or zero epoch is stale).
#[derive(Clone, Copy, Default)]
struct TlbEntry {
    epoch: u64,
    vpn: u64,
    ppn: u64,
    pte: u64,
}

/// A per-hart translation cache: `vpn → (ppn, leaf pte)`, so a repeated access skips
/// the 3-level page walk. **A pure speedup** — permissions are re-checked per access
/// against the cached PTE, and the whole cache is invalidated (O(1) epoch bump) on
/// `satp` write / `sfence.vma`, exactly like the decode and block caches. Clones as
/// data for a snapshot fork (the forked machine shares the same page tables, so the
/// entries stay valid until its next `satp`/`sfence`).
#[derive(Clone)]
pub(crate) struct Tlb {
    epoch: u64,
    entries: Box<[TlbEntry]>,
}

impl Default for Tlb {
    fn default() -> Self {
        // Epoch starts at 1 so the zero-initialised entries (epoch 0) are stale.
        Self { epoch: 1, entries: vec![TlbEntry::default(); TLB_ENTRIES].into_boxed_slice() }
    }
}

impl Tlb {
    #[inline]
    fn index(vpn: u64) -> usize {
        (vpn as usize) & (TLB_ENTRIES - 1)
    }

    /// The cached `(ppn, leaf pte)` for `vpn`, or `None` on a miss / stale entry.
    pub(crate) fn get(&self, vpn: u64) -> Option<(u64, u64)> {
        let e = &self.entries[Self::index(vpn)];
        (e.epoch == self.epoch && e.vpn == vpn).then_some((e.ppn, e.pte))
    }

    /// Cache the translation of `vpn` (found by a fresh walk).
    pub(crate) fn insert(&mut self, vpn: u64, ppn: u64, pte: u64) {
        self.entries[Self::index(vpn)] = TlbEntry { epoch: self.epoch, vpn, ppn, pte };
    }

    /// Invalidate every entry in O(1) — the guest changed the translation regime.
    pub(crate) fn flush(&mut self) {
        self.epoch += 1;
    }
}

#[cfg(test)]
mod tlb_tests {
    use super::Tlb;

    #[test]
    fn a_cached_translation_hits_until_flushed() {
        let mut tlb = Tlb::default();
        assert_eq!(tlb.get(0x8_0000), None, "cold miss");
        tlb.insert(0x8_0000, 0x4_2000, 0xf1);
        assert_eq!(tlb.get(0x8_0000), Some((0x4_2000, 0xf1)), "hit after insert");
        assert_eq!(tlb.get(0x8_0001), None, "a different vpn still misses");
        tlb.flush();
        assert_eq!(tlb.get(0x8_0000), None, "flush (satp/sfence) invalidates every entry");
    }

    #[test]
    fn an_aliasing_vpn_evicts_the_slot() {
        let mut tlb = Tlb::default();
        tlb.insert(0, 0x100, 0xf);
        // A vpn that maps to the same direct-mapped slot overwrites it.
        tlb.insert(super::TLB_ENTRIES as u64, 0x200, 0xf);
        assert_eq!(tlb.get(0), None, "evicted by the aliasing insert");
        assert_eq!(tlb.get(super::TLB_ENTRIES as u64), Some((0x200, 0xf)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{Memory, RAM_BASE};

    /// Physical page number of a page-aligned address.
    fn ppn(addr: u64) -> u64 {
        addr >> 12
    }

    fn sv39_satp(root: u64) -> u64 {
        (MODE_SV39 << SATP_MODE_SHIFT) | ppn(root)
    }

    #[test]
    fn bare_mode_is_identity() {
        let mem = Memory::new(0x1000);
        assert_eq!(translate(0, 0xdead_beef, Access::Load, &mem, S_MODE, NO_SUM), Ok(0xdead_beef));
    }

    #[test]
    fn sv39_walks_three_levels_to_a_leaf() {
        let mut mem = Memory::new(0x10000);
        let root = RAM_BASE;
        let l1 = RAM_BASE + 0x1000;
        let l0 = RAM_BASE + 0x2000;
        let target = RAM_BASE + 0x3000;
        // VPN[2]=1, VPN[1]=2, VPN[0]=3, offset=0x55.
        let va = (1 << 30) | (2 << 21) | (3 << 12) | 0x55;
        mem.write_u64(root + 8, (ppn(l1) << 10) | pte::V).unwrap();
        mem.write_u64(l1 + 2 * 8, (ppn(l0) << 10) | pte::V).unwrap();
        mem.write_u64(l0 + 3 * 8, (ppn(target) << 10) | pte::V | pte::R | pte::W)
            .unwrap();

        let pa = translate(sv39_satp(root), va, Access::Load, &mem, S_MODE, NO_SUM).unwrap();
        assert_eq!(pa, target + 0x55);
    }

    #[test]
    fn sv39_leaf_at_top_level_is_a_gigapage() {
        let mut mem = Memory::new(0x10000);
        let root = RAM_BASE;
        // Leaf at level 2 maps a 1 GiB range; low 30 bits come from the VA.
        let va = (5 << 30) | 0x12345;
        let leaf_base = 5u64 << 30; // ppn aligned to 1 GiB
        mem.write_u64(root + 5 * 8, (ppn(leaf_base) << 10) | pte::V | pte::R | pte::X)
            .unwrap();

        let pa = translate(sv39_satp(root), va, Access::Fetch, &mem, S_MODE, NO_SUM).unwrap();
        assert_eq!(pa, leaf_base | 0x12345);
    }

    #[test]
    fn invalid_pte_faults() {
        let mem = Memory::new(0x10000);
        let va = (1 << 30) | 0x40; // root PTE is zeroed -> not valid
        assert_eq!(
            translate(sv39_satp(RAM_BASE), va, Access::Load, &mem, S_MODE, NO_SUM),
            Err(PageFault)
        );
    }

    #[test]
    fn missing_permission_faults() {
        let mut mem = Memory::new(0x10000);
        let root = RAM_BASE;
        let va = 5 << 30;
        // Read-only gigapage; a store must fault.
        mem.write_u64(root + 5 * 8, (ppn(va) << 10) | pte::V | pte::R)
            .unwrap();
        assert_eq!(
            translate(sv39_satp(root), va, Access::Store, &mem, S_MODE, NO_SUM),
            Err(PageFault)
        );
    }

    /// Privilege/SUM args for the perm tests: (user?, sum?).
    const S_MODE: bool = false;
    const U_MODE: bool = true;
    const NO_SUM: bool = false;
    const SUM: bool = true;

    /// Map a gigapage leaf at `va` with the given flags; return the satp.
    fn gigapage(mem: &mut Memory, va: u64, flags: u64) -> u64 {
        let root = RAM_BASE;
        mem.write_u64(root + (va >> 30) * 8, (ppn(va) << 10) | pte::V | flags)
            .unwrap();
        sv39_satp(root)
    }

    #[test]
    fn user_mode_cannot_touch_a_supervisor_page() {
        // The isolation invariant: a U-mode read of a kernel page (U=0) faults.
        let mut mem = Memory::new(0x10000);
        let va = 5 << 30;
        let satp = gigapage(&mut mem, va, pte::R | pte::W); // no U bit = kernel page
        assert_eq!(
            translate(satp, va, Access::Load, &mem, U_MODE, NO_SUM),
            Err(PageFault),
            "U-mode must not read a supervisor page"
        );
        // ...while S-mode reads it fine.
        assert!(translate(satp, va, Access::Load, &mem, S_MODE, NO_SUM).is_ok());
    }

    #[test]
    fn user_mode_can_touch_a_user_page() {
        let mut mem = Memory::new(0x10000);
        let va = 5 << 30;
        let satp = gigapage(&mut mem, va, pte::R | pte::U);
        assert!(translate(satp, va, Access::Load, &mem, U_MODE, NO_SUM).is_ok());
    }

    #[test]
    fn supervisor_needs_sum_to_read_a_user_page() {
        let mut mem = Memory::new(0x10000);
        let va = 5 << 30;
        let satp = gigapage(&mut mem, va, pte::R | pte::U);
        assert_eq!(
            translate(satp, va, Access::Load, &mem, S_MODE, NO_SUM),
            Err(PageFault),
            "S-mode without SUM cannot read a user page"
        );
        assert!(
            translate(satp, va, Access::Load, &mem, S_MODE, SUM).is_ok(),
            "S-mode with SUM can read a user page"
        );
    }

    #[test]
    fn supervisor_never_fetches_from_a_user_page_even_with_sum() {
        // SUM permits data access, never instruction fetch — an S-mode fetch from
        // a U-page always faults (no executing user code at supervisor privilege).
        let mut mem = Memory::new(0x10000);
        let va = 5 << 30;
        let satp = gigapage(&mut mem, va, pte::X | pte::U);
        assert_eq!(
            translate(satp, va, Access::Fetch, &mem, S_MODE, SUM),
            Err(PageFault)
        );
    }
}
