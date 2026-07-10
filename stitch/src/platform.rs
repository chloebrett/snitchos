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

/// The emoji `hold` shows for a rights bitmask, one glyph per *category* present
/// (not per bit): 🪴 mint (`MINT` — the authority-growing right), 👀 read
/// (`RECV`/`WAIT`, the consumer ends), 📝 write (`EMIT`/`SEND`/`SIGNAL`, the
/// emitting/producer ends). Order is fixed (mint, read, write) and an empty mask
/// yields the empty string. This is the cap-domain's own display of an opaque
/// field — the sibling of [`ObjectKind::as_str`] — kept out of the generic table
/// renderer, which knows shapes, not rights.
#[must_use]
pub fn rights_glyphs(rights: Rights) -> alloc::string::String {
    use snitchos_abi::rights as r;
    let mut out = alloc::string::String::new();
    if rights & r::MINT != 0 {
        out.push('🪴');
    }
    if rights & (r::RECV | r::WAIT) != 0 {
        out.push('👀');
    }
    if rights & (r::EMIT | r::SEND | r::SIGNAL) != 0 {
        out.push('📝');
    }
    out
}

/// Parse a rights spec — one or more right *names* (`SEND`, `RECV`, `MINT`,
/// `EMIT`, `SIGNAL`, `WAIT`), case-insensitive, separated by space, comma, or
/// `|` — into the bitmask. The input side of [`rights_glyphs`], for the shell's
/// `grant`. `None` if the spec is empty or names an unknown right (strict, so a
/// typo fails loudly rather than granting less than asked).
#[must_use]
pub fn parse_rights(spec: &str) -> Option<Rights> {
    use snitchos_abi::rights as r;
    let mut bits = 0;
    for token in spec.split([' ', ',', '|']).filter(|t| !t.is_empty()) {
        bits |= match token.to_ascii_uppercase().as_str() {
            "EMIT" => r::EMIT,
            "SEND" => r::SEND,
            "RECV" => r::RECV,
            "MINT" => r::MINT,
            "SIGNAL" => r::SIGNAL,
            "WAIT" => r::WAIT,
            _ => return None,
        };
    }
    (bits != 0).then_some(bits)
}

/// Wrap each rights glyph in its ANSI color — the presentation companion to
/// [`rights_glyphs`], and the colorizer the REPL hands the box style when its
/// output channel supports color: 🪴 green (mint), 👀 blue (read), 📝 amber/yellow
/// (write). Keyed on the cell's **provenance**, not its content or column name:
/// `native` is true only for cells from a kernel-built record (a `DataValue`
/// whose `native` flag is set — `hold`'s rows), which user Stitch can never
/// forge. So a glyph a user prints in *any* column is left alone; only genuine
/// rights are painted. Amber uses SGR 33 (yellow) for portability — a bare UART
/// terminal needn't grok 256-color.
#[must_use]
pub fn colorize_rights(native: bool, cell: &str) -> alloc::string::String {
    if !native {
        return cell.into();
    }
    cell.replace('🪴', "\u{1b}[32m🪴\u{1b}[0m")
        .replace('👀', "\u{1b}[34m👀\u{1b}[0m")
        .replace('📝', "\u{1b}[33m📝\u{1b}[0m")
}

/// One capability the calling process holds — what `hold` enumerates. Pure data
/// (no kernel types), so a test can construct a cap table by hand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CapInfo {
    pub handle: Handle,
    pub kind: ObjectKind,
    pub rights: Rights,
    pub badge: u64,
    /// The cap's *object* name — a human label set by the object's creator (see
    /// `docs/capability-names-design.md`), resolved for display. Empty for objects
    /// with no name (bootstrap telemetry/span, unnamed endpoints). Opaque: shown,
    /// never used for authority or lookup.
    pub name: alloc::string::String,
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

    /// Read one raw input byte, or `None` when none is available. Unlike
    /// [`read_line`](Self::read_line) this bypasses all line discipline (echo,
    /// backspace) — the byte-at-a-time path the stim driver reads keystrokes
    /// through. The fake replays a scripted session then returns `None` forever;
    /// the on-target backend blocks until a byte arrives, so it never returns
    /// `None`. Defaults to "no input" for backends that source none.
    fn read_byte(&self) -> Option<u8> {
        None
    }

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

    /// Write `bytes` as the entire contents of the file named by the delegated
    /// cap at `file` — truncating to the payload length first, so a shorter save
    /// leaves no stale trailing bytes. Returns `true` on success, `false` if the
    /// cap refuses (a read-only cap → kernel-enforced `SyscallRefused`) or there
    /// is no FS. Backs `fsWrite`; the *shell* resolves the path, creates the file
    /// if absent, and delegates its cap — stim never walks the FS. Defaults to
    /// "can't write" (no filesystem).
    fn fs_write(&self, file: Handle, bytes: &[u8]) -> bool {
        let _ = (file, bytes);
        false
    }

    /// Revoke every capability *derived from* the holding at `handle` — the
    /// transitive reclaim. Returns the number of descendant caps invalidated
    /// (`0` if none were derived), or `None` if the caller holds no cap at
    /// `handle`. The holding at `handle` itself survives. Backs the shell's
    /// `revoke`; ungated, because giving up authority you granted grants nothing.
    /// The on-target backend calls the `Revoke` syscall, which emits a
    /// `CapEvent::Revoked` per swept cap.
    fn revoke(&self, handle: Handle) -> Option<usize>;

    /// Mint a fresh badged capability *derived from* the endpoint at `handle`,
    /// carrying `rights` and tagged with `badge`, into the caller's own table.
    /// Returns the new handle, or `None` if refused (you hold no cap at `handle`,
    /// or it lacks the `MINT` right). Backs the shell's `grant`; capability-
    /// mediated — the *kernel* enforces `MINT`, not an ambient authority. The
    /// on-target backend calls the `MintBadged` syscall, which emits a
    /// `CapEvent::Transferred` recording the parent→child derivation edge.
    fn grant(&self, handle: Handle, badge: u64, rights: Rights) -> Option<Handle>;
}

/// The default backend: a program with no platform installed. Reads nothing and
/// discards output — so a Stitch run that touches no effects (e.g. a pure
/// semantics test) needs no backend wired. The effect analogue of the empty
/// telemetry sink.
#[derive(Default)]
pub struct NullPlatform;

impl Platform for NullPlatform {
    fn revoke(&self, _handle: Handle) -> Option<usize> {
        None // holds nothing, so no handle resolves
    }

    fn grant(&self, _handle: Handle, _badge: u64, _rights: Rights) -> Option<Handle> {
        None // holds nothing to mint from
    }

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
    /// The cap table `hold` reports — mutable so `grant` appends and `revoke`
    /// reclaims, and a follow-up `hold` sees the change.
    caps: core::cell::RefCell<alloc::vec::Vec<CapInfo>>,
    /// Derivation edges (child handle → parent handle), grown by `grant`. Lets the
    /// fake model transitive `revoke` faithfully — the kernel tracks this via the
    /// cap-id spine; the fake needs just enough to reclaim descendants.
    parents: core::cell::RefCell<alloc::collections::BTreeMap<Handle, Handle>>,
    /// Monotonic handle allocator for minted caps — never reused, so a revoked
    /// handle can't be silently re-minted (matching the kernel's generation
    /// bump). `0` = uninitialized; seeded above the scripted caps on first grant.
    next_handle: core::cell::RefCell<Handle>,
    files: alloc::collections::BTreeMap<alloc::string::String, alloc::string::String>,
    /// Scripted raw bytes for `read_byte` — the stim driver's keystroke source.
    /// Drained front-to-back, then `None`.
    bytes: core::cell::RefCell<alloc::collections::VecDeque<u8>>,
    /// Recorded `fs_write`s — `(file-cap handle, payload)` — so a driver test can
    /// assert the saved bytes.
    writes: core::cell::RefCell<alloc::vec::Vec<(Handle, alloc::vec::Vec<u8>)>>,
    /// When set, `fs_write` refuses (returns `false`) — models a read-only cap.
    deny_write: core::cell::Cell<bool>,
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
        Self { caps: core::cell::RefCell::new(caps), ..Self::default() }
    }

    /// Script raw byte input for `read_byte` — the stim driver's keystroke source.
    #[must_use]
    pub fn with_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: core::cell::RefCell::new(bytes.iter().copied().collect()),
            ..Self::default()
        }
    }

    /// The `fs_write`s recorded so far — `(file-cap handle, payload)` in order.
    #[must_use]
    pub fn writes(&self) -> alloc::vec::Vec<(Handle, alloc::vec::Vec<u8>)> {
        self.writes.borrow().clone()
    }

    /// Make every subsequent `fs_write` refuse — models handing stim a read-only
    /// cap (the kernel refusal the on-target backend surfaces as `false`).
    pub fn deny_writes(&self) {
        self.deny_write.set(true);
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

    fn read_byte(&self) -> Option<u8> {
        self.bytes.borrow_mut().pop_front()
    }

    fn fs_write(&self, file: Handle, bytes: &[u8]) -> bool {
        if self.deny_write.get() {
            return false;
        }
        self.writes.borrow_mut().push((file, bytes.to_vec()));
        true
    }

    fn write(&self, text: &str) {
        self.output.borrow_mut().push_str(text);
    }

    fn hold(&self) -> alloc::vec::Vec<CapInfo> {
        self.caps.borrow().clone()
    }

    fn fs_read(&self, name: &str) -> Option<alloc::string::String> {
        self.files.get(name).cloned()
    }

    fn revoke(&self, handle: Handle) -> Option<usize> {
        if !self.caps.borrow().iter().any(|c| c.handle == handle) {
            return None; // no such handle held
        }
        // Sweep the derivation tree below `handle` (transitive), removing the
        // descendants; the holding itself survives — faithful to the kernel.
        let parents = self.parents.borrow();
        let mut doomed = alloc::vec::Vec::new();
        let mut frontier = alloc::vec![handle];
        while let Some(node) = frontier.pop() {
            for (&child, &parent) in parents.iter() {
                if parent == node && !doomed.contains(&child) {
                    doomed.push(child);
                    frontier.push(child);
                }
            }
        }
        drop(parents);
        self.caps.borrow_mut().retain(|c| !doomed.contains(&c.handle));
        self.parents.borrow_mut().retain(|child, _| !doomed.contains(child));
        Some(doomed.len())
    }

    fn grant(&self, handle: Handle, badge: u64, rights: Rights) -> Option<Handle> {
        let mut caps = self.caps.borrow_mut();
        // Must hold the parent endpoint and it must carry MINT.
        let parent = caps.iter().find(|c| c.handle == handle)?;
        if parent.rights & snitchos_abi::rights::MINT == 0 {
            return None;
        }
        // A minted cap points at the *same* object, so it inherits its name.
        let name = parent.name.clone();
        let mut next = self.next_handle.borrow_mut();
        if *next == 0 {
            *next = caps.iter().map(|c| c.handle).max().unwrap_or(0) + 1;
        }
        let new_handle = *next;
        *next += 1;
        caps.push(CapInfo { handle: new_handle, kind: ObjectKind::Endpoint, rights, badge, name });
        drop(caps);
        drop(next);
        self.parents.borrow_mut().insert(new_handle, handle);
        Some(new_handle)
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
    use alloc::collections::VecDeque;
    use alloc::string::String;
    use alloc::vec::Vec;

    use snitchos_user::{console_read, console_write, yield_now};

    #[derive(Default)]
    pub struct RuntimePlatform {
        editor: RefCell<LineEditor>,
        /// Refill buffer for `read_byte`: `console_read` returns chunks, but the
        /// stim driver wants one raw byte at a time. Bytes drain from the front;
        /// a fresh `console_read` refills when it's empty.
        bytes: RefCell<VecDeque<u8>>,
    }

    impl RuntimePlatform {
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl Platform for RuntimePlatform {
        fn read_byte(&self) -> Option<u8> {
            // Raw, one byte at a time — no line editor. Blocks (yielding) until a
            // byte arrives; a UART has no end-of-input, so this never returns
            // `None` on the metal (the driver runs until the process is killed).
            loop {
                if let Some(b) = self.bytes.borrow_mut().pop_front() {
                    return Some(b);
                }
                let mut buf = [0u8; 64];
                let n = console_read(&mut buf);
                if n == 0 {
                    yield_now();
                    continue;
                }
                self.bytes.borrow_mut().extend(&buf[..n]);
            }
        }

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
                    name: d.name_str().into(),
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

        fn fs_write(&self, file: super::Handle, bytes: &[u8]) -> bool {
            use fs_proto::{Op, Request, Response, UserBuf};
            use snitchos_user::Endpoint;

            // `file` is a delegated file cap — the shell resolved the path,
            // created it if absent, and handed us the cap. No walk here.
            let file = Endpoint::from_raw_handle(file as usize);

            // Truncate to the payload length first, so a shorter save leaves no
            // stale trailing bytes. A refused Truncate (no WRITE on the cap) fails
            // the whole op — the read-only enforcement stim's `:w` relies on.
            let trunc = Request::Truncate { len: bytes.len() as u64 };
            match file.call(trunc.encode()) {
                Ok((words, _)) if matches!(Response::decode(Op::Truncate, words), Ok(Response::Count(_))) => {}
                _ => return false,
            }

            // Write the payload in ≤256-byte chunks (the FS `DATA_CAP`).
            let mut offset = 0u64;
            for chunk in bytes.chunks(256) {
                let write = Request::Write {
                    offset,
                    src: UserBuf { ptr: chunk.as_ptr() as u64, len: chunk.len() as u64 },
                };
                match file.call(write.encode()) {
                    Ok((words, _)) if matches!(Response::decode(Op::Write, words), Ok(Response::Count(_))) => {}
                    _ => return false,
                }
                offset += chunk.len() as u64;
            }
            true
        }

        fn revoke(&self, handle: super::Handle) -> Option<usize> {
            // The `Revoke` syscall reclaims every cap derived from `handle` and
            // returns the count, or `usize::MAX` if the handle resolves nothing.
            match snitchos_user::revoke(handle as usize) {
                usize::MAX => None,
                count => Some(count),
            }
        }

        fn grant(&self, handle: super::Handle, badge: u64, rights: super::Rights) -> Option<super::Handle> {
            use snitchos_user::Endpoint;
            // `grant` mints from an endpoint, so wrapping the handle as one is
            // faithful. `mint_badged` (the `MintBadged` syscall) lands the child in
            // our own table and returns its handle, or `Err` on refusal (no MINT).
            Endpoint::from_raw_handle(handle as usize)
                .mint_badged(badge, rights)
                .ok()
                .map(|h| h as super::Handle)
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
        fn revoke(&self, _handle: Handle) -> Option<usize> {
            None
        }
        fn grant(&self, _handle: Handle, _badge: u64, _rights: Rights) -> Option<Handle> {
            None
        }
    }

    #[test]
    fn a_backend_that_sources_no_input_reads_no_byte() {
        // `NullPlatform` uses the trait's default `read_byte` — "no input".
        assert_eq!(NullPlatform.read_byte(), None);
    }

    #[test]
    fn a_backend_with_no_filesystem_cannot_write() {
        // `NullPlatform` uses the trait's default `fs_write` — no FS, no write.
        // (The mirror `-> false` mutant is equivalent — the default *is* `false`.)
        assert!(!NullPlatform.fs_write(0, b"x"));
    }

    #[test]
    fn fake_fs_write_records_the_write_and_reports_success() {
        // The delegated-cap write: stim hands a file-cap handle + the whole
        // payload; the fake records it and reports success.
        let fake = FakePlatform::new();
        assert!(fake.fs_write(7, b"hello world"));
        assert_eq!(fake.writes(), vec![(7u32, b"hello world".to_vec())]);
    }

    #[test]
    fn fake_fs_write_reports_refusal_when_the_cap_denies_it() {
        // A read-only cap → the kernel refuses the write; the fake models that as
        // `false`, recording nothing.
        let fake = FakePlatform::new();
        fake.deny_writes();
        assert!(!fake.fs_write(7, b"nope"));
        assert!(fake.writes().is_empty());
    }

    #[test]
    fn fake_read_byte_replays_scripted_bytes_then_none() {
        // Raw single-byte input for the stim driver: the fake hands back each
        // scripted byte in order, then `None` forever (a finite session).
        let fake = FakePlatform::with_bytes(b"jk:");
        assert_eq!(fake.read_byte(), Some(b'j'));
        assert_eq!(fake.read_byte(), Some(b'k'));
        assert_eq!(fake.read_byte(), Some(b':'));
        assert_eq!(fake.read_byte(), None);
        assert_eq!(fake.read_byte(), None); // stays exhausted
    }

    #[test]
    fn parse_rights_maps_names_to_the_bitmask() {
        use snitchos_abi::rights as r;
        assert_eq!(parse_rights("EMIT"), Some(r::EMIT));
        assert_eq!(parse_rights("SEND"), Some(r::SEND));
        assert_eq!(parse_rights("send"), Some(r::SEND)); // case-insensitive
        assert_eq!(parse_rights("RECV"), Some(r::RECV));
        assert_eq!(parse_rights("MINT"), Some(r::MINT));
        assert_eq!(parse_rights("SIGNAL"), Some(r::SIGNAL));
        assert_eq!(parse_rights("WAIT"), Some(r::WAIT));
        assert_eq!(parse_rights("SEND RECV"), Some(r::SEND | r::RECV));
        assert_eq!(parse_rights("SEND,RECV"), Some(r::SEND | r::RECV)); // comma too
        assert_eq!(parse_rights("MINT|SEND"), Some(r::MINT | r::SEND)); // and pipe
    }

    #[test]
    fn parse_rights_rejects_empty_and_unknown_names() {
        assert_eq!(parse_rights(""), None);
        assert_eq!(parse_rights("   "), None);
        assert_eq!(parse_rights("SEND FLY"), None); // one bad name fails the whole thing
    }

    #[test]
    fn colorize_rights_wraps_each_glyph_in_its_ansi_color() {
        let c = |cell| colorize_rights(true, cell); // native (kernel-built) cell
        assert_eq!(c("🪴"), "\u{1b}[32m🪴\u{1b}[0m"); // green mint
        assert_eq!(c("👀"), "\u{1b}[34m👀\u{1b}[0m"); // blue read
        assert_eq!(c("📝"), "\u{1b}[33m📝\u{1b}[0m"); // amber write
        assert_eq!(c("🪴👀📝"), "\u{1b}[32m🪴\u{1b}[0m\u{1b}[34m👀\u{1b}[0m\u{1b}[33m📝\u{1b}[0m");
    }

    #[test]
    fn colorize_rights_only_colors_native_cells() {
        // The whole point of keying on provenance: a glyph a user (or another
        // program) prints is never colored, because its cell isn't native.
        assert_eq!(colorize_rights(false, "🪴"), "🪴");
        assert_eq!(colorize_rights(false, "🪴👀📝"), "🪴👀📝");
        // A native cell with no glyphs (another cap column) passes through too.
        assert_eq!(colorize_rights(true, "Endpoint"), "Endpoint");
        assert_eq!(colorize_rights(true, ""), "");
    }

    #[test]
    fn rights_glyphs_maps_each_category_to_its_emoji() {
        use snitchos_abi::rights as r;
        assert_eq!(rights_glyphs(0), "");
        assert_eq!(rights_glyphs(r::MINT), "🪴");
        assert_eq!(rights_glyphs(r::RECV), "👀");
        assert_eq!(rights_glyphs(r::WAIT), "👀"); // WAIT is a read (consumer) right
        assert_eq!(rights_glyphs(r::EMIT), "📝");
        assert_eq!(rights_glyphs(r::SEND), "📝");
        assert_eq!(rights_glyphs(r::SIGNAL), "📝"); // SIGNAL is a write (producer) right
    }

    #[test]
    fn rights_glyphs_lists_mint_then_read_then_write_and_dedupes_a_category() {
        use snitchos_abi::rights as r;
        // SEND|RECV is one read and one write — a single 👀 and a single 📝.
        assert_eq!(rights_glyphs(r::SEND | r::RECV), "👀📝");
        assert_eq!(rights_glyphs(r::MINT | r::RECV | r::SEND), "🪴👀📝");
        // Two write rights collapse to one 📝 (category, not per-bit).
        assert_eq!(rights_glyphs(r::EMIT | r::SEND), "📝");
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
            CapInfo { handle: 2, kind: ObjectKind::Endpoint, rights: 0b0110, badge: 0, name: String::new() },
            // A badged endpoint — what a file cap looks like (no `File` kind).
            CapInfo { handle: 3, kind: ObjectKind::Endpoint, rights: 0b0010, badge: 7, name: String::new() },
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
