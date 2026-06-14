//! Preemption guard fixture (`workload=syscall-hog`): a userspace task that is
//! uncooperative *through syscalls*. It emits a one-shot `syscall_hog.alive`
//! marker span to prove it reached U-mode, then loops issuing a cheap ambient
//! syscall (`DebugWrite`) back-to-back, with no `yield`.
//!
//! The point: each `ecall` spends the bulk of its time in S-mode with interrupts
//! masked (RISC-V clears `SIE` on trap entry; SnitchOS never re-enables it during
//! trap handling), so the loop is in S-mode almost the whole time. A naive
//! "preempt only when the timer lands in U-mode" scheme might fear this task can
//! dodge preemption — it can't: the timer can't fire mid-syscall, so it fires the
//! instant the syscall `sret`s back to U-mode (`SPP == 0`) and the quantum check
//! deschedules it. This program lets the integration suite *prove* that. crt0 /
//! panic / syscalls come from the `snitchos-user` runtime.

#![no_std]
#![no_main]

use snitchos_user::{debug_write, entry, tracer};

#[entry]
fn main() {
    // Reached U-mode. Open+close a marker span so the wire shows the hog is the
    // running U-mode task (not something wedged in the kernel).
    let _ = tracer().span("syscall_hog.alive");

    // Refuse to cooperate, but via syscalls rather than a tight compute loop.
    // `DebugWrite` is ambient (no capability needed) and does real S-mode work
    // (copy-from-user + a `Log` frame over virtio), so each iteration parks the
    // task in interrupt-masked S-mode for the bulk of its time. We never `yield`,
    // so only the timer can reclaim the CPU.
    loop {
        debug_write(b"syscall_hog: still here, still snitching\n");
    }
}
