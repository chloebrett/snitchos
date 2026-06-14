//! Runtime-selectable boot workloads: the cooperative/SMP producer-consumer
//! demo (`workload`), the `workload=` bootarg accessor (`boot_workload`), and the
//! stress/regression storms (`storms`, `itest-workloads` builds only).
//!
//! Re-exported at the crate root (`pub(crate) use workloads::…`) so call sites
//! stay `crate::workload`, `crate::boot_workload`, `crate::storms`.

pub mod boot_workload;
pub mod workload;

/// The runtime-selectable stress/regression workloads (spawn storm, IPI pong,
/// shootdown storm, mutex storm, virtio storm). Compiled in only for
/// `itest-workloads` builds — never production. Formerly the `deflake-*` cargo
/// features; see `docs/runtime-workload-selection-design.md`.
#[cfg(feature = "itest-workloads")]
pub mod storms;
