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

#[cfg(test)]
mod tests {
    use super::*;

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
}
