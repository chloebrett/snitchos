# Plan: B3 — Telemetry over UART (M2)

**Branch**: `main` (this repo works directly on main — no feature branches)
**Status**: Active
**Design**: [docs/uart-telemetry-design.md](../docs/uart-telemetry-design.md)
**Milestone**: M2 in [plans/visionfive2-port.md](visionfive2-port.md)

## Goal

Get the `Frame` stream off the VisionFive 2 on a physical UART, without coupling
kernel timing to the wire — and do it so the transport becomes *one of four
sources* behind a single frame-stream interface, not a board special case.

## Why the ordering looks like this

Two forces shape it:

1. **Host-first.** A board round-trip is expensive — a stale image cost two
   flashes and a wrong diagnosis in one session. Everything provable under snemu
   is proved before the board is involved. Steps 1–6 need no hardware; step 7 is
   the cheapest possible board increment (one observable bit).
2. **Serve the long-term vision cheaply.** The end state is a collector that
   backs custom dashboards *and* a terminal, fed by any of four sources — in-tab
   wasm, host, board, replay — see
   [the design note's "Where this is going"](../docs/uart-telemetry-design.md).
   Two early steps (0 and
   3) cost little now and are expensive to retrofit: keeping the collector core
   wasm-clean, and making "source" a real abstraction before serial needs it.

**Replay lands before serial on purpose.** It is the cheapest source (no I/O),
it forces the source abstraction into existence under test, and it independently
buys shareable bug reports, hardware-free demos and regression triage. Serial
then becomes "another source" rather than "the thing that invented sources".

## Acceptance Criteria

- [ ] The board emits decoded `Frame`s to a host collector over a serial line
- [ ] Losing bytes costs at most one frame — the decoder resynchronises
- [ ] The kernel never blocks on the wire; dropped frames are counted and reported
- [ ] Kernel timing is independent of baud (no heartbeat stall behind TX)
- [ ] The human console still works on the board (`console=text` default)
- [ ] `cargo xtask reader` can replay a recorded stream with no hardware
- [ ] `collector`'s core compiles for `wasm32-unknown-unknown`
- [ ] One wire format across virtio and UART — `itest` tests what the board runs

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test. Gate is
`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
Mutation testing is `cargo xtask mutants <crate>` (host crates only — `kernel` is
bare-metal and excluded; its logic lives in `kernel-*`).

### Step 0: Keep the collector core wasm-buildable — ✅ DONE

**Acceptance criteria**: `cargo build -p collector --lib --no-default-features
--target wasm32-unknown-unknown` succeeds; the HTTP exporters are behind a default
feature; `cargo xtask test` fails if the wasm build breaks.
**RED**: `--lib --no-default-features --target wasm32` failed —
`ureq`→`ring` has no wasm build, and `--no-default-features` still pulled it in,
so `ureq` was a hard dependency.
**GREEN**: added a `native` default feature (`ureq`, `tiny_http`, `clap`
optional); created the previously-empty `lib.rs` as the real crate root
(`pub mod state`, `pub trait SpanExporter`, `caps`/`url` crate-internal,
`otlp`/`loki`/`prom` `#[cfg(feature = "native")]`); `main.rs` became a thin native
binary over `collector::`; added a `portability` build check to `run_unit_tests`.
**MUTATE**: n/a — build-config + module move, no new logic. The existing 82
collector tests passing is what proves the move preserved behaviour.
**REFACTOR**: made `caps`/`url` crate-internal `mod` rather than `pub` — they're
implementation details, and keeping them public tripped `new_without_default`.
**Scope note / correction**: I had claimed in the plan's framing that "collector
is already lib + bin" — it was **not**; `lib.rs` was 0 bytes and every module
lived in `main.rs`. So this step created the library, a real (if small) refactor,
not the pure feature-gate the plan implied. Also: there were **two** wasm-hostile
deps (`tiny_http` as well as `ureq`), not one.
**Known pre-existing, left alone**: `caps::observe` trips clippy
`too_many_arguments` (10 args). Predates this change, orthogonal to
wasm-buildability, and clippy isn't in the `xtask test` gate — a `CapEvent`-struct
refactor is its own task.
**Done when**: gate green, wasm target builds. ✅

### Step 1: COBS the wire format — ✅ DONE

**Acceptance criteria**: every frame on the wire is COBS-encoded and `0x00`
delimited; `itest` green; a frame whose encoding contains `0x00` survives (the
delimiter is unambiguous); truncation before the delimiter reads as "need more,"
not an error.
**RED**: three new `protocol::stream` tests against a not-yet-existent
`wire_encode` — round-trip through `decode_stream`, interior-`0x00` survival,
two-frames-back-to-back. Compile-failed (RED).
**GREEN**: added `protocol::wire_encode` (no_std, `to_slice_cobs`); rewrote
`try_decode_frame` (now returns `Result<Option<(OwnedFrame, usize)>, DecodeError>`
— splits on `0x00`, COBS-decodes the chunk) and `decode_stream` (COBS-in-place on
its owned buffer). Bumped `PROTOCOL_VERSION` 7→8 with a history note.
**Blast radius handled**: 3 kernel encode sites (`send_hello`, `KernelSink`,
`panic_log`) → `wire_encode`; the `kernel-obs` capture sink + its intern.rs/self
decoders → COBS; 4 external `try_decode_frame` consumers (snemu_audit ×2,
harness, snemu_diff) → new return shape; the `wire_stability` snapshot re-blessed
(only the version line changed — payload bytes identical, confirming framing-only).
`decode_stream`'s signature was **kept unchanged**, so its 3 consumers (collector,
measure, snemu/main) needed no edits.
**Scope correction**: the "corrupted byte costs one frame" *recovery* moved to
Step 2 — Step 1 is fail-fast on a bad chunk (correct for the lossless socket),
and the COBS framing is what makes Step 2's resync possible. `DecodeError.consumed`
already carries the resync offset Step 2 will use.
**MUTATE**: pending — `cargo xtask mutants protocol` before commit; delimiter
handling is the load-bearing logic to watch.
**Verified**: itest 121/121, `--scramble` 121/121, protocol/kernel-obs/xtask-snemu
suites green.
**Done when**: gate green, mutation reviewed, commit approved.

### Step 2: Decode errors become recoverable — ✅ DONE

**Acceptance criteria**: a stream with an undecodable frame between two good ones:
the decoder skips the bad `0x00`-delimited chunk, counts a resync, and still
delivers the neighbours; the socket path stays fail-fast (a bad frame on a
lossless wire is a real bug, surfaced as `Err`); the resync count is observable.
**RED**: four `protocol::stream` tests — resync skips a corrupt frame and delivers
neighbours (count 1), fail-policy returns `Err` on the same input, a clean stream
reports 0 resyncs, consecutive bad chunks don't wedge (count 2). Compile-failed.
**GREEN**: `OnDecodeError::{Fail, Resync}` policy enum + `DecodeSummary { resyncs }`
returned from `decode_stream`; the decode-error branch dispatches on the policy
(dropping the chunk past its delimiter *is* the resync). Home is `protocol::stream`
(where `decode_stream` lives), not `collector` as the plan first assumed — the
collector just picks the policy.
**MUTATE**: pending — `cargo xtask mutants protocol -f stream.rs` before commit.
**REFACTOR**: the per-transport policy is a **type at the call site**
(`OnDecodeError`), per the plan's steer, not a bool in the loop. Chosen over a
separate `decode_stream_resync` fn (user call): one entry point, explicit policy.
**Consumer churn**: `decode_stream` gained the policy arg, so all 6 callers
(collector, measure, snemu/main, snemu_diff ×2, harness) pass `OnDecodeError::Fail`
— all are lossless (socket / in-memory) today. The serial source (Step 10) is the
first `Resync` caller.
**Verified**: itest 121/121; protocol/collector/xtask-snemu/xtask-itest suites
green (207).
**Done when**: gate green, mutation reviewed, commit approved.

### Step 3: A frame stream is a source — add replay — ✅ DONE

**Acceptance criteria**: `cargo xtask reader --replay <file>` decodes a recorded
stream and produces the frames the recording holds; the source is an abstraction
the serial and socket paths both implement; the default (no `--replay`) still
connects to the socket.
**RED**: five `collector::source` tests — `resolve` picks Socket/Replay,
`policy` maps Socket→Fail / Replay→Resync, a wire-encoded recording written to a
real temp file replays to the same frames, and a corrupt frame in a recording is
skipped-and-counted (Resync). Module didn't exist → RED.
**GREEN**: `Source::{Socket, Replay}` with `resolve`/`policy`/`open` (→ `Box<dyn
Read>`) + `run_source(source, on_frame)` — the one place source, policy, and
`decode_stream` meet. `main` now resolves a `Source` and calls `run_source`
instead of hardcoding `UnixStream::connect`. Native-gated (opens sockets/files);
the wasm core doesn't include it and still builds.
**Replay policy = Resync** (user call): a recording can be *of* a lossy serial
capture, so replay reproduces what it can rather than aborting on a bad frame.
**MUTATE**: pending — `cargo xtask mutants collector -f source.rs` before commit.
**REFACTOR**: `run_source` is the seam Step 10's serial variant slots into — a new
`Source` arm + `open` case, no new decode loop.
**Verified**: 87 collector tests green; wasm core builds; `--replay` CLI routes
correctly (smoke). Added `tempfile` dev-dep for the replay round-trip test.
**Done when**: gate green, mutation reviewed, commit approved.

### Step 4: `console=` mode selects text or frames — ✅ DONE

**Acceptance criteria**: no bootarg → today's behaviour (human text on UART);
`console=frames` routes **every post-init** kernel `println!` through `Frame::Log`
on the telemetry wire (full macro routing — user call); pre-init and panic stay
raw UART always.
**RED**: 5 `kernel_boot::console_mode` parse tests (default text, parses frames /
text, unknown→text, coexists with `workload=`); an itest asserting a `Log`
carrying "entering heartbeat" under `console=frames`.
**GREEN**: `ConsoleMode` + `console_mode()` parser; kernel `console::write_console`
routes text→UART / frames→`tracing::emit_log`, behind rewritten `print!`/`println!`
macros (now `write_console(format_args!…)`); a re-entrancy guard (`IN_FRAMES`) drops
a `println!` fired from inside the emit path rather than deadlocking; `kmain` sets
the mode from the bootarg.
**MUTATE**: `console_mode` — 2 mutants, both caught, 0 missed.
**REFACTOR**: **retired `board-heartbeat-print`** — the `hb` line is now
`vf2 && console_mode()==Text` (frames mode gets liveness from the heartbeat span;
text mode keeps the raw pulse for headless bring-up). Feature deleted.
**Harness change**: the boot checkpoint ("entering heartbeat") becomes a `Log`
frame under `console=frames`, so `boot_snapshot` now scans the virtio TX stream as
well as the UART (`run_to_checkpoint`) — mode-robust, so the frames scenario shares
the snapshot instead of a slow fallback. The COBS payload preserves the ASCII
marker (no `0x00`).
**Known coarseness**: in frames mode each `print!` fragment becomes its own `Log`
(no line-buffer); fine for the whole-line output the kernel emits. **Out of scope**:
userspace `ConsoleWrite` still writes raw UART — so on the board's single wire it
would still interleave with frames; routing it is a follow-up (the REPL case).
**Verified**: kernel-boot 61 tests; itest 122/122; `--scramble` 122/122; both
kernel targets build.
**Done when**: gate green, mutation reviewed, commit approved. Default stays
`text`: the day telemetry breaks is the day you need the console most.

### Step 5: The collector is the terminal

**Acceptance criteria**: `cargo xtask reader` renders `Frame::Log` to stdout and
relays raw-mode stdin to the guest; the Stitch REPL is usable through it over the
**QEMU socket path** (no board needed).
**RED**: unit tests for the relay/render logic (byte in → guest write, `Log`
frame → rendered line), separated from the raw-mode terminal I/O.
**GREEN**: minimal relay + render.
**MUTATE**: `cargo xtask mutants collector`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green and the REPL is usable through the collector. *This is
the step that makes `console=frames` viable, and it is a down-payment on the
dashboards-plus-terminal end state — not a workaround for losing `screen`.*

### Step 6: Measure real telemetry throughput

**Acceptance criteria**: a documented steady-state bytes/second figure for a
representative workload, measured under snemu, recorded in the design note; a
baud target chosen from it.
**RED**: n/a — this is a measurement, not a behaviour change. Say so rather than
inventing a test.
**GREEN**: measure; write the number and method into
`docs/uart-telemetry-design.md`, replacing the two incidental data points.
**Done when**: the design note states a measured figure and a chosen baud. *The
existing ~60 KB/s estimate comes from two boots that disagree by more than the
workload difference explains; steps 7–10 depend on it, so it gets measured
properly first.*

### Step 7: Program the UART baud

**Acceptance criteria**: the kernel sets the divisor from the DTB clock and the
chosen baud; the board's console still prints when the terminal reconnects at the
new rate.
**RED**: pure divisor math in `kernel-devices::uart` — `divisor = clk / (16 ×
baud)`, host-tested, including rounding and a rejected-out-of-range case.
**GREEN**: divisor computation + the `LCR.DLAB` / `DLL` / `DLM` write sequence.
**MUTATE**: `cargo xtask mutants kernel-devices`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green and the board prints at the new baud. *The cheapest
possible board increment: one observable bit. Verify the USB-serial adapter's
ceiling first (original CP2102 ≈ 1 Mbaud). Document the new rate next to
`setenv bootargs` in the boot procedure.*

### Step 8: TX ring with THRE-interrupt drain

**Acceptance criteria**: bytes queued to the UART drain at wire speed without the
caller blocking; the heartbeat's period is unchanged with a full ring; a full
ring drops and counts rather than blocking.
**RED**: ring behaviour in `kernel-devices` (host-tested, alongside the existing
`ConsoleRing`): fill, drain, wrap, drop-on-full, count. Interrupt plumbing is
covered by `itest`, not unit tests.
**GREEN**: ring + THRE enable + ISR drain.
**MUTATE**: `cargo xtask mutants kernel-devices`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green, board boots with the ring in the print path. **Largest
and riskiest step**: the UART driver is polled-only by explicit design, and PLIC
routing for the console UART is not wired (the port plan scopes PLIC under M2.5).
If PLIC turns out to be its own milestone, the fallback is blocking writes at a
high baud — a deliberate decision to take, not a discovery to make.

### Step 9: `UartFrameSink`

**Acceptance criteria**: with `console=frames`, frames reach the ring; the sink
never blocks; drops are counted and surface as `Frame::Dropped`.
**RED**: a `FrameSink` impl test in `kernel-obs` against a mock byte sink —
frames encoded, backpressure drops counted, never blocks.
**GREEN**: the sink over step 8's ring.
**MUTATE**: `cargo xtask mutants kernel-obs`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green.

### Step 10: Collector `--serial` source

**Acceptance criteria**: `cargo xtask reader --serial <dev> --baud N` decodes the
board's live stream; the board reaches Grafana.
**RED**: the source abstraction from step 3 is already tested; add serial-specific
config/parse tests only.
**GREEN**: a serial source implementing step 3's abstraction.
**MUTATE**: `cargo xtask mutants collector`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green, board telemetry in Grafana. *Should be small — step 3
did the design work.*

## Pre-PR Quality Gate

Before each commit:
1. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`
2. `cargo xtask clippy` (never blanket `--fix` the kernel — `deref_addrof`)
3. Mutation testing for the touched host crate
4. `cargo xtask links` if any `.md` moved or gained links
5. Refactoring assessment

## Risks

- **Step 8 (PLIC + interrupts) is the schedule risk.** Everything before it is
  host-verifiable or a one-bit board check; step 8 is where real hardware
  uncertainty lives. Decide the blocking-write fallback deliberately if PLIC
  balloons.
- **Step 1 changes the wire format.** Old captures stop decoding. Acceptable (the
  corpus is regenerable) but note it before doing it.
- **Step 6's number gates steps 7–10.** If measured throughput exceeds what the
  adapter can carry, the one-cable decision reopens and two UARTs come back on the
  table — see the design note's rejected alternative.

---
*On completion, `git mv` this file to `plans/legacy/` (project override of the
planning skill's delete step) and run `cargo xtask links`.*
