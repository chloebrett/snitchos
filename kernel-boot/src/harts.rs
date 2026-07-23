//! Hart topology decisions: assign dense logical ids to the harts the DTB
//! reports, so the boot hart is always logical 0 and the other usable harts get
//! logical 1.. — independent of the platform's physical `mhartid` numbering.
//!
//! QEMU `virt` boots on mhartid 0 or 1, both usable. The JH7110 boots on an
//! arbitrary U74 (harts 1–4) with the S7 monitor at hart 0 marked
//! `status="disabled"` — so "the other hart is `1 - boot`" is wrong on the board.
//! This module is the pure replacement: given the enumerated hart list and the
//! boot `mhartid`, produce the logical→mhartid map the kernel installs into
//! `LOGICAL_TO_MHARTID`.

/// One hart as reported by the DTB `/cpus` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HartInfo {
    /// Platform `mhartid` — the DTB `cpu@N` `reg` value.
    pub mhartid: u64,
    /// Whether we bring this hart up. `false` for a `status=disabled` monitor
    /// core (the JH7110 S7 at hart 0). The boot hart is always brought up
    /// regardless of this flag — we're already running on it.
    pub usable: bool,
}

/// Fill `out` with the logical→mhartid mapping: `out[0]` = the boot hart, then
/// the other `usable` harts in ascending `mhartid` order. Unusable non-boot harts
/// are skipped. Writes at most `out.len()` entries (the per-hart array capacity,
/// `MAX_HARTS`) and returns how many were written.
#[must_use]
pub fn assign_logical(harts: &[HartInfo], boot_mhartid: u64, out: &mut [u64]) -> usize {
    if out.is_empty() {
        return 0;
    }
    // Logical 0 is always the boot hart — we're running on it, regardless of what
    // the DTB says about its `usable` flag.
    out[0] = boot_mhartid;
    let mut count = 1;

    // Fill logical 1.. by repeatedly selecting the smallest usable, non-boot
    // `mhartid` strictly greater than the last one placed. Selection rather than
    // sort-in-place keeps this alloc-free (`harts` is borrowed immutably) and
    // correct for any input order — the DTB's `/cpus` ordering isn't guaranteed.
    let mut last = boot_mhartid;
    let mut placed_any = false;
    while count < out.len() {
        let next = harts
            .iter()
            .filter(|h| h.usable && h.mhartid != boot_mhartid)
            .map(|h| h.mhartid)
            .filter(|&m| !placed_any || m > last)
            .min();
        match next {
            Some(m) => {
                out[count] = m;
                count += 1;
                last = m;
                placed_any = true;
            }
            None => break,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vf2_skips_s7_and_orders_u74s_ascending_with_arbitrary_boot_hart() {
        // JH7110: hart 0 = S7 monitor (disabled), harts 1–4 = U74s. OpenSBI
        // booted us on hart 2. Logical 0 must be the boot hart; logical 1.. the
        // other U74s ascending; the S7 skipped.
        let harts = [
            HartInfo { mhartid: 0, usable: false }, // S7 monitor
            HartInfo { mhartid: 1, usable: true },
            HartInfo { mhartid: 2, usable: true }, // boot
            HartInfo { mhartid: 3, usable: true },
            HartInfo { mhartid: 4, usable: true },
        ];
        let mut out = [0u64; 4];
        let n = assign_logical(&harts, 2, &mut out);
        assert_eq!(n, 4);
        assert_eq!(out, [2, 1, 3, 4], "boot first, then other U74s ascending");
    }

    #[test]
    fn qemu_two_harts_matches_one_minus_hartid() {
        // QEMU -smp 2, both usable, booted on hart 1. The old code computed
        // LOGICAL_TO_MHARTID = { 0 -> 1, 1 -> 0 } via `1 - hart_id`. The new
        // logic must agree so the two-hart gate doesn't regress.
        let harts = [
            HartInfo { mhartid: 0, usable: true },
            HartInfo { mhartid: 1, usable: true },
        ];
        let mut out = [0u64; 4];
        let n = assign_logical(&harts, 1, &mut out);
        assert_eq!(n, 2);
        assert_eq!(&out[..2], &[1, 0]);
    }

    #[test]
    fn caps_at_out_len_dropping_highest_mhartids() {
        // More usable harts than the per-hart array capacity: only `out.len()`
        // are assigned — the boot hart plus the lowest-mhartid others.
        let harts = [
            HartInfo { mhartid: 0, usable: true }, // boot
            HartInfo { mhartid: 1, usable: true },
            HartInfo { mhartid: 2, usable: true },
            HartInfo { mhartid: 3, usable: true },
            HartInfo { mhartid: 4, usable: true },
        ];
        let mut out = [0u64; 4];
        let n = assign_logical(&harts, 0, &mut out);
        assert_eq!(n, 4);
        assert_eq!(out, [0, 1, 2, 3], "hart 4 dropped by capacity");
    }

    #[test]
    fn boot_hart_leads_even_if_dtb_marks_it_unusable() {
        // Defensive: we're running on the boot hart, so it is logical 0 no matter
        // what the DTB says about its status.
        let harts = [
            HartInfo { mhartid: 0, usable: false },
            HartInfo { mhartid: 1, usable: true },
        ];
        let mut out = [0u64; 4];
        let n = assign_logical(&harts, 0, &mut out);
        assert_eq!(n, 2);
        assert_eq!(&out[..2], &[0, 1]);
    }
}
