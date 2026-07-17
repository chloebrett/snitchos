//! ELF64 loader front-end.
//!
//! Parses a static, position-dependent RISC-V executable image into a
//! [`LoadPlan`] — the entry point plus the `PT_LOAD` segments the
//! kernel needs to map and copy. Pure data transformation: bytes in,
//! a plan out. No `unsafe`, no MMIO; host-tested here, consumed by the
//! `kernel`-side loader (which does the mapping + copying).
//!
//! Scope is deliberately "only what loading needs": ELF header +
//! program headers, `PT_LOAD` only. No section headers, symbols,
//! relocations, or dynamic linking — the user program is linked
//! position-dependent at a fixed VA, so segments map at `p_vaddr`
//! verbatim. See `plans/v0.7a-first-userspace.md` and
//! `docs/v0.7-userspace-concepts.md`.
//!
//! This is a trust boundary even though v0.7a's input is embedded and
//! trusted: every field is validated and a malformed image yields an
//! [`ElfError`], never a panic. v0.10 (filesystem) loads untrusted
//! images through the same parser unchanged.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Read/write/execute permissions for a loadable segment, decoded from
/// the ELF program header `p_flags`. Pure perms — the kernel adds the
/// `U`/`G` page-table bits when it maps; this type stays decoupled from
/// `mmu::PtePerms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentPerms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

/// One `PT_LOAD` segment: copy `file_size` bytes from `image[file_offset..]`
/// to virtual address `vaddr`, then zero-fill up to `mem_size`
/// (`mem_size - file_size` is the bss tail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSegment {
    pub vaddr: usize,
    pub file_offset: usize,
    pub file_size: usize,
    pub mem_size: usize,
    pub perms: SegmentPerms,
}

/// Everything the kernel needs to load a program: where execution
/// starts (`entry`) and the segments to place in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPlan {
    pub entry: usize,
    pub segments: Vec<LoadSegment>,
}

/// Why an image could not be parsed into a [`LoadPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Image too short to contain the structure being read.
    TooShort,
    /// Bytes 0..4 are not the ELF magic `\x7fELF`.
    BadMagic,
    /// Not 64-bit (`EI_CLASS != ELFCLASS64`).
    NotElf64,
    /// `e_machine` is not RISC-V (`EM_RISCV = 243`).
    NotRiscv,
    /// `e_type` is not `ET_EXEC` — we only load static executables.
    NotExecutable,
    /// Program-header table location/size is inconsistent or
    /// out of bounds.
    BadProgramHeaders,
    /// A `PT_LOAD` segment's file range lies outside the image.
    SegmentOutOfBounds,
    /// A `PT_LOAD` segment has `p_filesz > p_memsz` (nonsensical).
    FileSizeExceedsMemSize,
}

/// Parse a static, position-dependent RISC-V ELF64 executable into a
/// [`LoadPlan`]. Returns [`ElfError`] for any malformed input.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ET_EXEC: u16 = 2;
const EM_RISCV: u16 = 243;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

fn read_u16(image: &[u8], off: usize) -> Result<u16, ElfError> {
    let bytes = image.get(off..off + 2).ok_or(ElfError::TooShort)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(image: &[u8], off: usize) -> Result<u32, ElfError> {
    let bytes = image.get(off..off + 4).ok_or(ElfError::TooShort)?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("4-byte slice")))
}

fn read_u64(image: &[u8], off: usize) -> Result<u64, ElfError> {
    let bytes = image.get(off..off + 8).ok_or(ElfError::TooShort)?;
    Ok(u64::from_le_bytes(bytes.try_into().expect("8-byte slice")))
}

pub fn parse(image: &[u8]) -> Result<LoadPlan, ElfError> {
    if image.len() < ELF_MAGIC.len() || image[..ELF_MAGIC.len()] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if *image.get(4).ok_or(ElfError::TooShort)? != ELFCLASS64 {
        return Err(ElfError::NotElf64);
    }
    if read_u16(image, 16)? != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }
    if read_u16(image, 18)? != EM_RISCV {
        return Err(ElfError::NotRiscv);
    }

    let entry = read_u64(image, 24)? as usize;
    let phoff = read_u64(image, 32)? as usize;
    let phentsize = read_u16(image, 54)? as usize;
    let phnum = read_u16(image, 56)? as usize;

    let mut segments = Vec::new();
    for i in 0..phnum {
        let base = phoff + i * phentsize;
        if read_u32(image, base)? != PT_LOAD {
            continue;
        }
        let flags = read_u32(image, base + 4)?;
        let file_offset = read_u64(image, base + 8)? as usize;
        let file_size = read_u64(image, base + 32)? as usize;
        let mem_size = read_u64(image, base + 40)? as usize;

        if file_size > mem_size {
            return Err(ElfError::FileSizeExceedsMemSize);
        }
        let end = file_offset
            .checked_add(file_size)
            .ok_or(ElfError::SegmentOutOfBounds)?;
        if end > image.len() {
            return Err(ElfError::SegmentOutOfBounds);
        }

        segments.push(LoadSegment {
            file_offset,
            vaddr: read_u64(image, base + 16)? as usize,
            file_size,
            mem_size,
            perms: SegmentPerms {
                read: flags & PF_R != 0,
                write: flags & PF_W != 0,
                exec: flags & PF_X != 0,
            },
        });
    }

    Ok(LoadPlan { entry, segments })
}

/// Why a [`LoadPlan`] cannot be turned into a page map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// Some page ends up both writable and executable once the perms of every
    /// segment touching it are unioned — a W^X violation. Two `PT_LOAD`s may
    /// legitimately share a page (R-X code + R-- rodata unions to R-X, which is
    /// fine), but a writable segment sharing a page with an executable one is
    /// not: mapping it would hand userspace an RWX page. Refuse the image
    /// instead. `page_va` is the offending page, for the refusal frame.
    WxViolation { page_va: usize },
}

impl SegmentPerms {
    /// The weakest perms allowing everything both sets allow — what a page
    /// shared by two segments must be mapped with.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            read: self.read || other.read,
            write: self.write || other.write,
            exec: self.exec || other.exec,
        }
    }

    /// Writable *and* executable — the combination W^X forbids.
    #[must_use]
    pub fn is_wx(self) -> bool {
        self.write && self.exec
    }
}

/// The page-aligned VAs a segment occupies in memory, `mem_size` (not
/// `file_size`) wide so the zero-filled bss tail gets pages too.
fn pages_of(seg: &LoadSegment, page_size: usize) -> impl Iterator<Item = usize> {
    let start = seg.vaddr & !(page_size - 1);
    let end = (seg.vaddr + seg.mem_size).div_ceil(page_size) * page_size;
    (start..end).step_by(page_size)
}

/// Map every page the plan's segments touch to the perms it must be mapped
/// with. Segments may share a page, so perms are unioned; the W^X check runs
/// on the *union*, because that is where the violation emerges — neither
/// segment alone need be both writable and executable.
///
/// Returns [`PlanError::WxViolation`] rather than silently mapping an RWX page.
pub fn page_perms(
    plan: &LoadPlan,
    page_size: usize,
) -> Result<BTreeMap<usize, SegmentPerms>, PlanError> {
    let mut by_page: BTreeMap<usize, SegmentPerms> = BTreeMap::new();
    for seg in &plan.segments {
        for page_va in pages_of(seg, page_size) {
            let merged = by_page.get(&page_va).map_or(seg.perms, |p| p.union(seg.perms));
            by_page.insert(page_va, merged);
        }
    }

    match by_page.iter().find(|(_, perms)| perms.is_wx()) {
        Some((&page_va, _)) => Err(PlanError::WxViolation { page_va }),
        None => Ok(by_page),
    }
}

/// One `copy_nonoverlapping` the loader must perform: copy `len` bytes from
/// `image[src_off..]` into the frame mapped at `page_va`, starting `page_off`
/// bytes into that frame.
///
/// Windows never span a page, because each page is a separately-allocated
/// frame — the loader resolves `page_va` to that frame's address and copies
/// within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyWindow {
    /// The page whose frame receives these bytes.
    pub page_va: usize,
    /// Offset into that page to start writing at.
    pub page_off: usize,
    /// Offset into the ELF image to read from.
    pub src_off: usize,
    /// How many bytes to copy. Never zero.
    pub len: usize,
}

/// Split a segment's file bytes into per-page copy windows.
///
/// Only `file_size` bytes are copied; the `mem_size - file_size` bss tail
/// yields no windows, leaving the zeroed frame zeroed. `page_off` and `src_off`
/// are derived from different bases (the page vs the segment's `vaddr`), which
/// is what makes a non-page-aligned `vaddr` worth testing.
pub fn copy_windows(seg: &LoadSegment, page_size: usize) -> impl Iterator<Item = CopyWindow> + '_ {
    let file_lo = seg.vaddr;
    let file_hi = seg.vaddr + seg.file_size;
    pages_of(seg, page_size).filter_map(move |page_va| {
        let lo = file_lo.max(page_va);
        let hi = file_hi.min(page_va + page_size);
        if lo >= hi {
            return None;
        }
        Some(CopyWindow {
            page_va,
            page_off: lo - page_va,
            src_off: seg.file_offset + (lo - file_lo),
            len: hi - lo,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A `PT_LOAD` program-header description for the test builder.
    struct Ph {
        p_type: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        filesz: u64,
        memsz: u64,
    }

    /// Build a little-endian ELF64 image: a 64-byte header, then the
    /// program-header table immediately after it, then `tail_len`
    /// bytes of padding so segment file ranges can point into the
    /// image. Field bytes are written by absolute offset so individual
    /// tests can corrupt one field.
    fn build_elf(e_type: u16, e_machine: u16, ei_class: u8, entry: u64, phs: &[Ph], tail_len: usize) -> Vec<u8> {
        const EH_SIZE: usize = 64;
        const PH_SIZE: usize = 56;
        let phoff = EH_SIZE;
        let mut img = vec![0u8; EH_SIZE + phs.len() * PH_SIZE + tail_len];

        img[0..4].copy_from_slice(&ELF_MAGIC);
        img[4] = ei_class; // EI_CLASS (2 = ELFCLASS64)
        img[5] = 1; // EI_DATA = little-endian
        img[6] = 1; // EI_VERSION
        img[16..18].copy_from_slice(&e_type.to_le_bytes());
        img[18..20].copy_from_slice(&e_machine.to_le_bytes());
        img[24..32].copy_from_slice(&entry.to_le_bytes());
        img[32..40].copy_from_slice(&(phoff as u64).to_le_bytes()); // e_phoff
        img[54..56].copy_from_slice(&(PH_SIZE as u16).to_le_bytes()); // e_phentsize
        img[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes()); // e_phnum

        for (i, ph) in phs.iter().enumerate() {
            let base = phoff + i * PH_SIZE;
            img[base..base + 4].copy_from_slice(&ph.p_type.to_le_bytes());
            img[base + 4..base + 8].copy_from_slice(&ph.flags.to_le_bytes());
            img[base + 8..base + 16].copy_from_slice(&ph.offset.to_le_bytes());
            img[base + 16..base + 24].copy_from_slice(&ph.vaddr.to_le_bytes());
            img[base + 32..base + 40].copy_from_slice(&ph.filesz.to_le_bytes());
            img[base + 40..base + 48].copy_from_slice(&ph.memsz.to_le_bytes());
        }
        img
    }

    /// A minimal well-formed RISC-V ELF64 executable: 64-bit, machine
    /// 243, `ET_EXEC`.
    fn valid_elf(entry: u64, phs: &[Ph], tail_len: usize) -> Vec<u8> {
        build_elf(2, 243, 2, entry, phs, tail_len)
    }

    #[test]
    fn parses_real_toolchain_elf_output() {
        // A *frozen* real toolchain ELF (a checked-in `user/hello` build),
        // kept solely as a parser fixture — the kernel embeds freshly-built
        // programs now (see `kernel/build.rs`), nothing ships this. Frozen on
        // purpose: it pins the parser against real linker output (GNU_STACK,
        // RISC-V attributes header, zero-filled bss) without churning when
        // `hello` changes. Not a hand-built buffer.
        let img = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/sample-user.elf"));
        let plan = parse(img).expect("the sample user ELF should parse");

        // Non-PIE ET_EXEC linked at the fixed low-half VA (see user.ld).
        assert_eq!(plan.entry, 0x1000_0000);
        // Exactly the two PT_LOAD segments; GNU_STACK and the RISC-V
        // attributes header (PT 0x70000003) are skipped.
        assert_eq!(plan.segments.len(), 2);

        let code = plan.segments[0];
        assert_eq!(code.vaddr, 0x1000_0000);
        assert_eq!(code.perms, SegmentPerms { read: true, write: false, exec: true });

        let bss = plan.segments[1];
        assert!(bss.mem_size > bss.file_size, "stack/bss is zero-filled (memsz > filesz)");
        assert_eq!(bss.perms, SegmentPerms { read: true, write: true, exec: false });
    }

    #[test]
    fn rejects_an_image_without_the_elf_magic() {
        let not_elf = [0u8; 64];
        assert_eq!(parse(&not_elf), Err(ElfError::BadMagic));
    }

    /// 4 KiB — the page size the kernel loader maps with. Passed explicitly so
    /// `elf` stays free of any page-size assumption of its own.
    const PAGE: usize = 4096;

    fn seg(vaddr: usize, file_size: usize, mem_size: usize, perms: SegmentPerms) -> LoadSegment {
        LoadSegment { vaddr, file_offset: 0, file_size, mem_size, perms }
    }

    const RX: SegmentPerms = SegmentPerms { read: true, write: false, exec: true };
    const RW: SegmentPerms = SegmentPerms { read: true, write: true, exec: false };
    const R: SegmentPerms = SegmentPerms { read: true, write: false, exec: false };

    #[test]
    fn refuses_a_page_shared_by_executable_and_writable_segments() {
        // The W^X guard. Two PT_LOADs sharing one page union to R+W+X: exactly
        // what `user.ld` intends to prevent by page-aligning the writable
        // segment, and what a non-empty `.data` would silently reintroduce
        // (the linker does NOT page-align new PT_LOADs here — today's `init`
        // has an R-- rodata segment starting mid-page at 0x100006B0).
        // Nothing else in the tree asserts this; refuse rather than map RWX.
        let plan = LoadPlan {
            entry: 0x1000,
            segments: vec![seg(0x1000, 0x100, 0x100, RX), seg(0x1100, 0x80, 0x80, RW)],
        };
        assert_eq!(page_perms(&plan, PAGE), Err(PlanError::WxViolation { page_va: 0x1000 }));
    }

    #[test]
    fn allows_executable_and_read_only_segments_to_share_a_page() {
        // The guard must not over-refuse: this is exactly today's `init` —
        // R-X code at 0x10000000 and R-- rodata starting mid-page at
        // 0x100006B0. R-X ∪ R-- = R-X, no W, so it maps fine. If this ever
        // fails, the kernel refuses to boot its own root process.
        let plan = LoadPlan {
            entry: 0x1000,
            segments: vec![seg(0x1000, 0x6B0, 0x6B0, RX), seg(0x16B0, 0x258, 0x258, R)],
        };
        let pages = page_perms(&plan, PAGE).expect("R-X ∪ R-- is not a W^X violation");
        assert_eq!(pages, BTreeMap::from([(0x1000, RX)]));
    }

    #[test]
    fn union_takes_each_permission_bit_from_either_side() {
        // Per-bit OR, not AND: a page shared by a read-only and an exec-only
        // segment must keep BOTH bits. Every other test here uses segments that
        // are all `read: true`, which cannot tell OR from AND on that bit — so
        // this asymmetric case is the one that pins it. An AND would drop `R`
        // from a shared page and fault the program on its first load.
        let exec_only = SegmentPerms { read: false, write: false, exec: true };
        assert_eq!(R.union(exec_only), RX);
        assert_eq!(exec_only.union(R), RX, "union is symmetric");
    }

    #[test]
    fn detects_a_wx_violation_that_only_exists_after_the_union() {
        // Neither segment is itself W+X — the violation is created *by* sharing
        // the page. Pins that the check runs on the union, not per-segment.
        let plan = LoadPlan {
            entry: 0x2000,
            segments: vec![seg(0x2000, 0x10, 0x10, RX), seg(0x2010, 0x10, 0x10, RW)],
        };
        assert!(!RX.is_wx() && !RW.is_wx(), "neither segment alone is W+X");
        assert_eq!(page_perms(&plan, PAGE), Err(PlanError::WxViolation { page_va: 0x2000 }));
    }

    #[test]
    fn a_writable_segment_on_its_own_page_is_not_a_violation() {
        // The layout `user.ld` intends: .bss page-aligned, so RW never meets
        // R-X. Two pages, two distinct perms.
        let plan = LoadPlan {
            entry: 0x1000,
            segments: vec![seg(0x1000, 0x100, 0x100, RX), seg(0x2000, 0x100, 0x100, RW)],
        };
        let pages = page_perms(&plan, PAGE).expect("page-separated RW and R-X are fine");
        assert_eq!(pages, BTreeMap::from([(0x1000, RX), (0x2000, RW)]));
    }

    #[test]
    fn pages_cover_the_zero_filled_bss_tail_beyond_the_file_bytes() {
        // mem_size spans into a second page that file_size doesn't reach. Both
        // pages must be mapped or the bss tail (which holds the user stack —
        // see user.ld) is unmapped and `sp` faults.
        let plan = LoadPlan { entry: 0, segments: vec![seg(0x1000, 0x10, PAGE + 0x10, RW)] };
        let pages = page_perms(&plan, PAGE).expect("plain RW segment");
        assert_eq!(pages.keys().copied().collect::<Vec<_>>(), vec![0x1000, 0x2000]);
    }

    #[test]
    fn copies_a_page_straddling_segment_contiguously_from_the_file() {
        // A segment whose vaddr is NOT page-aligned, spanning two pages. `dst`
        // and `src` are computed from different bases (page_va vs vaddr), so an
        // off-by-one here copies the image shifted by a few bytes — the program
        // then executes garbage. Pins that consecutive windows read a
        // *contiguous* file range: window 1 ends at src 0x500+0xF00 == window
        // 2's src 0x1400.
        let s = LoadSegment {
            vaddr: 0x1100,
            file_offset: 0x500,
            file_size: 0x1000,
            mem_size: 0x1000,
            perms: R,
        };
        let windows: Vec<_> = copy_windows(&s, PAGE).collect();
        assert_eq!(
            windows,
            vec![
                CopyWindow { page_va: 0x1000, page_off: 0x100, src_off: 0x500, len: 0xF00 },
                CopyWindow { page_va: 0x2000, page_off: 0, src_off: 0x1400, len: 0x100 },
            ]
        );
        let copied: usize = windows.iter().map(|w| w.len).sum();
        assert_eq!(copied, s.file_size, "every file byte is copied exactly once");
    }

    #[test]
    fn skips_the_bss_tail_so_the_zeroed_frame_stays_zero() {
        // file_size < mem_size: the tail pages have no file bytes. Emitting a
        // window for them would copy unrelated image bytes over what must stay
        // zeroed bss (which holds the user stack).
        let s = LoadSegment {
            vaddr: 0x1000,
            file_offset: 0x40,
            file_size: 0x10,
            mem_size: PAGE * 2,
            perms: RW,
        };
        let windows: Vec<_> = copy_windows(&s, PAGE).collect();
        assert_eq!(
            windows,
            vec![CopyWindow { page_va: 0x1000, page_off: 0, src_off: 0x40, len: 0x10 }],
            "only the file bytes are copied; the bss tail page gets no window"
        );
    }

    #[test]
    fn a_pure_bss_segment_copies_nothing() {
        // Today's `init` RW segment: FileSize 0, MemSize 524336. Pages must be
        // mapped (page_perms covers that) but nothing is copied.
        let s =
            LoadSegment { vaddr: 0x1000, file_offset: 0, file_size: 0, mem_size: PAGE, perms: RW };
        assert_eq!(copy_windows(&s, PAGE).count(), 0);
    }

    #[test]
    fn a_segment_ending_exactly_on_a_page_boundary_maps_no_extra_page() {
        // Pins the end-rounding against an off-by-one: a segment exactly one
        // page long must map one page, not two.
        let plan = LoadPlan { entry: 0, segments: vec![seg(0x1000, PAGE, PAGE, RW)] };
        let pages = page_perms(&plan, PAGE).expect("plain RW segment");
        assert_eq!(pages.keys().copied().collect::<Vec<_>>(), vec![0x1000]);
    }

    #[test]
    fn rejects_an_image_that_is_only_the_magic() {
        // Valid magic but nothing after it: the length guard must let
        // this through to header reads, which then run out of bytes.
        // (Pins the magic length-check against `<` -> `==`/`<=`.)
        assert_eq!(parse(&ELF_MAGIC), Err(ElfError::TooShort));
    }

    #[test]
    fn parses_a_valid_header_with_no_loadable_segments() {
        // 64-byte image: every header field is in bounds, phnum == 0.
        // Pins the 16-bit reads against a slice-end mutation that only
        // diverges when the wider slice overruns a small image.
        let img = valid_elf(0xABCD, &[], 0);
        let plan = parse(&img).expect("valid header should parse");
        assert_eq!(plan.entry, 0xABCD);
        assert!(plan.segments.is_empty());
    }

    #[test]
    fn accepts_a_segment_that_exactly_fills_the_image() {
        // file_offset + file_size == image.len(); the bounds check must
        // use `>` (accept), not `>=` (would spuriously reject).
        let img = valid_elf(
            0x1000,
            &[Ph { p_type: PT_LOAD, flags: PF_R, offset: 120, vaddr: 0x1000, filesz: 64, memsz: 64 }],
            64, // image is exactly header(64) + phdr(56) + 64 = 184; segment ends at 184
        );
        let plan = parse(&img).expect("a segment ending exactly at EOF is valid");
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].file_offset, 120);
    }

    #[test]
    fn decodes_the_read_bit_independently_of_the_other_flags() {
        // Read-only segment (read must stay true) + write-only segment
        // (read must stay false). Pins `flags & PF_R` against `|` (would
        // force read always true) and `^` (would flip a read-only seg).
        let img = valid_elf(
            0x1000,
            &[
                Ph { p_type: PT_LOAD, flags: PF_R, offset: 176, vaddr: 0x1000, filesz: 0x10, memsz: 0x10 },
                Ph { p_type: PT_LOAD, flags: PF_W, offset: 192, vaddr: 0x2000, filesz: 0x10, memsz: 0x10 },
            ],
            0x100,
        );
        let plan = parse(&img).expect("valid ELF");
        assert!(plan.segments[0].perms.read, "read-only segment is readable");
        assert!(!plan.segments[1].perms.read, "write-only segment is not readable");
    }

    #[test]
    fn captures_the_bss_tail_when_memsz_exceeds_filesz() {
        let img = valid_elf(
            0x2000,
            &[Ph { p_type: PT_LOAD, flags: PF_R | PF_W, offset: 0x80, vaddr: 0x2000, filesz: 0x20, memsz: 0x80 }],
            0x80,
        );
        let plan = parse(&img).expect("valid ELF");
        let seg = plan.segments[0];
        assert_eq!(seg.file_size, 0x20);
        assert_eq!(seg.mem_size, 0x80); // 0x60 of bss, zero-filled at load
        assert_eq!(seg.perms, SegmentPerms { read: true, write: true, exec: false });
    }

    #[test]
    fn skips_non_load_headers_and_preserves_load_order() {
        let img = valid_elf(
            0x1000,
            &[
                Ph { p_type: 0, flags: 0, offset: 0, vaddr: 0, filesz: 0, memsz: 0 }, // PT_NULL — skipped
                Ph { p_type: PT_LOAD, flags: PF_R | PF_X, offset: 0x100, vaddr: 0x1000, filesz: 0x40, memsz: 0x40 },
                Ph { p_type: PT_LOAD, flags: PF_R | PF_W, offset: 0x140, vaddr: 0x2000, filesz: 0x20, memsz: 0x80 },
            ],
            0x200,
        );
        let plan = parse(&img).expect("valid ELF");
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].vaddr, 0x1000);
        assert_eq!(plan.segments[0].perms, SegmentPerms { read: true, write: false, exec: true });
        assert_eq!(plan.segments[1].vaddr, 0x2000);
        assert_eq!(plan.segments[1].perms, SegmentPerms { read: true, write: true, exec: false });
    }

    #[test]
    fn rejects_a_segment_whose_file_range_runs_past_the_image() {
        // offset 0x80 + filesz 0x100 = 0x180, but the image only has
        // 0x80 bytes of tail after the header+phdr.
        let img = valid_elf(
            0x1000,
            &[Ph { p_type: PT_LOAD, flags: PF_R, offset: 0x80, vaddr: 0x1000, filesz: 0x100, memsz: 0x100 }],
            0x80,
        );
        assert_eq!(parse(&img), Err(ElfError::SegmentOutOfBounds));
    }

    #[test]
    fn rejects_a_segment_with_filesz_greater_than_memsz() {
        let img = valid_elf(
            0x1000,
            &[Ph { p_type: PT_LOAD, flags: PF_R, offset: 0x80, vaddr: 0x1000, filesz: 0x40, memsz: 0x10 }],
            0x80,
        );
        assert_eq!(parse(&img), Err(ElfError::FileSizeExceedsMemSize));
    }

    #[test]
    fn rejects_a_32_bit_elf() {
        // EI_CLASS = 1 (ELFCLASS32).
        let img = build_elf(2, 243, 1, 0x1000, &[], 0);
        assert_eq!(parse(&img), Err(ElfError::NotElf64));
    }

    #[test]
    fn rejects_a_non_riscv_machine() {
        // e_machine = 62 (x86-64).
        let img = build_elf(2, 62, 2, 0x1000, &[], 0);
        assert_eq!(parse(&img), Err(ElfError::NotRiscv));
    }

    #[test]
    fn rejects_a_non_executable_type() {
        // e_type = 3 (ET_DYN — a shared object / PIE, which we don't load).
        let img = build_elf(3, 243, 2, 0x1000, &[], 0);
        assert_eq!(parse(&img), Err(ElfError::NotExecutable));
    }

    #[test]
    fn parses_entry_point_and_a_single_load_segment() {
        let img = valid_elf(
            0x1000_0040,
            &[Ph {
                p_type: PT_LOAD,
                flags: PF_R | PF_X,
                offset: 0x80,
                vaddr: 0x1000_0000,
                filesz: 0x40,
                memsz: 0x40,
            }],
            0x80, // tail so the segment's [0x80, 0xC0) file range is in-bounds
        );

        let plan = parse(&img).expect("valid ELF should parse");

        assert_eq!(plan.entry, 0x1000_0040);
        assert_eq!(
            plan.segments,
            vec![LoadSegment {
                vaddr: 0x1000_0000,
                file_offset: 0x80,
                file_size: 0x40,
                mem_size: 0x40,
                perms: SegmentPerms { read: true, write: false, exec: true },
            }]
        );
    }
}
