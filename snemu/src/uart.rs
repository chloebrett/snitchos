//! A minimal ns16550a UART: enough of the register set for console output and
//! input. Writes to the transmit register are captured into an output buffer the
//! host harness drains; host-injected input queues for the guest to read via the
//! receive buffer, with the line-status register signalling data-ready.

use std::collections::VecDeque;
use std::sync::Mutex;

/// ns16550a register offsets from the device base.
pub(crate) mod reg {
    /// Transmit holding register (write) / receive buffer (read) — same offset.
    pub const THR: usize = 0;
    /// Receive buffer register (read side of offset 0). A read pops one byte.
    pub const RBR: usize = 0;
    /// Line status register.
    pub const LSR: usize = 5;
}

/// Line-status-register bits.
pub(crate) mod lsr {
    /// Data ready — the receive buffer holds at least one byte.
    pub const DR: u8 = 0x01;
    /// Transmit holding register empty (ready to accept a byte).
    pub const THRE: u8 = 0x20;
    /// Transmitter empty.
    pub const TEMT: u8 = 0x40;
}

/// A minimal ns16550a UART. Transmitted bytes accumulate in `out` (the host
/// harness drains them); received bytes queue in `rx` (the host injects console
/// input there). The transmitter is modeled as always ready.
///
/// `rx` is a `Mutex` (not a `RefCell`) so a **read** of the RBR register — which
/// pops a byte, an MMIO side effect — can happen behind the bus's `&self` read
/// path *and* leave the `Machine` `Sync`. That matters because the boot-once audit
/// shares a booted snapshot across worker threads to `clone` (fork) it per
/// scenario; a `RefCell` would make `Machine` non-`Sync` and force group-atomic
/// parallelism. The lock is only taken on actual UART MMIO (the bus dispatches by
/// address first), so it's off the interpreter's hot path.
pub(crate) struct Uart {
    out: Vec<u8>,
    rx: Mutex<VecDeque<u8>>,
}

impl Clone for Uart {
    fn clone(&self) -> Self {
        Self {
            out: self.out.clone(),
            rx: Mutex::new(self.rx.lock().expect("uart rx").clone()),
        }
    }
}

impl Uart {
    pub(crate) fn new() -> Self {
        Self { out: Vec::new(), rx: Mutex::new(VecDeque::new()) }
    }

    pub(crate) fn read(&self, offset: usize) -> u8 {
        match offset {
            // RBR (== THR offset): pop one received byte, or 0 if the FIFO's dry.
            reg::RBR => self.rx.lock().expect("uart rx").pop_front().unwrap_or(0),
            reg::LSR => {
                let dr = if self.rx.lock().expect("uart rx").is_empty() { 0 } else { lsr::DR };
                lsr::THRE | lsr::TEMT | dr
            }
            _ => 0,
        }
    }

    pub(crate) fn write(&mut self, offset: usize, value: u8) {
        if offset == reg::THR {
            self.out.push(value);
        }
    }

    /// Queue host-supplied console input for the guest to read via RBR. The
    /// interactive audit harness calls this to inject a scenario's keystrokes.
    pub(crate) fn push_input(&mut self, bytes: &[u8]) {
        self.rx.get_mut().expect("uart rx").extend(bytes.iter().copied());
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

    #[test]
    fn received_bytes_set_data_ready_and_read_back_in_order() {
        // The RX path the kernel's `read_byte` polls: LSR bit 0 (DR) signals a
        // waiting byte, RBR (offset 0, read side) returns it and pops.
        let mut uart = Uart::new();
        assert_eq!(uart.read(reg::LSR) & lsr::DR, 0, "no data ready when idle");

        uart.push_input(b"Hi");
        assert_eq!(uart.read(reg::LSR) & lsr::DR, lsr::DR, "data ready after input");
        assert_eq!(uart.read(reg::RBR), b'H', "FIFO order: first byte first");
        assert_eq!(uart.read(reg::LSR) & lsr::DR, lsr::DR, "still ready with one left");
        assert_eq!(uart.read(reg::RBR), b'i');
        assert_eq!(uart.read(reg::LSR) & lsr::DR, 0, "DR clears once drained");
        assert_eq!(uart.read(reg::RBR), 0, "an empty RBR reads zero");
    }
}
