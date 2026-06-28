//! The system bus: address-decodes guest physical accesses to RAM or a
//! memory-mapped device. Holds RAM and the devices; the CPU goes through it
//! for every fetch, load, and store.

use crate::mem::{BusError, Memory};
use crate::uart::Uart;
use crate::virtio::Virtio;

/// ns16550a UART base on the QEMU `virt` machine, and its register window.
const UART_BASE: u64 = 0x1000_0000;
const UART_SIZE: u64 = 0x100;

/// Offset into the UART register window, or `None` if `addr` isn't the UART.
fn uart_offset(addr: u64) -> Option<usize> {
    (UART_BASE..UART_BASE + UART_SIZE)
        .contains(&addr)
        .then(|| (addr - UART_BASE) as usize)
}

pub(crate) struct Bus {
    ram: Memory,
    uart: Uart,
    virtio: Virtio,
}

impl Bus {
    pub(crate) fn new(ram: Memory) -> Self {
        Self {
            ram,
            uart: Uart::new(),
            virtio: Virtio::new(),
        }
    }

    pub(crate) fn uart_output(&self) -> &[u8] {
        self.uart.output()
    }

    /// Bytes the virtio-console has transmitted (the telemetry frame stream).
    pub(crate) fn virtio_tx_output(&self) -> &[u8] {
        self.virtio.tx_output()
    }

    /// RAM, for the page-table walker (PTEs always live in physical memory).
    pub(crate) fn ram(&self) -> &Memory {
        &self.ram
    }

    pub(crate) fn read_u8(&self, addr: u64) -> Result<u8, BusError> {
        match uart_offset(addr) {
            Some(off) => Ok(self.uart.read(off)),
            None => self.ram.read_u8(addr),
        }
    }

    pub(crate) fn read_u16(&self, addr: u64) -> Result<u16, BusError> {
        match uart_offset(addr) {
            Some(off) => Ok(u16::from(self.uart.read(off))),
            None => self.ram.read_u16(addr),
        }
    }

    pub(crate) fn read_u32(&self, addr: u64) -> Result<u32, BusError> {
        if Virtio::in_window(addr) {
            return Ok(self.virtio.read(addr));
        }
        match uart_offset(addr) {
            Some(off) => Ok(u32::from(self.uart.read(off))),
            None => self.ram.read_u32(addr),
        }
    }

    pub(crate) fn read_u64(&self, addr: u64) -> Result<u64, BusError> {
        match uart_offset(addr) {
            Some(off) => Ok(u64::from(self.uart.read(off))),
            None => self.ram.read_u64(addr),
        }
    }

    pub(crate) fn write_u8(&mut self, addr: u64, value: u8) -> Result<(), BusError> {
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value);
                Ok(())
            }
            None => self.ram.write_u8(addr, value),
        }
    }

    pub(crate) fn write_u16(&mut self, addr: u64, value: u16) -> Result<(), BusError> {
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value as u8);
                Ok(())
            }
            None => self.ram.write_u16(addr, value),
        }
    }

    pub(crate) fn write_u32(&mut self, addr: u64, value: u32) -> Result<(), BusError> {
        if Virtio::in_window(addr) {
            self.virtio.write(addr, value);
            if Virtio::is_notify(addr) {
                // A queue notify hands the device control: drain the TX ring
                // from guest RAM and publish the used ring back into it.
                self.virtio.service_tx(&mut self.ram);
            }
            return Ok(());
        }
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value as u8);
                Ok(())
            }
            None => self.ram.write_u32(addr, value),
        }
    }

    pub(crate) fn write_u64(&mut self, addr: u64, value: u64) -> Result<(), BusError> {
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value as u8);
                Ok(())
            }
            None => self.ram.write_u64(addr, value),
        }
    }
}
