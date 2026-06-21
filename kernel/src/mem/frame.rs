//! Physical frame allocator. Wraps `kernel_core::frame::Bitmap` with
//! a global `Mutex`, the static backing storage, and the public
//! `alloc` / `free` API.
//!
//! Telemetry counters are atomics that the heartbeat thread drains;
//! the allocator never calls into the tracing path while holding its
//! own lock (would deadlock — same constraint as the IRQ handler).

use fdt::Fdt;
use kernel_core::frame::Bitmap;
use kernel_core::mmu::{pa_to_kernel_va, va_to_pa};

use crate::counter::DeferredCounter;

pub const FRAME_SIZE: usize = 4096;

/// 2 MiB — kept local to avoid coupling with `mmu.rs`'s constant.
const PAGE_2MIB: usize = 2 * 1024 * 1024;

/// Hard cap on tracked RAM. Sized for 4 GiB so the bitmap is 128 KiB
/// regardless of the actual machine. `init_from_dtb` clamps to the
/// DTB-reported size, so smaller machines are fine; only matters if
/// someone runs with more than 4 GiB.
const MAX_RAM_BYTES: usize = 4 * 1024 * 1024 * 1024;
const MAX_FRAMES: usize = MAX_RAM_BYTES / FRAME_SIZE;
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

/// Backing storage for the bitmap. Lives in `.bss` inside the kernel
/// image, so the kernel-image reservation covers it automatically.
static mut FRAME_BITS: [u64; BITMAP_WORDS] = [0u64; BITMAP_WORDS];

/// The global frame allocator. Populated by `init_from_dtb`.
static FRAME_ALLOC: crate::sync::Once<crate::sync::Mutex<Allocator>> = crate::sync::Once::new();

/// Counters drained by the heartbeat thread. Bumped outside the allocator lock
/// to keep emission off the critical path; the [`DeferredCounter`] registry
/// owns the name + drain.
///
/// [`DeferredCounter`]: crate::counter::DeferredCounter
pub static ALLOC_COUNT: DeferredCounter = DeferredCounter::new("snitchos.frames.allocated_total");
pub static FREE_COUNT: DeferredCounter = DeferredCounter::new("snitchos.frames.freed_total");
pub static ALLOC_FAIL_COUNT: DeferredCounter = DeferredCounter::new("snitchos.frames.alloc_failed_total");

#[derive(Debug)]
pub enum InitError {
    /// DTB has no `/memory` node — shouldn't happen on a valid
    /// platform.
    NoRam,
}

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Frame allocator state. The bitmap tracks frame indices; `ram_base`
/// translates between indices and physical addresses.
struct Allocator {
    bitmap: Bitmap<'static>,
    ram_base: usize,
}

impl Allocator {
    fn alloc(&mut self) -> Option<PhysFrame> {
        let idx = self.bitmap.alloc()?;
        Some(PhysFrame(self.ram_base + idx * FRAME_SIZE))
    }

    fn free(&mut self, frame: PhysFrame) {
        let idx = (frame.0 - self.ram_base) / FRAME_SIZE;
        self.bitmap.free(idx);
    }

    fn stats(&self) -> Stats {
        let total = self.bitmap.capacity();
        let free = self.bitmap.count_free();
        Stats { total, in_use: total - free, free }
    }
}

/// A physical frame handed out by the allocator. 4 KiB, page-aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PhysFrame(usize);

impl PhysFrame {
    /// Physical address of the frame.
    pub fn addr(self) -> usize {
        self.0
    }

    /// Reconstruct a frame handle from a page-aligned physical address. Used by
    /// address-space reclaim (`mmu::free_user_root`), which recovers frame PAs by
    /// walking a page table rather than holding the original [`PhysFrame`]s. The
    /// caller guarantees `pa` is a real, frame-aligned allocator frame.
    pub(crate) fn from_addr(pa: usize) -> Self {
        PhysFrame(pa)
    }

    /// Kernel-reachable VA via the linear map (`mmu::pa_to_kernel_va`).
    /// Dereferenceable as long as the linear map is installed (which
    /// it is from `mmu::enable` onward).
    pub fn kernel_va(self) -> usize {
        pa_to_kernel_va(self.0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    #[expect(
        dead_code,
        reason = "part of the frame-stats snapshot; in_use/free are surfaced today, total kept for completeness"
    )]
    pub total: usize,
    pub in_use: usize,
    pub free: usize,
}

/// Walk the DTB's `/memory` node, decide what's reserved
/// (SBI firmware below the kernel image, the kernel image itself,
/// the DTB region), and release every other frame into the free pool.
///
/// # Safety
///
/// Must be called exactly once. The DTB must be valid and the kernel
/// must be running at higher-half PC (so the `__kernel_start` /
/// `__kernel_end` symbol VAs resolve and `va_to_pa` strips
/// `KERNEL_OFFSET` to give the right physical bounds).
pub unsafe fn init_from_dtb(dtb: &Fdt, dtb_phys: usize) -> Result<(), InitError> {
    let region = dtb.memory().regions().next().ok_or(InitError::NoRam)?;
    let ram_base = region.starting_address as usize;
    let ram_size = region.size.unwrap_or(0).min(MAX_RAM_BYTES);
    let total_frames = ram_size / FRAME_SIZE;

    // Reserved physical bounds. Post-trampoline `&raw const SYMBOL` is
    // a higher-half VA; `va_to_pa` recovers the physical address.
    let kernel_start = va_to_pa((&raw const __kernel_start) as usize);
    let kernel_end = va_to_pa((&raw const __kernel_end) as usize);
    let dtb_start = dtb_phys & !(PAGE_2MIB - 1);
    let dtb_end = dtb_start + PAGE_2MIB;

    // SAFETY: `init_from_dtb` is documented to run exactly once at
    // boot; no other code touches FRAME_BITS.
    #[allow(
        clippy::deref_addrof,
        reason = "`&mut *(&raw mut STATIC)` is the required raw-pointer-to-static reference idiom; clippy's deref_addrof misreads `*(&raw mut X)` as a redundant `*&`"
    )]
    let bits: &'static mut [u64] = unsafe { &mut *(&raw mut FRAME_BITS) };
    let mut bitmap = Bitmap::new(bits, total_frames);

    // All frames start in-use. Release everything that isn't in a
    // reserved region: the SBI hole below the kernel image, the kernel
    // image itself, and the DTB.
    kernel_core::frame::release_unreserved(
        &mut bitmap,
        ram_base,
        FRAME_SIZE,
        &[(0, kernel_start), (kernel_start, kernel_end), (dtb_start, dtb_end)],
    );

    FRAME_ALLOC.call_once(|| crate::sync::Mutex::new(Allocator { bitmap, ram_base }));
    Ok(())
}

/// Allocate one physical frame. Returns `None` if no frames are free.
pub fn alloc() -> Option<PhysFrame> {
    let alloc = FRAME_ALLOC.get()?;
    let result = alloc.lock().alloc();
    match result {
        Some(frame) => {
            ALLOC_COUNT.inc();
            Some(frame)
        }
        None => {
            ALLOC_FAIL_COUNT.inc();
            None
        }
    }
}

/// Allocate a frame and zero it via the linear map. Useful for fresh
/// page tables.
pub fn alloc_zeroed() -> Option<PhysFrame> {
    let frame = alloc()?;
    // SAFETY: `kernel_va` returns a VA in the linear map, which covers
    // all of physical RAM with R/W permissions. The frame is fresh —
    // nothing else holds a reference to its bytes.
    unsafe {
        (frame.kernel_va() as *mut u8).write_bytes(0, FRAME_SIZE);
    }
    Some(frame)
}

/// Return a frame to the free pool. Double-free is idempotent
/// (`Bitmap::free` is); out-of-range frames panic (programmer error).
pub fn free(frame: PhysFrame) {
    if let Some(alloc) = FRAME_ALLOC.get() {
        alloc.lock().free(frame);
        FREE_COUNT.inc();
    }
}

/// Snapshot of the allocator's state. Briefly takes the lock — don't
/// call from inside an allocator-using critical section.
pub fn stats() -> Option<Stats> {
    Some(FRAME_ALLOC.get()?.lock().stats())
}
