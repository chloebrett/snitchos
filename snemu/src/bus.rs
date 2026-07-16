//! The system bus: address-decodes guest physical accesses to RAM or a
//! memory-mapped device. Holds RAM and the devices; the CPU goes through it
//! for every fetch, load, and store.

use crate::fwcfg::{Fwcfg, RamfbCfg};
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

/// QEMU `virt` fw_cfg MMIO base (fixed at `0x1010_0000`) and its register page.
/// The four registers the kernel driver actually touches; anything else in the
/// page falls back to the old no-op stub behaviour (reads 0, writes dropped) —
/// un-modeled but still safe, never a bus fault.
const FWCFG_BASE: u64 = 0x1010_0000;
const FWCFG_SIZE: u64 = 0x1000;
const FWCFG_REG_DATA: u64 = FWCFG_BASE;
const FWCFG_REG_SELECTOR: u64 = FWCFG_BASE + 0x08;
const FWCFG_REG_DMA_ADDR_HIGH: u64 = FWCFG_BASE + 0x10;
const FWCFG_REG_DMA_ADDR_LOW: u64 = FWCFG_BASE + 0x14;

/// Whether `addr` falls in the fw_cfg register page.
fn in_fwcfg(addr: u64) -> bool {
    (FWCFG_BASE..FWCFG_BASE + FWCFG_SIZE).contains(&addr)
}

#[derive(Clone)]
pub(crate) struct Bus {
    ram: Memory,
    uart: Uart,
    virtio: Virtio,
    fwcfg: Fwcfg,
}

impl Bus {
    pub(crate) fn new(ram: Memory) -> Self {
        Self {
            ram,
            uart: Uart::new(),
            virtio: Virtio::new(),
            fwcfg: Fwcfg::new(),
        }
    }

    /// Make `etc/ramfb` exist in the fw_cfg directory — the snemu
    /// equivalent of passing `-device ramfb` to real QEMU. Off by default.
    pub(crate) fn fwcfg_enable_ramfb(&mut self) {
        self.fwcfg.enable_ramfb();
    }

    /// The captured `etc/ramfb` config, if a DMA write has completed.
    pub(crate) fn fwcfg_ramfb_cfg(&self) -> Option<RamfbCfg> {
        self.fwcfg.ramfb_cfg()
    }

    pub(crate) fn uart_output(&self) -> &[u8] {
        self.uart.output()
    }

    /// Queue host console input for the guest to read via the UART receive
    /// buffer — the interactive harness injecting a scenario's keystrokes.
    pub(crate) fn push_console_input(&mut self, bytes: &[u8]) {
        self.uart.push_input(bytes);
    }

    /// Overwrite guest RAM (used to patch a snapshot's DTB before a fork).
    pub(crate) fn write_ram(&mut self, addr: u64, bytes: &[u8]) -> Result<(), BusError> {
        self.ram.write_bytes(addr, bytes)
    }

    /// Bytes the virtio-console has transmitted (the telemetry frame stream).
    pub(crate) fn virtio_tx_output(&self) -> &[u8] {
        self.virtio.tx_output()
    }

    /// Fold the bus's guest-visible state into `h` for the machine state hash: guest
    /// RAM, plus the UART and virtio output streams (the emitted telemetry/console —
    /// device progress that a determinism divergence would show up in).
    pub(crate) fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.ram.hash_state(h);
        self.uart.output().hash(h);
        self.virtio.tx_output().hash(h);
    }

    /// RAM, for the page-table walker (PTEs always live in physical memory).
    pub(crate) fn ram(&self) -> &Memory {
        &self.ram
    }

    pub(crate) fn read_u8(&self, addr: u64) -> Result<u8, BusError> {
        if addr == FWCFG_REG_DATA {
            return Ok(self.fwcfg.read_data_byte());
        }
        if in_fwcfg(addr) {
            return Ok(0); // un-modeled offset in the page — old stub behaviour
        }
        match uart_offset(addr) {
            Some(off) => Ok(self.uart.read(off)),
            None => self.ram.read_u8(addr),
        }
    }

    pub(crate) fn read_u16(&self, addr: u64) -> Result<u16, BusError> {
        if in_fwcfg(addr) {
            return Ok(0);
        }
        match uart_offset(addr) {
            Some(off) => Ok(u16::from(self.uart.read(off))),
            None => self.ram.read_u16(addr),
        }
    }

    pub(crate) fn read_u32(&self, addr: u64) -> Result<u32, BusError> {
        if in_fwcfg(addr) {
            return Ok(0);
        }
        if Virtio::in_window(addr) {
            return Ok(self.virtio.read(addr));
        }
        match uart_offset(addr) {
            Some(off) => Ok(u32::from(self.uart.read(off))),
            None => self.ram.read_u32(addr),
        }
    }

    pub(crate) fn read_u64(&self, addr: u64) -> Result<u64, BusError> {
        if in_fwcfg(addr) {
            return Ok(0);
        }
        match uart_offset(addr) {
            Some(off) => Ok(u64::from(self.uart.read(off))),
            None => self.ram.read_u64(addr),
        }
    }

    pub(crate) fn write_u8(&mut self, addr: u64, value: u8) -> Result<(), BusError> {
        if in_fwcfg(addr) {
            return Ok(());
        }
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value);
                Ok(())
            }
            None => self.ram.write_u8(addr, value),
        }
    }

    pub(crate) fn write_u16(&mut self, addr: u64, value: u16) -> Result<(), BusError> {
        if addr == FWCFG_REG_SELECTOR {
            // fw_cfg registers are big-endian on the wire regardless of guest
            // endianness; `value` is the raw wire value the guest's store
            // produced, so recover the logical key with `from_be` before
            // handing it to the device (which works in logical values only).
            self.fwcfg.write_selector(u16::from_be(value));
            return Ok(());
        }
        if in_fwcfg(addr) {
            return Ok(()); // un-modeled offset in the page — old stub behaviour
        }
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value as u8);
                Ok(())
            }
            None => self.ram.write_u16(addr, value),
        }
    }

    pub(crate) fn write_u32(&mut self, addr: u64, value: u32) -> Result<(), BusError> {
        if addr == FWCFG_REG_DMA_ADDR_HIGH {
            self.fwcfg.write_dma_addr_high(u32::from_be(value));
            return Ok(());
        }
        if addr == FWCFG_REG_DMA_ADDR_LOW {
            // The low-half write is the DMA trigger (matches the real device
            // and the kernel driver's write order: high then low). Assemble
            // the descriptor address, then do the RAM-touching work — same
            // "bus detects trigger register, device does the transfer"
            // split as `Virtio::service_tx` below.
            let desc_pa = self.fwcfg.write_dma_addr_low(u32::from_be(value));
            self.fwcfg.complete_dma(&mut self.ram, desc_pa);
            return Ok(());
        }
        if in_fwcfg(addr) {
            return Ok(()); // un-modeled offset in the page — old stub behaviour
        }
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
        if in_fwcfg(addr) {
            return Ok(());
        }
        match uart_offset(addr) {
            Some(off) => {
                self.uart.write(off, value as u8);
                Ok(())
            }
            None => self.ram.write_u64(addr, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Bus, FWCFG_BASE};
    use crate::mem::Memory;

    /// Supersedes the old no-op stub's `fwcfg_reads_zero_and_swallows_writes`:
    /// selecting the file directory through the bus (the exact register
    /// sequence the kernel driver performs — `value.to_be()` then a plain
    /// store) now reads back the **real** one-entry directory instead of an
    /// empty one. Still proves the load-bearing safety property the old test
    /// pinned — these accesses don't bus-fault (every `.unwrap()` below would
    /// panic on a `BusError` if they did) — just against real content now.
    #[test]
    fn fwcfg_selector_and_data_registers_serve_the_real_directory() {
        let mut bus = Bus::new(Memory::new(0x1000));
        bus.fwcfg_enable_ramfb();
        // FW_CFG_FILE_DIR = 0x19; the driver writes `.to_be()` of the logical
        // value, so the raw wire value here is 0x0019u16.to_be().
        bus.write_u16(FWCFG_BASE + 0x08, 0x0019u16.to_be()).unwrap();
        let count_bytes: [u8; 4] = std::array::from_fn(|_| bus.read_u8(FWCFG_BASE).unwrap());
        assert_eq!(u32::from_be_bytes(count_bytes), 1, "one real file, not an empty directory");
    }

    /// Without `fwcfg_enable_ramfb`, the directory is empty — matches a real
    /// machine booted without `-device ramfb`. This is the property that
    /// keeps `framebuffer-absent-degrades-gracefully` passing under
    /// `snemu-itest`: most scenarios never call `fwcfg_enable_ramfb`, so
    /// they must see no file, not the one `framebuffer-presents` opts into.
    #[test]
    fn fwcfg_directory_is_empty_without_enabling_ramfb() {
        let mut bus = Bus::new(Memory::new(0x1000));
        bus.write_u16(FWCFG_BASE + 0x08, 0x0019u16.to_be()).unwrap();
        let count_bytes: [u8; 4] = std::array::from_fn(|_| bus.read_u8(FWCFG_BASE).unwrap());
        assert_eq!(u32::from_be_bytes(count_bytes), 0, "no file until opted in");
    }

    /// The stub is scoped to its page — RAM immediately below and above still works.
    #[test]
    fn addresses_outside_the_fwcfg_page_are_untouched() {
        let mut bus = Bus::new(Memory::new(64 * 1024 * 1024));
        bus.write_u32(crate::mem::RAM_BASE, 0x1234_5678).unwrap();
        assert_eq!(bus.read_u32(crate::mem::RAM_BASE).unwrap(), 0x1234_5678, "RAM still routes");
    }

    /// End-to-end DMA select+write through the bus: the driver's actual
    /// register sequence (stage the descriptor + payload in RAM, write the
    /// DMA address register's high half then low half — the low-half write
    /// is the trigger) results in the captured `RamfbCfg` being observable
    /// via the bus, and the descriptor's `control` field cleared to `0`.
    #[test]
    fn dma_write_through_the_bus_captures_the_ramfb_config() {
        let desc_pa = crate::mem::RAM_BASE + 0x1000;
        let payload_pa = crate::mem::RAM_BASE + 0x2000;
        let mut bus = Bus::new(Memory::new(0x10000));

        // Stage the descriptor: control = (0x42<<16)|SELECT(0x08)|WRITE(0x10),
        // length=28, address=payload_pa — all big-endian, matching
        // `kernel/src/device/fwcfg.rs::write_file`'s staging exactly.
        let control: u32 = (0x42 << 16) | 0x08 | 0x10;
        bus.write_ram(desc_pa, &control.to_be_bytes()).unwrap();
        bus.write_ram(desc_pa + 4, &28u32.to_be_bytes()).unwrap();
        bus.write_ram(desc_pa + 8, &payload_pa.to_be_bytes()).unwrap();
        // Stage the 28-byte RAMFBCfg payload.
        let mut cfg = [0u8; 28];
        cfg[0..8].copy_from_slice(&0x8000_3000u64.to_be_bytes()); // addr
        cfg[16..20].copy_from_slice(&1024u32.to_be_bytes()); // width
        cfg[20..24].copy_from_slice(&768u32.to_be_bytes()); // height
        bus.write_ram(payload_pa, &cfg).unwrap();

        // The driver's actual write order: DMA address high, then low
        // (the trigger), both `.to_be()`'d.
        bus.write_u32(FWCFG_BASE + 0x10, ((desc_pa >> 32) as u32).to_be()).unwrap();
        bus.write_u32(FWCFG_BASE + 0x14, (desc_pa as u32).to_be()).unwrap();

        let cfg = bus.fwcfg_ramfb_cfg().expect("DMA write should have captured a config");
        assert_eq!(cfg.addr, 0x8000_3000);
        assert_eq!(cfg.width, 1024);
        assert_eq!(cfg.height, 768);

        let control_after = u32::from_be_bytes(std::array::from_fn(|i| {
            bus.read_u8(desc_pa + i as u64).unwrap()
        }));
        assert_eq!(control_after, 0, "control cleared to 0 on success");
    }
}
