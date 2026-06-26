//! User-pointer validation probe (`workload=userspace-bad-ptr`): pass an
//! in-range but **unmapped** user VA to `DebugWrite`. The kernel's
//! `copy_from_user` must *refuse* it (`BadUserRange`) — walking the page table
//! before the `SUM` deref — rather than faulting to S-mode (a kernel panic).
//!
//! We never dereference the pointer in U-mode; only the kernel touches it (and
//! refuses). The syscall returns `usize::MAX` on refusal; we emit a survival
//! marker only then — proving both that the kernel refused *and* that the
//! process is still running (no panic). Linked at the same fixed VA as `hello`
//! (never loaded together); crt0 / panic / syscalls come from the runtime.

#![no_std]
#![no_main]

use snitchos_user::{debug_write_raw, entry, register_counter};

/// A VA in the user half (`< USER_VA_END = 1 << 38`) that the program never
/// maps — well above its image (`0x1000_0000`), stack, and heap. `copy_from_user`
/// must refuse it, not fault.
const UNMAPPED_USER_VA: usize = 0x20_0000_0000;

/// Emitted iff `DebugWrite` on the unmapped pointer was refused (`usize::MAX`)
/// and we lived to tell — the `userspace-bad-ptr` scenario asserts it.
const REFUSED_AND_SURVIVED: i64 = 0x0BAD;

#[entry]
fn main() {
    let status = debug_write_raw(UNMAPPED_USER_VA, 8);
    if status == usize::MAX {
        register_counter("snitchos.bad_ptr.marker").emit(REFUSED_AND_SURVIVED);
    }
}
