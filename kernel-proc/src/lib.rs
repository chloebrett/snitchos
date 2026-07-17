//! Tasks, their authority, and their lifecycle — host-tested.
//!
//! The last carve-out from `kernel-core` (see `plans/kernel-core-split.md`), and
//! the one concept that crate was really about once memory, observability,
//! devices and boot had been lifted out of it.
//!
//! - **lifecycle** — the runqueue + preemption/kill policy (`sched`), exit/wait
//!   reaping (`reap`), blocking (`notify`), stack accounting (`stack`)
//! - **authority** — the per-process capability table (`cap`), the synchronous
//!   endpoint rendezvous (`ipc`)
//! - **what a process is made of** — the ELF loader front-end + page planning
//!   (`elf`), and the per-process name quotas (`metric`, `span_name`)
//!
//! `cap`, `ipc`, `notify` and `sched` are one crate on purpose: their only
//! coupling is a handful of ID newtypes (`TaskId`, `EndpointId`,
//! `NotificationId`). An earlier plan proposed a shared `kernel-ids` crate to
//! break that — grouping by concept removed the need instead.
//!
//! Pure bookkeeping as always: no asm, no MMIO, no statics. The `kernel` binary
//! owns the task table, does the `switch`, and enters U-mode; this crate decides
//! *who runs next* and *whether that invocation is allowed*.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod cap;
pub mod elf;
pub mod ipc;
pub mod metric;
pub mod notify;
pub mod reap;
pub mod sched;
pub mod span_name;
pub mod stack;
