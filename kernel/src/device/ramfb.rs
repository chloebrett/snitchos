//! `ramfb` display bring-up. Allocates a contiguous physical
//! framebuffer, maps it into a dedicated VA window, and hands its
//! physical address to QEMU via the `etc/ramfb` `fw_cfg` file.
//!
//! Degrades gracefully: a machine booted without `-device ramfb` has
//! no `etc/ramfb` file, so `init` snitches a refusal and returns
//! `Err` — boot continues, the kernel just has no display.

use core::sync::atomic::{AtomicBool, Ordering};

use kernel_core::framebuffer::Framebuffer as PixelView;
use kernel_core::mmu::PtePerms;
use kernel_core::ramfb::{FOURCC_XRGB8888, RamfbCfg};

use crate::counter::DeferredCounter;
use crate::{frame, mmu};

/// Fixed mode for this milestone: 1024x768 XRGB8888, no row padding
/// (`stride == width * 4`) — exactly 3 MiB, 768 frames.
pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 768;
pub const STRIDE: usize = WIDTH * 4;
const SIZE_BYTES: usize = STRIDE * HEIGHT;
const FRAMES: usize = SIZE_BYTES / frame::FRAME_SIZE;

/// Dedicated 1 GiB VA window for the framebuffer: root PTE slot 258,
/// immediately above the kstack guard-page window (256 = heap,
/// 257 = kstack; see `kernel_core::mmu`/`kernel_core::stack`). Shared
/// across every address space for free — `new_user_root` copies root
/// slots `256..512` into every process — though only the kernel
/// touches it in this milestone.
pub const FB_VA_BASE: usize = 0xffff_ffc0_8000_0000;

pub static FRAMES_PRESENTED: DeferredCounter =
    DeferredCounter::new("snitchos.display.frames_presented_total");
pub static INIT_REFUSED: DeferredCounter =
    DeferredCounter::new("snitchos.display.init_refused_total");

/// Whether `init` succeeded — `present` is a silent no-op until this
/// is set, so a machine without `-device ramfb` just never presents.
static READY: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum InitError {
    /// No `etc/ramfb` file — QEMU wasn't given `-device ramfb`.
    NotFound,
    OutOfFrames,
    MapFailed,
    Dma(crate::fwcfg::Error),
}

/// Bring up the framebuffer: find `etc/ramfb`, allocate + map its
/// backing frames, and hand QEMU the config.
///
/// # Safety
///
/// Must run after `heap::init` (needs the frame allocator and the
/// live linear map) and after `mmu::enable`, exactly once, before any
/// other user of root PTE slot 258.
pub unsafe fn init() -> Result<(), InitError> {
    let file = match unsafe { crate::fwcfg::find_file("etc/ramfb") } {
        Some(f) => f,
        None => {
            INIT_REFUSED.inc();
            return Err(InitError::NotFound);
        }
    };

    let base_frame = frame::alloc_contiguous(FRAMES).ok_or(InitError::OutOfFrames)?;
    let perms = PtePerms::R.union(PtePerms::W).union(PtePerms::G);
    for i in 0..FRAMES {
        let va = FB_VA_BASE + i * frame::FRAME_SIZE;
        let pa = base_frame.addr() + i * frame::FRAME_SIZE;
        mmu::map(va, pa, perms).map_err(|_| InitError::MapFailed)?;
    }

    let cfg = RamfbCfg {
        addr: base_frame.addr() as u64,
        fourcc: FOURCC_XRGB8888,
        flags: 0,
        width: WIDTH as u32,
        height: HEIGHT as u32,
        stride: STRIDE as u32,
    };
    // SAFETY: `mmu::enable` has run (precondition of this function);
    // no other fwcfg operation is in flight (boot-time, single hart).
    unsafe { crate::fwcfg::write_file(file.select_key, &cfg.to_bytes()) }
        .map_err(InitError::Dma)?;

    READY.store(true, Ordering::Relaxed);
    Ok(())
}

/// Clear the framebuffer to a fixed color and bump the present
/// counter. No-op (doesn't bump the counter) until `init` has
/// succeeded. Called once per heartbeat tick.
pub fn present() {
    if !READY.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: `READY` is only set after `init` has mapped exactly
    // `[FB_VA_BASE, FB_VA_BASE + SIZE_BYTES)` R+W and handed that same
    // region's physical address to the device — nothing else maps or
    // touches this VA range, so a mutable byte slice over it is sound.
    let bytes = unsafe { core::slice::from_raw_parts_mut(FB_VA_BASE as *mut u8, SIZE_BYTES) };
    let mut fb = PixelView::new(bytes, WIDTH, HEIGHT, STRIDE);
    fb.clear(0x20_20_40);
    FRAMES_PRESENTED.inc();
}
