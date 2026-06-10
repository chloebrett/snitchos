//! A v0.7b userspace process: an address space plus the capabilities it
//! was granted.
//!
//! In v0.7b a "process" is one thread + one address space + one root
//! capability (the `TelemetrySink`). The full process object — multiple
//! threads, transferable caps, a real `init` grant graph — grows here in
//! v0.8. The capability *machinery* is pure and host-tested in
//! [`kernel_core::cap`]; this module only decides *where the table lives*
//! and grants the bootstrap capability. See `plans/v0.7b-capabilities.md`.

use core::sync::atomic::AtomicPtr;

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
}

impl Process {
    /// Build the process for `root_pa` and grant it its bootstrap
    /// capabilities: exactly one `TelemetrySink` bound to
    /// `telemetry_counter`, with `EMIT` — the "root cap to init only"
    /// policy. Returns the process and the well-known [`Handle`] the sink
    /// landed at (the handle the user program is told to invoke).
    ///
    /// The grant itself is the only authority a v0.7b userspace process is
    /// born with; the caller snitches it (`cap.grants_total`).
    pub fn bootstrap(root_pa: usize, telemetry_counter: StringId) -> (Self, Handle) {
        let (table, handle) = CapTable::bootstrap_telemetry(telemetry_counter);
        (Self { root_pa, caps: Mutex::new(table) }, handle)
    }
}
