//! A v0.7b userspace process: an address space plus the capabilities it
//! was granted.
//!
//! In v0.7b a "process" is one thread + one address space + one root
//! capability (the `TelemetrySink`). The full process object — multiple
//! threads, transferable caps, a real `init` grant graph — grows here in
//! v0.8. The capability *machinery* is pure and host-tested in
//! [`kernel_core::cap`]; this module only decides *where the table lives*
//! and grants the bootstrap capability. See `plans/v0.7b-capabilities.md`.

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use kernel_core::cap::{CapTable, Handle};
use protocol::StringId;

use crate::percpu::{MAX_HARTS, PerCpu};
use crate::sync::Mutex;

/// The process running on each hart, so the syscall trap handler can reach
/// its [`CapTable`] to validate an invocation. Mirrors
/// `sched::CURRENT_SPAN_CURSOR`. Set by `user::run` before the `sret` into
/// U-mode; read in `trap::handle_user_ecall`.
///
/// `Relaxed`: a per-CPU pointer whose only reader is the syscall trap on
/// the *same* hart that stored it (trap-return synchronises by hardware).
/// The pointed-at `Process` lives in `run`'s frame, which never returns.
pub static CURRENT_PROCESS: PerCpu<AtomicPtr<Process>> =
    PerCpu::new([const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_HARTS]);

/// Monotonic source of **global** capability ids for `CapEvent` frames.
/// Distinct from the per-process [`Handle`]: a handle is local and
/// ambiguous across processes, but the host needs a stable global id to
/// thread the derivation tree. Starts at 1 so `0` is the "root / no parent"
/// sentinel in `CapEvent.parent_cap_id`. `Relaxed`: a unique-id counter
/// needs atomicity, not ordering.
static NEXT_CAP_ID: AtomicU64 = AtomicU64::new(1);

/// Mint the next global capability id (for a `CapEvent`).
pub fn next_cap_id() -> u64 {
    NEXT_CAP_ID.fetch_add(1, Ordering::Relaxed)
}

/// One userspace process. Owns its address space (`root_pa`) and its
/// capability table.
pub struct Process {
    /// Physical address of this process's root page table — its address
    /// space (built by `mmu::new_user_root`).
    pub root_pa: usize,

    /// The capabilities this process holds. Behind a [`Mutex`] from day
    /// one — even though v0.7b runs one thread per process — so grant and
    /// (future) revoke are multi-hart-correct when v0.8 adds a second
    /// process. **Never held across `sret`/`yield_now`** (the cooperative
    /// lock discipline). Read by `trap::handle_user_ecall` via
    /// [`CURRENT_PROCESS`] to validate a capability invocation.
    pub caps: Mutex<CapTable>,

    /// Count of **distinct span names this process has introduced** into the
    /// global intern table. Bounds the per-process contribution to the
    /// permanent `Box::leak` of each new span name (and the table's growth):
    /// once it reaches [`Process::MAX_SPAN_NAMES`] a span-open with a new name
    /// is refused. Repeats and names another process already registered cost
    /// nothing (they resolve via `lookup_by_content`). Incremented under the
    /// intern lock at the registration site, so the check-and-bump is precise.
    pub span_names_registered: AtomicU32,

    /// Top of this process's growable heap region (the next VA the `Sbrk`
    /// syscall will map). Starts at [`Process::HEAP_BASE`]; the runtime's
    /// allocator grows it on demand, capped at `HEAP_BASE + HEAP_MAX`. The
    /// process runs on one hart at a time, so the atomic is just for `&self`
    /// access; `Relaxed` suffices.
    pub heap_top: AtomicUsize,
}

impl Process {
    /// Cap on distinct span names a single process may introduce. Generous
    /// for a real program (a handful of `worker.tick`-style names), small
    /// enough that a misbehaving program can't leak unbounded `'static`
    /// strings or grow the intern table without limit.
    pub const MAX_SPAN_NAMES: u32 = 16;

    /// Base VA of the per-process growable heap region. Well clear of the
    /// program image (linked at `0x1000_0000`, 16 MiB) and the kernel
    /// high-half; in the Sv39 user half. The `Sbrk` syscall maps frames here
    /// on demand.
    pub const HEAP_BASE: usize = 0x2000_0000;

    /// Maximum a single process's heap may grow — bounds the frames a
    /// misbehaving program can pin via `Sbrk`. 16 MiB is generous for the demo.
    pub const HEAP_MAX: usize = 16 * 1024 * 1024;

    /// Build the process for `root_pa` and grant it its bootstrap
    /// capabilities: a `TelemetrySink` bound to `telemetry_counter` and a
    /// `SpanSink`, each with `EMIT` — the "root caps to init only" policy.
    /// Returns the process and the two well-known [`Handle`]s (telemetry,
    /// span) the sinks landed at, which the kernel hands to the program.
    ///
    /// These grants are the only authority a userspace process is born with;
    /// the caller snitches each (`cap.grants_total` + a `CapEvent`).
    pub fn bootstrap(root_pa: usize, telemetry_counter: StringId) -> (Self, Handle, Handle) {
        let (table, telemetry, span) = CapTable::bootstrap(telemetry_counter);
        let process = Self {
            root_pa,
            caps: Mutex::new(table),
            span_names_registered: AtomicU32::new(0),
            heap_top: AtomicUsize::new(Self::HEAP_BASE),
        };
        (process, telemetry, span)
    }
}
