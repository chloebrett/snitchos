//! The audio syscalls — emit samples to the DAC, gated on an `AudioSink` capability.
//! The authority decision is the pure, host-tested `kernel_proc::cap::authorize_audio`;
//! these handlers marshal the frame and copy the samples out of user memory.
//!
//! Two paths share the cap gate and the copy:
//! - [`handle_audio_write`] (`AudioWrite`, v1) — **blocking**: hands the samples
//!   straight to the kernel's PWMDAC driver, which paces them to `WDATA` and returns
//!   only when the play is done. Fine for one client; the whole play blocks the caller.
//! - [`handle_audio_enqueue`] (`AudioEnqueue`, v2) — **non-blocking**: pushes the
//!   samples into the async DAC ring and returns the accepted count immediately, so a
//!   near-full ring back-pressures the caller. A timer drain feeds the DAC from the
//!   ring (v2 Increment 4). This is the path mixing / sonifier / modem build on.
//!
//! See `plans/glitch.md` and `plans/glitch-v2-async-ring.md`.

use kernel_mem::mmu::MAX_USER_STR_LEN;
use kernel_proc::cap::{authorize_audio, Handle};
use protocol::RefusalReason;
use snitchos_abi::Syscall;

use crate::trap::TrapFrame;

/// Largest sample batch one `AudioWrite` accepts, bounding the copy buffer. Capped
/// by the kernel's per-syscall user-copy limit (`MAX_USER_STR_LEN` bytes): a sample
/// is 2 bytes, so at most `MAX_USER_STR_LEN / 2` samples cross in one call — a
/// larger batch is refused by `copy_from_user` as an over-long range. The `glitch`
/// server chunks longer audio into repeated paced calls (`AUDIO_WRITE_MAX`).
const MAX_SAMPLES: usize = MAX_USER_STR_LEN / 2;

// A batch of `MAX_SAMPLES` i16s must fit the per-copy byte cap, or every write
// would be refused (the bug the glitch-beep itest caught).
const _: () = assert!(MAX_SAMPLES * 2 <= MAX_USER_STR_LEN);

/// `AudioWrite`: `a0` = `AudioSink` handle, `a1` = ptr to `[i16]` samples in the
/// caller's memory, `a2` = sample count. Validates the cap, copies the samples in,
/// and plays them (the kernel owns the DAC MMIO + pacing). Refuses a non-holder or
/// an over-long / out-of-range batch — never silent.
pub(super) fn handle_audio_write(frame: &mut TrapFrame) {
    let sc = Syscall::AudioWrite as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Authority: the caller must hold an `AudioSink` cap (with `AUDIO`) at `a0`.
    // A failed cap gate is a capability denial — snitch the rate as well as the
    // per-call `SyscallRefused`, exactly as the other cap-gated syscalls do.
    let denied = {
        let caps = proc.caps.lock();
        authorize_audio(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        if let Some(id) = crate::user::cap_denied_metric_id() {
            crate::tracing::emit_metric(id, 1);
        }
        super::refuse(frame, sc, super::refusal_for(d));
        return;
    }

    let count = frame.a2 as usize;
    if count > MAX_SAMPLES {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }
    let byte_len = count * 2;
    let mut buf = [0u8; MAX_SAMPLES * 2];
    let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, byte_len, &mut buf) else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    crate::pwmdac::play_samples(bytes);
    frame.a0 = 0;
}

/// `AudioEnqueue`: `a0` = `AudioSink` handle, `a1` = ptr to `[i16]` samples, `a2` =
/// sample count. Validates the cap, copies the samples in, and pushes them into the
/// async DAC ring **without blocking** — returning the accepted count in `a0` (fewer
/// than offered when the ring is near full, so the caller re-submits the tail). The
/// timer drain feeds the DAC. Refuses a non-holder or an over-long / out-of-range
/// batch — never silent.
pub(super) fn handle_audio_enqueue(frame: &mut TrapFrame) {
    let sc = Syscall::AudioEnqueue as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let denied = {
        let caps = proc.caps.lock();
        authorize_audio(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        if let Some(id) = crate::user::cap_denied_metric_id() {
            crate::tracing::emit_metric(id, 1);
        }
        super::refuse(frame, sc, super::refusal_for(d));
        return;
    }

    let count = frame.a2 as usize;
    if count > MAX_SAMPLES {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }
    let byte_len = count * 2;
    let mut buf = [0u8; MAX_SAMPLES * 2];
    let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, byte_len, &mut buf) else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let mut samples = [0i16; MAX_SAMPLES];
    for (slot, chunk) in samples.iter_mut().zip(bytes.chunks_exact(2)) {
        *slot = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    frame.a0 = crate::pwmdac::enqueue(&samples[..count]) as u64;
}
