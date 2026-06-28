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

/// A capability handle — an index into the calling process's own `CapTable`.
pub type Handle = u32;

/// A capability rights bitmask (`snitchos_abi::rights` bits).
pub type Rights = u32;

/// What kind of object a capability names — the human-facing tag `hold` shows.
/// A userspace-facing mirror of the kernel's cap object kinds, kept here (not
/// tied to kernel types) so it is host-constructable in tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectKind {
    TelemetrySink,
    SpanSink,
    Endpoint,
    Notification,
    File,
    Unknown,
}

impl ObjectKind {
    /// The display name `hold` reports for this kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::TelemetrySink => "TelemetrySink",
            ObjectKind::SpanSink => "SpanSink",
            ObjectKind::Endpoint => "Endpoint",
            ObjectKind::Notification => "Notification",
            ObjectKind::File => "File",
            ObjectKind::Unknown => "Unknown",
        }
    }
}

/// One capability the calling process holds — what `hold` enumerates. Pure data
/// (no kernel types), so a test can construct a cap table by hand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CapInfo {
    pub handle: Handle,
    pub kind: ObjectKind,
    pub rights: Rights,
    pub badge: u64,
}

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

    /// Enumerate the capabilities the calling process holds — introspection of
    /// its own authority (no new authority is granted by looking). Backs the
    /// shell's `hold`. The on-target backend reads its own `CapTable` via the
    /// `CapList` syscall.
    fn hold(&self) -> alloc::vec::Vec<CapInfo>;
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

    fn hold(&self) -> alloc::vec::Vec<CapInfo> {
        alloc::vec::Vec::new()
    }
}

/// A host test backend: scripted console input, recorded output — the fake the
/// shell's effect tests assert against (output today; spawns/grants as those
/// slices land). Host-only; the on-target build uses `RuntimePlatform`.
#[cfg(not(target_os = "none"))]
#[derive(Default)]
pub struct FakePlatform {
    input: core::cell::RefCell<alloc::collections::VecDeque<alloc::string::String>>,
    output: core::cell::RefCell<alloc::string::String>,
    caps: alloc::vec::Vec<CapInfo>,
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
            ..Self::default()
        }
    }

    /// Script the capability table `hold` reports.
    #[must_use]
    pub fn with_caps(caps: alloc::vec::Vec<CapInfo>) -> Self {
        Self { caps, ..Self::default() }
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

    fn hold(&self) -> alloc::vec::Vec<CapInfo> {
        self.caps.clone()
    }
}

/// The on-target backend: console I/O over the real `SnitchOS` syscalls. `write`
/// is a `ConsoleWrite`; `read_line` drives the [`LineEditor`](crate::line_edit)
/// over `ConsoleRead` chunks — echoing as it goes, yielding while no input is
/// pending — and returns each finished line. Never reaches end-of-input (a UART
/// has none), so `read_line` blocks until a line completes. Mirrors
/// `RuntimeTelemetry`.
#[cfg(target_os = "none")]
pub use on_target::RuntimePlatform;

#[cfg(target_os = "none")]
mod on_target {
    use super::{CapInfo, Platform};
    use crate::line_edit::LineEditor;

    use core::cell::RefCell;
    use alloc::string::String;
    use alloc::vec::Vec;

    use snitchos_user::{console_read, console_write, yield_now};

    #[derive(Default)]
    pub struct RuntimePlatform {
        editor: RefCell<LineEditor>,
    }

    impl RuntimePlatform {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl Platform for RuntimePlatform {
        fn read_line(&self) -> Option<String> {
            let mut buf = [0u8; 64];
            loop {
                if let Some(line) = self.editor.borrow_mut().next_line() {
                    return Some(line);
                }
                let n = console_read(&mut buf);
                if n == 0 {
                    yield_now();
                    continue;
                }
                let echo = self.editor.borrow_mut().feed(&buf[..n]);
                console_write(&echo);
            }
        }

        fn write(&self, text: &str) {
            console_write(text.as_bytes());
        }

        fn hold(&self) -> Vec<CapInfo> {
            // No-op until the `CapList` syscall + runtime wrapper land (the
            // kernel side of `hold`); a process can't yet enumerate its own
            // cap table. Returns empty so `hold` is callable on-target meanwhile.
            Vec::new()
        }
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
        fn hold(&self) -> Vec<CapInfo> {
            Vec::new()
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

    #[test]
    fn fake_replays_scripted_input_line_by_line() {
        let fake = FakePlatform::with_input("first\nsecond");

        assert_eq!(fake.read_line().as_deref(), Some("first"));
        assert_eq!(fake.read_line().as_deref(), Some("second"));
        assert_eq!(fake.read_line(), None);
    }

    #[test]
    fn fake_with_input_treats_a_trailing_newline_as_no_extra_line() {
        let fake = FakePlatform::with_input("only\n");

        assert_eq!(fake.read_line().as_deref(), Some("only"));
        assert_eq!(fake.read_line(), None);
    }

    #[test]
    fn fake_records_writes_in_order() {
        let fake = FakePlatform::new();

        fake.write("ab");
        fake.write("cd");

        assert_eq!(fake.output(), "abcd");
    }

    #[test]
    fn a_fresh_fake_reads_nothing_and_has_no_output() {
        let fake = FakePlatform::new();

        assert_eq!(fake.read_line(), None);
        assert_eq!(fake.output(), "");
    }

    #[test]
    fn fake_returns_its_scripted_caps() {
        let caps = vec![
            CapInfo { handle: 2, kind: ObjectKind::Endpoint, rights: 0b0110, badge: 0 },
            CapInfo { handle: 3, kind: ObjectKind::File, rights: 0b0001, badge: 7 },
        ];
        let fake = FakePlatform::with_caps(caps.clone());

        assert_eq!(fake.hold(), caps);
    }

    #[test]
    fn a_fresh_fake_holds_no_caps() {
        assert_eq!(FakePlatform::new().hold(), vec![]);
    }
}
