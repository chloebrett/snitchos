//! `snitchos-std` — a **std-shaped facade** over `core` + `alloc` + the
//! SnitchOS userspace runtime (`snitchos-user`, our platform / `sys` layer).
//!
//! **This is not the real `std` crate.** Programs still write `#![no_std]`
//! `#![no_main]` and the runtime still provides `_start` / the allocator /
//! `main` — dropping `#![no_std]` needs a real `*-snitchos` *target* (nightly
//! `build-std` + a `sys` backend). This facade is the *stepping stone*: it maps
//! std's surface onto SnitchOS so we can write std-idiomatic code on **stable**
//! today and see exactly what's left.
//!
//! Reading this crate top to bottom is the map: parts backed by what we have
//! are **wired**; the rest are `todo!("…why…")`. The `todo!` messages double as
//! the spec — and crucially they encode the **capability** design, not POSIX:
//! `fs`/`net`/`env` are capability-rooted or unsupported, never ambient. An
//! eventual real `std` target reuses this same mapping in its `sys` backend.
//!
//! Already free (re-exported from `core`/`alloc`, no platform needed): `Vec`,
//! `String`, `Box`, `Rc`/`Arc`, `format!`, the `BTree*`/`VecDeque` collections,
//! iterators, `Option`/`Result` — i.e. most of std's *non-platform* surface.

#![no_std]

extern crate alloc;

// --- Free: re-exports of the non-platform surface (core + alloc) ---

pub use alloc::{boxed, format, string, vec};
pub use core::{cmp, fmt, iter, mem, ops, slice, str};

/// A std-like prelude: `use snitchos_std::prelude::*;`.
pub mod prelude {
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};
}

// --- Wired: std API backed by syscalls we already have ---

/// `std::thread`.
pub mod thread {
    /// Yield the CPU to another ready task (cooperative). Backed by the `Yield`
    /// syscall — a direct 1:1 with `std::thread::yield_now`.
    pub fn yield_now() {
        snitchos_user::yield_now();
    }

    /// `std::thread::spawn`. TODO: needs multi-threaded processes (one thread
    /// per process today) + a thread-create syscall.
    pub fn spawn() {
        todo!("thread::spawn — needs multi-threaded processes + a spawn syscall")
    }

    /// `std::thread::sleep`. TODO: needs a timer/sleep syscall.
    pub fn sleep() {
        todo!("thread::sleep — needs a timer syscall")
    }
}

/// `std::process`.
pub mod process {
    /// Terminate the process. Backed by the `Exit` syscall. (`_code` is ignored
    /// until `Exit` carries an exit status — a small ABI extension.)
    pub fn exit(_code: i32) -> ! {
        snitchos_user::exit()
    }

    /// Abort the process. Backed by the `Exit` syscall.
    pub fn abort() -> ! {
        snitchos_user::exit()
    }
}

/// `std::time`.
pub mod time {
    /// `Duration` lives in `core` — free.
    pub use core::time::Duration;

    /// `std::time::Instant`. TODO: needs a read-monotonic-clock syscall (the
    /// kernel has the clock; userspace can't read it yet).
    pub struct Instant(());
    impl Instant {
        pub fn now() -> Instant {
            todo!("time::Instant::now — needs a read-clock syscall")
        }
    }
}

/// `std::sync`.
pub mod sync {
    /// `Arc` works today — atomic refcount, and riscv64gc has the `a` extension.
    pub use alloc::sync::Arc;

    /// `std::sync::Mutex`. TODO: trivial single-threaded, but the std API
    /// (poisoning, `MutexGuard`) and real *blocking* need threads + a futex.
    pub struct Mutex<T>(core::marker::PhantomData<T>);
    impl<T> Mutex<T> {
        pub fn new(_value: T) -> Mutex<T> {
            todo!("sync::Mutex — needs threads/futex for blocking")
        }
    }
}

/// `std::collections`.
pub mod collections {
    /// The `alloc` collections are free.
    pub use alloc::collections::{BTreeMap, BTreeSet, BinaryHeap, LinkedList, VecDeque};

    /// `HashMap`/`HashSet`. TODO: `hashbrown` + a `RandomState` seed (an
    /// entropy syscall) for DoS-resistant hashing — or a fixed hasher.
    pub fn hashmap() {
        todo!("collections::HashMap — needs hashbrown + a hash seed")
    }
}

// --- Stubbed: the ambient-namespace surface, capability-rooted or unsupported ---

/// `std::io` — `print!`/`println!` to stdout. Each line is a `DebugWrite`
/// syscall → a snitched `Frame::Log`, so userspace stdout is observable on the
/// wire (the SnitchOS twist on "stdout"). Wired.
pub mod io {
    use core::fmt;

    /// The standard output stream — writes via the `DebugWrite` syscall.
    pub struct Stdout;

    impl fmt::Write for Stdout {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            // Chunk to the kernel's per-write limit; a long write becomes
            // several `Log` frames.
            for chunk in s.as_bytes().chunks(snitchos_user::DEBUG_WRITE_MAX) {
                snitchos_user::debug_write(chunk);
            }
            Ok(())
        }
    }

    /// `std::io::stdout()`.
    #[must_use]
    pub fn stdout() -> Stdout {
        Stdout
    }

    /// Backs the `print!`/`println!` macros. Formats into a heap string first
    /// so one `print!` is one `Log` frame (not one per format fragment), then
    /// writes it. Not for direct use.
    #[doc(hidden)]
    pub fn _print(args: fmt::Arguments) {
        use fmt::Write;
        let mut s = alloc::string::String::new();
        let _ = s.write_fmt(args);
        for chunk in s.as_bytes().chunks(snitchos_user::DEBUG_WRITE_MAX) {
            snitchos_user::debug_write(chunk);
        }
    }
}

/// `print!` — write to stdout (a snitched `Log` frame), no trailing newline.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::io::_print(::core::format_args!($($arg)*))
    };
}

/// `println!` — write a line to stdout (a snitched `Log` frame).
#[macro_export]
macro_rules! println {
    () => { $crate::io::_print(::core::format_args!("\n")) };
    ($($arg:tt)*) => {
        $crate::io::_print(::core::format_args!("{}\n", ::core::format_args!($($arg)*)))
    };
}

/// `std::fs` — **capability-rooted, not ambient**. TODO: the v0.10 capability
/// `Filesystem`; `File::open` resolves against a *granted directory capability*
/// (WASI-style preopens), never a global namespace. See
/// `plans/userspace-runtime-maturity.md` (the `std::fs` design constraint).
pub mod fs {
    pub fn open() {
        todo!("fs::File::open — capability-rooted (v0.10 Filesystem), not ambient")
    }
}

/// `std::net` — **capability-rooted, not ambient**. TODO: a socket is a granted
/// endpoint capability; needs the post-v1.0 network stack.
pub mod net {
    pub fn connect() {
        todo!("net::TcpStream::connect — capability-rooted endpoint, needs the net stack")
    }
}

/// `std::env` — **not ambient**. TODO: args/vars need a startup-info mechanism
/// (a `BootInfo` page handed at entry), not a global environment.
pub mod env {
    pub fn args() {
        todo!("env::args — needs a startup-info (BootInfo) mechanism, not ambient")
    }
}
