//! The kernel ↔ userspace ABI.
//!
//! Shared by the kernel (syscall dispatch) and userspace programs (the
//! `ecall` site) so neither side hard-codes a magic number. `no_std`,
//! no dependencies — just the contract.
//!
//! The kernel surface is capability-mediated: a program names a capability by an
//! opaque handle (an index into *its own* `CapTable`) and the kernel validates
//! every use against that table — no ambient authority. See
//! `docs/capability-system-design.md`.
//!
//! Calling convention (RISC-V, Linux/SBI-style): syscall number in `a7`,
//! arguments in `a0..`, result in `a0`. By convention `a0` on return is `0` (or
//! a useful value) on success and `usize::MAX` on a refused/denied call.

#![no_std]

/// Syscall numbers, passed in register `a7` at the `ecall`.
///
/// Postcard-free, plain integers — this is a register ABI, not a wire format,
/// so the numbers are **not** frozen the way the postcard `Frame` discriminants
/// are: the kernel and userspace rebuild from this crate together (the user ELFs
/// are embedded into the kernel image at build time) and nothing persists a
/// syscall number. New calls normally append, but a removed call may be deleted
/// and the survivors renumbered — as `Invoke` was (debt #2 Step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Syscall {
    /// Terminate the calling process with exit status `a0` (an `i32`). Does not
    /// return — the kernel records the status (reaping bookkeeping, v0.12), wakes
    /// any parent blocked in [`Self::Wait`] on this task, and switches the hart to
    /// its next ready task. (Not capability-mediated: a process can always end
    /// itself. v0.7b leaks the address space + caps on exit; reclamation is later.)
    Exit = 0,
    /// Voluntarily yield the CPU. The kernel runs `yield_now()` on the
    /// caller's behalf — switching to the next ready task — then returns
    /// here on a later reschedule, so control resumes after the `ecall`.
    /// Not capability-mediated: yielding grants no authority, it only
    /// relinquishes the CPU. The cooperative path; preemption (v0.8) is
    /// the involuntary counterpart.
    Yield = 1,
    /// Open a span. `a0` = `SpanSink` capability handle, `a1` = pointer to
    /// the span name in user memory, `a2` = its length. The kernel copies
    /// and interns the name, opens a span on the caller's task cursor, and
    /// returns an opaque span id in `a0` (or an error sentinel if refused).
    SpanOpen = 2,
    /// Close a span previously opened with [`Self::SpanOpen`]. `a0` = the
    /// span id the open returned. Emits the matching `SpanEnd`.
    SpanClose = 3,
    /// Map a fresh anonymous memory region. `a0` = bytes requested
    /// (page-aligned by the runtime). The kernel maps that many bytes of fresh
    /// zeroed frames into the process's address space and returns the region's
    /// **base** VA in `a0`, or `usize::MAX` if refused (out of frames, or past
    /// the per-process memory cap). mmap-shaped, not `brk`: a region is
    /// returned (individually unmappable later, and a `MemoryRegion` capability
    /// eventually), and the runtime allocator (`talc`) `claim`s each one — it
    /// does not assume regions abut, so the kernel may place them disjointly.
    MapAnon = 4,
    /// Write bytes to the debug/stdout channel. `a0` = pointer to the bytes,
    /// `a1` = length. The kernel copies them out and emits a snitched `Log`
    /// wire frame (so stdout is observable). Returns bytes written in `a0`, or
    /// `usize::MAX` if refused (bad pointer). Backs `println!`.
    DebugWrite = 5,
    /// Send an inline message over a synchronous IPC endpoint (v0.9). `a0` =
    /// `Endpoint` capability handle (needs `SEND`), `a1..=a4` = the four
    /// message words. Rendezvous semantics: if a receiver is waiting the
    /// message is delivered and both proceed; otherwise the sender blocks until
    /// one arrives. Returns `0` in `a0` on success, `usize::MAX` if refused
    /// (bad/again wrong-rights/wrong-object handle).
    Send = 6,
    /// Receive an inline message from a synchronous IPC endpoint (v0.9). `a0` =
    /// `Endpoint` capability handle (needs `RECV`). Blocks until a sender
    /// rendezvouses; returns `0` in `a0` and the four message words in
    /// `a1..=a4`, or `usize::MAX` in `a0` if refused. For an RPC `call`, the
    /// reply-cap handle is returned in `a5` (`0` for a one-way `send`).
    Receive = 7,
    /// RPC `call` over a synchronous endpoint (v0.9b): send a request **and**
    /// block for a reply. `a0`=`Endpoint` handle (needs `SEND`), `a1..=a4`=
    /// request words. The kernel mints a one-shot reply cap into the receiver
    /// at the rendezvous; the caller parks until `reply`. Returns `0` in `a0`
    /// and the reply words in `a1..=a4` (or `usize::MAX` if refused).
    Call = 8,
    /// Answer an RPC (v0.9b). `a0`=reply-cap handle (from `receive`'s `a5`),
    /// `a1..=a4`=response words. Wakes the blocked caller and **consumes** the
    /// one-shot reply cap (a second `reply` is refused). Returns `0`, or
    /// `usize::MAX` if the handle is not a live reply cap.
    Reply = 9,
    /// Fused `reply`-then-`receive` (v0.9b) — the server hot path. `a0` =
    /// `Endpoint` handle (needs `RECV`), `a5` = the previous request's reply
    /// handle (`0` on the first iteration — no prior reply), `a1..=a4` = the
    /// response to that previous request. Replies the previous caller (if any),
    /// then blocks receiving the next request: returns `0` in `a0`, the next
    /// request words in `a1..=a4`, and its reply handle in `a5`. One trap
    /// instead of two per request.
    ReplyRecv = 10,
    /// Mint a badged `SEND` capability for an endpoint the caller owns (v0.9c).
    /// `a0` = endpoint handle (needs `MINT`), `a1` = the server-chosen `badge`
    /// (u64), `a2` = the requested rights bits. The kernel derives a child cap
    /// naming the same endpoint, stamped with the badge + rights, and inserts it
    /// into the caller's own table. Returns the new handle in `a0`, or
    /// `usize::MAX` if refused (handle lacks `MINT` / names no endpoint). The
    /// minted cap is handed to a client via cap-transfer (a later step).
    MintBadged = 11,
    /// Copy bytes **from a blocked caller's** address space into the server's
    /// own (v0.10, option D). `a0` = a one-shot reply-cap handle the server
    /// holds (names the blocked caller — authorizes the access; **not**
    /// consumed), `a1` = source VA in the *caller's* space, `a2` = length, `a3`
    /// = destination VA in the *server's* space. The kernel walks the caller's
    /// page table, validates both ranges (user-half, mapped, `R|U` source /
    /// `W|U` dest), and copies through the linear map. Returns bytes copied in
    /// `a0`, or `usize::MAX` if refused. The `write`/`create`-name half.
    CopyFromCaller = 12,
    /// Copy bytes **to a blocked caller's** address space from the server's own
    /// (v0.10, option D). The mirror of [`Self::CopyFromCaller`]: `a0` = reply
    /// handle, `a1` = source VA in the *server's* space, `a2` = length, `a3` =
    /// destination VA in the *caller's* space. The `read` half.
    CopyToCaller = 13,
    /// Drain buffered console (UART) input into the caller's buffer (v0.11
    /// Tier-0). `a0` = destination pointer in the caller's space, `a1` = max
    /// length. The kernel copies up to that many buffered input bytes in and
    /// returns the count in `a0` (0 if nothing is buffered — non-blocking), or
    /// `usize::MAX` on a bad/unwritable range. Ambient, like `DebugWrite`: the
    /// console terminal is not a capability (cap-mediated input is Tier-1).
    ConsoleRead = 14,
    /// Spawn a new userspace process, delegating a chosen subset of the caller's
    /// own capabilities to it (v0.11 — `spawn(program, caps)`). `a0` = program
    /// selector (an embedded-program id), `a1` = pointer in the caller's space to
    /// a `[u32; N]` array of cap handles to delegate, `a2` = `N`. The kernel
    /// resolves every handle in the *caller's* table (refusing the whole spawn if
    /// any is unheld — no forging, no partial delegation), builds the child with
    /// those caps plus its own bootstrap telemetry/span, and returns the child's
    /// task id in `a0` (or `usize::MAX` on refusal).
    Spawn = 15,
    /// Register a userspace-named metric (debt #2). `a0` = `TelemetrySink`
    /// capability handle (the gate — needs `EMIT`; holding it is the authority
    /// to name metrics), `a1` = pointer to the metric name in user memory, `a2`
    /// = its length, `a3` = the metric kind (`0` = Counter, `1` = Gauge, `2` =
    /// Histogram — the `protocol::MetricKind` discriminant order). The kernel
    /// copies + interns the name into a **fresh** id (no cross-process dedup —
    /// each emitter gets its own `StringId`), stores it in the *caller's*
    /// per-process metric table, and returns an opaque metric handle (an index
    /// into that table) in `a0`. Refused with `usize::MAX` on a bad cap, bad
    /// user range, bad UTF-8, or once the table is at its `MAX_METRIC_NAMES`
    /// quota. The handle — not the name — is what [`Self::EmitMetric`] presents,
    /// so the string crosses the boundary exactly once.
    RegisterMetric = 16,
    /// Emit a sample to a metric this process registered (debt #2). `a0` = the
    /// metric handle [`Self::RegisterMetric`] returned, `a1` = the value
    /// (`i64`). The kernel resolves the handle against the *caller's own* metric
    /// table → the bound `StringId` and emits the sample. A handle the caller
    /// never registered (out of range) is **refused** (`usize::MAX`, snitched as
    /// `SyscallRefused`), never silently emitted — possession of a valid handle
    /// is the authority, and a handle is unforgeable as an index into the
    /// issuing table. No cap argument: the registration was already cap-gated,
    /// so the hot emit path is a bare table lookup.
    EmitMetric = 17,
    /// Wait for a child to exit and collect its status (v0.12). `a0` = the child's
    /// task id (as returned by [`Self::Spawn`]). Blocks until that task `Exit`s,
    /// then returns its exit status in `a0`; if the child had already exited, the
    /// status is returned immediately (the zombie is reaped). Same-hart in v0.12
    /// (a parent waits on a child it spawned on its own hart); cross-hart wait is
    /// a deferred follow-on.
    Wait = 18,
    /// Write bytes to the interactive console (UART TX) for U-mode. `a0` =
    /// pointer, `a1` = length. Copies the bytes out (range-validated,
    /// SUM-guarded), validates UTF-8, and writes them to the same UART the kernel
    /// `print!`s to — the human terminal, distinct from the `DebugWrite`
    /// telemetry channel. Returns bytes written in `a0` (or `u64::MAX` on a bad
    /// pointer / non-UTF-8). Ambient, the mirror of [`Self::ConsoleRead`] — the
    /// shell is the trusted session root and writes its own terminal directly;
    /// capability-mediated console output is the Tier-1 server story.
    ConsoleWrite = 19,
    /// Read the monotonic clock — the kernel `time` CSR tick count (the same
    /// source spans are timestamped from). No arguments; returns the current tick
    /// count in `a0`. Ambient (reading a clock is not an authority). Ticks are at
    /// the platform timebase (10 MHz on QEMU `virt` → 1 tick = 0.1 µs). Lets
    /// userspace time its own work without a span round-trip; the stdlib
    /// `Instant::now()` rides on it.
    ClockNow = 20,
    /// Create a fresh notification and return a capability handle to it (v0.12).
    /// No arguments; returns in `a0` a handle to a new `Notification` cap the
    /// caller holds with both `SIGNAL` and `WAIT` rights (the caller then
    /// attenuates + delegates the end(s) it wants). Ambient, like [`Self::MapAnon`]:
    /// making your own notification needs no prior authority; delegating the ends
    /// is where the authority split happens.
    NotifyCreate = 21,
    /// Signal a notification — the producer end (v0.12). `a0` = a `Notification`
    /// capability handle (needs `SIGNAL`), `a1` = a userspace-defined bit mask.
    /// OR-s the mask into the notification's pending bits and wakes any parked
    /// waiter; never blocks. Returns `0` in `a0`, or `usize::MAX` if refused
    /// (bad handle / lacks `SIGNAL` / wrong object).
    Signal = 22,
    /// Wait on a notification — the consumer end (v0.12). `a0` = a `Notification`
    /// capability handle (needs `WAIT`). If bits are pending, returns them in `a0`
    /// (read-and-cleared); otherwise blocks until a [`Self::Signal`] arrives, then
    /// returns the bits. `usize::MAX` if refused (bad handle / lacks `WAIT` / wrong
    /// object / another task already waiting — one waiter per notification).
    WaitNotify = 23,
    /// Wait for **any** child to exit and collect its id + status (v0.13) — the
    /// supervising-parent variant of [`Self::Wait`]. No arguments. Blocks until
    /// any task this caller spawned `Exit`s (returning immediately if one already
    /// has), then returns the exited child's status in `a0` and its task id in
    /// `a1`; the zombie is reaped. Used by `init` to supervise children whose ids
    /// it needn't track and that exit in any order. Same-hart in v0.13.
    WaitAny = 24,
    /// Create a fresh IPC endpoint and return an owning capability to it (v0.13).
    /// No arguments; returns in `a0` a handle to a new `Endpoint` cap the caller
    /// holds with `RECV | MINT` — it owns the endpoint (may receive, and mint
    /// badged `SEND` caps for clients). Ambient, like [`Self::NotifyCreate`]:
    /// manufacturing your own endpoint needs no prior authority; delegating
    /// `SEND`/`RECV` ends is where the authority split happens. Lets a process
    /// (e.g. `init`) build its own IPC world instead of the kernel pre-creating it.
    EndpointCreate = 25,
    /// Spawn a new userspace process from a **caller-supplied ELF image** (vs
    /// [`Self::Spawn`], which selects a kernel-embedded program by id) — the path
    /// for running an executable read out of the filesystem. `a0` = pointer in
    /// the caller's space to the ELF bytes, `a1` = their length, `a2` = pointer to
    /// a `[u32; N]` array of cap handles to delegate, `a3` = `N`. The kernel
    /// copies the image in, validates + loads it, delegates exactly those caps
    /// (all-or-nothing, like `Spawn`) plus bootstrap telemetry/span, and returns
    /// the child's task id in `a0` (or `usize::MAX` on refusal — bad range,
    /// oversized image, malformed ELF, or an unheld handle).
    SpawnImage = 26,
    /// Enumerate the caller's **own** capability table — introspection, not new
    /// authority (a process may always see what it already holds; ambient like
    /// [`Self::ClockNow`]). `a0` = pointer in the caller's space to a `[CapDesc; N]`
    /// buffer, `a1` = `N` (its capacity in entries). The kernel writes up to `N`
    /// live capabilities as packed [`CapDesc`] records (a *packed hitch* — the
    /// schema is this ABI, not shipped inline) and returns the **total** live count
    /// in `a0` (so a too-small buffer is detectable: returned `>` `N`), or
    /// `usize::MAX` on a bad/unwritable buffer range. Backs the shell's `hold`.
    CapList = 27,
    /// Revoke the capabilities **derived from** the holding `a0` (a [`Handle`]) names
    /// — its transitive descendants in the cap derivation tree, wherever they were
    /// delegated. Authority is implicit: holding the handle *is* the right to reclaim
    /// what you granted from it. The caller's own holding survives; each revoked
    /// descendant's handle goes stale and emits a `CapEvent::Revoked`. Returns the
    /// number revoked in `a0`, or `usize::MAX` if the handle resolves nothing. The
    /// reclaim half of the shell powerbox's grant→use→reclaim. (`a0` = handle.)
    Revoke = 28,
    /// Read the platform timebase frequency in Hz — the rate [`Self::ClockNow`]'s
    /// ticks advance at (the DTB `timebase-frequency`; 10 MHz on QEMU `virt`). No
    /// arguments; returns the frequency in `a0`. Ambient, like `ClockNow`. Lets the
    /// stdlib convert clock ticks to a real `Duration` without hardcoding the
    /// platform rate — `Instant::elapsed()` divides a tick delta by this.
    ClockFreq = 29,
    /// Terminate a child process named by an `Object::Process` capability (`a0` =
    /// [`Handle`]) the caller holds with the `KILL` right — minted into the parent's
    /// table at [`Self::Spawn`]. Tears down + reaps the target (its `WaitAny` parent
    /// wakes with a killed status). Capability-authorized, so it composes to a
    /// sub-supervisor granted `KILL` over its subtree (supervision v2a).
    Kill = 30,
}

impl Syscall {
    /// Resolve a raw `a7` value to a known syscall, or `None` if the
    /// number names nothing. The kernel uses this to reject unknown
    /// syscalls rather than trusting the register blindly.
    #[must_use]
    pub const fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::Exit),
            1 => Some(Self::Yield),
            2 => Some(Self::SpanOpen),
            3 => Some(Self::SpanClose),
            4 => Some(Self::MapAnon),
            5 => Some(Self::DebugWrite),
            6 => Some(Self::Send),
            7 => Some(Self::Receive),
            8 => Some(Self::Call),
            9 => Some(Self::Reply),
            10 => Some(Self::ReplyRecv),
            11 => Some(Self::MintBadged),
            12 => Some(Self::CopyFromCaller),
            13 => Some(Self::CopyToCaller),
            14 => Some(Self::ConsoleRead),
            15 => Some(Self::Spawn),
            16 => Some(Self::RegisterMetric),
            17 => Some(Self::EmitMetric),
            18 => Some(Self::Wait),
            19 => Some(Self::ConsoleWrite),
            20 => Some(Self::ClockNow),
            21 => Some(Self::NotifyCreate),
            22 => Some(Self::Signal),
            23 => Some(Self::WaitNotify),
            24 => Some(Self::WaitAny),
            25 => Some(Self::EndpointCreate),
            26 => Some(Self::SpawnImage),
            27 => Some(Self::CapList),
            28 => Some(Self::Revoke),
            29 => Some(Self::ClockFreq),
            30 => Some(Self::Kill),
            _ => None,
        }
    }
}

/// Capability rights bits — the bitmask carried on a capability and on the
/// `CapEvent` wire frame, and the rights a [`Syscall::MintBadged`] requests.
/// The single source of truth: the kernel's typed `kernel_core::cap::Rights`
/// wraps these, and userspace passes them raw. Neither side hard-codes the
/// values. Binary literals (next bit `0b1_0000`) — no `1 << n` to misread.
pub mod rights {
    /// May emit telemetry through a `TelemetrySink`.
    pub const EMIT: u32 = 0b0001;
    /// May `send` on an `Endpoint`.
    pub const SEND: u32 = 0b0010;
    /// May `receive` on an `Endpoint`.
    pub const RECV: u32 = 0b0100;
    /// May mint badged `SEND` caps for an `Endpoint` the holder owns (v0.9c).
    pub const MINT: u32 = 0b1000;
    /// May `signal` a `Notification` — the producer end (v0.12).
    pub const SIGNAL: u32 = 0b1_0000;
    /// May `wait` on a `Notification` — the consumer end (v0.12). Disjoint from
    /// `SIGNAL` so a cap can grant either end or both.
    pub const WAIT: u32 = 0b10_0000;
    /// May `kill` the process an `Object::Process` cap names (supervision v2a).
    pub const KILL: u32 = 0b100_0000;
}

/// Object-kind discriminants for a [`CapDesc`]'s `kind` field — what sort of
/// object a capability names. These mirror the variant order of
/// `protocol::CapObject` (the telemetry-wire encoding); both are positional, so
/// keep them in step. Note there is no `File` kind: a file capability is a
/// badged `Endpoint` (`ENDPOINT` with `badge != 0`).
pub mod object_kind {
    /// A `TelemetrySink` — may `emit`.
    pub const TELEMETRY_SINK: u32 = 0;
    /// A `SpanSink` — may open spans.
    pub const SPAN_SINK: u32 = 1;
    /// An IPC `Endpoint` (a badged one is a file or per-object cap).
    pub const ENDPOINT: u32 = 2;
    /// A one-shot `Reply` cap held by a server mid-RPC.
    pub const REPLY: u32 = 3;
    /// A `Notification`.
    pub const NOTIFICATION: u32 = 4;
    /// A `Process` — a child's lifecycle handle, carrying `KILL` (supervision v2a).
    pub const PROCESS: u32 = 5;
}

/// One capability in a process's own table, as written by [`Syscall::CapList`] —
/// a **packed hitch**: positional, with the schema being *this struct* rather
/// than shipped inline. Field order is deliberately padding-free (all four `u32`s
/// before the `u64`), so the kernel never copies uninitialized bytes out to
/// userspace. `kind` is an [`object_kind`] discriminant, `rights` a [`rights`]
/// bitmask, and `badge` the endpoint badge (`0` unless this names a badged
/// endpoint, e.g. a file cap). `reserved` is `0` today — room for `cap_id` /
/// multiplicity later. Userspace `unhitch`es a buffer of these into named
/// records (the `hold` lift); the kernel and userspace agree on this layout.
// `#[derive(Pod)]` compile-checks the padding-free + all-scalar + `repr(C)`
// invariants the doc comment claims, so the byte cast in `CapList` is sound by
// construction rather than by hand-audit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, hitch_pod::Pod)]
pub struct CapDesc {
    pub handle: u32,
    pub kind: u32,
    pub rights: u32,
    pub reserved: u32,
    pub badge: u64,
    /// The cap's *object* name (see `docs/capability-names-design.md`) — a human
    /// label, NUL-padded UTF-8, empty (all-zero) if the object is unnamed. Trails
    /// the `u64` so the struct stays 48 bytes, 8-aligned, padding-free (the
    /// `Pod` derive enforces it). Read with [`CapDesc::name_str`]; the kernel packs
    /// it with [`pack_name`]. Opaque: shown, never used for authority or lookup.
    pub name: [u8; CAP_NAME_LEN],
}

/// The maximum length, in bytes, of a capability object name — the inline bound
/// that keeps [`CapDesc`] (and the `CapEvent` wire frame) fixed-size. UTF-8;
/// overlong names truncate on a character boundary (see [`pack_name`]).
pub const CAP_NAME_LEN: usize = 24;

/// Pack a name string into a fixed `[u8; CAP_NAME_LEN]`, NUL-padded, truncating on
/// a UTF-8 character boundary so the stored bytes are always valid UTF-8. The
/// kernel calls this when a creator names an object; the inverse is
/// [`CapDesc::name_str`].
#[must_use]
pub fn pack_name(name: &str) -> [u8; CAP_NAME_LEN] {
    let take = utf8_chunk_end(name.as_bytes(), CAP_NAME_LEN);
    let mut out = [0u8; CAP_NAME_LEN];
    out[..take].copy_from_slice(&name.as_bytes()[..take]);
    out
}

impl CapDesc {
    /// The object name as a string (see [`name_str`]).
    #[must_use]
    pub fn name_str(&self) -> &str {
        name_str(&self.name)
    }
}

/// A NUL-padded name buffer as a string — the UTF-8 prefix before the first NUL
/// ([`pack_name`] NUL-pads). Empty if unnamed. Shared by [`CapDesc`] and the
/// `CapEvent` wire frame, which carry an object name the same way.
#[must_use]
pub fn name_str(name: &[u8]) -> &str {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..end]).unwrap_or("")
}

/// The number of inline `u64` words a single IPC message carries. The single
/// source of truth shared by the kernel, the userspace runtime, and any wire
/// protocol layered on IPC (e.g. `fs-proto`). Larger payloads cross via a
/// copy/`MemoryRegion` mechanism, not by widening this.
pub const MSG_WORDS: usize = 4;

/// The length of the longest prefix of `bytes` that is at most `max` bytes and
/// does not split a UTF-8 character — so, for valid-UTF-8 input, the prefix is
/// itself valid UTF-8. Callers chunk console output with this: `ConsoleWrite`
/// validates each syscall's bytes as UTF-8 (it forwards through the kernel's
/// `str`-based console), so a naive `chunks(max)` byte-split would hand the
/// kernel a partial character and the whole write would be refused. Always
/// returns at least 1 for non-empty input, so a chunking loop makes progress
/// even in the degenerate case of a single char wider than `max`.
#[must_use]
pub fn utf8_chunk_end(bytes: &[u8], max: usize) -> usize {
    if bytes.len() <= max {
        return bytes.len();
    }
    // `bytes[max]` starts the next chunk; if it's a UTF-8 continuation byte
    // (`0b10xx_xxxx`) the char straddles the boundary, so back up to its start.
    let mut end = max;
    while end > 0 && bytes[end] & 0b1100_0000 == 0b1000_0000 {
        end -= 1;
    }
    if end == 0 { max } else { end }
}

/// Pack a raw (possibly boundary-truncated) UTF-8 name buffer — e.g. the bytes a
/// fixed-size syscall read copied out of user memory — into a `[u8; CAP_NAME_LEN]`,
/// distinguishing the two ways a fixed-size read can end mid-UTF-8:
///
/// - an **incomplete trailing sequence** (the read split a codepoint at the byte
///   bound): keep the valid prefix, truncated on the last char boundary — the
///   cap-names design's "truncate on a char boundary";
/// - a **genuinely invalid byte** mid-string: reject (`None`) — a garbage name is
///   not a long name, so it stays a refusal.
///
/// The `&[u8]` counterpart of [`pack_name`], for callers (the `EndpointCreate`
/// syscall) that hold raw bytes rather than a `&str`. A naive `from_utf8` would
/// refuse the first case too, rejecting a valid name whose 24th byte happened to
/// split a codepoint.
#[must_use]
pub fn pack_name_bytes(bytes: &[u8]) -> Option<[u8; CAP_NAME_LEN]> {
    let valid = match core::str::from_utf8(bytes) {
        Ok(s) => s,
        // `error_len() == None` = an incomplete trailing sequence: the read split a
        // codepoint at its end. Keep the valid prefix (`valid_up_to()` is a char
        // boundary of already-validated bytes, so this re-validation can't fail).
        Err(e) if e.error_len().is_none() => core::str::from_utf8(&bytes[..e.valid_up_to()]).ok()?,
        // A genuinely invalid byte mid-string: refuse.
        Err(_) => return None,
    };
    Some(pack_name(valid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_name_round_trips_and_defaults_empty() {
        assert_eq!(CapDesc::default().name_str(), "");
        let d = CapDesc { name: pack_name("fs"), ..CapDesc::default() };
        assert_eq!(d.name_str(), "fs");
    }

    #[test]
    fn cap_name_truncates_at_the_24_byte_bound() {
        // 36 ASCII bytes → the first 24 (a..x) survive.
        let d = CapDesc { name: pack_name("abcdefghijklmnopqrstuvwxyz0123456789"), ..CapDesc::default() };
        assert_eq!(d.name_str(), "abcdefghijklmnopqrstuvwx");
        assert_eq!(d.name_str().len(), CAP_NAME_LEN);
    }

    #[test]
    fn pack_name_bytes_accepts_valid_utf8() {
        let packed = pack_name_bytes(b"fs").expect("valid UTF-8 name");
        assert_eq!(name_str(&packed), "fs");
    }

    #[test]
    fn pack_name_bytes_truncates_a_codepoint_split_at_the_bound() {
        // A fixed 24-byte read of "abc..w" (23 ASCII) + 'é' (2 bytes) copies the
        // 23 ASCII plus the *first* byte of 'é' — an incomplete trailing sequence.
        // The design says truncate on a char boundary (drop the partial 'é'), NOT
        // refuse a valid name. Raw `from_utf8` would reject this.
        let full = "abcdefghijklmnopqrstuvwé".as_bytes(); // 25 bytes
        let cut = &full[..CAP_NAME_LEN]; // 24 bytes: 23 ASCII + first byte of 'é'
        let packed = pack_name_bytes(cut).expect("a boundary split truncates, not refuses");
        assert_eq!(name_str(&packed), "abcdefghijklmnopqrstuvw");
    }

    #[test]
    fn pack_name_bytes_refuses_a_genuinely_invalid_byte() {
        // 0xFF never appears in valid UTF-8, mid-string (not a boundary artifact) —
        // a garbage name is refused, not silently truncated to its prefix.
        assert_eq!(pack_name_bytes(b"fs\xffbar"), None);
    }

    #[test]
    fn cap_name_truncation_never_splits_a_char() {
        // "abc..w" is 23 bytes; the 2-byte 'é' straddling the 24-byte bound is
        // dropped whole, so the stored name stays valid UTF-8.
        let d = CapDesc { name: pack_name("abcdefghijklmnopqrstuvwé"), ..CapDesc::default() };
        assert_eq!(d.name_str(), "abcdefghijklmnopqrstuvw");
    }

    #[test]
    fn utf8_chunk_end_returns_whole_slice_when_it_fits() {
        assert_eq!(utf8_chunk_end(b"abcd", 4), 4);
        assert_eq!(utf8_chunk_end(b"ab", 10), 2);
        assert_eq!(utf8_chunk_end(b"", 4), 0);
    }

    #[test]
    fn utf8_chunk_end_never_splits_a_multibyte_char() {
        // "a─b" = [0x61, 0xE2,0x94,0x80, 0x62] — the box-drawing '─' is 3 bytes.
        let s = "a─b".as_bytes();
        // A boundary landing inside '─' backs up to just before it.
        assert_eq!(utf8_chunk_end(s, 2), 1); // don't split 'a─'
        assert_eq!(utf8_chunk_end(s, 3), 1);
        // A boundary just past '─' keeps the whole char.
        assert_eq!(utf8_chunk_end(s, 4), 4);
        // Landing exactly on a char start (the 'a') takes just it.
        assert_eq!(utf8_chunk_end(s, 1), 1);
    }

    #[test]
    fn utf8_chunk_end_stays_in_bounds_on_malformed_input() {
        // Defensive: a slice starting with continuation bytes isn't valid UTF-8,
        // but the `end > 0` guard must stop the backup at the start rather than
        // underflow. (Real console output is always valid UTF-8.)
        assert_eq!(utf8_chunk_end(&[0x80, 0x80, 0x80], 1), 1);
    }

    #[test]
    fn utf8_chunk_end_makes_progress_on_a_char_longer_than_max() {
        // A 4-byte emoji with max below one char: still return >=1 to avoid a
        // stuck loop (degenerate; real writes use max=256 >> 4).
        let emoji = "🪴".as_bytes(); // 4 bytes
        assert!(utf8_chunk_end(emoji, 2) >= 1);
    }

    #[test]
    fn syscall_numbers_round_trip() {
        assert_eq!(Syscall::Exit as usize, 0);
        assert_eq!(Syscall::Yield as usize, 1);
        assert_eq!(Syscall::SpanOpen as usize, 2);
        assert_eq!(Syscall::SpanClose as usize, 3);
        assert_eq!(Syscall::MapAnon as usize, 4);
        assert_eq!(Syscall::DebugWrite as usize, 5);
        assert_eq!(Syscall::Send as usize, 6);
        assert_eq!(Syscall::Receive as usize, 7);
        assert_eq!(Syscall::Call as usize, 8);
        assert_eq!(Syscall::Reply as usize, 9);
        assert_eq!(Syscall::ReplyRecv as usize, 10);
        assert_eq!(Syscall::MintBadged as usize, 11);
        assert_eq!(Syscall::CopyFromCaller as usize, 12);
        assert_eq!(Syscall::CopyToCaller as usize, 13);
        assert_eq!(Syscall::ConsoleRead as usize, 14);
        assert_eq!(Syscall::Spawn as usize, 15);
        assert_eq!(Syscall::RegisterMetric as usize, 16);
        assert_eq!(Syscall::EmitMetric as usize, 17);
        assert_eq!(Syscall::Wait as usize, 18);
        assert_eq!(Syscall::ConsoleWrite as usize, 19);
        assert_eq!(Syscall::ClockNow as usize, 20);
        assert_eq!(Syscall::NotifyCreate as usize, 21);
        assert_eq!(Syscall::Signal as usize, 22);
        assert_eq!(Syscall::WaitNotify as usize, 23);
        assert_eq!(Syscall::WaitAny as usize, 24);
        assert_eq!(Syscall::EndpointCreate as usize, 25);
        assert_eq!(Syscall::SpawnImage as usize, 26);
        assert_eq!(Syscall::CapList as usize, 27);
        assert_eq!(Syscall::Revoke as usize, 28);
        assert_eq!(Syscall::ClockFreq as usize, 29);

        assert_eq!(Syscall::from_usize(0), Some(Syscall::Exit));
        assert_eq!(Syscall::from_usize(1), Some(Syscall::Yield));
        assert_eq!(Syscall::from_usize(2), Some(Syscall::SpanOpen));
        assert_eq!(Syscall::from_usize(3), Some(Syscall::SpanClose));
        assert_eq!(Syscall::from_usize(4), Some(Syscall::MapAnon));
        assert_eq!(Syscall::from_usize(5), Some(Syscall::DebugWrite));
        assert_eq!(Syscall::from_usize(6), Some(Syscall::Send));
        assert_eq!(Syscall::from_usize(7), Some(Syscall::Receive));
        assert_eq!(Syscall::from_usize(8), Some(Syscall::Call));
        assert_eq!(Syscall::from_usize(9), Some(Syscall::Reply));
        assert_eq!(Syscall::from_usize(10), Some(Syscall::ReplyRecv));
        assert_eq!(Syscall::from_usize(11), Some(Syscall::MintBadged));
        assert_eq!(Syscall::from_usize(12), Some(Syscall::CopyFromCaller));
        assert_eq!(Syscall::from_usize(13), Some(Syscall::CopyToCaller));
        assert_eq!(Syscall::from_usize(14), Some(Syscall::ConsoleRead));
        assert_eq!(Syscall::from_usize(15), Some(Syscall::Spawn));
        assert_eq!(Syscall::from_usize(16), Some(Syscall::RegisterMetric));
        assert_eq!(Syscall::from_usize(17), Some(Syscall::EmitMetric));
        assert_eq!(Syscall::from_usize(18), Some(Syscall::Wait));
        assert_eq!(Syscall::from_usize(19), Some(Syscall::ConsoleWrite));
        assert_eq!(Syscall::from_usize(20), Some(Syscall::ClockNow));
        assert_eq!(Syscall::from_usize(21), Some(Syscall::NotifyCreate));
        assert_eq!(Syscall::from_usize(22), Some(Syscall::Signal));
        assert_eq!(Syscall::from_usize(23), Some(Syscall::WaitNotify));
        assert_eq!(Syscall::from_usize(24), Some(Syscall::WaitAny));
        assert_eq!(Syscall::from_usize(25), Some(Syscall::EndpointCreate));
        assert_eq!(Syscall::from_usize(26), Some(Syscall::SpawnImage));
        assert_eq!(Syscall::from_usize(27), Some(Syscall::CapList));
        assert_eq!(Syscall::from_usize(28), Some(Syscall::Revoke));
        assert_eq!(Syscall::from_usize(29), Some(Syscall::ClockFreq));
        assert_eq!(Syscall::from_usize(30), None);
    }

    #[test]
    fn object_kind_discriminants_are_stable() {
        // Mirror `protocol::CapObject`'s variant order; both ends agree on these.
        assert_eq!(object_kind::TELEMETRY_SINK, 0);
        assert_eq!(object_kind::SPAN_SINK, 1);
        assert_eq!(object_kind::ENDPOINT, 2);
        assert_eq!(object_kind::REPLY, 3);
        assert_eq!(object_kind::NOTIFICATION, 4);
    }

    #[test]
    fn cap_desc_has_a_padding_free_layout() {
        // A packed-hitch entry: kernel and userspace agree on this exact shape.
        // Field order is chosen so there is no implicit padding (so no
        // uninitialized kernel bytes are ever copied out): 4×u32 + u64 = 24, then
        // the inline name = 24, total 48, 8-aligned.
        assert_eq!(core::mem::size_of::<CapDesc>(), 24 + CAP_NAME_LEN);
        assert_eq!(core::mem::align_of::<CapDesc>(), 8);
    }
}
