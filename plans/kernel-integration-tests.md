# Kernel integration tests via QEMU

Add the first end-to-end test layer: boot the kernel in QEMU,
read the virtio-console telemetry stream from the host, decode it
into `protocol::Frame`s, assert on the sequence, kill QEMU on
match-or-timeout.

## Why this milestone

Unit tests in `kernel-core` cover the data-handling logic. They
say nothing about whether the kernel actually boots, whether the
virtio-console handshake completes, whether the timer IRQ
actually fires, or whether `Hello` is the first byte on the wire.
Today, the only way to find out is to run `cargo xtask up` in
one terminal, `cargo xtask reader` in another, and squint at the
output.

This is the second test layer: integration tests that observe
the kernel-as-shipped through its real wire output. If a future
refactor breaks the boot sequence or the wire ordering, these
tests fail.

## Decisions locked in (from prior conversation)

- **Feedback channel**: virtio-console telemetry frames. Decoded
  via `protocol`. No `println!` text matching.
- **Lifecycle**: read-until-expected with a per-test wallclock
  deadline; kill QEMU on match or timeout.
- **Harness location**: a new `xtask test` subcommand. The QEMU
  invocation already lives in `xtask::up`; reuse it.
- **No kernel changes for v1**: tests observe the existing boot
  → heartbeat sequence. Trap-injection and panic tests come
  later (they need a debug command channel).

## Big-picture step list

| # | step | size |
|---|---|---|
| 1 | Promote `decode_stream` from `collector` to `protocol::stream` (feature-gated on `std`) | small |
| 2 | New `xtask test` subcommand skeleton — parses scenario name, dispatches | small |
| 3 | Test harness: spawn QEMU, accept on socket, read+decode in thread, channel to assertions | medium |
| 4 | Frame-matcher DSL — small predicate type for "this frame is a SpanStart whose name resolves to 'kernel.boot'" | small |
| 5 | Scenario: boot-reaches-heartbeat | small |
| 6 | Scenario: heartbeat-cadence (two heartbeats arrive in expected wall-time band) | small |
| 7 | Scenario: pre-init buffering preserves order across the flush | small |
| 8 | README / CLAUDE.md updates: how to run, what each scenario covers | content |

Steps 1, 3, and 4 carry all the design weight. The scenarios on top
are short (~15 lines each) once the harness exists.

## Architectural decisions

### A. Decoder lives in `protocol::stream`, std-feature-gated

Today `decode_stream` lives in `collector/src/main.rs` (uses
`std::io::Read` + `Vec`, so it's `std`-only). The xtask harness
needs the same loop. Options:

- **Duplicate the decoder** in xtask. No. Two sources of truth.
- **Move it into a new `protocol-stream` crate.** Overkill for ~30
  lines.
- **Add an opt-in `std` feature to `protocol`, expose decoder
  there.** `protocol` stays `no_std` by default (kernel keeps using
  it). `collector` and `xtask` opt in via `features = ["std"]`.

Going with the third. Concretely:

```toml
# protocol/Cargo.toml
[features]
default = []
std = []
```

```rust
// protocol/src/lib.rs
#![no_std]

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
pub mod stream;  // try_decode_frame + decode_stream
```

The existing collector tests for `decode_stream` move with it (or
stay as collector smoke tests calling the moved fn — fine either
way; they're cheap).

### B. Read-decode loop runs on a thread; assertions block on a channel

`std::net::UnixStream::read` blocks. The test needs a wallclock
deadline. Cleanest portable pattern without async:

```
main thread                       reader thread
-----------                       -------------
spawn qemu                ----->  accept socket
spawn reader thread       ----->  decode_stream into channel
poll channel w/ deadline           push each Frame
on match: kill qemu, ok
on timeout: kill qemu, fail
```

The reader is `decode_stream(socket, |f| tx.send(f.into_owned()))`.
Owned because `Frame<'a>` borrows from the read buffer, which goes
out of scope per-iteration. Define `OwnedFrame` in
`protocol::stream` — same enum shape, but `StringRegister`
carries `String` instead of `&'a str`.

Alternative considered: re-decode each frame on the main thread
from a `Vec<Vec<u8>>` of raw bytes. Avoids the OwnedFrame type but
makes the matcher API ugly (every comparison takes encoded bytes
plus a decode call). Not worth it.

### C. Frame matcher is a closure, not a typed pattern

Tempting to define `enum Pattern { SpanStart(name: &str), Hello, ... }`
and match structurally. Simpler: assertions are `Fn(&OwnedFrame) -> bool`
closures, helpers compose them.

```rust
let boot_span_start = |f: &OwnedFrame| matches!(
    f,
    OwnedFrame::SpanStart { name_id, .. }
        if string_table.get(*name_id) == Some("kernel.boot")
);
```

The harness keeps a running `StringTable` (id → string, populated
by observed `StringRegister` frames) so matchers can ask "is this
span 'kernel.boot'?" without hard-coding ids.

### D. Per-test wallclock deadline, not per-frame timeout

Each scenario declares a total budget (e.g. 5s for boot, 10s for
two-heartbeat cadence). Inside that budget the test reads frames
freely. Per-frame timeouts would flake on slow CI.

### E. Skip cleanly if QEMU is not installed

`which qemu-system-riscv64` at test entry; if missing, print a
"skipping — qemu not installed" line and exit 0. CI without QEMU
shouldn't fail the suite. Document the requirement; don't enforce it
through the test runner.

### F. No `cargo test` integration for now

A `tests/` directory under a crate would let `cargo test` run
these, but each test would shell out to `cargo build` for the
kernel (or assume the binary already exists), and Cargo's harness
has poor lifecycle control (kill signals, parallel-runs against
the same socket path). Run via `cargo xtask test [scenario]`.
Migrate to `cargo test` if we ever want them on every PR — for
now they're explicit.

### G. Socket-per-test, not shared

Each test invocation uses a fresh socket path like
`/tmp/snitch-itest-<scenario>-<pid>.sock` and removes it on exit.
Avoids cross-test contamination if someone runs in parallel.

## Step-by-step

### Step 1: extract `protocol::stream`

- `protocol/Cargo.toml`: add `[features] std = []`, default empty.
- `protocol/src/stream.rs`: new module, `#[cfg(feature = "std")]`,
  contains `try_decode_frame`, `decode_stream`, and `OwnedFrame` +
  `Frame::into_owned()`.
- `collector/Cargo.toml`: `protocol = { path = "...", features = ["std"] }`.
- Delete `try_decode_frame` / `decode_stream` from collector, import
  from `protocol::stream`. Tests stay green (they were testing the
  moved fn through collector; redirect or move them).

Verify: `cargo test -p protocol` (host) and `cargo test -p collector`
both pass.

### Step 2: `xtask test` subcommand skeleton

```rust
#[derive(Subcommand)]
enum Cmd {
    // ... existing ...
    /// Run kernel integration tests. With no scenario, runs all.
    Test { scenario: Option<String> },
}
```

`fn test(scenario: Option<String>) -> ExitCode` enumerates the
known scenarios (start with stubs that return `unimplemented!()`)
and dispatches by name. Empty `scenario` runs them all and
aggregates exit code.

### Step 3: test harness

`xtask/src/itest/mod.rs` (or inline):

```rust
pub struct Harness {
    qemu: Child,
    rx: Receiver<OwnedFrame>,
    string_table: HashMap<StringId, String>,
}

impl Harness {
    pub fn spawn(socket_path: &Path) -> Self { ... }

    /// Block up to `budget`, returning the next frame matching `f`.
    /// Returns `None` on deadline. All frames consumed along the way
    /// are still applied to the string table (so later matchers can
    /// see the names).
    pub fn wait_for(
        &mut self,
        budget: Duration,
        f: impl Fn(&OwnedFrame, &HashMap<StringId, String>) -> bool,
    ) -> Option<OwnedFrame> { ... }

    pub fn kill(&mut self) { ... }
}

impl Drop for Harness {
    fn drop(&mut self) { self.kill(); }
}
```

Wire: spawn QEMU pointing at the per-test socket, accept the
connection, spawn a reader thread that calls
`decode_stream(stream, |f| tx.send(f.to_owned()))`. The main thread
drains the channel via `recv_timeout`, updates the string table on
`StringRegister`, evaluates the matcher, returns on match-or-budget.

### Step 4: matchers

A handful of helpers in `xtask/src/itest/matchers.rs`:

```rust
pub fn is_hello() -> impl Fn(&OwnedFrame, &StringTable) -> bool { ... }
pub fn is_span_start_named(name: &'static str) -> impl ... { ... }
pub fn is_metric_named(name: &'static str) -> impl ... { ... }
pub fn is_dropped(expected_count: u32) -> impl ... { ... }
```

Closures so callers can `||` / `&&` them or define ad-hoc
predicates inline.

### Step 5: scenario `boot-reaches-heartbeat`

```rust
fn boot_reaches_heartbeat() -> Result<(), String> {
    let mut h = Harness::spawn(&socket_path("boot"));
    h.wait_for(SEC * 3, is_hello())
        .ok_or("no Hello frame")?;
    h.wait_for(SEC * 3, is_span_start_named("kernel.boot"))
        .ok_or("no kernel.boot span")?;
    h.wait_for(SEC * 5, is_dropped(0))
        .ok_or("no Dropped(0) checkpoint after flush_pre_init")?;
    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat span within budget")?;
    Ok(())
}
```

What it pins:
- `Hello` first on the wire (anchor for host-side wall-clock).
- `kernel.boot` span fires before heartbeat (proves init order).
- `Dropped(0)` follows the buffer flush (proves pre-init buffer
  drained cleanly, no overflow).
- First `kernel.heartbeat` arrives within ~5s of boot (proves the
  timer IRQ is firing).

### Step 6: scenario `heartbeat-cadence`

Two consecutive `kernel.heartbeat` SpanStarts. Assert their `t`
timestamps differ by 1 timebase tick interval ± a tolerance band
(say ±50% — generous because QEMU's `time` CSR is wall-clock-ish but
can stall when the host is busy).

What it pins: the timer IRQ keeps firing, the `TIMER_INTERVAL_TICKS`
math is right, the wfi+IRQ wakeup path is intact.

### Step 7: scenario `pre-init-order`

Doesn't need a new kernel mode — re-uses the boot sequence. Asserts:

1. The first `StringRegister` on the wire is for `kernel.boot`
   (which is registered before virtio-console is up — proves
   the pre-init buffer captured it).
2. Every `SpanStart`'s `name_id` resolves via a `StringRegister`
   that appeared earlier in the stream (proves frame ordering
   survives the flush; if the buffer were dequeued out-of-order
   we'd see SpanStarts referencing unknown ids).

The second assertion is a general stream invariant we'd want
anyway — worth keeping even after we add other scenarios.

### Step 8: docs

- `README.md`: section on running integration tests, the QEMU
  dependency, the per-scenario CLI.
- `.claude/CLAUDE.md` (project): add a "Kernel integration tests"
  section under "Running the game" / parallel to snapshot tests:
  what command, when to run, what each scenario covers.

## Risks and known weaknesses

- **Heartbeat cadence is timing-sensitive.** QEMU's `time` CSR
  advances based on host wall-clock under default config; under
  load (CI, parallel tests, sleeping host) gaps widen. Mitigation:
  generous tolerance band, single-threaded scenario runs.
- **Socket races.** If a prior run left `/tmp/snitch-itest-*.sock`
  behind, QEMU's `server=on` will refuse to bind. Harness
  pre-deletes; double-binding by parallel tests still possible if
  paths collide. Mitigation: pid in path.
- **QEMU stdout pollution.** QEMU writes its own banner / SBI
  output to stderr/stdout. We don't read them, but they show up in
  test logs. Mitigation: redirect QEMU stderr to a per-test file,
  surface it only on failure.
- **No SBI shutdown yet.** We just `Child::kill()` to terminate.
  That sends SIGKILL — fine for tests, but if we later care about
  clean teardown we'll want the kernel to invoke
  `sbi_system_reset`.
- **OwnedFrame drift.** `OwnedFrame` is a hand-typed parallel of
  `Frame`. Adding a new `Frame` variant requires updating both.
  Hard to forget (test will fail to compile) but worth flagging.
- **CI without QEMU.** Test runner skips with exit 0; this means a
  silently-broken CI image hides regressions. Mitigation:
  separate `cargo xtask test --require-qemu` mode that fails on
  missing dependency, used in any CI that should run them.

## What we're not doing (yet)

- **Inducing traps from tests.** Needs a kernel-side debug
  command channel (e.g. a stdin path the kernel reads). Separate
  milestone.
- **Panic-handler tests.** Same — need a way to trigger a panic
  from outside.
- **Long-running stability tests.** A "100 heartbeats over 60s"
  test would catch slow leaks. Cheap to write later.
- **Determinism via `-icount`.** QEMU has a deterministic-clock
  mode that would eliminate cadence flakiness. Worth considering
  if scenarios 5/6 flake; otherwise YAGNI.
- **Migrating to `cargo test`.** See decision F.

## Done state

- `cargo xtask test` runs three scenarios, all green, in ~10s
  wallclock.
- Each scenario asserts on the wire-frame sequence, not text.
- `cargo build -p kernel --target riscv64gc-unknown-none-elf`
  still passes. No production kernel code touched.
- `protocol::stream` is the one place stream decoding lives;
  collector and xtask both consume it.
- README + CLAUDE.md document how to run.
