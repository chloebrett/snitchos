//! Panic handler. Bypasses `CONSOLE` / `UART` mutexes so it's usable
//! from inside fatal paths (a panic mid-`println!` would otherwise
//! deadlock on the held lock).

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::console;
use crate::uart;

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

        // Snitch the panic on the *structured* channel too, not just the UART —
        // for an observability-first kernel, its own death is the one event most
        // worth a frame. Best-effort and panic-safe (see `snitch_panic`).
        snitch_panic();
    }
    loop {
        unsafe {
            asm!("wfi");
        }
    }
}

/// Best-effort emit of a `kernel panic` telemetry frame on the virtio-console.
///
/// Panic-safe by construction, because a panic can fire from anywhere:
/// - **no alloc / no intern** — [`kernel_core::panic_log::encode`] reuses
///   `Frame::Log` (inlines its `&str`) and encodes into a fixed buffer;
/// - **no blocking** — [`virtio_console::try_send_panic`](crate::virtio_console::try_send_panic)
///   `try_lock`s the console and skips on contention (a hart may have panicked
///   *mid-send*, already holding the lock — a blocking send would deadlock);
/// - **single writer** — the `PANICKING` swap in [`panic`] admits exactly one
///   hart into the panic branch, so this is the sole writer to the static buffer.
///
/// If the console isn't up yet, or its lock is held, the frame is silently
/// dropped — the emergency-UART message already went out, so nothing is lost that
/// a human can't see.
fn snitch_panic() {
    /// `.bss`-resident so its VA translates for the device (heap VAs don't); 256 B
    /// matches the console staging buffer, with headroom for a richer message
    /// (increment 6 of `plans/panic-emits-telemetry.md`).
    static mut PANIC_FRAME_BUF: [u8; 256] = [0u8; 256];

    let task_id = crate::sched::current_task_id().0;
    let hart_id = crate::percpu::current_hartid() as u8;
    let t = crate::tracing::timestamp();

    // SAFETY: the `PANICKING` guard admits exactly one hart into the panic branch,
    // so this is the sole writer to `PANIC_FRAME_BUF` — no aliasing.
    #[allow(
        clippy::deref_addrof,
        reason = "the required &mut *(&raw mut STATIC) idiom; a direct &mut STATIC is forbidden"
    )]
    let buf = unsafe { &mut *(&raw mut PANIC_FRAME_BUF) };
    if let Some(n) = kernel_core::panic_log::encode(buf, "kernel panic", task_id, t, hart_id) {
        let _ = crate::virtio_console::try_send_panic(&buf[..n]);
    }
}
