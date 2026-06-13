//! The SnitchOS userspace runtime — crt0, panic handler, and typed
//! capability bindings shared by every U-mode program.
//!
//! A program crate depends on this, declares `#![no_std] #![no_main]`, and
//! defines a plain `#[unsafe(no_mangle)] extern "C" fn main()`. It carries no
//! `_start`, no panic handler, and no raw `ecall` — `start.S` sets up the
//! stack and tail-calls `__snitchos_start`, which inits the heap, publishes the
//! startup capabilities (delivered in `a0`/`a1`) for the [`telemetry`] /
//! [`tracer`] accessors, calls `main`, then `exit`s. The API below wraps the
//! syscall ABI and the userspace allocator.
//!
//! The API is **capability-shaped**, not POSIX-shaped: a program reaches its
//! authority through typed handles (`TelemetrySink`, `Tracer`) that the kernel
//! validates against *its own* capability table. Naming an integer is not
//! authority. (`main()` taking nothing and calling accessors for its caps is
//! the std-like shape, not ambient authority — the handles still come from the
//! kernel-granted startup set; see `docs/capability-system-design.md`.)

#![no_std]

use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use linked_list_allocator::LockedHeap;
use snitchos_abi::Syscall;

core::arch::global_asm!(include_str!("start.S"));

/// Userspace heap arena — a fixed region in `.bss` that the ELF loader maps
/// (the same way it maps the stack). 64 KiB is generous for the small programs
/// we run today; growing it on demand (a `brk`-style syscall + `Heap::extend`)
/// is a later step. Running out is a clean alloc error, not UB.
const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// The userspace global allocator: a spinlock-wrapped linked-list heap over
/// [`HEAP`]. Initialised once in [`__snitchos_start`] before any program code
/// runs. (The lock gives this `&self` static its interior mutability; it has
/// nothing to do with the heap being fixed-size.)
#[global_allocator]
static ALLOC: LockedHeap = LockedHeap::empty();

// The two startup capability handles the kernel delivers in `a0`/`a1`.
// `__snitchos_start` stores them here before calling `main`; the free
// accessors below read them — the std-like shape where `main()` takes nothing
// and you call library functions for your environment. Two atomics rather than
// a `static mut` (no reference to a mutable static; userspace is
// single-threaded so `Relaxed` suffices). Set before `main` runs; `0` is never
// read.
static STARTUP_TELEMETRY: AtomicUsize = AtomicUsize::new(0);
static STARTUP_SPAN: AtomicUsize = AtomicUsize::new(0);

/// This process's `TelemetrySink` capability (delivered at startup).
#[must_use]
pub fn telemetry() -> TelemetrySink {
    TelemetrySink::from_raw_handle(STARTUP_TELEMETRY.load(Ordering::Relaxed))
}

/// This process's `SpanSink` capability — authority to open spans.
#[must_use]
pub fn tracer() -> Tracer {
    Tracer::from_raw_handle(STARTUP_SPAN.load(Ordering::Relaxed))
}

unsafe extern "C" {
    /// The program entry, provided by each binary as
    /// `#[unsafe(no_mangle)] extern "C" fn main()`. Returns `()` — the runtime
    /// calls [`exit`] afterward, so the program never has to, and every RAII
    /// guard (e.g. a span [`Span`]) drops on return, before the process ends.
    fn main();
}

/// Runtime entry — `crt0` (`start.S`) tail-calls here with the kernel's two
/// startup handles in `a0`/`a1` (two plain scalars, no struct-in-registers
/// ABI assumption). Inits the heap, publishes the handles for the accessors,
/// runs the program, then terminates the process once `main` returns.
#[unsafe(no_mangle)]
extern "C" fn __snitchos_start(telemetry_handle: usize, span_handle: usize) -> ! {
    // SAFETY: `HEAP` is a static `.bss` arena the loader maps; this runs once,
    // before `main` (and thus before any allocation). `addr_of_mut!` avoids
    // forming a reference to the `static mut`.
    unsafe {
        ALLOC
            .lock()
            .init(core::ptr::addr_of_mut!(HEAP).cast::<u8>(), HEAP_SIZE);
    }
    STARTUP_TELEMETRY.store(telemetry_handle, Ordering::Relaxed);
    STARTUP_SPAN.store(span_handle, Ordering::Relaxed);
    // SAFETY: every program links this runtime and provides `main`.
    unsafe {
        main();
    }
    exit();
}

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

/// Voluntarily yield the CPU. We can't call the kernel's `yield_now` directly
/// (it runs on kernel stacks); instead we `ecall` `Yield` and the kernel
/// yields on our behalf, returning here on a later reschedule. The kernel
/// saves and restores our full register frame across the trap, so all
/// registers are intact on return — nothing to clobber.
pub fn yield_now() {
    // SAFETY: `ecall` traps to the kernel, which runs `yield_now()` and
    // resumes us at the instruction after the `ecall` with our frame intact.
    unsafe {
        asm!("ecall", in("a7") Syscall::Yield as usize);
    }
}

/// A capability to emit telemetry — an unforgeable handle the kernel checks
/// against this process's table. Holding the integer is not authority.
#[derive(Clone, Copy)]
pub struct TelemetrySink {
    handle: usize,
}

impl TelemetrySink {
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

/// A capability to open spans — an unforgeable handle the kernel checks
/// against this process's table, distinct from the telemetry sink.
#[derive(Clone, Copy)]
pub struct Tracer {
    handle: usize,
}

impl Tracer {
    /// Wrap a raw span-sink handle (the kernel validates it on use).
    #[must_use]
    pub const fn from_raw_handle(handle: usize) -> Self {
        Self { handle }
    }

    /// Open a span named `name`, returning an RAII [`Span`] guard that closes
    /// it on drop. The kernel validates our `SpanSink` cap, copies and interns
    /// `name`, and opens a span on this task's cursor. If the kernel refuses
    /// (bad capability, bad name pointer, or per-process span-name quota), the
    /// guard is a no-op — there's nothing to close.
    pub fn span(self, name: &str) -> Span {
        let id: usize;
        let parent: usize;
        // SAFETY: `ecall` traps to the kernel, which validates the handle,
        // copies `name` under `user_range_ok`, opens the span, and returns the
        // id in a0 and parent in a1 (a0 = `usize::MAX` on refusal). We hold the
        // `{id, parent}` as an opaque close token, exactly as the kernel's own
        // `Span` guard does.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::SpanOpen as usize,
                inlateout("a0") self.handle => id,
                inlateout("a1") name.as_ptr() as usize => parent,
                in("a2") name.len(),
            );
        }
        Span {
            token: (id != usize::MAX).then_some((id as u64, parent as u64)),
        }
    }
}

/// RAII guard for an open span: closes it on drop (the kernel emits `SpanEnd`),
/// mirroring the kernel's own `Span` guard. Holds `None` when the open was
/// refused, in which case drop is a no-op. `mem::forget`ting it leaks the span
/// (no `SpanEnd`) — a self-inflicted, observable bug, same as kernel-side.
#[must_use = "dropping the Span closes it; binding to `_` closes it immediately"]
pub struct Span {
    token: Option<(u64, u64)>,
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((id, parent)) = self.token else { return };
        // SAFETY: `ecall`; the kernel validates `id` against the cursor top and
        // emits `SpanEnd`. a0 returns a status we ignore.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::SpanClose as usize,
                inlateout("a0") id => _,
                in("a1") parent,
            );
        }
    }
}
