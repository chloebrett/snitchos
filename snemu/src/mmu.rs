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
/// per-access permissions.
pub(crate) fn translate(
    satp: u64,
    va: u64,
    access: Access,
    mem: &Memory,
) -> Result<u64, PageFault> {
    if satp >> SATP_MODE_SHIFT != MODE_SV39 {
        return Ok(va);
    }

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
            check_perms(pte, access)?;
            let mask = (1u64 << (12 + 9 * level)) - 1;
            return Ok(((ppn << 12) & !mask) | (va & mask));
        }
        table = ppn << 12;
    }
    Err(PageFault)
}

fn check_perms(pte: u64, access: Access) -> Result<(), PageFault> {
    let ok = match access {
        Access::Fetch => pte & pte::X != 0,
        Access::Load => pte & pte::R != 0,
        Access::Store => pte & pte::W != 0,
    };
    if ok { Ok(()) } else { Err(PageFault) }
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
        assert_eq!(translate(0, 0xdead_beef, Access::Load, &mem), Ok(0xdead_beef));
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

        let pa = translate(sv39_satp(root), va, Access::Load, &mem).unwrap();
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

        let pa = translate(sv39_satp(root), va, Access::Fetch, &mem).unwrap();
        assert_eq!(pa, leaf_base | 0x12345);
    }

    #[test]
    fn invalid_pte_faults() {
        let mem = Memory::new(0x10000);
        let va = (1 << 30) | 0x40; // root PTE is zeroed -> not valid
        assert_eq!(
            translate(sv39_satp(RAM_BASE), va, Access::Load, &mem),
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
            translate(sv39_satp(root), va, Access::Store, &mem),
            Err(PageFault)
        );
    }
}
