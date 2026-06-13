//! Preemption fixture (`workload=user-hog`): a deliberately uncooperative
//! userspace task. It emits a one-shot `user_hog.alive` marker span to prove it
//! reached U-mode, then runs a tight `loop {}` with **no syscalls and no
//! `yield`** — so the only way the kernel can take the CPU back is timer-driven
//! preemption (v0.8 Step 4). Co-located with a cooperative `worker_a` peer that
//! starves until preemption exists. crt0 / panic / syscalls come from the
//! `snitchos-user` runtime.

#![no_std]
#![no_main]

use snitchos_user::{entry, tracer};

#[entry]
fn main() {
    // Reached U-mode. Open+close a marker span so the wire shows the hog is the
    // running U-mode task (not something wedged in the kernel).
    let _ = tracer().span("user_hog.alive");

    // Refuse to cooperate. No `ecall` ever leaves this loop, so a cooperative
    // scheduler can never reclaim the CPU — only the timer can.
    loop {
        core::hint::spin_loop();
    }
}
