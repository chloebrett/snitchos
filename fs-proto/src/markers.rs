//! Demo signal markers — the value-sentinel contract between the `user/fs`
//! client binary and the `fs-*` itest scenarios.
//!
//! The client emits one of these to `snitchos.user.telemetry_total` when a
//! step's post-condition holds; the matching itest asserts a `Metric` frame
//! carrying that value. Defined here, in the crate both sides already share, so
//! the two can never drift — the v0.10 "repurposed client" bug was exactly that
//! drift (an emitter and its assertion falling out of sync). The values are
//! arbitrary, distinct sentinels; only equality across the boundary matters.

/// The root stats back as an empty `Dir` (`fs-stat-root`).
pub const STAT_ROOT_OK: i64 = 0x57A7;

/// A freshly-created node stats back as an empty `File` (`fs-create-stat`).
pub const CREATE_STAT_OK: i64 = 0x5C7E;

/// Bytes survive the write→read round-trip both ways (`fs-write-read`).
pub const WRITE_READ_OK: i64 = 0x317E;

/// `readdir` lists the one entry and then reports end-of-list (`fs-readdir`).
pub const READDIR_OK: i64 = 0x5D14;

/// An authorized write through a `READ|WRITE` lookup succeeds — the rights
/// gate's positive control (`fs-lookup-rights-gate`).
pub const WRITE_AUTHORIZED_OK: i64 = 0x600D;

/// A removed file no longer resolves (`fs-remove`).
pub const REMOVE_OK: i64 = 0xDE1E;
