//! Memory syscall: `MapAnon` — map fresh anonymous pages into the calling
//! process's heap region. Ambient (not cap-mediated), bounded by a per-process
//! memory cap.

use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Map a fresh anonymous memory region for U-mode. `a0` = bytes requested (the
/// runtime page-aligns). Maps that many zeroed frames into the process's heap
/// region and returns the region's **base** VA in `a0` (or `u64::MAX` if
/// refused — out of frames, or past the per-process memory cap). mmap-shaped:
/// the runtime allocator `claim`s the returned region. Placement is a simple
/// bump pointer (`heap_top`) for now; the allocator doesn't assume regions
/// abut, so disjoint placement + unmap can land later without an ABI change.
pub(super) fn handle_map_anon(frame: &mut TrapFrame) {
    use kernel_core::mmu::PtePerms;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    use crate::frame::FRAME_SIZE;
    use crate::process::Process;

    let sc = Syscall::MapAnon as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let bytes = (frame.a0 as usize).next_multiple_of(FRAME_SIZE);
    let base = proc.heap_top.load(Ordering::Relaxed);
    let end = base.saturating_add(bytes);
    if bytes == 0 || end > Process::HEAP_BASE + Process::HEAP_MAX {
        super::refuse(frame, sc, RefusalReason::OutOfMemory);
        return;
    }

    let perms = PtePerms::U.union(PtePerms::R).union(PtePerms::W);
    let mut va = base;
    while va < end {
        let Some(f) = crate::frame::alloc_zeroed() else {
            // Out of frames mid-map: the already-mapped pages leak until process
            // teardown (none in v0.7), and `heap_top` isn't advanced, so the
            // runtime never `claim`s a partial region.
            super::refuse(frame, sc, RefusalReason::OutOfMemory);
            return;
        };
        if crate::mmu::map_in(proc.root_pa, va, f.addr(), perms).is_err() {
            super::refuse(frame, sc, RefusalReason::OutOfMemory);
            return;
        }
        va += FRAME_SIZE;
    }
    // Make the new pages visible on this hart. SAFETY: flush stale (negative)
    // TLB entries for the freshly-mapped VAs; new mappings, so a local sfence
    // suffices — nothing on another hart cached them.
    unsafe { asm!("sfence.vma", options(nostack, nomem)) };

    proc.heap_top.store(end, Ordering::Relaxed);
    frame.a0 = base as u64;
}
