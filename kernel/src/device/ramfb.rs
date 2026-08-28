//! `ramfb` display bring-up. Allocates a contiguous physical
//! framebuffer, maps it into a dedicated VA window, and hands its
//! physical address to QEMU via the `etc/ramfb` `fw_cfg` file.
//!
//! Degrades gracefully: a machine booted without `-device ramfb` has
//! no `etc/ramfb` file, so `init` snitches a refusal and returns
//! `Err` — boot continues, the kernel just has no display.

use core::sync::atomic::{AtomicBool, Ordering};

use kernel_devices::framebuffer::Framebuffer as PixelView;
use kernel_mem::mmu::PtePerms;
use kernel_devices::ramfb::{FOURCC_XRGB8888, RamfbCfg};

use crate::counter::DeferredCounter;
use crate::{frame, mmu};

/// Fixed mode: 1280x720 XRGB8888, no row padding (`stride == width * 4`) —
/// exactly 3,600 KiB, 900 frames.
///
/// 720p rather than the 1024x768 this milestone first pinned, for three
/// reasons: it is a standard CEA mode a real HDMI sink will accept (the VF2
/// display driver has to output *something* real); its bytes divide evenly
/// into frames; and at the 8x16 cell grid `kitsch` composites into (see
/// `docs/kitsch-design.md`) 1280 gives **160 columns — exactly two 80-column
/// panes**, where 1024's 128 splits into a cramped 64+64.
/// `docs/framebuffer-design.md` pinned the original mode and said to revisit it.
pub const WIDTH: usize = 1280;
pub const HEIGHT: usize = 720;
pub const STRIDE: usize = WIDTH * 4;
const SIZE_BYTES: usize = STRIDE * HEIGHT;
const FRAMES: usize = kernel_devices::ramfb::frames_needed(HEIGHT, STRIDE, frame::FRAME_SIZE);

/// Dedicated 1 GiB VA window for the framebuffer: root PTE slot 258,
/// immediately above the kstack guard-page window (256 = heap,
/// 257 = kstack; see `kernel_mem::mmu`/`kernel_proc::stack`). Shared
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

/// Set once a `DisplaySink` capability has been granted to userspace: somebody
/// out there owns the screen now.
///
/// The heartbeat's milestone-0 [`present`] clears the *whole* framebuffer every
/// tick. That was fine when the kernel was the only thing that could draw; the
/// moment a userspace compositor exists it is a tick-rate eraser fighting it for
/// the glass. Rather than delete the demo clear (it is still the proof that
/// `ramfb::init` worked on a machine with no compositor), it yields: the kernel
/// draws only while nothing else claims the display.
///
/// Found by asserting on pixels — the screen showed one correct row and two
/// clobbered ones, which no telemetry assertion would ever have noticed.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Record that userspace holds the display. Called where the `DisplaySink` cap
/// is minted; one-way, because a compositor that exits leaves its last frame up
/// rather than handing the screen back to a clear loop.
pub fn claim() {
    CLAIMED.store(true, Ordering::Relaxed);
}

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
    let Some(file) = (unsafe { crate::fwcfg::find_file("etc/ramfb") }) else {
        INIT_REFUSED.inc();
        return Err(InitError::NotFound);
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
    if !READY.load(Ordering::Relaxed) || CLAIMED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: `READY` is only set after `init` has mapped exactly
    // `[FB_VA_BASE, FB_VA_BASE + SIZE_BYTES)` R+W and handed that same
    // region's physical address to the device — nothing else maps or
    // touches this VA range, so a mutable slice over it is sound. The
    // `u32` element type is sound too: `FB_VA_BASE` is frame-aligned (it
    // is a page-granular mapping) and `SIZE_BYTES` is `STRIDE * HEIGHT`
    // with `STRIDE = WIDTH * 4`, so the region is a whole number of
    // 4-byte-aligned pixels. That alignment is what lets `fill_rect`
    // compile to one `sw` per pixel instead of four `sb`.
    let pixels =
        unsafe { core::slice::from_raw_parts_mut(FB_VA_BASE as *mut u32, SIZE_BYTES / 4) };
    let mut fb = PixelView::new(pixels, WIDTH, HEIGHT, STRIDE / 4);
    fb.clear(0x20_20_40);
    FRAMES_PRESENTED.inc();
}

/// Copy one horizontal run of already-packed XRGB8888 pixels to `(x, y)`.
///
/// The `Present` syscall's back end: `kitsch` rasterizes a damage span and hands
/// the pixels over, the kernel copies them in. **The copy is the design, not an
/// inefficiency** — a client buffer scanned out directly could not be greyscaled
/// or dimmed by the compositor (`docs/kitsch-design.md` §4).
///
/// Clips to the framebuffer and, critically, to the **end of the row**: a run is
/// horizontal, so a too-long one is truncated rather than wrapped into the next
/// line, which would look like corruption rather than a bug. Silent no-op until
/// `init` has succeeded.
/// Returns `false` when there is no framebuffer to present to, so the caller can
/// **refuse** rather than report a success that reached nothing.
pub fn present_span(x: usize, y: usize, run: &[u32]) -> bool {
    if !READY.load(Ordering::Relaxed) {
        return false;
    }
    if y >= HEIGHT || x >= WIDTH {
        // Off-screen is a clip, not a failure: the geometry was valid, there is
        // simply nothing of it on the glass.
        return true;
    }
    let n = run.len().min(WIDTH - x);
    // SAFETY: identical to `present` — `READY` is only set after `init` mapped
    // exactly this region R+W as a whole number of 4-byte-aligned pixels, and
    // nothing else touches the VA range.
    let pixels =
        unsafe { core::slice::from_raw_parts_mut(FB_VA_BASE as *mut u32, SIZE_BYTES / 4) };
    let start = y * (STRIDE / 4) + x;
    pixels[start..start + n].copy_from_slice(&run[..n]);
    FRAMES_PRESENTED.inc();
    true
}
