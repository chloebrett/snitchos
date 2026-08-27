//! The display syscall — present pixels to the screen, gated on a `DisplaySink`
//! capability. The authority decision is the pure, host-tested
//! `kernel_proc::cap::authorize_display`; this handler marshals the frame and
//! copies the pixels out of user memory.
//!
//! The display twin of [`super::audio`], and deliberately the same shape: one
//! holder (`kitsch`) mediates the single scarce scanout, clients reach it over
//! IPC by handing over cells, and nobody but the kernel touches the hardware.
//!
//! **A run, not a frame.** A 1280x720 frame is 3.6 MB — far past the per-syscall
//! user-copy cap, and there is no bulk path. So `Present` carries one horizontal
//! **run** of pixels, which is exactly what `kitsch-render`'s damage spans
//! produce. The cost profile that falls out is the right way round: a
//! damage-driven update is a few cells and a handful of calls, and only the cold
//! full-screen paint is expensive (~14.4k calls, once).
//!
//! Two ways to make the cold paint cheap when it matters, neither taken yet:
//! a display-specific larger copy bound, or mapping the framebuffer into
//! `kitsch` once memory-object capabilities exist. See `plans/kitsch-v1.md`.

use kernel_mem::mmu::MAX_USER_STR_LEN;
use kernel_proc::cap::{authorize_display, Handle};
use protocol::RefusalReason;
use snitchos_abi::Syscall;

use crate::trap::TrapFrame;

/// Largest pixel run one `Present` accepts, bounding the copy buffer. A pixel is
/// 4 bytes, so the per-syscall user-copy cap sets this at 64.
const MAX_PIXELS: usize = MAX_USER_STR_LEN / 4;

// A run of `MAX_PIXELS` must fit the per-copy byte cap, or every present would be
// refused — the same latent bug the glitch-beep itest caught for audio.
const _: () = assert!(MAX_PIXELS * 4 <= MAX_USER_STR_LEN);

/// `Present`: `a0` = `DisplaySink` handle, `a1` = ptr to `[u32]` XRGB8888 pixels
/// in the caller's memory, `a2` = pixel count, `a3` = destination x, `a4` =
/// destination y. Validates the cap, copies the run in, and blits it. Refuses a
/// non-holder or an over-long / out-of-range run — never silent.
pub(super) fn handle_present(frame: &mut TrapFrame) {
    let sc = Syscall::Present as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Authority: the caller must hold a `DisplaySink` cap (with `DISPLAY`) at
    // `a0`. A failed cap gate is a capability denial — snitch the rate as well
    // as the per-call `SyscallRefused`, as every other cap-gated syscall does.
    let denied = {
        let caps = proc.caps.lock();
        authorize_display(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        if let Some(id) = crate::user::cap_denied_metric_id() {
            crate::tracing::emit_metric(id, 1);
        }
        super::refuse(frame, sc, super::refusal_for(d));
        return;
    }

    let count = frame.a2 as usize;
    if count > MAX_PIXELS {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }
    let mut buf = [0u8; MAX_PIXELS * 4];
    let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, count * 4, &mut buf) else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };

    // Repack the copied bytes as pixels. The guest is little-endian and the
    // framebuffer is too, so this is a reinterpretation, spelled out rather than
    // transmuted because the staging buffer's alignment is only 1.
    let mut run = [0u32; MAX_PIXELS];
    for (px, chunk) in run.iter_mut().zip(bytes.chunks_exact(4)) {
        *px = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    crate::ramfb::present_span(frame.a3 as usize, frame.a4 as usize, &run[..count]);
    frame.a0 = 0;
}
