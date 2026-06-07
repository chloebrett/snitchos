//! Panic handler and the lock-free `tag` UART helper. Both bypass
//! `CONSOLE` / `UART` mutexes so they're usable from inside fatal
//! paths (panic) and forensic instrumentation (trap exit).

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::console;
use crate::uart;

/// DEFLAKE: lock-free single-line UART tag. Originally used to bisect
/// where the kernel dies during the Bug B silent reset; now also the
/// trap-return MMIO fence keeping the residual cross-hart race
/// suppressed. SAFETY: bypasses CONSOLE/UART mutex, so output may
/// interleave with concurrent prints — accepted for forensic
/// instrumentation. `emergency_uart_base` reads satp so this works
/// in any boot stage.
pub(crate) fn tag(s: &str) {
    use core::fmt::Write;
    let mut uart = unsafe { uart::Uart16550::new(console::emergency_uart_base()) };
    let _ = writeln!(&mut uart, "[TAG] {s}");
}

/// Recursion guard for the panic handler. Set on entry; if already
/// set, we must already be panicking and shouldn't try to print again
/// (formatting the panic info could itself panic, leading to infinite
/// recursion).
///
/// `Relaxed` on the `swap`: the guard prevents *re-entry on this same
/// hart* (formatting that itself panics). The atomic value is the
/// whole signal; no payload to publish. SMP later:
/// `scaling-corners.md` documents "any hart panics → whole system
/// panics" as the v0.1 contract — when fault isolation lands this
/// will become a per-hart guard.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// Panic handler. Bypasses the UART mutex to avoid deadlocking if a
/// panic fires mid-`println!` (the lock would already be held by the
/// outer caller). Uses a recursion guard so a panic-during-panic
/// doesn't infinite-loop.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if !PANICKING.swap(true, Ordering::Relaxed) {
        // First time through. Print directly to a fresh UART, no
        // lock.
        //
        // SAFETY: bypassing the lock means we may interleave with
        // whatever was printing when the panic fired — accepted
        // because we're already in a fatal state.
        // `emergency_uart_base` reads satp so this works in any boot
        // stage (MMU off, identity-MMIO mapped, or
        // higher-half-only).
        use core::fmt::Write;
        let mut uart = unsafe { uart::Uart16550::new(console::emergency_uart_base()) };
        let _ = writeln!(&mut uart, "Kernel panic: {}", info);
    }
    loop {
        unsafe {
            asm!("wfi");
        }
    }
}
