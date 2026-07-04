//! Guest physical memory: a flat byte array at the QEMU `virt` RAM base,
//! with width-typed little-endian accessors and out-of-range detection.

/// Physical base of guest RAM on the QEMU `virt` machine.
pub const RAM_BASE: u64 = 0x8000_0000;

/// A guest physical access fell outside mapped RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    OutOfRange { addr: u64 },
}

/// Flat guest RAM, addressed by absolute guest physical address.
#[derive(Clone)]
pub struct Memory {
    ram: Vec<u8>,
}

/// Generates a little-endian read/write pair backed by [`Memory::span`].
macro_rules! accessors {
    ($read:ident, $write:ident, $ty:ty, $len:literal) => {
        pub fn $read(&self, addr: u64) -> Result<$ty, BusError> {
            let span = self.span(addr, $len)?;
            // span() guarantees exactly $len bytes, so the conversion is total.
            let bytes = <[u8; $len]>::try_from(&self.ram[span]).unwrap();
            Ok(<$ty>::from_le_bytes(bytes))
        }

        pub fn $write(&mut self, addr: u64, value: $ty) -> Result<(), BusError> {
            let span = self.span(addr, $len)?;
            self.ram[span].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    };
}

impl Memory {
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self { ram: vec![0; size] }
    }

    /// Copy `bytes` into RAM starting at `addr` (used by the ELF loader).
    pub(crate) fn write_bytes(&mut self, addr: u64, bytes: &[u8]) -> Result<(), BusError> {
        let span = self.span(addr, bytes.len())?;
        self.ram[span].copy_from_slice(bytes);
        Ok(())
    }

    accessors!(read_u8, write_u8, u8, 1);
    accessors!(read_u16, write_u16, u16, 2);
    accessors!(read_u32, write_u32, u32, 4);
    accessors!(read_u64, write_u64, u64, 8);

    /// The byte range for a `len`-wide access at `addr`, or `OutOfRange` if it
    /// would fall outside mapped RAM.
    fn span(&self, addr: u64, len: usize) -> Result<std::ops::Range<usize>, BusError> {
        let start = usize::try_from(addr.wrapping_sub(RAM_BASE))
            .ok()
            .filter(|_| addr >= RAM_BASE)
            .ok_or(BusError::OutOfRange { addr })?;
        let end = start.checked_add(len).ok_or(BusError::OutOfRange { addr })?;
        if end > self.ram.len() {
            return Err(BusError::OutOfRange { addr });
        }
        Ok(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_then_reads_a_byte_at_the_ram_base() {
        let mut mem = Memory::new(0x1000);
        mem.write_u8(RAM_BASE, 0xab).unwrap();
        assert_eq!(mem.read_u8(RAM_BASE).unwrap(), 0xab);
    }

    #[test]
    fn multi_width_round_trips() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x1234).unwrap();
        assert_eq!(mem.read_u16(RAM_BASE).unwrap(), 0x1234);
        mem.write_u32(RAM_BASE, 0xdead_beef).unwrap();
        assert_eq!(mem.read_u32(RAM_BASE).unwrap(), 0xdead_beef);
        mem.write_u64(RAM_BASE, 0x0123_4567_89ab_cdef).unwrap();
        assert_eq!(mem.read_u64(RAM_BASE).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn out_of_range_access_is_an_error() {
        let mut mem = Memory::new(0x1000);
        let below = RAM_BASE - 1;
        let past_end = RAM_BASE + 0x1000;
        assert_eq!(mem.read_u8(below), Err(BusError::OutOfRange { addr: below }));
        assert_eq!(
            mem.write_u8(past_end, 0),
            Err(BusError::OutOfRange { addr: past_end })
        );
        // A width that starts in range but straddles the end is rejected whole.
        let straddle = RAM_BASE + 0x1000 - 2;
        assert_eq!(
            mem.read_u32(straddle),
            Err(BusError::OutOfRange { addr: straddle })
        );
    }

    #[test]
    fn stores_little_endian() {
        let mut mem = Memory::new(0x1000);
        mem.write_u32(RAM_BASE, 0xdead_beef).unwrap();
        assert_eq!(mem.read_u8(RAM_BASE).unwrap(), 0xef);
        assert_eq!(mem.read_u8(RAM_BASE + 1).unwrap(), 0xbe);
        assert_eq!(mem.read_u8(RAM_BASE + 2).unwrap(), 0xad);
        assert_eq!(mem.read_u8(RAM_BASE + 3).unwrap(), 0xde);
    }
}
