//! Driver for the NS16550A UART and register-compatible clones — QEMU `virt`'s
//! byte-spaced ns16550a and the JH7110's `snps,dw-apb-uart` (DesignWare 8250),
//! which spaces its registers 4 bytes apart (`reg-shift = 2`) and takes 32-bit
//! accesses (`reg-io-width = 4`). The register *map* (offsets, status bits) lives
//! in `kernel_devices::uart` and is host-tested; this file does the MMIO.

use core::fmt::Write;

use kernel_devices::uart::{reg_offset, LSR, LSR_DR, LSR_THRE, THR_RBR};

/// Driver for an 8250-family UART at a given MMIO base, with the register layout
/// (`reg_shift`) and access width (`io_width`, 1 or 4 bytes) the DTB reports.
///
/// Storing the base as `usize` rather than `*mut u8` sidesteps `*mut`'s `!Sync`
/// default — the struct is naturally `Send + Sync` because it's just integers
/// wearing a type.
///
/// Known weaknesses:
/// - **Polled output only.** `putchar` spins on LSR bit 5. No interrupts, no
///   FIFO use, no flow control. Fine for v0.1; the moment we have something
///   meaningful to log we'll want interrupt-driven TX.
/// - **No initialization step.** Real drivers configure baud rate (DLL/DLM
///   divisor latches), line control (LCR: bits, parity, stop), and the FIFO
///   (FCR). We rely on whatever `OpenSBI` configured during M-mode init — which
///   holds on QEMU `virt` and on the VisionFive 2.
/// - **No error checking.** LSR error bits (overrun, parity, framing) are
///   ignored. A real driver surfaces them.
/// - **Multiple `Uart16550`s pointing at the same MMIO address don't
///   coordinate.** The `&self` API is correct because the struct has no
///   state to race over, but the *device* does, and `&self` doesn't help
///   there. Serialization is provided externally via `kernel::sync::Mutex<Uart16550>`.
pub struct Uart16550 {
    base: usize,
    /// Register spacing exponent: register `r` sits at `base + (r << reg_shift)`.
    reg_shift: u8,
    /// Access width in bytes (1 or 4). DesignWare (`reg-io-width = 4`) requires
    /// 32-bit accesses; the meaningful byte is the low 8 bits.
    io_width: u8,
}

impl Uart16550 {
    /// Construct a driver with the register layout the DTB reports — `reg_shift`
    /// (spacing) and `io_width` (1 or 4 bytes). The JH7110 `snps,dw-apb-uart` is
    /// `with_layout(base, 2, 4)`; QEMU's byte-spaced ns16550a is
    /// `with_layout(base, 0, 1)`.
    ///
    /// # Safety
    ///
    /// `base` must be the MMIO base of a real 8250-compatible UART, and `reg_shift`
    /// / `io_width` must match the hardware (or the driver pokes the wrong offsets /
    /// widths). The caller must ensure any other code touching the same registers
    /// either coordinates through this driver (a shared `Mutex`) or doesn't
    /// conflict — two uncoordinated `Uart16550`s on one region is UB at the
    /// device-state level (the type system can't see it).
    pub const unsafe fn with_layout(base: usize, reg_shift: u8, io_width: u8) -> Self {
        Uart16550 { base, reg_shift, io_width }
    }

    /// MMIO address of a logical register.
    fn addr(&self, reg: u8) -> usize {
        self.base + reg_offset(reg, self.reg_shift)
    }

    /// Read a logical register, honoring the access width.
    ///
    /// # Safety
    ///
    /// MMIO read of a register this driver owns; see the type contract.
    unsafe fn read_reg(&self, reg: u8) -> u8 {
        let addr = self.addr(reg);
        // SAFETY: `addr` is within this UART's MMIO block; width matches the DTB.
        unsafe {
            if self.io_width == 4 {
                (addr as *const u32).read_volatile() as u8
            } else {
                (addr as *const u8).read_volatile()
            }
        }
    }

    /// Write a logical register, honoring the access width.
    ///
    /// # Safety
    ///
    /// MMIO write to a register this driver owns; see the type contract.
    unsafe fn write_reg(&self, reg: u8, val: u8) {
        let addr = self.addr(reg);
        // SAFETY: `addr` is within this UART's MMIO block; width matches the DTB.
        unsafe {
            if self.io_width == 4 {
                (addr as *mut u32).write_volatile(u32::from(val));
            } else {
                (addr as *mut u8).write_volatile(val);
            }
        }
    }

    /// Block until the transmit holding register is empty, then send one byte.
    ///
    /// Spins on LSR `THRE` (Transmit Holding Register Empty). At 115200 baud each
    /// byte takes ~87 microseconds on the wire; the CPU spins millions of times
    /// faster, so this loop dominates transmit time.
    pub fn putchar(&self, c: u8) {
        // SAFETY: LSR/THR MMIO on a UART this driver owns; see the type contract.
        unsafe {
            while self.read_reg(LSR) & LSR_THRE == 0 {}
            self.write_reg(THR_RBR, c);
        }
    }

    /// Read one byte if the receiver has data waiting, else `None`.
    ///
    /// Polled RX — the mirror of [`putchar`](Self::putchar). Checks LSR `DR` (Data
    /// Ready); if set, reads the waiting byte from `RBR` (the read side of the same
    /// logical register `THR` writes). Non-blocking: returns `None` when nothing is
    /// buffered, so the timer-driven drain never spins on the hardware.
    pub fn read_byte(&self) -> Option<u8> {
        // SAFETY: LSR/RBR MMIO on a UART this driver owns; see the type contract.
        unsafe {
            if self.read_reg(LSR) & LSR_DR != 0 {
                return Some(self.read_reg(THR_RBR));
            }
        }
        None
    }
}

/// `core::fmt::Write` impl so the UART can back the `print!`/`println!` macros.
/// `write_str` needs `&mut self` per trait contract; we delegate to `&self`
/// `putchar` because the struct itself has no state to mutate.
impl Write for Uart16550 {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            // Real serial terminals need CR+LF: a bare `\n` steps down without
            // returning to column 0 (harmless on QEMU, a staircase on the board).
            if byte == b'\n' {
                self.putchar(b'\r');
            }
            self.putchar(byte);
        }
        Ok(())
    }
}
