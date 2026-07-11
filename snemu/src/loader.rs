//! A minimal ELF64 loader: parse the program headers, copy each `PT_LOAD`
//! segment to its physical address, and hand back a `Cpu` ready to run from the
//! entry point (a0 = hartid 0, a1 = null DTB — both the register defaults).

use crate::cpu::Cpu;
use crate::machine::Machine;
use crate::mem::{Memory, RAM_BASE};

/// Default offset (from `RAM_BASE`) where snemu places the device tree blob —
/// high, clear of the kernel image + heap. Clamped down to fit a smaller machine
/// (see [`load_memory`]); the kernel reads the final address from `a1` and both
/// sizes its frame pool from the DTB and reserves the DTB region, so it must land
/// inside the RAM the DTB itself declares.
const DTB_OFFSET: u64 = 0x0700_0000;

/// The DTB placement address for the **default** (largest) machine — where the
/// blob lands when RAM is big enough for the full [`DTB_OFFSET`]. The
/// snapshot/fork harness overwrites the DTB here to re-patch bootargs; it only
/// runs the default-size machine, so the clamp in [`load_memory`] is a no-op for
/// it and this address is exact.
pub const DTB_ADDR: u64 = RAM_BASE + DTB_OFFSET;

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
    pub const P_VADDR: usize = 0x10;
    pub const P_PADDR: usize = 0x18;
    pub const P_FILESZ: usize = 0x20;
    pub const P_MEMSZ: usize = 0x28;
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
/// a single-hart `Cpu` positioned at the entry point. If a `dtb` is given, it's
/// placed in RAM and its address handed to the kernel in `a1` (as firmware would).
pub fn load(image: &[u8], ram_size: usize, dtb: Option<&[u8]>) -> Result<Cpu, ElfError> {
    let (mem, entry_pc, dtb_addr) = load_memory(image, ram_size, dtb)?;
    let mut cpu = Cpu::new(mem);
    cpu.set_pc(entry_pc);
    if let Some(addr) = dtb_addr {
        cpu.set_reg(11, addr); // a1 = DTB address
    }
    Ok(cpu)
}

/// Like [`load`], but into a multi-hart [`Machine`] with `hart_count` harts. Hart
/// 0 boots at the entry with `a1` = DTB; the secondaries start parked until the
/// kernel's `hart_start`.
pub fn load_machine(
    image: &[u8],
    ram_size: usize,
    dtb: Option<&[u8]>,
    hart_count: usize,
) -> Result<Machine, ElfError> {
    let (mem, entry_pc, dtb_addr) = load_memory(image, ram_size, dtb)?;
    let mut machine = Machine::new(mem, hart_count);
    machine.set_pc(0, entry_pc);
    if let Some(addr) = dtb_addr {
        machine.set_reg(0, 11, addr); // a1 = DTB address
    }
    // Resolve the entry PCs of `memset`/`memcpy` from the ELF symbol table so the
    // native-op helper can intercept them (no-op if enabling is left off, or if the
    // binary is stripped).
    machine.set_native_op_pcs(
        crate::symbols::function_addr(image, "memset"),
        crate::symbols::function_addr(image, "memcpy"),
    );
    Ok(machine)
}

/// Parse the ELF and lay out RAM: copy each `PT_LOAD` segment, place the DTB (if
/// any), and return `(memory, entry_pc, dtb_addr)`. The entry is translated
/// through its segment's vaddr->paddr mapping (higher-half kernels boot at the
/// physical entry), falling back to the raw entry if no segment covers it.
fn load_memory(
    image: &[u8],
    ram_size: usize,
    dtb: Option<&[u8]>,
) -> Result<(Memory, u64, Option<u64>), ElfError> {
    validate_header(image)?;
    let entry = u64_at(image, off::E_ENTRY)?;
    let phoff = u64_at(image, off::E_PHOFF)?;
    let phentsize = u64::from(u16_at(image, off::E_PHENTSIZE)?);
    let phnum = u64::from(u16_at(image, off::E_PHNUM)?);

    let mut mem = Memory::new(ram_size);
    let mut entry_pa = None;
    for i in 0..phnum {
        let base = (phoff + i * phentsize) as usize;
        if u32_at(image, base + ph::P_TYPE)? != PT_LOAD {
            continue;
        }
        let offset = u64_at(image, base + ph::P_OFFSET)? as usize;
        let vaddr = u64_at(image, base + ph::P_VADDR)?;
        let paddr = u64_at(image, base + ph::P_PADDR)?;
        let filesz = u64_at(image, base + ph::P_FILESZ)? as usize;
        let memsz = u64_at(image, base + ph::P_MEMSZ)?;
        let end = offset.checked_add(filesz).ok_or(ElfError::Truncated)?;
        let bytes = image.get(offset..end).ok_or(ElfError::Truncated)?;
        mem.write_bytes(paddr, bytes)
            .map_err(|_| ElfError::SegmentOutOfRange)?;
        if (vaddr..vaddr.wrapping_add(memsz)).contains(&entry) {
            entry_pa = Some(entry - vaddr + paddr);
        }
    }

    let dtb_addr = match dtb {
        Some(dtb) => {
            // Place the DTB near the top of RAM but always *within* it: the fixed
            // high offset for the default machine, clamped down for a smaller one
            // so the kernel (which sizes its frame pool from the DTB and reserves
            // the DTB region) finds it in range.
            let margin = 0x1000u64; // a page of headroom above the blob
            let max_offset = (ram_size as u64).saturating_sub(dtb.len() as u64 + margin);
            let addr = RAM_BASE + DTB_OFFSET.min(max_offset);
            mem.write_bytes(addr, dtb)
                .map_err(|_| ElfError::SegmentOutOfRange)?;
            Some(addr)
        }
        None => None,
    };

    Ok((mem, entry_pa.unwrap_or(entry), dtb_addr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::RAM_BASE;

    /// Build a minimal valid ELF64 with a single `PT_LOAD` segment.
    fn tiny_elf(entry: u64, vaddr: u64, paddr: u64, segment: &[u8]) -> Vec<u8> {
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
        ph[0x10..0x18].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
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
        let img = tiny_elf(entry, entry, entry, &segment);

        let mut cpu = load(&img, 0x1000, None).unwrap();
        assert_eq!(cpu.pc(), entry);

        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 42);
    }

    #[test]
    fn higher_half_entry_is_translated_to_physical() {
        // Linked at a higher-half VA, loaded at a physical paddr.
        let vaddr = 0xffff_ffff_8000_0000 + 0x100;
        let paddr = RAM_BASE + 0x100;
        let segment = 0x02a0_0093_u32.to_le_bytes(); // addi x1, x0, 42
        let img = tiny_elf(vaddr, vaddr, paddr, &segment);

        let mut cpu = load(&img, 0x1000, None).unwrap();
        assert_eq!(cpu.pc(), paddr); // started at the physical entry
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 42);
    }

    #[test]
    fn rejects_a_non_elf_image() {
        assert!(matches!(load(&[0, 1, 2, 3], 0x1000, None), Err(ElfError::BadMagic)));
    }

    #[test]
    fn rejects_a_non_riscv_elf() {
        let mut img = tiny_elf(RAM_BASE, RAM_BASE, RAM_BASE, &[0; 4]);
        img[0x12..0x14].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        assert!(matches!(load(&img, 0x1000, None), Err(ElfError::Unsupported)));
    }
}
