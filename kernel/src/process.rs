//! A v0.7b userspace process: an address space plus the capabilities it
//! was granted.
//!
//! In v0.7b a "process" is one thread + one address space + one root
//! capability (the `TelemetrySink`). The full process object — multiple
//! threads, transferable caps, a real `init` grant graph — grows here in
//! v0.8. The capability *machinery* is pure and host-tested in
//! [`kernel_core::cap`]; this module only decides *where the table lives*
//! and grants the bootstrap capability. See `plans/v0.7b-capabilities.md`.

use kernel_core::cap::{CapTable, Handle};
use protocol::StringId;

use crate::sync::Mutex;

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
    /// lock discipline). The Step 5 syscall dispatcher is the first
    /// reader.
    #[allow(dead_code, reason = "read by the Step 5 capability-invocation syscall dispatcher")]
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
