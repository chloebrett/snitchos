//! Userspace program embedding and loading (v0.7a).
//!
//! The first userspace program, `user/hello`, is baked into the kernel
//! image at build time. `build.rs` resolves the path: the freshly-built
//! artifact when building via `cargo xtask build` (which compiles `hello`
//! first and passes `SNITCHOS_USER_ELF`), otherwise the committed fixture
//! `kernel-core/fixtures/hello.elf`.
//!
//! [`load`] parses the embedded ELF with [`kernel_core::elf`] and maps its
//! segments into the boot page table's low half with the `U` bit set, then
//! returns the entry point. v0.7a Step A loads into the shared boot table;
//! Step B will load into a per-process root instead. Entering U-mode (the
//! `sret`) and the syscall handler are the caller's job.

use kernel_core::elf::{self, SegmentPerms};
use kernel_core::mmu::{MapError, PtePerms};

use crate::frame::{self, FRAME_SIZE};
use crate::mmu;

/// The embedded `user/hello` ELF image (a static, position-dependent
/// RISC-V executable linked at `0x1000_0000`).
pub static HELLO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_ELF"));

/// A loaded program, ready to enter.
#[allow(dead_code, reason = "consumed by the Step 4b enter sequence")]
pub struct Loaded {
    /// The entry-point VA (`e_entry`) to put in `sepc`.
    pub entry: usize,
    /// A sane initial user `sp` (top of the highest writable segment). The
    /// program's `_start` also sets `sp` itself via the linker `__stack_top`
    /// symbol, so this is belt-and-suspenders.
    pub initial_sp: usize,
}

/// Why loading the embedded program failed.
#[derive(Debug)]
#[allow(dead_code, reason = "variants are reported via panic in Step 4d")]
pub enum LoadError {
    /// The embedded image is not a valid ELF we can load.
    Parse(elf::ElfError),
    /// The frame allocator is exhausted.
    OutOfFrames,
    /// Installing a page-table entry failed.
    Map(MapError),
}

/// Translate ELF segment R/W/X flags into page-table perms, always with
/// the `U` bit so U-mode may access the page.
fn perms_for(p: SegmentPerms) -> PtePerms {
    let mut perms = PtePerms::U;
    if p.read {
        perms = perms.union(PtePerms::R);
    }
    if p.write {
        perms = perms.union(PtePerms::W);
    }
    if p.exec {
        perms = perms.union(PtePerms::X);
    }
    perms
}

/// Parse [`HELLO_ELF`] and map every `PT_LOAD` segment into the boot page
/// table's low half: one fresh zeroed frame per page, mapped `U` + the
/// segment's perms, with the segment's file bytes copied in (the rest left
/// zero — that's the bss). Returns the entry point and a default user `sp`.
#[allow(dead_code, reason = "wired into boot by Step 4d")]
pub fn load() -> Result<Loaded, LoadError> {
    let plan = elf::parse(HELLO_ELF).map_err(LoadError::Parse)?;
    let mut max_writable_end = 0usize;

    for seg in &plan.segments {
        let perms = perms_for(seg.perms);
        let page_start = seg.vaddr & !(FRAME_SIZE - 1);
        let page_end = (seg.vaddr + seg.mem_size + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);

        let mut page_va = page_start;
        while page_va < page_end {
            let f = frame::alloc_zeroed().ok_or(LoadError::OutOfFrames)?;
            mmu::map(page_va, f.addr(), perms).map_err(LoadError::Map)?;

            // Copy the slice of the segment's file bytes that lands in this
            // page. The frame is fresh-zeroed, so any tail beyond file_size
            // (the bss) is already 0.
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.file_size;
            let lo = file_lo.max(page_va);
            let hi = file_hi.min(page_va + FRAME_SIZE);
            if lo < hi {
                let src = seg.file_offset + (lo - file_lo);
                let dst = f.kernel_va() + (lo - page_va);
                // SAFETY: `dst` is this fresh frame's linear-map VA (writable,
                // covers all RAM); the copy length is at most one page; `src`
                // is in-bounds of HELLO_ELF (the parser validated the segment
                // file range against the image length).
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        HELLO_ELF.as_ptr().add(src),
                        dst as *mut u8,
                        hi - lo,
                    );
                }
            }
            page_va += FRAME_SIZE;
        }

        if seg.perms.write {
            max_writable_end = max_writable_end.max(seg.vaddr + seg.mem_size);
        }
    }

    Ok(Loaded { entry: plan.entry, initial_sp: max_writable_end })
}
