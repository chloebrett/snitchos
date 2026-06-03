//! Kernel heap. A `#[global_allocator]` backed by a contiguous run of
//! physical frames mapped through the linear map. Once `init` runs,
//! `Box`, `Vec`, `String`, and the rest of `alloc::` are usable.
//!
//! Region strategy is (a) from the step-4 plan: one contiguous frame
//! run grabbed at boot, addressed via `pa_to_kernel_va`. Fixed-size
//! for v0.4; growable variant is a fast-follow.
//!
//! Telemetry counters are atomics drained by the heartbeat thread —
//! never emit a frame from inside `GlobalAlloc::alloc` or `dealloc`,
//! the virtio TX path takes locks that would deadlock if re-entered
//! via an allocation.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

use linked_list_allocator::Heap;

use crate::frame;
use kernel_core::mmu::pa_to_kernel_va;

/// 4 MiB heap = 1024 frames. Fixed for v0.4.
pub const HEAP_FRAMES: usize = 1024;
pub const HEAP_SIZE: usize = HEAP_FRAMES * frame::FRAME_SIZE;

/// Counters drained by the heartbeat thread. Updated outside the heap
/// lock to keep emission off the allocator's critical path. Capacity
/// and live bytes-used come from `stats()` (a brief lock take from the
/// heartbeat) — the allocator already tracks those internally, so
/// mirroring them in atomics would be redundant. Note `Heap::used()`
/// sums alignment-padded `layout.size()` for live allocations; it does
/// not include hole-list metadata bytes, so it's a slight undercount
/// of how much of the region is unavailable.
pub static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
pub static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
pub static ALLOC_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    /// Total heap region size in bytes.
    pub capacity: usize,
    /// Sum of alignment-padded `layout.size()` across live allocations.
    /// Excludes hole-list metadata, so slightly undercounts unavailable
    /// bytes vs the true `capacity - free`.
    pub used: usize,
    /// Bytes the heap considers free — `capacity - used`, so does
    /// include hole-list metadata as "free" even though it isn't
    /// usable for allocations.
    pub free: usize,
}

/// `GlobalAlloc` wrapper around a `spin::Mutex<Heap>`. We don't use
/// `linked_list_allocator::LockedHeap` directly because we need to
/// bump `ALLOC_COUNT` / `DEALLOC_COUNT` / `ALLOC_FAIL_COUNT` in the
/// alloc/dealloc paths, and LockedHeap doesn't expose hooks for that.
/// Using project `spin` rather than the crate-bundled lock also keeps
/// the lock type consistent with the rest of the kernel.
struct KernelHeap {
    inner: spin::Mutex<Heap>,
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap {
    inner: spin::Mutex::new(Heap::empty()),
};

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = self.inner.lock().allocate_first_fit(layout);
        match result {
            Ok(nn) => {
                ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                nn.as_ptr()
            }
            Err(_) => {
                ALLOC_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
                core::ptr::null_mut()
            }
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
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
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
    Some(Stats {
        capacity: h.size(),
        used: h.used(),
        free: h.free(),
    })
}

/// Initialise the kernel heap. Pulls `HEAP_FRAMES` contiguous physical
/// frames, computes their linear-map VA, hands the region to
/// `linked_list_allocator`.
///
/// # Safety
///
/// Must be called exactly once, after `frame::init_from_dtb` and
/// before any code that allocates (anything in `alloc::` — `Box`,
/// `Vec`, formatted strings that need heap, etc.). The linear map
/// (set up by `mmu::enable`) must be live, since the heap lives at
/// `pa_to_kernel_va(first_frame_pa)`.
pub unsafe fn init() {
    let first = frame::alloc_contiguous(HEAP_FRAMES)
        .expect("heap init: no contiguous run of HEAP_FRAMES frames");
    let va = pa_to_kernel_va(first.addr()) as *mut u8;
    // SAFETY: `va..va+HEAP_SIZE` is HEAP_FRAMES contiguous frames just
    // reserved by `frame::alloc_contiguous`. The linear-map leaf in
    // BOOT_PT_ROOT[322] makes the VA range writable. Nothing else
    // aliases — the bitmap marked these frames in-use atomically.
    unsafe { HEAP.inner.lock().init(va, HEAP_SIZE) };
}
