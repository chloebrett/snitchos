//! snemu-driven xtask tooling: the QEMU differential oracle (`snemu_diff`),
//! the measurement spine (`snemu_bench`), and the guest instret profiler
//! (`snemu_profile`). Extracted from the `xtask` binary crate so that editing
//! integration-test scenarios doesn't recompile the emulator-facing tooling
//! (and the heavy `snemu` opt-3 dep tree it pulls in).
//!
//! `qemu` lives in the `xtask-qemu` crate; it's aliased here so the modules'
//! existing `crate::qemu::…` references keep resolving unchanged.
use xtask_qemu as qemu;

pub mod snemu_bench;
pub mod snemu_diff;
pub mod snemu_profile;
