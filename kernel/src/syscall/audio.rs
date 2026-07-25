//! The `AudioWrite` syscall — emit samples to the DAC, gated on an `AudioSink`
//! capability. The authority decision is the pure, host-tested
//! `kernel_proc::cap::authorize_audio`; this handler marshals the frame, copies the
//! samples out of user memory, and hands them to the kernel's PWMDAC driver (the
//! MMIO the `glitch` audio server can't do itself). See `plans/glitch.md`.

use kernel_proc::cap::{authorize_audio, Handle};
use protocol::RefusalReason;
use snitchos_abi::Syscall;

use crate::trap::TrapFrame;

/// Largest sample batch one `AudioWrite` accepts, bounding the copy buffer. The
/// `glitch` server chunks longer audio into repeated paced calls.
const MAX_SAMPLES: usize = 256;

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
