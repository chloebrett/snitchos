//! Debug-channel syscall: `DebugWrite` — emit a `Log` frame for U-mode.
//! Ambient (writing to the debug log is not an authority, like `Yield`).

use crate::trap::TrapFrame;

/// Write bytes to the debug/stdout channel for U-mode. `a0` = pointer, `a1` =
/// length. Copies the bytes out (range-validated, SUM-guarded) and emits a
/// snitched `Log` frame attributed to the caller. Returns bytes written in
/// `a0` (or `u64::MAX` on a bad pointer). Ungated — writing to the debug log is
/// not an authority, like `Yield`. The runtime chunks writes to fit
/// `MAX_USER_STR_LEN`; a longer write becomes several `Log` frames.
pub(super) fn handle_debug_write(frame: &mut TrapFrame) {
    use kernel_mem::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::DebugWrite as u8;
    let mut buf = [0u8; MAX_USER_STR_LEN];
    let Some(bytes) = crate::user::copy_from_user(frame.a0 as usize, frame.a1 as usize, &mut buf)
    else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let Ok(msg) = core::str::from_utf8(bytes) else {
        super::refuse(frame, sc, RefusalReason::BadUtf8);
        return;
    };
    crate::tracing::emit_log(msg);
    frame.a0 = bytes.len() as u64;
}
