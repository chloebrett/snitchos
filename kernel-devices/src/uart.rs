//! NS16550A / DesignWare 8250 UART register map — the *layout* logic, no MMIO.
//!
//! Which byte offset a logical register sits at depends on the DTB's `reg-shift`
//! (register spacing): QEMU's `ns16550a` is byte-spaced (`reg-shift = 0`), the
//! JH7110's `snps,dw-apb-uart` is 4-byte-spaced (`reg-shift = 2`). The register
//! *numbers* are identical across both; only the stride differs. The kernel driver
//! (`kernel/src/uart.rs`) does the volatile MMIO at these offsets — and picks the
//! access *width* from `reg-io-width` (1 vs 4 bytes), which is a separate concern.

/// Transmit Holding / Receive Buffer register (logical register 0): write it to
/// send a byte, read it (as RBR) to receive one.
pub const THR_RBR: u8 = 0;
/// Line Status Register (logical register 5).
pub const LSR: u8 = 5;
/// LSR bit 5 — Transmit Holding Register Empty: set when it's safe to write the
/// next byte to `THR`.
pub const LSR_THRE: u8 = 0b0010_0000;
/// LSR bit 0 — Data Ready: set when a byte is waiting in the RX FIFO (`RBR`).
pub const LSR_DR: u8 = 0b0000_0001;

/// Byte offset of a logical `reg` given the DTB `reg-shift`. Registers are spaced
/// `1 << reg_shift` bytes apart, so the offset is `reg << reg_shift`.
#[must_use]
pub fn reg_offset(reg: u8, reg_shift: u8) -> usize {
    (reg as usize) << reg_shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_spaced_layout_is_just_the_register_number() {
        // QEMU `ns16550a`: `reg-shift = 0`, so LSR sits at `base + 5`.
        assert_eq!(reg_offset(THR_RBR, 0), 0);
        assert_eq!(reg_offset(LSR, 0), 5);
    }

    #[test]
    fn shift_two_spaces_registers_four_bytes_apart() {
        // JH7110 `snps,dw-apb-uart`: `reg-shift = 2`, so LSR sits at `base + 20`.
        assert_eq!(reg_offset(THR_RBR, 2), 0);
        assert_eq!(reg_offset(LSR, 2), 20);
    }

    #[test]
    fn lsr_status_bits_are_the_8250_positions() {
        assert_eq!(LSR_THRE, 0b0010_0000, "THRE is LSR bit 5");
        assert_eq!(LSR_DR, 0b0000_0001, "Data Ready is LSR bit 0");
    }
}
