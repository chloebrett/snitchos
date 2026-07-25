//! RISC-V PLIC register-offset logic — pure, host-tested. The MMIO adapter (the
//! volatile reads/writes, the claim→handle→complete sequence against a live
//! device) lives in `kernel/src/`; this is only the byte-offset arithmetic, which
//! is exactly what's easy to get subtly wrong (the `0x80` per-context stride, the
//! source/32 word split) and worth pinning against the spec.
//!
//! Layout (PLIC spec, all offsets in bytes from the PLIC base):
//! - `base + 4·source`               — interrupt `source`'s priority (WARL)
//! - `base + 0x1000 + 4·(source/32)` — pending bits
//! - `base + 0x2000 + 0x80·ctx + 4·(source/32)` — per-context enable words
//! - `base + 0x20_0000 + 0x1000·ctx` — per-context priority threshold
//! - `base + 0x20_0004 + 0x1000·ctx` — per-context claim (read) / complete (write)
//!
//! "Context" is a (hart, privilege) pair the platform numbers; the caller derives
//! it from the DTB (`interrupts-extended`) and passes it in — this module never
//! assumes a mapping.

/// Byte offset of interrupt `source`'s priority register. Source 0 is the
/// "no interrupt" sentinel and has no priority; real sources are `≥ 1`.
#[must_use]
pub const fn priority_offset(source: u32) -> usize {
    source as usize * 4
}

/// Byte offset of the enable *word* that holds `source`'s bit for `context`.
/// The bit within the word is [`enable_bit`].
#[must_use]
pub const fn enable_offset(context: u32, source: u32) -> usize {
    0x2000 + context as usize * 0x80 + (source as usize / 32) * 4
}

/// The bit position of `source` within its [`enable_offset`] word (and its
/// pending word) — sources are packed 32 to a word.
#[must_use]
pub const fn enable_bit(source: u32) -> u32 {
    source % 32
}

/// Byte offset of `context`'s priority-threshold register. An interrupt fires to
/// a context only if its priority is *strictly greater* than this threshold, so
/// a threshold of 0 lets any nonzero-priority source through.
#[must_use]
pub const fn threshold_offset(context: u32) -> usize {
    0x20_0000 + context as usize * 0x1000
}

/// Byte offset of `context`'s claim/complete register: reading it *claims* the
/// highest-priority pending source (returning its id, or 0 for none); writing the
/// id back *completes* it, re-arming the source.
#[must_use]
pub const fn claim_offset(context: u32) -> usize {
    0x20_0004 + context as usize * 0x1000
}

/// MMIO access to a PLIC, abstracted so the register *sequence* logic here stays
/// host-tested without a live device — the same seam `FwCfgTransport` gives the
/// fw_cfg handshake. The kernel supplies the volatile-register impl.
pub trait PlicTransport {
    fn read_reg(&self, offset: usize) -> u32;
    fn write_reg(&mut self, offset: usize, value: u32);
}

/// Route interrupt `source` to `context`: give it a nonzero priority, drop the
/// context threshold to 0 (accept any nonzero-priority source), and set the
/// source's enable bit — **preserving the other bits in that enable word**, since
/// a context may enable several sources.
pub fn enable_source<T: PlicTransport>(plic: &mut T, context: u32, source: u32) {
    plic.write_reg(priority_offset(source), 1);
    plic.write_reg(threshold_offset(context), 0);
    let word = enable_offset(context, source);
    let bit = 1u32 << enable_bit(source);
    let current = plic.read_reg(word);
    plic.write_reg(word, current | bit);
}

/// Claim the highest-priority interrupt pending for `context` (reads the
/// claim register). `None` when nothing is pending — the PLIC returns id 0, the
/// "no interrupt" sentinel — so the caller can loop until it drains.
pub fn claim<T: PlicTransport>(plic: &T, context: u32) -> Option<u32> {
    let id = plic.read_reg(claim_offset(context));
    (id != 0).then_some(id)
}

/// Signal completion of `source` for `context` (writes the id back to the
/// claim/complete register), re-arming the source for the next interrupt.
pub fn complete<T: PlicTransport>(plic: &mut T, context: u32, source: u32) {
    plic.write_reg(claim_offset(context), source);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    /// Mock PLIC: a sparse register map. Reads default to 0 (a freshly-reset PLIC).
    #[derive(Default)]
    struct MockPlic {
        regs: BTreeMap<usize, u32>,
    }
    impl PlicTransport for MockPlic {
        fn read_reg(&self, offset: usize) -> u32 {
            self.regs.get(&offset).copied().unwrap_or(0)
        }
        fn write_reg(&mut self, offset: usize, value: u32) {
            self.regs.insert(offset, value);
        }
    }

    // Concrete exemplar: QEMU `virt` routes UART0 (ns16550a) to PLIC source 10,
    // and hart 0's S-mode is context 1. These are the numbers the kernel will use,
    // so the offsets are pinned to their spec-derived values, not to my arithmetic.
    const UART0_SOURCE: u32 = 10;
    const HART0_S_CONTEXT: u32 = 1;

    #[test]
    fn priority_offset_is_four_bytes_per_source() {
        assert_eq!(priority_offset(UART0_SOURCE), 0x28);
        assert_eq!(priority_offset(1), 0x04);
    }

    #[test]
    fn enable_offset_strides_0x80_per_context() {
        // Context 1, source 10: base of enables (0x2000) + one context stride (0x80).
        assert_eq!(enable_offset(HART0_S_CONTEXT, UART0_SOURCE), 0x2080);
        // Context 0 sits at the enable base.
        assert_eq!(enable_offset(0, UART0_SOURCE), 0x2000);
    }

    #[test]
    fn enable_offset_and_bit_split_sources_into_32_bit_words() {
        // Source 40 lives in the *second* word (40/32 = 1) at bit 40%32 = 8.
        assert_eq!(enable_offset(HART0_S_CONTEXT, 40), 0x2084);
        assert_eq!(enable_bit(40), 8);
        assert_eq!(enable_bit(UART0_SOURCE), 10);
    }

    #[test]
    fn threshold_and_claim_stride_0x1000_per_context() {
        assert_eq!(threshold_offset(HART0_S_CONTEXT), 0x20_1000);
        assert_eq!(claim_offset(HART0_S_CONTEXT), 0x20_1004);
        // Context 0 (typically hart 0 M-mode) is the block base.
        assert_eq!(threshold_offset(0), 0x20_0000);
        assert_eq!(claim_offset(0), 0x20_0004);
    }

    #[test]
    fn enable_source_sets_priority_threshold_and_the_enable_bit() {
        let mut plic = MockPlic::default();
        enable_source(&mut plic, HART0_S_CONTEXT, UART0_SOURCE);
        assert_ne!(plic.read_reg(priority_offset(UART0_SOURCE)), 0, "source needs a nonzero priority");
        assert_eq!(plic.read_reg(threshold_offset(HART0_S_CONTEXT)), 0, "threshold 0 accepts it");
        let word = plic.read_reg(enable_offset(HART0_S_CONTEXT, UART0_SOURCE));
        assert_eq!(word & (1 << enable_bit(UART0_SOURCE)), 1 << enable_bit(UART0_SOURCE));
    }

    #[test]
    fn enable_source_preserves_other_bits_in_the_enable_word() {
        // Two sources share the same enable word; enabling the second must not
        // clear the first — a read-modify-write, not a blind store.
        let mut plic = MockPlic::default();
        enable_source(&mut plic, HART0_S_CONTEXT, 5);
        enable_source(&mut plic, HART0_S_CONTEXT, UART0_SOURCE);
        let word = plic.read_reg(enable_offset(HART0_S_CONTEXT, UART0_SOURCE));
        assert_eq!(word & (1 << 5), 1 << 5, "the earlier source stayed enabled");
        assert_eq!(word & (1 << UART0_SOURCE), 1 << UART0_SOURCE);
    }

    #[test]
    fn enabling_a_source_twice_leaves_it_enabled() {
        // The enable is a read-modify-write with OR — idempotent. A stray XOR would
        // toggle the bit back OFF on the second call (re-enabling an already-enabled
        // source is a normal thing to do during re-init).
        let mut plic = MockPlic::default();
        enable_source(&mut plic, HART0_S_CONTEXT, UART0_SOURCE);
        enable_source(&mut plic, HART0_S_CONTEXT, UART0_SOURCE);
        let word = plic.read_reg(enable_offset(HART0_S_CONTEXT, UART0_SOURCE));
        assert_eq!(word & (1 << enable_bit(UART0_SOURCE)), 1 << enable_bit(UART0_SOURCE), "still enabled");
    }

    #[test]
    fn claim_returns_the_pending_source_or_none() {
        let mut plic = MockPlic::default();
        assert_eq!(claim(&plic, HART0_S_CONTEXT), None, "id 0 = nothing pending");
        plic.write_reg(claim_offset(HART0_S_CONTEXT), UART0_SOURCE);
        assert_eq!(claim(&plic, HART0_S_CONTEXT), Some(UART0_SOURCE));
    }

    #[test]
    fn complete_writes_the_source_back_to_the_claim_register() {
        let mut plic = MockPlic::default();
        complete(&mut plic, HART0_S_CONTEXT, UART0_SOURCE);
        assert_eq!(plic.read_reg(claim_offset(HART0_S_CONTEXT)), UART0_SOURCE);
    }
}
