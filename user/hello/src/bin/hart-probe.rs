//! `workload=user-on-hart0` — the multi-hart userspace de-risk (v2b step 1).
//!
//! A trivial userspace process the kernel places on **hart 0** (userspace normally
//! runs on hart 1). It opens one span and exits. The `SpanStart` frame is emitted by
//! the `SpanOpen` syscall handler *on whichever hart is running this task*, so if this
//! ran on hart 0 the frame carries `hart_id == 0` — the proof that U-mode works on the
//! boot hart, not just hart 1. If hart-0 U-mode were broken, the span never appears.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit_with, tracer};

#[entry]
fn main() {
    // Open-and-close a distinctively named span (SpanStart + SpanEnd). The itest keys
    // on the SpanStart's `hart_id`.
    let _ = tracer().span("hart_probe.hello");
    exit_with(0);
}
