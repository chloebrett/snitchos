//! The SnitchOS userspace runtime — crt0, panic handler, and typed
//! capability bindings shared by every U-mode program.
//!
//! A program crate depends on this, declares `#![no_std] #![no_main]`, and
//! defines a plain `#[snitchos_user::entry] fn main()`. It carries no
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

use core::alloc::Layout;
use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use snitchos_abi::Syscall;
use talc::locking::AssumeUnlockable;
use talc::{OomHandler, Span as TalcSpan, Talc, Talck};

/// Mark a program's entry function. Write `#[snitchos_user::entry] fn main()`
/// (or `use snitchos_user::entry;` then `#[entry]`); the macro supplies the
/// `#[unsafe(no_mangle)] extern "C"` decoration that [`__snitchos_start`] calls.
pub use snitchos_user_macros::entry;

core::arch::global_asm!(include_str!("start.S"));

/// Page size — must match the kernel's `FRAME_SIZE`.
const PAGE_SIZE: usize = 4096;
/// Minimum bytes to `map_anon` per growth, to amortize the syscall across many
/// small allocations rather than one map per object.
const MIN_MAP: usize = 64 * 1024;

/// Grow-on-demand hook: when `talc` can't satisfy an allocation, it calls this,
/// which `map_anon`s a fresh region (sized for the request + headroom) and
/// `claim`s it. Disjoint regions are fine — `talc` is multi-region — so the
/// kernel may place them anywhere.
struct MmapOnOom;

impl OomHandler for MmapOnOom {
    fn handle_oom(talc: &mut Talc<Self>, layout: Layout) -> Result<(), ()> {
        let size = layout.size().next_multiple_of(PAGE_SIZE) + MIN_MAP;
        let base = sys_map_anon(size);
        if base == usize::MAX {
            return Err(()); // kernel refused — out of frames / over the cap
        }
        let span = TalcSpan::new(base as *mut u8, base.wrapping_add(size) as *mut u8);
        // SAFETY: the kernel just mapped `size` bytes of fresh, exclusively-owned
        // R/W frames at `base`; the span is page-aligned and ours alone.
        unsafe { talc.claim(span) }.map(|_| ())
    }
}

/// The userspace global allocator: `talc` with the grow-on-demand OOM handler,
/// behind a no-op lock (userspace is single-threaded). Starts empty — the first
/// allocation triggers the first `map_anon`.
#[global_allocator]
static ALLOC: Talck<AssumeUnlockable, MmapOnOom> = Talck::new(Talc::new(MmapOnOom));

/// `MapAnon` syscall: ask the kernel for `bytes` of fresh anonymous memory,
/// returning the region's base VA (or `usize::MAX` if refused).
fn sys_map_anon(bytes: usize) -> usize {
    let base: usize;
    // SAFETY: `ecall` traps to the kernel, which maps `bytes` of anon R/W
    // frames and returns the base in a0.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::MapAnon as usize,
            inlateout("a0") bytes => base,
        );
    }
    base
}

// The two startup capability handles the kernel delivers in `a0`/`a1`.
// `__snitchos_start` stores them here before calling `main`; the free
// accessors below read them — the std-like shape where `main()` takes nothing
// and you call library functions for your environment. Two atomics rather than
// a `static mut` (no reference to a mutable static; userspace is
// single-threaded so `Relaxed` suffices). Set before `main` runs; `0` is never
// read.
static STARTUP_TELEMETRY: AtomicUsize = AtomicUsize::new(0);
static STARTUP_SPAN: AtomicUsize = AtomicUsize::new(0);
static STARTUP_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

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

/// This process's IPC `Endpoint` capability (delivered at startup; `0` if the
/// program was launched without one — its `send`/`receive` would be refused).
#[must_use]
pub fn endpoint() -> Endpoint {
    Endpoint::from_raw_handle(STARTUP_ENDPOINT.load(Ordering::Relaxed))
}

/// The raw handle of this process's startup `TelemetrySink` cap — for delegating
/// it to a child via [`spawn`]. (`telemetry()` wraps the same handle.)
#[must_use]
pub fn telemetry_handle() -> u32 {
    STARTUP_TELEMETRY.load(Ordering::Relaxed) as u32
}

/// The raw handle of this process's startup `SpanSink` cap — for delegating it to
/// a child via [`spawn`].
#[must_use]
pub fn span_handle() -> u32 {
    STARTUP_SPAN.load(Ordering::Relaxed) as u32
}

/// The handle at which a spawned child's `i`th delegated capability lands — the
/// startup-cap ABI (v0.11). A child is born with its two bootstrap caps at
/// handles 0 (telemetry) and 1 (span), then the parent-delegated caps in order
/// from handle 2. A spawnee reads a delegated cap via, e.g.,
/// `Tracer::from_raw_handle(delegated_handle(0))`.
#[must_use]
pub const fn delegated_handle(i: usize) -> usize {
    2 + i
}

/// Spawn program `program_id` (an index into the kernel's spawnable registry) as
/// a new process, delegating the capabilities named by `handles` (raw handle
/// values from this process's own table). Returns the child's task id, or `None`
/// if refused (unknown program, an unheld handle, or a bad pointer). The child is
/// born with bootstrap telemetry/span plus the delegated caps at handles `2..`
/// (see [`delegated_handle`]).
#[must_use]
pub fn spawn(program_id: usize, handles: &[u32]) -> Option<u32> {
    let ret: usize;
    // SAFETY: `ecall`; the kernel resolves the program + every handle against this
    // process's table (refusing the whole spawn on any miss) and returns the child
    // task id, or `usize::MAX` on refusal. `handles` is range-validated kernel-side.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::Spawn as usize,
            inlateout("a0") program_id => ret,
            in("a1") handles.as_ptr() as usize,
            in("a2") handles.len(),
        );
    }
    if ret == usize::MAX { None } else { Some(ret as u32) }
}

unsafe extern "C" {
    /// The program entry, provided by each binary's `#[entry] fn main` (the
    /// macro emits the `#[unsafe(no_mangle)] extern "C"` symbol). Returns `()` — the runtime
    /// calls [`exit`] afterward, so the program never has to, and every RAII
    /// guard (e.g. a span [`Span`]) drops on return, before the process ends.
    fn main();
}

/// Runtime entry — `crt0` (`start.S`) tail-calls here with the kernel's two
/// startup handles in `a0`/`a1` (two plain scalars, no struct-in-registers
/// ABI assumption). Inits the heap, publishes the handles for the accessors,
/// runs the program, then terminates the process once `main` returns.
#[unsafe(no_mangle)]
extern "C" fn __snitchos_start(
    telemetry_handle: usize,
    span_handle: usize,
    endpoint_handle: usize,
) -> ! {
    // The heap needs no init — `talc` is lazy; the first allocation triggers
    // its OOM handler, which `map_anon`s the first region.
    STARTUP_TELEMETRY.store(telemetry_handle, Ordering::Relaxed);
    STARTUP_SPAN.store(span_handle, Ordering::Relaxed);
    STARTUP_ENDPOINT.store(endpoint_handle, Ordering::Relaxed);
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

/// Terminate this process with exit status `0`. Never returns.
pub fn exit() -> ! {
    exit_with(0)
}

/// Terminate this process with exit status `code` (collected by a parent's
/// [`wait`]). The kernel records the status, wakes any waiting parent, and
/// switches the hart away; never returns.
pub fn exit_with(code: i32) -> ! {
    // SAFETY: `Exit` never returns to us — the kernel switches the hart away.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::Exit as usize,
            in("a0") code as usize,
            options(noreturn),
        );
    }
}

/// Block until child task `child` (a [`spawn`] return value) exits, and return
/// its exit status. If the child already exited, returns its status immediately.
#[must_use]
pub fn wait(child: u32) -> i32 {
    let ret: usize;
    // SAFETY: `ecall`; the kernel blocks us until the child exits, then returns
    // its status in `a0` (resuming us right here on a later reschedule).
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::Wait as usize,
            inlateout("a0") child as usize => ret,
        );
    }
    ret as i32
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

/// Largest single `debug_write` the kernel will copy — matches its
/// `MAX_USER_STR_LEN`. Callers (e.g. `snitchos-std`'s `println!`) must chunk to
/// this; a longer write would be refused.
pub const DEBUG_WRITE_MAX: usize = 256;

/// Write up to [`DEBUG_WRITE_MAX`] bytes to the debug/stdout channel (the
/// `DebugWrite` syscall). The kernel copies them out and emits a `Log` frame.
/// Backs `snitchos_std::println!`.
pub fn debug_write(bytes: &[u8]) {
    let _ = debug_write_raw(bytes.as_ptr() as usize, bytes.len());
}

/// Raw [`debug_write`]: issue the `DebugWrite` syscall on an arbitrary
/// `(ptr, len)` and return the kernel's status word — bytes written, or
/// `usize::MAX` if refused (e.g. a bad/unmapped pointer). Unlike [`debug_write`]
/// this takes no `&[u8]`, so it can probe the kernel's user-pointer validation
/// with a pointer that doesn't back a valid slice. Safe: the kernel validates
/// `(ptr, len)` and refuses a bad range — the bytes are never dereferenced in
/// U-mode.
#[must_use]
pub fn debug_write_raw(ptr: usize, len: usize) -> usize {
    let ret: usize;
    // SAFETY: `ecall`; the kernel range-validates `(ptr, len)` and either copies
    // + emits a `Log` frame or refuses (a0 = usize::MAX). `ptr` is never
    // dereferenced here.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::DebugWrite as usize,
            inlateout("a0") ptr => ret,
            in("a1") len,
        );
    }
    ret
}

/// Read up to `dst.len()` buffered console-input bytes into `dst`; returns how
/// many were read (`0` if nothing is buffered — non-blocking). The input mirror
/// of [`debug_write`] (the `ConsoleRead` syscall). A caller wanting a full line
/// loops, yielding between empty reads.
#[must_use]
pub fn console_read(dst: &mut [u8]) -> usize {
    let ret: usize;
    // SAFETY: `ecall`; the kernel validates the writable range and copies up to
    // `dst.len()` bytes in, returning the count (or usize::MAX on a bad range).
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::ConsoleRead as usize,
            inlateout("a0") dst.as_mut_ptr() as usize => ret,
            in("a1") dst.len(),
        );
    }
    if ret == usize::MAX { 0 } else { ret }
}

/// Write all of `bytes` to the UART terminal (the `ConsoleWrite` syscall) — the
/// output mirror of [`console_read`], sharing the one console the kernel
/// `print!`s to. The kernel refuses a single write longer than `MAX_USER_STR_LEN`,
/// so this chunks to [`DEBUG_WRITE_MAX`]. Returns the total bytes written; stops
/// early if a chunk is refused (e.g. a bad pointer). No trailing newline — the
/// caller controls layout (prompts, escape sequences).
pub fn console_write(bytes: &[u8]) -> usize {
    let mut written = 0;
    for chunk in bytes.chunks(DEBUG_WRITE_MAX) {
        let ret: usize;
        // SAFETY: `ecall`; the kernel range-validates `(ptr, len)`, copies the
        // bytes to the UART or refuses (a0 = usize::MAX). `ptr` isn't deref'd here.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::ConsoleWrite as usize,
                inlateout("a0") chunk.as_ptr() as usize => ret,
                in("a1") chunk.len(),
            );
        }
        if ret == usize::MAX {
            break;
        }
        written += ret;
    }
    written
}

/// Read the monotonic clock — the kernel tick counter (the `ClockNow` syscall),
/// at the platform timebase (10 MHz on QEMU `virt` → 1 tick = 0.1 µs). Lets a
/// program time its own work; subtract two reads for an elapsed-tick duration.
#[must_use]
pub fn clock_now() -> u64 {
    let ret: u64;
    // SAFETY: `ecall`; `ClockNow` takes no arguments and returns the tick count
    // in `a0`. No memory is touched.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::ClockNow as usize,
            out("a0") ret,
        );
    }
    ret
}

/// A capability to register + emit named metrics — an unforgeable handle the
/// kernel checks against this process's table. Holding the integer is not
/// authority. The sink itself emits nothing; it is the gate for
/// [`register_metric`](Self::register_metric) (and the free [`register_counter`]
/// / [`register_gauge`] / [`register_histogram`]), which hand back a [`Metric`]
/// to emit through.
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

    /// Register a userspace-named metric, returning a [`Metric`] to emit through.
    /// The name crosses the kernel boundary *once*, here: the kernel interns it
    /// into a fresh id and binds it to a handle in this process's own metric
    /// table. [`Metric::emit`] then carries only the handle + value, so the hot
    /// path ships no string. The process names its own metrics; the kernel learns
    /// them at registration, never ahead of time.
    ///
    /// On refusal (this `TelemetrySink` cap is invalid, the name pointer is bad,
    /// or the per-process metric quota is full) the returned `Metric` is a
    /// **no-op** — its `emit` does nothing — mirroring how [`Tracer::span`]
    /// yields an inert [`Span`]. The kernel snitches the refusal, so userspace
    /// need not branch. Most callers want the free [`register_counter`] /
    /// [`register_gauge`] / [`register_histogram`] functions, which read this
    /// process's startup `TelemetrySink` for you.
    pub fn register_metric(self, name: &str, kind: MetricKind) -> Metric {
        let handle: usize;
        // SAFETY: `ecall`; the kernel validates this `TelemetrySink` handle,
        // copies `name` under `user_range_ok`, interns it, stores it in our
        // metric table, and returns the metric handle in a0 (`usize::MAX` on
        // refusal). `name` is never dereferenced in U-mode.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::RegisterMetric as usize,
                inlateout("a0") self.handle => handle,
                in("a1") name.as_ptr() as usize,
                in("a2") name.len(),
                in("a3") kind as usize,
            );
        }
        Metric {
            handle: (handle != usize::MAX).then_some(handle),
        }
    }
}

/// Register a process-named counter through this process's startup
/// `TelemetrySink`, returning a [`Metric`] to [`emit`](Metric::emit) through.
/// The ergonomic front door to [`TelemetrySink::register_metric`]: no cap or
/// kind to name. A refused registration yields an inert `Metric` (the kernel
/// snitches it), so the call site stays a one-liner.
#[must_use]
pub fn register_counter(name: &str) -> Metric {
    telemetry().register_metric(name, MetricKind::Counter)
}

/// Register a process-named gauge — a snapshot value. See [`register_counter`].
#[must_use]
pub fn register_gauge(name: &str) -> Metric {
    telemetry().register_metric(name, MetricKind::Gauge)
}

/// Register a process-named histogram — a sample distribution. See [`register_counter`].
#[must_use]
pub fn register_histogram(name: &str) -> Metric {
    telemetry().register_metric(name, MetricKind::Histogram)
}

/// The kind of a userspace-registered metric, as the `RegisterMetric` syscall
/// carries it in `a3`. The discriminants match `protocol::MetricKind`'s order —
/// the single fact both sides agree on (the runtime stays ABI-only, with no
/// dependency on `protocol`).
#[derive(Clone, Copy)]
pub enum MetricKind {
    /// A monotonically increasing total.
    Counter = 0,
    /// A snapshot value that can go up or down.
    Gauge = 1,
    /// A distribution of samples.
    Histogram = 2,
}

/// A capability-shaped handle to a metric *this process registered* (via
/// [`register_counter`] / [`register_gauge`] / [`register_histogram`] or
/// [`TelemetrySink::register_metric`]). Emitting carries only the handle + value
/// — the kernel resolves it against this process's own metric table, so a
/// process can only emit to metrics it named, never forge another's. Holding an
/// integer is not authority: an unregistered handle is refused.
///
/// An inert `Metric` (`handle == None`) is what a refused registration returns;
/// its `emit` is a no-op, mirroring an inert [`Span`]. Cheap to hold and emit
/// through unconditionally — no `if let` at the call site.
#[derive(Clone, Copy)]
pub struct Metric {
    handle: Option<usize>,
}

impl Metric {
    /// Wrap an arbitrary raw metric handle. Naming one is free; *emitting*
    /// through it is what the kernel validates — so this is how a program
    /// reaches for a metric it may never have registered (and is refused).
    #[must_use]
    pub const fn from_raw_handle(handle: usize) -> Self {
        Self { handle: Some(handle) }
    }

    /// Emit `value` to this metric. A no-op on an inert `Metric` (a refused
    /// registration) — nothing to emit to. Otherwise fire-and-forget: the kernel
    /// resolves the handle and emits, or snitches a `SyscallRefused` if the
    /// handle names no metric this process registered (telemetry never makes the
    /// caller handle its own refusal).
    pub fn emit(self, value: i64) {
        let Some(handle) = self.handle else { return };
        // SAFETY: `ecall`; the kernel resolves the handle against our metric
        // table and emits the sample. a0 returns a status we ignore — a refused
        // emit is snitched kernel-side, not surfaced here.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::EmitMetric as usize,
                inlateout("a0") handle => _,
                in("a1") value as usize,
            );
        }
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

/// The number of inline words a single IPC message carries. Re-exported from
/// the shared [`snitchos_abi`] ABI — the single source of truth the kernel and
/// any IPC wire protocol (`fs-proto`) read from too.
pub use snitchos_abi::MSG_WORDS;

/// A capability to a synchronous IPC endpoint. `send` and `receive` are
/// rendezvous operations — each blocks until a peer arrives. Which ops are
/// permitted depends on the rights the kernel granted (`SEND`/`RECV`); holding
/// the integer is not authority, the kernel validates on every call.
/// Capability rights bits (`rights::SEND`, …) — re-exported from the shared
/// [`snitchos_abi::rights`] ABI so a program stamps minted caps from the same
/// source of truth the kernel reads. Pass these to [`Endpoint::mint_badged`].
pub use snitchos_abi::rights;

#[derive(Clone, Copy)]
pub struct Endpoint {
    handle: usize,
}

/// What [`Endpoint::receive_with_reply`] hands back: the message words, the
/// reply handle (`Some` if it came from a `call` — answer it with [`reply`]),
/// and the **sender cap's badge** (`0` = a bare endpoint) — the unforgeable
/// demux value a server uses to tell its objects/clients apart (v0.9c).
pub struct Received {
    pub msg: [u64; MSG_WORDS],
    pub reply: Option<usize>,
    pub badge: u64,
}

impl Endpoint {
    /// Wrap a raw endpoint handle (the kernel validates it on use).
    #[must_use]
    pub const fn from_raw_handle(handle: usize) -> Self {
        Self { handle }
    }

    /// Send an inline message, blocking until a receiver rendezvouses.
    /// `Err(Denied)` if the kernel refused the capability (no `SEND`, or not an
    /// endpoint handle).
    pub fn send(self, msg: [u64; MSG_WORDS]) -> Result<(), Denied> {
        let ret: usize;
        // SAFETY: `ecall` traps to the kernel, which validates the handle
        // (needs `SEND`), copies the four words, and rendezvouses with a
        // receiver (blocking us until one arrives). a0 returns 0 on success.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::Send as usize,
                inlateout("a0") self.handle => ret,
                in("a1") msg[0],
                in("a2") msg[1],
                in("a3") msg[2],
                in("a4") msg[3],
            );
        }
        if ret == 0 { Ok(()) } else { Err(Denied) }
    }

    /// Mint a badged `SEND` capability for this endpoint, returning its raw
    /// handle. Requires this endpoint cap to carry `MINT` (a server owner cap);
    /// `Err(Denied)` if the kernel refused. The minted cap names the same
    /// endpoint, stamped with `badge` (the server's demux value) + `rights`
    /// (e.g. [`rights::SEND`]) — hand it to a client so its messages arrive
    /// badged. The cap lands in *this* process's table for now.
    pub fn mint_badged(self, badge: u64, rights: u32) -> Result<usize, Denied> {
        let ret: usize;
        // SAFETY: `ecall` traps to the kernel, which validates `MINT` on the
        // handle, derives the badged child, inserts it into our table, and
        // returns its handle in a0 (or usize::MAX if refused).
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::MintBadged as usize,
                inlateout("a0") self.handle => ret,
                in("a1") badge,
                in("a2") rights as usize,
            );
        }
        if ret == usize::MAX { Err(Denied) } else { Ok(ret) }
    }

    /// Receive an inline message, blocking until a sender rendezvouses.
    /// Returns the four words, or `Err(Denied)` if the kernel refused the
    /// capability (no `RECV`, or not an endpoint handle).
    pub fn receive(self) -> Result<[u64; MSG_WORDS], Denied> {
        let ret: usize;
        let w0: u64;
        let w1: u64;
        let w2: u64;
        let w3: u64;
        // SAFETY: `ecall` traps to the kernel, which validates the handle
        // (needs `RECV`), blocks us until a sender rendezvouses, then writes
        // status into a0 and the four message words into a1..=a4.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::Receive as usize,
                inlateout("a0") self.handle => ret,
                out("a1") w0,
                out("a2") w1,
                out("a3") w2,
                out("a4") w3,
            );
        }
        if ret == 0 { Ok([w0, w1, w2, w3]) } else { Err(Denied) }
    }

    /// RPC `call`: send a request and **block until the server replies**,
    /// returning the response words **and** any capability the server
    /// transferred back (`Some(handle)` — e.g. a badged endpoint cap from
    /// `reply_with_cap`; `None` for a plain reply). The caller's open span stays
    /// open across the round-trip, so the server's handling span nests under it.
    /// `Err(Denied)` if the kernel refused the capability.
    pub fn call(self, msg: [u64; MSG_WORDS]) -> Result<([u64; MSG_WORDS], Option<usize>), Denied> {
        let ret: usize;
        let r0: u64;
        let r1: u64;
        let r2: u64;
        let r3: u64;
        let cap: usize;
        // SAFETY: `ecall`; the kernel validates the handle (needs `SEND`),
        // delivers the request, mints a reply cap into the server, parks us
        // until `reply`, then writes status in a0, the response in a1..=a4, and
        // any transferred cap handle in a5 (0 = none).
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::Call as usize,
                inlateout("a0") self.handle => ret,
                inlateout("a1") msg[0] => r0,
                inlateout("a2") msg[1] => r1,
                inlateout("a3") msg[2] => r2,
                inlateout("a4") msg[3] => r3,
                out("a5") cap,
            );
        }
        if ret != 0 {
            return Err(Denied);
        }
        Ok(([r0, r1, r2, r3], (cap != 0).then_some(cap)))
    }

    /// Receive a message **and** the reply handle: `Some(handle)` if it came
    /// from a `call` (answer it with [`reply`]), `None` for a one-way `send`.
    /// The RPC server's receive primitive.
    pub fn receive_with_reply(self) -> Result<Received, Denied> {
        let ret: usize;
        let w0: u64;
        let w1: u64;
        let w2: u64;
        let w3: u64;
        let reply_handle: usize;
        let badge: u64;
        // SAFETY: as `receive`, plus the kernel returns the reply-cap handle in
        // a5 (0 = one-way `send`) and the sender cap's badge in a6 (0 = bare).
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::Receive as usize,
                inlateout("a0") self.handle => ret,
                out("a1") w0,
                out("a2") w1,
                out("a3") w2,
                out("a4") w3,
                out("a5") reply_handle,
                out("a6") badge,
            );
        }
        if ret != 0 {
            return Err(Denied);
        }
        Ok(Received {
            msg: [w0, w1, w2, w3],
            reply: (reply_handle != 0).then_some(reply_handle),
            badge,
        })
    }

    /// Fused reply-then-receive — the RPC server hot path. Answers the previous
    /// request (`prev = Some((reply_handle, response))`; `None` on the first
    /// iteration) and blocks for the next request in one syscall, returning it as
    /// a [`Received`] — message, reply handle, **and the sender cap's badge**
    /// (the receive half is exactly [`receive_with_reply`](Self::receive_with_reply),
    /// so it carries the same demux value). The canonical loop:
    /// `let mut prev = None; loop { let r = ep.reply_recv(prev)?; prev = r.reply.map(|h| (h, handle(r))); }`.
    pub fn reply_recv(self, prev: Option<(usize, [u64; MSG_WORDS])>) -> Result<Received, Denied> {
        let (prev_handle, resp) = prev.map_or((0, [0u64; MSG_WORDS]), |(h, r)| (h, r));
        let status: usize;
        let w0: u64;
        let w1: u64;
        let w2: u64;
        let w3: u64;
        let next_handle: usize;
        let badge: u64;
        // SAFETY: `ecall`; a0=endpoint→status, a1..=a4=response→next request,
        // a5=prev reply handle→next reply handle, a6→sender badge (0 = bare). The
        // kernel replies the previous caller (if `prev_handle != 0`) then blocks
        // receiving.
        unsafe {
            asm!(
                "ecall",
                in("a7") Syscall::ReplyRecv as usize,
                inlateout("a0") self.handle => status,
                inlateout("a1") resp[0] => w0,
                inlateout("a2") resp[1] => w1,
                inlateout("a3") resp[2] => w2,
                inlateout("a4") resp[3] => w3,
                inlateout("a5") prev_handle => next_handle,
                out("a6") badge,
            );
        }
        if status != 0 {
            return Err(Denied);
        }
        Ok(Received {
            msg: [w0, w1, w2, w3],
            reply: (next_handle != 0).then_some(next_handle),
            badge,
        })
    }
}

/// Answer an RPC: send `msg` back through a `reply_handle` obtained from
/// [`Endpoint::receive_with_reply`]. Wakes the blocked caller and consumes the
/// one-shot reply cap. `Err(Denied)` if the handle is not a live reply cap.
pub fn reply(reply_handle: usize, msg: [u64; MSG_WORDS]) -> Result<(), Denied> {
    reply_inner(reply_handle, msg, 0)
}

/// Answer an RPC **and transfer a capability** to the caller (v0.9c): `cap` is a
/// handle in *this* process's table (e.g. from [`Endpoint::mint_badged`]); the
/// kernel moves it into the caller's table, and the caller's `call` returns its
/// new handle. This is how a server hands out badged endpoint caps.
pub fn reply_with_cap(reply_handle: usize, msg: [u64; MSG_WORDS], cap: usize) -> Result<(), Denied> {
    reply_inner(reply_handle, msg, cap)
}

/// Shared `reply` body. `transfer` is a cap handle to hand the caller (`0` =
/// none) — always written to `a6` so the kernel never reads a stale register.
fn reply_inner(reply_handle: usize, msg: [u64; MSG_WORDS], transfer: usize) -> Result<(), Denied> {
    let ret: usize;
    // SAFETY: `ecall`; the kernel resolves + consumes the reply cap, optionally
    // moves the `a6` cap to the caller, stashes the response, and wakes the
    // caller. a0 returns 0 on success.
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::Reply as usize,
            inlateout("a0") reply_handle => ret,
            in("a1") msg[0],
            in("a2") msg[1],
            in("a3") msg[2],
            in("a4") msg[3],
            in("a6") transfer,
        );
    }
    if ret == 0 { Ok(()) } else { Err(Denied) }
}

/// Copy `len` bytes from a blocked caller's memory (`src_va`, in *their* address
/// space) into this server's buffer at `dst_va` (option D, v0.10). `reply_handle`
/// is the one-shot reply cap naming the caller — borrowed (not consumed), so the
/// server may copy as many times as it needs before its final `reply`. Returns
/// the bytes copied, or `Err(Denied)` if the kernel refused (bad cap / pointer /
/// range). The `write`/`create`-name half of the cross-AS copy.
pub fn copy_from_caller(
    reply_handle: usize,
    src_va: usize,
    len: usize,
    dst_va: usize,
) -> Result<usize, Denied> {
    let ret: usize;
    // SAFETY: `ecall`; the kernel resolves the reply cap → the caller's address
    // space, validates both ranges, copies, and returns bytes copied in a0 (or
    // usize::MAX on refusal).
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::CopyFromCaller as usize,
            inlateout("a0") reply_handle => ret,
            in("a1") src_va,
            in("a2") len,
            in("a3") dst_va,
        );
    }
    if ret == usize::MAX { Err(Denied) } else { Ok(ret) }
}

/// Copy `len` bytes from this server's buffer (`src_va`, in *our* space) into a
/// blocked caller's memory at `dst_va` (in *their* space) — the mirror of
/// [`copy_from_caller`], the `read` half. `reply_handle` names + authorizes the
/// caller (borrowed). Returns bytes copied, or `Err(Denied)` if refused.
pub fn copy_to_caller(
    reply_handle: usize,
    src_va: usize,
    len: usize,
    dst_va: usize,
) -> Result<usize, Denied> {
    let ret: usize;
    // SAFETY: `ecall`; the kernel resolves the reply cap → the caller's address
    // space, validates both ranges, copies, and returns bytes copied in a0 (or
    // usize::MAX on refusal).
    unsafe {
        asm!(
            "ecall",
            in("a7") Syscall::CopyToCaller as usize,
            inlateout("a0") reply_handle => ret,
            in("a1") src_va,
            in("a2") len,
            in("a3") dst_va,
        );
    }
    if ret == usize::MAX { Err(Denied) } else { Ok(ret) }
}
