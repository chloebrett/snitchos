//! `snitchos-std` — a **std-shaped facade** over `core` + `alloc` + the
//! SnitchOS userspace runtime (`snitchos-user`, our platform / `sys` layer).
//!
//! **This is not the real `std` crate.** Programs still write `#![no_std]`
//! `#![no_main]` and the runtime still provides `_start` / the allocator /
//! `main` — dropping `#![no_std]` needs a real `*-snitchos` *target* (nightly
//! `build-std` + a `sys` backend). This facade is the *stepping stone*: it maps
//! std's surface onto SnitchOS so we can write std-idiomatic code on **stable**
//! today. An eventual real `std` target reuses this same mapping in its `sys`
//! backend.
//!
//! **The surface only contains what actually works.** Every item here is backed
//! by a real syscall or is a free `core`/`alloc` re-export — nothing type-checks
//! and then panics at runtime. The mapping is deliberately **capability-shaped**,
//! not POSIX: `fs`/`net`/`env` are capability-rooted or unsupported, never
//! ambient. What SnitchOS can't yet provide is documented under *Not yet
//! provided* below and tracked in `plans/userspace-runtime-maturity.md`; it is
//! kept out of the callable surface on purpose.
//!
//! Already free (re-exported from `core`/`alloc`, no platform needed): `Vec`,
//! `String`, `Box`, `Rc`/`Arc`, `format!`, the `BTree*`/`VecDeque` collections,
//! iterators, `Option`/`Result` — i.e. most of std's *non-platform* surface.
//!
//! # Not yet provided (and why)
//!
//! These parts of std are **absent from the surface**, not stubbed, because the
//! mechanism they need doesn't exist yet:
//!
//! - `thread::spawn` / `sync::Mutex` (blocking) — need multi-threaded processes
//!   (one thread per process today), a thread-create syscall, and a futex.
//! - `thread::sleep` — needs a block-until-deadline syscall; a cooperative
//!   spin-yield would busy-wait, not sleep, so it's left out.
//! - `collections::HashMap`/`HashSet` — need `hashbrown` + a `RandomState` seed
//!   (an entropy syscall) for DoS-resistant hashing, or a fixed hasher.
//! - `fs` — capability-rooted (a granted directory capability, WASI-style
//!   preopens), never a global namespace; see the v0.10 `Filesystem`.
//! - `net` — a socket is a granted endpoint capability; needs the network stack.
//! - `env` — args/vars need a startup-info (`BootInfo`) mechanism, not a global
//!   environment.

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
}

/// `std::process`.
pub mod process {
    /// Terminate the process with exit status `code`. Backed by the `Exit`
    /// syscall; the status is what a parent's `wait` collects.
    pub fn exit(code: i32) -> ! {
        snitchos_user::exit_with(code)
    }

    /// Abnormally terminate the process. Backed by the `Exit` syscall with a
    /// non-zero status (`134` = `128 + SIGABRT`, the conventional abort code) so
    /// a waiting parent can tell it apart from a clean `exit(0)`.
    pub fn abort() -> ! {
        snitchos_user::exit_with(134)
    }
}

/// `std::time`.
pub mod time {
    /// `Duration` lives in `core` — free.
    pub use core::time::Duration;

    /// `std::time::Instant` — a monotonic timestamp, backed by the kernel's
    /// `ClockNow` tick counter. `elapsed`/`duration_since` convert a tick delta to
    /// a `Duration` using the platform timebase (`ClockFreq`), so nothing here
    /// hardcodes the clock rate. Monotonic and never rolls backward.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant(u64);

    impl Instant {
        /// Read the monotonic clock now.
        #[must_use]
        pub fn now() -> Instant {
            Instant(snitchos_user::clock_now())
        }

        /// Time elapsed since this instant was captured.
        #[must_use]
        pub fn elapsed(&self) -> Duration {
            Instant::now().duration_since(*self)
        }

        /// `self - earlier` as a `Duration`, saturating at zero when `earlier` is
        /// the later instant (the clock is monotonic, but instants may be compared
        /// out of order). Matches `std`'s saturating semantics.
        #[must_use]
        pub fn duration_since(&self, earlier: Instant) -> Duration {
            ticks_to_duration(self.0.saturating_sub(earlier.0))
        }

        /// Alias of [`Instant::duration_since`], which already saturates.
        #[must_use]
        pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
            self.duration_since(earlier)
        }
    }

    /// Convert a `ClockNow` tick delta to a `Duration` at the platform timebase
    /// (`ClockFreq`, cached in the runtime). `u128` intermediate so `ticks · 1e9`
    /// can't overflow for deltas beyond ~18 s.
    fn ticks_to_duration(ticks: u64) -> Duration {
        let hz = snitchos_user::clock_freq();
        if hz == 0 {
            return Duration::ZERO;
        }
        let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(hz);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// `std::sync`.
pub mod sync {
    /// `Arc` works today — atomic refcount, and riscv64gc has the `a` extension.
    /// `Mutex` is *Not yet provided* (needs threads + a futex to block).
    pub use alloc::sync::Arc;
}

/// `std::collections`.
pub mod collections {
    /// The `alloc` collections are free. `HashMap`/`HashSet` are *Not yet
    /// provided* (need `hashbrown` + a hash seed).
    pub use alloc::collections::{BTreeMap, BTreeSet, BinaryHeap, LinkedList, VecDeque};
}

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
