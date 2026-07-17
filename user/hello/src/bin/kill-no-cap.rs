//! `workload=kill-no-cap` — the supervision v2a negative: `Kill` needs the cap.
//!
//! This process holds only its bootstrap caps (telemetry, span) — no
//! `Object::Process` capability. It tries to `Kill` through a handle it does not
//! hold, and the kernel must **refuse** (`SyscallRefused{Kill}`) rather than end
//! anything. The refusal is what proves killing is a real, cap-gated authority — not
//! ambient. The process survives the refusal and reports it (`killnocap.refused`);
//! if the kill were ever *allowed*, it would report `killnocap.allowed` instead (a
//! bug the itest would catch).

#![no_std]
#![no_main]

use snitchos_user::{entry, exit_with, kill, register_counter};

#[entry]
fn main() {
    // Handle 99 names no cap in our table — we hold no Process/KILL cap at all — so
    // the kernel refuses (`CapNotFound`). A refusal is the expected, correct outcome.
    match kill(99) {
        Err(_) => register_counter("snitchos.killnocap.refused").emit(1),
        Ok(()) => register_counter("snitchos.killnocap.allowed").emit(1),
    }
    exit_with(0);
}
