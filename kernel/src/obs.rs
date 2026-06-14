//! Observability: span/metric emission and the intern/registry plumbing
//! (`tracing`), and the per-tick metric drain loop (`heartbeat`).
//!
//! Re-exported at the crate root (`pub(crate) use obs::…`) so call sites stay
//! `crate::tracing`, `crate::heartbeat`.

pub mod heartbeat;
pub mod tracing;
