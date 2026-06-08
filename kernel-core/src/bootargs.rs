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
