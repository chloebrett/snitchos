//! Multi-hart support: per-CPU storage (`percpu`), secondary-hart bring-up
//! (`secondary`), inter-processor interrupts (`ipi`), and the kernel locking
//! chokepoint (`sync`).
//!
//! Re-exported at the crate root (`pub(crate) use smp::…`) so call sites stay
//! `crate::percpu`, `crate::sync`, etc.

pub mod ipi;
pub mod percpu;
pub mod secondary;
pub mod sync;
