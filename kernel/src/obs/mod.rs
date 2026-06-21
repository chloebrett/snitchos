//! Observability: span/metric emission and the intern/registry plumbing
//! (`tracing`), the [`DeferredCounter`](counter::DeferredCounter) registry, and
//! the per-tick metric drain loop (`heartbeat`).
//!
//! Re-exported at the crate root (`pub(crate) use obs::…`) so call sites stay
//! `crate::tracing`, `crate::heartbeat`, `crate::counter`.

pub mod counter;
pub mod heartbeat;
pub mod tracing;
