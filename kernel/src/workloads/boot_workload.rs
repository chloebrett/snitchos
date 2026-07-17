//! Runtime boot-workload selection. `kmain` resolves the `workload=`
//! kernel bootarg once at boot and records it here; the spawn sites and
//! the heartbeat (which runs on its own task, so it can't be passed the
//! value directly) read it back. `None` means "run the default demo."
//!
//! See `docs/runtime-workload-selection-design.md`.

use kernel_boot::bootargs::WorkloadKind;

use crate::sync::Once;

static SELECTED: Once<Option<WorkloadKind>> = Once::new();

/// Record the resolved selection. Call exactly once from `kmain`,
/// before the heartbeat task starts ticking.
pub fn init(selected: Option<WorkloadKind>) {
    SELECTED.call_once(|| selected);
}

/// The selected workload, or `None` for the default demo (also `None`
/// if `init` hasn't run yet — i.e. before `kmain`'s dispatch).
pub fn selected() -> Option<WorkloadKind> {
    SELECTED.get().copied().flatten()
}
