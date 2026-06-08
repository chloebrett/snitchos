//! Kernel command-line (`/chosen/bootargs`) parsing — pure logic.
//!
//! The kernel selects its boot workload at runtime from a `workload=`
//! key in the bootargs string QEMU passes via `-append`. Parsing is
//! pure and host-tested here; `kmain` reads the raw string from the
//! DTB and feeds it in. See `docs/runtime-workload-selection-design.md`.

/// Which boot workload to run. `kmain` maps each variant to a set of
/// task spawns (and, for some, heartbeat behaviour). The *default*
/// demo is the absence of a selection (`select` returns `None`), so it
/// is deliberately not a variant here — adding a variant must mean
/// "an alternate workload," never "the default."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadKind {
    /// Cross-hart producer/consumer: producer on hart 0, consumer on
    /// hart 1 (v0.6 step 11).
    Smp,
    /// Frame-allocator OOM: keep the default demo tasks, but the
    /// heartbeat leaks frames each tick until the pool exhausts.
    FrameOom,
    /// Kernel-heap OOM: default demo tasks, but the heartbeat leaks
    /// heap blocks each tick until the heap exhausts.
    HeapOom,
    /// Cross-hart spawn storm: hart 0 runs a serialised `spawn_on(1, …)`
    /// loop; hart 1 stays idle until poked. Heartbeat-driven.
    SpawnStorm,
    /// Tight cross-hart IPI wakeup loop (hart 0 → hart 1).
    /// Heartbeat-driven.
    IpiPong,
    /// Tight `mmu::shootdown` loop from hart 0. Heartbeat-driven.
    ShootdownStorm,
    /// Two tasks (one per hart) hammer a shared `Mutex`. Task-driven.
    MutexStorm,
    /// hart 0 emit-storm over the virtio TX path, hart 1 atomic spin.
    /// Task-driven.
    VirtioStorm,
}

impl WorkloadKind {
    /// True for the `*-storm` workloads, which drive hart 1 themselves
    /// (spawn their own hart-1 task, or poke an intentionally-idle
    /// hart 1). `kmain` uses this to decide whether to spawn the
    /// default `hart_1_probe`.
    pub fn is_storm(self) -> bool {
        matches!(
            self,
            Self::SpawnStorm
                | Self::IpiPong
                | Self::ShootdownStorm
                | Self::MutexStorm
                | Self::VirtioStorm
        )
    }
}

/// Parse a `workload=<name>` selection out of the bootargs string.
/// Returns `None` when no `workload=` key is present (run the default
/// demo) or when the value is unrecognised (also default — a typo
/// should fail safe to default rather than silently match something).
pub fn select(bootargs: &str) -> Option<WorkloadKind> {
    bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("workload="))
        .and_then(|name| match name {
            "smp" => Some(WorkloadKind::Smp),
            "frame-oom" => Some(WorkloadKind::FrameOom),
            "heap-oom" => Some(WorkloadKind::HeapOom),
            "spawn-storm" => Some(WorkloadKind::SpawnStorm),
            "ipi-pong" => Some(WorkloadKind::IpiPong),
            "shootdown-storm" => Some(WorkloadKind::ShootdownStorm),
            "mutex-storm" => Some(WorkloadKind::MutexStorm),
            "virtio-storm" => Some(WorkloadKind::VirtioStorm),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_smp_from_workload_key() {
        assert_eq!(select("workload=smp"), Some(WorkloadKind::Smp));
    }

    #[test]
    fn selects_frame_oom() {
        assert_eq!(select("workload=frame-oom"), Some(WorkloadKind::FrameOom));
    }

    #[test]
    fn selects_heap_oom() {
        assert_eq!(select("workload=heap-oom"), Some(WorkloadKind::HeapOom));
    }

    #[test]
    fn is_storm_classifies_each_kind() {
        assert!(WorkloadKind::SpawnStorm.is_storm());
        assert!(WorkloadKind::IpiPong.is_storm());
        assert!(WorkloadKind::ShootdownStorm.is_storm());
        assert!(WorkloadKind::MutexStorm.is_storm());
        assert!(WorkloadKind::VirtioStorm.is_storm());
        assert!(!WorkloadKind::Smp.is_storm());
        assert!(!WorkloadKind::FrameOom.is_storm());
        assert!(!WorkloadKind::HeapOom.is_storm());
    }

    #[test]
    fn selects_storm_workloads() {
        assert_eq!(select("workload=spawn-storm"), Some(WorkloadKind::SpawnStorm));
        assert_eq!(select("workload=ipi-pong"), Some(WorkloadKind::IpiPong));
        assert_eq!(select("workload=shootdown-storm"), Some(WorkloadKind::ShootdownStorm));
        assert_eq!(select("workload=mutex-storm"), Some(WorkloadKind::MutexStorm));
        assert_eq!(select("workload=virtio-storm"), Some(WorkloadKind::VirtioStorm));
    }

    #[test]
    fn empty_bootargs_is_default() {
        assert_eq!(select(""), None);
    }

    #[test]
    fn no_workload_key_is_default() {
        assert_eq!(select("console=ttyS0 loglevel=7"), None);
    }

    #[test]
    fn unknown_workload_value_is_default() {
        assert_eq!(select("workload=does-not-exist"), None);
    }

    #[test]
    fn finds_workload_key_among_other_tokens() {
        assert_eq!(select("console=ttyS0 workload=smp loglevel=7"), Some(WorkloadKind::Smp));
    }

    #[test]
    fn workload_key_position_independent() {
        assert_eq!(select("loglevel=7 workload=smp"), Some(WorkloadKind::Smp));
    }
}
