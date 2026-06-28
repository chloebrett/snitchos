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
/// Mirrors the kernel's cap object kinds (`snitchos_abi::object_kind`), kept here
/// (not tied to kernel types) so it is host-constructable in tests. There is no
/// `File` kind: a file capability is a badged `Endpoint` (`badge != 0`). `Unknown`
/// is the forward-compat catch-all for a discriminant this build doesn't know.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectKind {
    TelemetrySink,
    SpanSink,
    Endpoint,
    Reply,
    Notification,
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
            ObjectKind::Reply => "Reply",
            ObjectKind::Notification => "Notification",
            ObjectKind::Unknown => "Unknown",
        }
    }

    /// Map a packed `CapDesc.kind` discriminant (`snitchos_abi::object_kind`) to a
    /// kind — the `unhitch` step for the kind field. An unrecognized discriminant
    /// (a future kernel kind) becomes [`Unknown`](Self::Unknown).
    #[must_use]
    pub fn from_abi(kind: u32) -> ObjectKind {
        use snitchos_abi::object_kind as k;
        match kind {
            k::TELEMETRY_SINK => ObjectKind::TelemetrySink,
            k::SPAN_SINK => ObjectKind::SpanSink,
            k::ENDPOINT => ObjectKind::Endpoint,
            k::REPLY => ObjectKind::Reply,
            k::NOTIFICATION => ObjectKind::Notification,
            _ => ObjectKind::Unknown,
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

    /// Read the named file's contents as a UTF-8 string, or `None` if it doesn't
    /// exist, isn't valid UTF-8, or there is no filesystem. Backs `readFile`
    /// (and so `view`). The on-target backend does an FS-over-IPC lookup + read
    /// through its endpoint cap; gated in the language by the `FsRead` authority.
    fn fs_read(&self, name: &str) -> Option<alloc::string::String>;
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

    fn fs_read(&self, _name: &str) -> Option<alloc::string::String> {
        None
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
    files: alloc::collections::BTreeMap<alloc::string::String, alloc::string::String>,
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

    /// Script the files `fs_read` (and so `view`) can read, as `(name, contents)`.
    #[must_use]
    pub fn with_files(files: &[(&str, &str)]) -> Self {
        Self {
            files: files.iter().map(|(n, c)| ((*n).into(), (*c).into())).collect(),
            ..Self::default()
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

    fn hold(&self) -> alloc::vec::Vec<CapInfo> {
        self.caps.clone()
    }

    fn fs_read(&self, name: &str) -> Option<alloc::string::String> {
        self.files.get(name).cloned()
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
            use snitchos_abi::CapDesc;
            // Read the process's own cap table via `CapList`; the syscall reports
            // the total even when the buffer was too small, so grow + retry once.
            let mut buf = alloc::vec![CapDesc::default(); 16];
            let total = snitchos_user::cap_list(&mut buf);
            if total > buf.len() {
                buf = alloc::vec![CapDesc::default(); total];
                let _ = snitchos_user::cap_list(&mut buf);
            }
            buf.truncate(total.min(buf.len()));
            // `unhitch` each packed `CapDesc` into the typed `CapInfo`.
            buf.into_iter()
                .map(|d| CapInfo {
                    handle: d.handle,
                    kind: super::ObjectKind::from_abi(d.kind),
                    rights: d.rights,
                    badge: d.badge,
                })
                .collect()
        }

        fn fs_read(&self, name: &str) -> Option<String> {
            use fs_proto::{FileRights, Op, Request, Response, UserBuf};
            use snitchos_user::{Endpoint, endpoint};

            // Attach to the FS server (the startup endpoint) for the root dir cap,
            // `Lookup` the name with READ, then `Read` it in ≤256-byte chunks.
            // `None` if there's no FS endpoint, the file is missing, or non-UTF-8.
            let (_r, root_cap) = endpoint().call([0, 0, 0, 0]).ok()?;
            let root = Endpoint::from_raw_handle(root_cap?);

            let nb = name.as_bytes();
            let lookup = Request::Lookup {
                name: UserBuf { ptr: nb.as_ptr() as u64, len: nb.len() as u64 },
                rights: FileRights::READ,
            };
            let (_l, file_cap) = root.call(lookup.encode()).ok()?;
            let file = Endpoint::from_raw_handle(file_cap?);

            let mut bytes = Vec::new();
            let mut offset = 0u64;
            let mut chunk = [0u8; 256];
            loop {
                let read = Request::Read {
                    offset,
                    dst: UserBuf { ptr: chunk.as_mut_ptr() as u64, len: chunk.len() as u64 },
                };
                let (words, _) = file.call(read.encode()).ok()?;
                let n = match Response::decode(Op::Read, words) {
                    Ok(Response::Count(n)) => n as usize,
                    _ => break,
                };
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                offset += n as u64;
                if n < chunk.len() {
                    break;
                }
            }
            String::from_utf8(bytes).ok()
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
        fn fs_read(&self, _name: &str) -> Option<String> {
            None
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
        assert_eq!(NullPlatform.fs_read("anything"), None);
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
            // A badged endpoint — what a file cap looks like (no `File` kind).
            CapInfo { handle: 3, kind: ObjectKind::Endpoint, rights: 0b0010, badge: 7 },
        ];
        let fake = FakePlatform::with_caps(caps.clone());

        assert_eq!(fake.hold(), caps);
    }

    #[test]
    fn fake_reads_a_scripted_file() {
        let fake = FakePlatform::with_files(&[("notes", "buy milk\n"), ("empty", "")]);

        assert_eq!(fake.fs_read("notes").as_deref(), Some("buy milk\n"));
        assert_eq!(fake.fs_read("empty").as_deref(), Some(""));
        assert_eq!(fake.fs_read("absent"), None);
    }

    #[test]
    fn object_kind_from_abi_maps_each_known_discriminant() {
        use snitchos_abi::object_kind as k;
        assert_eq!(ObjectKind::from_abi(k::TELEMETRY_SINK), ObjectKind::TelemetrySink);
        assert_eq!(ObjectKind::from_abi(k::SPAN_SINK), ObjectKind::SpanSink);
        assert_eq!(ObjectKind::from_abi(k::ENDPOINT), ObjectKind::Endpoint);
        assert_eq!(ObjectKind::from_abi(k::REPLY), ObjectKind::Reply);
        assert_eq!(ObjectKind::from_abi(k::NOTIFICATION), ObjectKind::Notification);
    }

    #[test]
    fn object_kind_from_abi_unknown_discriminant_is_unknown() {
        assert_eq!(ObjectKind::from_abi(999), ObjectKind::Unknown);
    }

    #[test]
    fn a_fresh_fake_holds_no_caps() {
        assert_eq!(FakePlatform::new().hold(), vec![]);
    }
}
