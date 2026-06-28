//! A minimal ELF64 loader: parse the program headers, copy each `PT_LOAD`
//! segment to its physical address, and hand back a `Cpu` ready to run from the
//! entry point (a0 = hartid 0, a1 = null DTB — both the register defaults).

use crate::cpu::Cpu;
use crate::mem::Memory;

const ELF64_HEADER_SIZE: usize = 64;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_RISCV: u16 = 243;
const PT_LOAD: u32 = 1;

/// ELF64 header field offsets.
mod off {
    pub const E_MACHINE: usize = 0x12;
    pub const E_ENTRY: usize = 0x18;
    pub const E_PHOFF: usize = 0x20;
    pub const E_PHENTSIZE: usize = 0x36;
    pub const E_PHNUM: usize = 0x38;
}

/// ELF64 program-header field offsets (relative to the header's start).
mod ph {
    pub const P_TYPE: usize = 0x00;
    pub const P_OFFSET: usize = 0x08;
    pub const P_PADDR: usize = 0x18;
    pub const P_FILESZ: usize = 0x20;
}

/// Why an image could not be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Not an ELF file (bad magic).
    BadMagic,
    /// Not the ELF64 / little-endian / RISC-V flavor snemu loads.
    Unsupported,
    /// The image is shorter than a field or segment it references.
    Truncated,
    /// A segment's physical address falls outside RAM.
    SegmentOutOfRange,
}

/// Reads a little-endian integer from `image` at `off`, or `Truncated`.
macro_rules! le_reader {
    ($name:ident, $ty:ty, $n:literal) => {
        fn $name(image: &[u8], off: usize) -> Result<$ty, ElfError> {
            let bytes = image.get(off..off + $n).ok_or(ElfError::Truncated)?;
            // Length guaranteed by the slice range above.
            Ok(<$ty>::from_le_bytes(bytes.try_into().unwrap()))
        }
    };
}
le_reader!(u16_at, u16, 2);
le_reader!(u32_at, u32, 4);
le_reader!(u64_at, u64, 8);

fn validate_header(image: &[u8]) -> Result<(), ElfError> {
    if !image.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return Err(ElfError::BadMagic);
    }
    if image.len() < ELF64_HEADER_SIZE {
        return Err(ElfError::Truncated);
    }
    if image[4] != ELFCLASS64 || image[5] != ELFDATA2LSB {
        return Err(ElfError::Unsupported);
    }
    if u16_at(image, off::E_MACHINE)? != EM_RISCV {
        return Err(ElfError::Unsupported);
    }
    Ok(())
}

/// Load an ELF64 RISC-V `image` into a fresh `ram_size`-byte machine and return
/// a `Cpu` positioned at the entry point.
pub fn load(image: &[u8], ram_size: usize) -> Result<Cpu, ElfError> {
    validate_header(image)?;
    let entry = u64_at(image, off::E_ENTRY)?;
    let phoff = u64_at(image, off::E_PHOFF)?;
    let phentsize = u64::from(u16_at(image, off::E_PHENTSIZE)?);
    let phnum = u64::from(u16_at(image, off::E_PHNUM)?);

    let mut mem = Memory::new(ram_size);
    for i in 0..phnum {
        let base = (phoff + i * phentsize) as usize;
        if u32_at(image, base + ph::P_TYPE)? != PT_LOAD {
            continue;
        }
        let offset = u64_at(image, base + ph::P_OFFSET)? as usize;
        let paddr = u64_at(image, base + ph::P_PADDR)?;
        let filesz = u64_at(image, base + ph::P_FILESZ)? as usize;
        let end = offset.checked_add(filesz).ok_or(ElfError::Truncated)?;
        let bytes = image.get(offset..end).ok_or(ElfError::Truncated)?;
        mem.write_bytes(paddr, bytes)
            .map_err(|_| ElfError::SegmentOutOfRange)?;
    }

    let mut cpu = Cpu::new(mem);
    cpu.set_pc(entry);
    Ok(cpu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::RAM_BASE;

    /// Build a minimal valid ELF64 with a single `PT_LOAD` segment.
    fn tiny_elf(entry: u64, paddr: u64, segment: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; 64];
        img[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        img[4] = 2; // ELFCLASS64
        img[5] = 1; // ELFDATA2LSB
        img[6] = 1; // EV_CURRENT
        img[0x12..0x14].copy_from_slice(&243u16.to_le_bytes()); // EM_RISCV
        img[0x18..0x20].copy_from_slice(&entry.to_le_bytes()); // e_entry
        img[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        img[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        img[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let mut ph = vec![0u8; 56];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        ph[0x08..0x10].copy_from_slice(&120u64.to_le_bytes()); // p_offset (64 + 56)
        ph[0x18..0x20].copy_from_slice(&paddr.to_le_bytes()); // p_paddr
        ph[0x20..0x28].copy_from_slice(&(segment.len() as u64).to_le_bytes()); // p_filesz
        ph[0x28..0x30].copy_from_slice(&(segment.len() as u64).to_le_bytes()); // p_memsz
        img.extend_from_slice(&ph);
        img.extend_from_slice(segment);
        img
    }

    #[test]
    fn loads_segment_and_runs_from_entry() {
        let entry = RAM_BASE + 0x100;
        let segment = 0x02a0_0093_u32.to_le_bytes(); // addi x1, x0, 42
        let img = tiny_elf(entry, entry, &segment);

        let mut cpu = load(&img, 0x1000).unwrap();
        assert_eq!(cpu.pc(), entry);

        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 42);
    }

    #[test]
    fn rejects_a_non_elf_image() {
        assert!(matches!(load(&[0, 1, 2, 3], 0x1000), Err(ElfError::BadMagic)));
    }

    #[test]
    fn rejects_a_non_riscv_elf() {
        let mut img = tiny_elf(RAM_BASE, RAM_BASE, &[0; 4]);
        img[0x12..0x14].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        assert!(matches!(load(&img, 0x1000), Err(ElfError::Unsupported)));
    }
}
