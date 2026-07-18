//! Standalone xtask commands with no coupling to the integration-test harness:
//! the crate/dead-code auditor (`audit`), the LOC counter (`loc`), the doc-link
//! checker (`links`), the live-QEMU measurement command (`measure`), the
//! Sonnet-assisted staging tool (`snip`), and the shared source-tokenizer
//! (`source`) that `audit`/`loc` use to mask test lines.
//!
//! Extracted from the `xtask` binary crate so editing an itest scenario doesn't
//! recompile them. `qemu` lives in `xtask-qemu`; it's aliased here so
//! `measure`'s `crate::qemu::…` references keep resolving unchanged.
use xtask_qemu as qemu;

pub mod audit;
pub mod links;
pub mod loc;
pub mod measure;
pub mod snip;
pub mod source;
