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

### Step 0: Keep the collector core wasm-buildable

**Acceptance criteria**: `cargo build -p collector --target wasm32-unknown-unknown
--no-default-features` succeeds; the OTLP and Loki exporters are behind a default
feature; `cargo xtask test` fails if the wasm build breaks.
**RED**: A gate test asserting the wasm target builds (mirroring the existing
`xtask` policy tests in `plan.rs`).
**GREEN**: Feature-gate `ureq` and the `otlp`/`loki` modules; add the target to
the gate.
**MUTATE**: n/a (build-config change) — note this explicitly rather than skipping
silently.
**REFACTOR**: none expected.
**Done when**: gate green, wasm target builds. *Cheap now (ureq is confined to two
files), expensive after more exporters land.*

### Step 1: COBS the wire format, both transports

**Acceptance criteria**: every frame on the wire is COBS-encoded and `0x00`
delimited, on **both** virtio and (future) UART; `itest` is green; a byte
inserted mid-stream costs exactly one frame, not the stream.
**RED**: `protocol` unit tests — encode/decode round-trip through COBS; a frame
containing `0x00` bytes in its payload survives; a truncated frame yields
`DeserializeUnexpectedEnd`, not garbage.
**GREEN**: switch the encode site to `postcard::to_slice_cobs`; teach
`try_decode_frame`/`decode_stream` the delimiter.
**MUTATE**: `cargo xtask mutants protocol`.
**KILL MUTANTS**: expect survivors around delimiter handling — that is the
load-bearing logic, so strengthen rather than accept.
**REFACTOR**: assess.
**Done when**: gate green. **This changes the wire format** — do it in one
increment across both transports so the two never diverge.

### Step 2: Decode errors become recoverable

**Acceptance criteria**: given a stream with a corrupt/partial frame, the decoder
skips to the next `0x00`, counts a resync, and continues delivering subsequent
frames; the socket path's fail-fast behaviour is preserved where it is correct.
**RED**: `collector` unit tests over a synthetic byte stream with damage injected
at frame boundaries, mid-frame, and in the delimiter itself.
**GREEN**: resync loop + a `resyncs` counter.
**MUTATE**: `cargo xtask mutants collector`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: the error policy differs per transport (lossless socket vs lossy
serial) — prefer expressing that in a type over a bool flag threaded through the
loop.
**Done when**: gate green. Still no hardware.

### Step 3: A frame stream is a source — add replay

**Acceptance criteria**: `cargo xtask reader --replay <file>` decodes a recorded
stream and produces identical output to the live path; the source is an
abstraction the serial and socket paths will both implement.
**RED**: collector test — the same recorded bytes through the replay source and
through a socket-shaped source yield the same frames.
**GREEN**: introduce the source abstraction; implement replay over a file.
**MUTATE**: `cargo xtask mutants collector`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess; the abstraction should make step 10 near-trivial.
**Done when**: gate green, a recorded boot replays. *Deliberately before serial —
it invents the abstraction under test with zero hardware risk.*

### Step 4: `console=` mode selects text or frames

**Acceptance criteria**: with no bootarg the board behaves exactly as today
(human-readable text); `console=frames` routes kernel `println!` through
`Frame::Log`; pre-init and panic paths stay raw text in **both** modes.
**RED**: `kernel_boot` parse tests for the new bootarg (host-tested, like
`workload=`); an `itest` scenario asserting a `Frame::Log` reaches the wire under
`console=frames`.
**GREEN**: parse arm + a console-mode dispatch in the print path.
**MUTATE**: `cargo xtask mutants kernel-boot`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: this subsumes the `board-heartbeat-print` feature added as a
workaround — **retire it in this step** rather than leaving two mechanisms.
**Done when**: gate green. Default stays `text`: the day telemetry breaks is the
day you need the console most.

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
