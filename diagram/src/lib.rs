//! Diagram generation for `SnitchOS`: typed diagram values plus projections
//! from a source of truth. Static (B1) projections read the workspace
//! (`cargo metadata`); runtime (B2) projections fold a captured telemetry
//! stream (`protocol::stream::OwnedFrame`). All logic here is pure and
//! host-tested; `xtask` owns the I/O (the `cargo metadata` shell-out, driving
//! snemu for a capture, `--check` diffs, file writes) and delegates projection
//! to this crate. See `docs/diagrams-design.md`.

pub mod caps;
pub mod deps;
pub mod itest_matrix;
pub mod model;
