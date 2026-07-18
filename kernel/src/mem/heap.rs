//! Kernel heap. A `#[global_allocator]` backed by physical frames
//! mapped one-by-one into a dedicated 1 GiB VA window via
//! `mmu::map`. Once `init` runs, `Box`, `Vec`, `String`, and the
//! rest of `alloc::` are usable. The heap grows on demand from the
//! heartbeat loop when free bytes drop below a watermark.
//!
//! Region strategy is (b) from the step-4 plan: a dedicated VA range
//! at `HEAP_VA_BASE` (root PTE 256), per-frame PTE install. Heap VAs
//! are contiguous by construction; frame PAs are scattered.
//!
//! Telemetry counters are atomics drained by the heartbeat thread —
//! never emit a frame from inside `GlobalAlloc::alloc` or `dealloc`,
//! the virtio TX path takes locks that would deadlock if re-entered
//! via an allocation.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use linked_list_allocator::Heap;

use crate::counter::DeferredCounter;
use crate::{frame, mmu};
use kernel_mem::mmu::{HEAP_VA_BASE, PtePerms};

pub use kernel_mem::heap::{Stats, WatermarkConfig, watermark_grow_decision};

/// Watermark policy for this kernel. Grow at 25% free, 256 frames per
/// grow event, ceiling at `MAX_HEAP_SIZE`.
pub const WATERMARK: WatermarkConfig = WatermarkConfig {
    max_size: MAX_HEAP_SIZE,
    free_threshold_pct: 25,
    grow_frames: 256,
    frame_size: frame::FRAME_SIZE,
};

/// Initial heap = 4 MiB = 1024 frames. The heap may grow up to
/// `MAX_HEAP_FRAMES` via `extend` from the heartbeat loop.
pub const INITIAL_HEAP_FRAMES: usize = 1024;
pub const INITIAL_HEAP_SIZE: usize = INITIAL_HEAP_FRAMES * frame::FRAME_SIZE;

/// Heap ceiling — one full root-PTE slot = 1 GiB = 262144 frames. Past
/// this we'd need a second root-PTE slot; flag as OOM instead for v0.4.
pub const MAX_HEAP_FRAMES: usize = (1024 * 1024 * 1024) / frame::FRAME_SIZE;
pub const MAX_HEAP_SIZE: usize = MAX_HEAP_FRAMES * frame::FRAME_SIZE;

/// Counters drained by the heartbeat thread. Updated outside the heap
/// lock to keep emission off the allocator's critical path. Capacity
/// and live bytes-used come from `stats()` (a brief lock take from the
/// heartbeat) — the allocator already tracks those internally, so
/// mirroring them in atomics would be redundant. Note `Heap::used()`
/// sums alignment-padded `layout.size()` for live allocations; it does
/// not include hole-list metadata bytes, so it's a slight undercount
/// of how much of the region is unavailable.
/// `Relaxed` everywhere: pure tallies. See `kernel::percpu` for the
/// kernel-wide ordering discipline.
pub static ALLOC_COUNT: DeferredCounter = DeferredCounter::new("snitchos.heap.alloc_total");
pub static DEALLOC_COUNT: DeferredCounter = DeferredCounter::new("snitchos.heap.dealloc_total");
pub static ALLOC_FAIL_COUNT: DeferredCounter = DeferredCounter::new("snitchos.heap.alloc_failed_total");

/// Counters for grow attempts — bumped from `extend` (heartbeat path,
/// so it's safe to take the allocator lock when emitting metrics).
pub static GROW_COUNT: DeferredCounter = DeferredCounter::new("snitchos.heap.grow_total");
pub static GROW_FAIL_COUNT: DeferredCounter = DeferredCounter::new("snitchos.heap.grow_failed_total");

/// Highest VA currently mapped + 1. Bumped by `init` and `extend`.
/// Single-writer (boot, then heartbeat); reads in the same context.
/// `Relaxed`: the atomic is for lock-free reads, no other memory
/// synchronises through this value — actual mapping state is published
/// via `mmu::map`'s `sfence.vma` on the same hart.
static HEAP_TOP: AtomicUsize = AtomicUsize::new(HEAP_VA_BASE);

/// `GlobalAlloc` wrapper around a `kernel::sync::Mutex<Heap>`. We
/// don't use `linked_list_allocator::LockedHeap` directly because we
/// need to bump `ALLOC_COUNT` / `DEALLOC_COUNT` / `ALLOC_FAIL_COUNT`
/// in the alloc/dealloc paths, and `LockedHeap` doesn't expose hooks
/// for that. Going through `kernel::sync` also keeps the lock type
/// consistent with the rest of the kernel — preempt/IRQ-disable
/// hooks land in one place when they land.
struct KernelHeap {
    inner: crate::sync::Mutex<Heap>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    inner: crate::sync::Mutex::new(Heap::empty()),
};

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = self.inner.lock().allocate_first_fit(layout);
        if let Ok(nn) = result {
            ALLOC_COUNT.inc();
            nn.as_ptr()
        } else {
            ALLOC_FAIL_COUNT.inc();
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller's contract — `ptr`/`layout` were returned by
        // a previous `alloc`.
        unsafe {
            self.inner
                .lock()
                .deallocate(core::ptr::NonNull::new_unchecked(ptr), layout);
        }
        DEALLOC_COUNT.inc();
    }
}

/// Snapshot of the heap's occupancy. Briefly takes the heap lock —
/// safe to call from the heartbeat thread (single-threaded, no
/// contention with the allocator). Returns `None` before `init` runs.
pub fn stats() -> Option<Stats> {
    let h = HEAP.inner.lock();
    if h.size() == 0 {
        return None;
    }
    let (free_blocks, largest_free_block) = h.free_block_stats();
    Some(Stats {
        capacity: h.size(),
        used: h.used(),
        free: h.free(),
        free_blocks,
        largest_free_block,
    })
}

/// Initialise the kernel heap. Allocates `INITIAL_HEAP_FRAMES`
/// individual physical frames and `map`s each into the heap VA
/// range starting at `HEAP_VA_BASE`. PAs are scattered; VAs are
/// contiguous by construction.
///
/// # Safety
///
/// Call exactly once, after `frame::init_from_dtb` and before any
/// `alloc::` use. The kernel must be at higher-half PC so `mmu::map`
/// resolves `BOOT_PT_ROOT`'s PA correctly.
pub unsafe fn init() {
    grow_va_range(INITIAL_HEAP_FRAMES).expect("heap init: out of frames or map() failed");
    // SAFETY: `grow_va_range` just installed PTEs for
    // `[HEAP_VA_BASE, HEAP_VA_BASE + INITIAL_HEAP_SIZE)` mapping into
    // freshly-allocated frames with R+W permissions; nothing else
    // aliases that VA window (root PTE 256 is exclusively ours).
    unsafe {
        HEAP.inner.lock().init(HEAP_VA_BASE as *mut u8, INITIAL_HEAP_SIZE);
    }
}

/// Grow the heap by `extra_frames` frames. Allocates frames, maps
/// them above the current top, and tells `linked_list_allocator`
/// about the new bytes. Returns `Err(())` on any frame-alloc or
/// `map` failure; partial progress is *not* unwound (consistent with
/// the step-5 plan's leak-on-failure policy).
///
/// Bumps `GROW_COUNT` on success, `GROW_FAIL_COUNT` on failure.
pub fn extend(extra_frames: usize) -> Result<(), ()> {
    let Ok(extra_bytes) = grow_va_range(extra_frames) else {
        GROW_FAIL_COUNT.inc();
        return Err(());
    };
    // SAFETY: `grow_va_range` just installed PTEs for
    // `[prev_top, prev_top + extra_bytes)`, contiguous with
    // the existing heap top. `linked_list_allocator::extend`
    // requires exactly that.
    unsafe { HEAP.inner.lock().extend(extra_bytes) };
    GROW_COUNT.inc();
    Ok(())
}

/// Allocate `n` frames and `map` each into the heap VA range
/// starting at the current `HEAP_TOP`. On success returns the
/// number of bytes added. Common path for both `init` and `extend`.
fn grow_va_range(n: usize) -> Result<usize, ()> {
    let start_top = HEAP_TOP.load(Ordering::Relaxed);
    let ceiling = HEAP_VA_BASE + MAX_HEAP_SIZE;
    let end_top =
        kernel_mem::heap::next_heap_top(start_top, n, frame::FRAME_SIZE, ceiling).ok_or(())?;
    let perms = PtePerms::R.union(PtePerms::W).union(PtePerms::G);
    for i in 0..n {
        let frame = frame::alloc_zeroed().ok_or(())?;
        let va = start_top + i * frame::FRAME_SIZE;
        mmu::map(va, frame.addr(), perms).map_err(|_| ())?;
    }
    HEAP_TOP.store(end_top, Ordering::Relaxed);
    Ok(end_top - start_top)
}
