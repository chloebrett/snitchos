//! Boot-time decisions, host-tested: the `workload=` bootarg parser +
//! `WorkloadKind` registry (`bootargs`), the producer/consumer workload logic
//! (`workload`), and `scause` decoding (`trap`).
//!
//! Carved out of `kernel-core` — see `plans/kernel-core-split.md`. These are the
//! pure answers to "what was the kernel asked to run?" and "what just trapped?".
//! Acting on either — spawning the tasks, servicing the trap — stays in
//! `kernel/`, next to the CSRs and the asm.
//!
//! No dependencies at all, and the production code doesn't even allocate: it's
//! string parsing and bit-twiddling over values the caller already holds.

#![no_std]
#![forbid(unsafe_code)]

// Only `workload`'s tests reach for `alloc` (a `BTreeSet` to assert histogram
// bins are distinct); the boot-time logic itself allocates nothing.
#[cfg(test)]
extern crate alloc;

pub mod bootargs;
pub mod trap;
pub mod workload;
