//! The SnitchOS userspace runtime — crt0, panic handler, and typed
//! capability bindings shared by every U-mode program.
//!
//! A program crate depends on this, declares `#![no_std] #![no_main]`, and
//! defines a single `#[no_mangle] extern "C" fn rust_main() -> !`. It carries
//! no `_start`, no panic handler, and no raw `ecall` — `start.S` sets up the
//! stack and calls `rust_main`, and the API below wraps the syscall ABI.
//!
//! The API is **capability-shaped**, not POSIX-shaped: a program holds typed
//! handles (`TelemetrySink`) that the kernel validates against *its own*
//! capability table. Naming an integer is not authority. (v0.7b: the
//! bootstrap handle is well-known; v0.8 delivers the initial capability set
//! at startup — see `docs/capability-system-design.md`.)

#![no_std]

use core::arch::asm;

use snitchos_abi::{Syscall, TELEMETRY_SINK_HANDLE};

core::arch::global_asm!(include_str!("start.S"));

/// Minimal panic handler: a U-mode program has nowhere to report to yet, so
/// spin. (A future `Exit`-with-status, or a debug-console-write capability,
/// could surface the panic instead.)
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// The kernel refused a capability invocation — the handle named no
/// capability in our table, or the capability lacked the required right.
/// (Userspace only learns *that* it was denied, not the kernel's reason.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denied;

/// Invoke a capability by raw handle. `Ok` if the kernel performed the
/// operation, `Err(Denied)` if it refused.
fn invoke(handle: usize, arg: usize) -> Result<(), Denied> {
    let ret: usize;
    // SAFETY: `ecall` traps to the kernel, which reads a7/a0/a1, validates
    // the handle against our `CapTable`, performs the op, and returns the
    // result in a0 (0 = ok).
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::Invoke as usize,
            inlateout("a0") handle => ret,
            in("a1") arg,
        );
    }
    if ret == 0 { Ok(()) } else { Err(Denied) }
}

/// Terminate this process. The kernel marks us exited and switches the hart
/// to its next task; never returns.
pub fn exit() -> ! {
    // SAFETY: `Exit` never returns to us — the kernel switches the hart away.
    unsafe {
        asm!("ecall", in("a7") Syscall::Exit as usize, options(noreturn));
    }
}

/// A capability to emit telemetry — an unforgeable handle the kernel checks
/// against this process's table. Holding the integer is not authority.
#[derive(Clone, Copy)]
pub struct TelemetrySink {
    handle: usize,
}

impl TelemetrySink {
    /// The bootstrap sink every process is granted at startup. (v0.7b: a
    /// well-known handle; v0.8: handed over via the startup capability set.)
    #[must_use]
    pub const fn bootstrap() -> Self {
        Self { handle: TELEMETRY_SINK_HANDLE }
    }

    /// Wrap an arbitrary raw handle. Naming a handle is free; *using* it is
    /// what the kernel validates — so this is how a program reaches for
    /// authority (and is refused, if it was never granted that handle).
    #[must_use]
    pub const fn from_raw_handle(handle: usize) -> Self {
        Self { handle }
    }

    /// Emit `value` to the sink. `Ok` if the kernel accepted it,
    /// `Err(Denied)` if it refused the invocation.
    pub fn emit(self, value: i64) -> Result<(), Denied> {
        invoke(self.handle, value as usize)
    }
}
