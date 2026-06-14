//! Workload selection + bodies (host-tested, pure): the `workload=` bootarg
//! parsing and `WorkloadKind` registry (`bootargs`), and the producer/consumer
//! workload logic (`workload`).
//!
//! Re-exported at the crate root (`pub use workloads::…`) so the public API
//! stays `kernel_core::bootargs`, `kernel_core::workload`.

pub mod bootargs;
pub mod workload;
