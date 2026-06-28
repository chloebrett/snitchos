//! A minimal ns16550a UART: enough of the register set for console output.
//! Writes to the transmit register are captured into an output buffer that the
//! host harness drains; the line-status register always reports "ready".

/// ns16550a register offsets from the device base.
pub(crate) mod reg {
    /// Transmit holding register (write) / receive buffer (read).
    pub const THR: usize = 0;
    /// Line status register.
    pub const LSR: usize = 5;
}

/// Line-status-register bits.
pub(crate) mod lsr {
    /// Transmit holding register empty (ready to accept a byte).
    pub const THRE: u8 = 0x20;
    /// Transmitter empty.
    pub const TEMT: u8 = 0x40;
}

/// A minimal ns16550a UART. Transmitted bytes accumulate in `out`; the host
/// harness drains them. The transmitter is modeled as always ready.
pub(crate) struct Uart {
    out: Vec<u8>,
}

impl Uart {
    pub(crate) fn new() -> Self {
        Self { out: Vec::new() }
    }

    #[allow(
        clippy::unused_self,
        reason = "register reads will consult device state once RX is modeled"
    )]
    pub(crate) fn read(&self, offset: usize) -> u8 {
        match offset {
            reg::LSR => lsr::THRE | lsr::TEMT,
            _ => 0,
        }
    }

    pub(crate) fn write(&mut self, offset: usize, value: u8) {
        if offset == reg::THR {
            self.out.push(value);
        }
    }

    pub(crate) fn output(&self) -> &[u8] {
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thr_writes_append_to_output() {
        let mut uart = Uart::new();
        uart.write(reg::THR, b'H');
        uart.write(reg::THR, b'i');
        assert_eq!(uart.output(), b"Hi");
    }

    #[test]
    fn lsr_reports_transmitter_ready() {
        let uart = Uart::new();
        assert_eq!(uart.read(reg::LSR) & lsr::THRE, lsr::THRE);
    }

    #[test]
    fn other_registers_read_zero_and_ignore_writes() {
        let mut uart = Uart::new();
        uart.write(1, 0xff); // IER: ignored
        assert_eq!(uart.read(1), 0);
        assert!(uart.output().is_empty());
    }
}
