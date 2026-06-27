//! The platform effect seam: *what happens* when a Stitch program touches the
//! outside world — console, capabilities, processes, the filesystem — decoupled
//! from the natives that trigger it. The async/effectful sibling of the
//! [`Telemetry`](crate::telemetry::Telemetry) backend, and the seam that makes
//! the shell host-testable: the on-target backend wraps the real syscalls, while
//! a host fake records what was done. See `docs/stitch-test-library-design.md`.
//!
//! The trait grows method-by-method as natives need it; console comes first.

// `String` is referenced fully-qualified below rather than imported: under
// `cargo test` the crate builds with `std`, where a `use` of a prelude item
// would be flagged redundant; the path resolves in both `std` and `no_std`.

/// What happens when a Stitch program touches the outside world, decoupled from
/// the natives that trigger it. Methods take `&self` (backends use interior
/// mutability) so one backend is shared, via `Rc`, across every scope and
/// closure of a run — exactly like [`Telemetry`](crate::telemetry::Telemetry).
pub trait Platform {
    /// Read one finished line of console input (no trailing newline), or `None`
    /// at end of input. Line discipline (echo, backspace) is the backend's job;
    /// see [`line_edit`](crate::line_edit).
    fn read_line(&self) -> Option<alloc::string::String>;

    /// Write text to the console (the human terminal).
    fn write(&self, text: &str);
}

/// The default backend: a program with no platform installed. Reads nothing and
/// discards output — so a Stitch run that touches no effects (e.g. a pure
/// semantics test) needs no backend wired. The effect analogue of the empty
/// telemetry sink.
#[derive(Default)]
pub struct NullPlatform;

impl Platform for NullPlatform {
    fn read_line(&self) -> Option<alloc::string::String> {
        None
    }

    fn write(&self, _text: &str) {}
}

/// A host test backend: scripted console input, recorded output — the fake the
/// shell's effect tests assert against (output today; spawns/grants as those
/// slices land). Host-only; the on-target build uses `RuntimePlatform`.
#[cfg(not(target_os = "none"))]
#[derive(Default)]
pub struct FakePlatform {
    input: core::cell::RefCell<alloc::collections::VecDeque<alloc::string::String>>,
    output: core::cell::RefCell<alloc::string::String>,
}

#[cfg(not(target_os = "none"))]
impl FakePlatform {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the console input: each line (`\n`-separated) is one `read_line`.
    #[must_use]
    pub fn with_input(text: &str) -> Self {
        Self {
            input: core::cell::RefCell::new(text.lines().map(alloc::string::String::from).collect()),
            output: core::cell::RefCell::new(alloc::string::String::new()),
        }
    }

    /// Everything written through `write` so far.
    #[must_use]
    pub fn output(&self) -> alloc::string::String {
        self.output.borrow().clone()
    }
}

#[cfg(not(target_os = "none"))]
impl Platform for FakePlatform {
    fn read_line(&self) -> Option<alloc::string::String> {
        self.input.borrow_mut().pop_front()
    }

    fn write(&self, text: &str) {
        self.output.borrow_mut().push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use core::cell::Cell;

    #[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
    use crate::prelude::*;

    #[derive(Default)]
    struct CountingPlatform {
        writes: Cell<u32>,
    }

    impl Platform for CountingPlatform {
        fn read_line(&self) -> Option<String> {
            None
        }
        fn write(&self, _text: &str) {
            self.writes.set(self.writes.get() + 1);
        }
    }

    #[test]
    fn env_routes_console_writes_to_the_installed_platform() {
        let backend = Rc::new(CountingPlatform::default());
        let env = Env::new().with_platform(backend.clone());

        env.platform().write("x");

        assert_eq!(backend.writes.get(), 1);
    }

    #[test]
    fn null_platform_reads_nothing() {
        assert_eq!(NullPlatform.read_line(), None);
    }

    #[test]
    fn a_derived_env_shares_the_platform() {
        let backend = Rc::new(CountingPlatform::default());
        let env = Env::new().with_platform(backend.clone());

        env.globals_only().platform().write("x");

        assert_eq!(backend.writes.get(), 1);
    }
}
