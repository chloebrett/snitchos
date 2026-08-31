# Plan: B3 — Telemetry over UART (M2)

**Status (2026-08-30)**: 🟡 **Steps 0–4, 6, 8, 9, 10a landed and gate-green. Step
10b is half-verified: telemetry frames DID cross a real UART and decoded perfectly
— but the kernel wedges shortly after boot, so there is no sustained stream and B3
is not done.**

Measured 2026-08-28 ([../notes/board-session-2026-08-28.md](../notes/board-session-2026-08-28.md)).
Booting with `console=frames` puts real postcard frames on the board's UART, and
`collector --replay` of the raw capture decodes **all** of them — `BuildInfo`,
`StringRegister`s, nested `SpanStart`/`SpanEnd` with monotonic timestamps,
`Dropped { count: 0 }`, `MetricRegister`s. **So the collector's serial path is not
the problem**; the half of 10b that was genuinely untested (`serialport::open` and
the live read loop) works.

What stops it is a kernel bug, since root-caused: the kernel **inherits U-Boot's
PLIC enable bitmap**, so the first `sstatus.SEIE` takes an interrupt for a device it
has no handler for and `handle_external`'s claim loop livelocks. The stream dies
~500 bytes in, mid-backlog. Fix (`kernel_devices::plic::reset_context`) is written
and host-tested but **has never run on the board** — that confirmation boot is what
10b now needs, and it is one `cargo xtask board uboot` invocation.

Note the failure is *not* specific to `console=frames`: a `console=text` boot wedges
in the same place. Frames mode merely made it unmissable.

Then Step 5b (interactive relay); Step 7 (programming the baud) is deferred as
optional headroom — Step 6 measured steady-state telemetry at single-digit
throughput, so 115200 suffices.
**Builds on this**: [board-bridge.md](board-bridge.md) — the *programmatic* half of
driving the board (`cargo xtask board exec`, reboot) starts where Step 10 finishes.
**Design**: [docs/uart-telemetry-design.md](../docs/uart-telemetry-design.md)
**Milestone**: M2 in [plans/visionfive2-port.md](visionfive2-port.md)
**Write-up**: [post 68 — telemetry off the VF2's serial line](../posts/post-68-telemetry-off-the-vf2-serial-line.md)

Verified in-tree 2026-08-25: `UartFrameSink` is host-tested
(`kernel-obs/src/uart_sink.rs`, 8/8 mutants killed) and the collector now has its
serial source — `Source::Serial`, `SerialReader`, `--serial`/`--baud`
(`collector/src/source.rs`). Gate: `cargo xtask test --no-fail-fast` 2811/2811,
`cargo xtask itest` and `--scramble` 132/132 each.

**What that does *not* establish.** No byte of this has crossed a real UART. The one
piece of 10b with no test coverage is the `serialport::open` call itself, and Step
10's acceptance criterion is *"the board reaches Grafana"* — a claim only hardware
can settle. Treat the code as landed and the step as open until that run happens.
(The prior version of this paragraph confidently described in-tree state that had
since moved; the same failure mode produced the netboot claim corrected in
[docs/board-agent-bridge-design.md](../docs/board-agent-bridge-design.md). State
what was checked, and when.)

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

### Step 5: The collector renders the log (the *render* half of "collector as terminal")

**Scope split (decided in-flight):** the original Step 5 conflated two halves with
very different dependencies. The **render** half — `Frame::Log` → clean stdout text
— is pure, testable, and immediately useful under `console=frames`. The
**interactive relay** half — raw stdin → the guest REPL — is entangled: the REPL's
I/O is on the *console/UART* channel, not telemetry (two separate sockets on QEMU,
one wire only on the board), and it needs userspace `ConsoleWrite`→frames routing
(deferred from Step 4). So the relay moves to **Step 5b, after Step 10** (the
bidirectional `--serial` source on the board's single wire), where the model is
clean. This step is the render half only.

**Acceptance criteria**: in `cargo xtask reader` (`--text`), a `Frame::Log` prints
as just its message line (the guest's own console output), not the `Log { msg: …,
task_id: … }` Debug dump; other frames still show Debug for inspection.
**RED**: `collector` unit test — a pure `log_text(frame)` returns the message for
a `Log` and `None` for telemetry frames.
**GREEN**: the pure extractor + wire it into the reader's frame printer.
**MUTATE**: `log_text` — 4 mutants, all caught. — ✅ DONE
**Verified**: `log_text` unit tests + an end-to-end integration test
(`collector/tests/replay_render.rs`) that runs the real binary over a recorded
`--replay` file and asserts `Log` frames print as bare lines (no `Log { … }` dump)
while telemetry still shows Debug. wasm core still builds (log_text is in it).
**Done when**: gate green; `reader` shows clean log lines. ✅

### Step 5b (after Step 10): the interactive relay — raw stdin → guest REPL

Deferred here because it needs the bidirectional serial wire (Step 10) and
`ConsoleWrite`→frames routing. Raw-mode stdin → serial TX; the Stitch REPL usable
through `reader` on the board. The down-payment on the dashboards-plus-terminal end
state — not a workaround for losing `screen`.

This is the **human** surface: a terminal a person types at. Its sibling is the
**programmatic** one — `cargo xtask board exec "<text>"`, capture-until-a-stop-
condition, structured frames back — planned in [board-bridge.md](board-bridge.md).
They share the serial handle and nothing else; either can land first.

### Step 6: Measure real telemetry throughput — ✅ DONE

**Result**: measured steady-state (two-point delta, excluding the boot transient)
under snemu — `init` **≈ 5.5 KB/s**, `demo` **≈ 2.7 KB/s**. The earlier "~60 KB/s"
was boot-transient-dominated (~5-heartbeat sample). Both are **single-digit KB/s,
under 115200's ~11.5 KB/s** → **chosen baud: 115200** (inherited from OpenSBI, no
divisor programming). Method + tables written into `docs/uart-telemetry-design.md`
("Throughput — measured, and 115200 suffices").
**RED**: n/a — a measurement, not a behaviour change.
**Done when**: the design note states the measured figures and a chosen baud. ✅
**Consequence**: **Step 7 drops from prerequisite to optional headroom** (below).

### Step 7: Program the UART baud — ⏸ OPTIONAL (deferred; Step 6 showed 115200 suffices)

**Downgraded by Step 6's measurement:** steady-state telemetry is single-digit
KB/s, under 115200's 11.5 KB/s, so the board keeps OpenSBI's 115200 and needs no
divisor programming. This step is kept as documented *headroom* for a future
task-heavy workload or a tighter no-drop guarantee — **do it only if a measured
workload exceeds 115200.** Not on the critical path to Step 8/9/10.

**Acceptance criteria** *(if pursued)*: the kernel sets the divisor from the DTB
clock and the chosen baud; the board's console still prints when the terminal
reconnects at the new rate.
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

### Step 8: TX ring with THRE-interrupt drain — full PLIC path — ✅ DONE

**Decision (2026-07):** the drain is the **full PLIC + THRE-interrupt** subsystem
(user call), not a blocking or cooperative interim — the "right" long-term design.
The `ConsoleRing` (drop-on-full byte ring) is **reused as-is** for TX, so there is
no ring to build; the work is entirely the external-interrupt subsystem, which
does not exist yet (the trap handler dispatches only timer + software IPIs; console
RX is timer-*polled*).

**Test strategy: snemu now models the PLIC** (was QEMU-only). The interrupt path
runs in the **deterministic** gate, no QEMU flakiness. What was added to snemu:
- `snemu::plic` — a PLIC device model (registers + level-triggered gateway +
   claim/complete + `seip`), 7 host tests.
- `snemu::uart` — IER modelled + `interrupt_asserted()` (THRE always ready, so the
   TX line follows `ETBEI`), 3 new tests.
- `snemu::bus` — owns the PLIC, routes its MMIO window, syncs the UART line on
   every UART write, exposes `external_pending(context)`.
- `snemu::cpu` — external-interrupt delivery: `SUPERVISOR_EXTERNAL`/`SIE_SEIE`, a
   per-hart `hartid` → S-context, `external_interrupt_pending(bus)` (derived like
   the timer), delivered highest-priority in `step`.
All inert until the kernel enables `SEIE` + the UART THRE interrupt (Commit C) —
so the existing gate is unchanged, and **Commit A can un-gate `cfg(vf2)` once B/C
exercise it** through this model. (`--engine qemu` remains the fidelity oracle.)

Increments:
1. **PLIC register offsets** (`kernel-devices::plic`, pure) — priority / enable /
   threshold / claim-complete byte offsets, pinned to spec values. — ✅ DONE
   (4 tests, 32/32 mutants caught).
2. **PLIC driver logic** (`kernel-devices::plic`, over a `PlicTransport` trait,
   host-tested against a mock — the `FwCfgTransport` pattern): `enable_source`
   (read-modify-write, idempotent), `claim` (None on sentinel 0), `complete`. —
   ✅ DONE (9 tests, 41/41 mutants caught — a `|`→`^` miss killed by a
   double-enable idempotency test).
3. **Kernel MMIO glue** (`kernel/`) — the `PlicTransport` impl over volatile
   registers, `init()` routing the UART source to hart-0 S-context. **Commit A: ✅
   DONE** (`kernel/src/device/plic.rs`, `init()` called in kmain).
   - **Gated `cfg(vf2)` (board-only).** snemu returns `OutOfRange` for writes below
     RAM base (`mem.rs`), so a PLIC write faults the itest guest — the module stays
     out of the non-vf2 itest build (snemu gate provably unaffected) and is live on
     the board. **Un-gate once snemu models a PLIC** (then it's testable in the
     deterministic gate). Source 10 / context 1 / base `0x0c00_0000` hardcoded for
     QEMU-`virt`; `// board: derive from DTB` markers left.
   - Inert at runtime: `init()` only enables the source; nothing asserts until the
     UART's THRE interrupt is turned on (Commit C).
4. **External-interrupt trap dispatch + `SEIE`** — **Commit B: ✅ DONE.**
   - **Un-gated the `cfg(vf2)`** now that snemu models the PLIC: `plic::init` +
     claim/complete wrappers, the `SupervisorExternalInterrupt` trap arm
     (`handle_external`: claim → dispatch → complete), and `enable_external_interrupts`
     (`sie.SEIE`) all run in the itest build.
   - **Inert:** the only routed source is the UART, whose THRE interrupt isn't
     enabled yet (Commit C), so `handle_external` doesn't run at runtime. Validated
     by the gate staying green — which *also* exercises snemu's PLIC model (the
     kernel's PLIC register writes hit it) and confirms no spurious assertion.
   - The UART ISR body (drain TX ring) is the `// Next increment` stub.
5. **Wire the TX ring** into the emit path (push = non-blocking drop-and-count);
   the THRE ISR drains at wire speed. — **Commit C: ✅ DONE.** `TX_RING`
   (`Mutex<ConsoleRing<512>>`), `tx_push` (interrupt-masked push + IER-enable),
   `drain_tx` (THRE-drain, disables IER when empty), UART `thre`/`write_thr`/
   `set_tx_interrupt`, `without_interrupts`, `IER`/`IER_ETBEI`.
6. **itest — interrupt-driven TX end to end.** — **Commit C: ✅ DONE, and in the
   *deterministic* snemu gate, not QEMU-only.** The `tx-irq-delivers` scenario
   asserts the `tx-irq-ok` boot marker reaches the wire through PLIC → `SEIE` →
   THRE → `drain_tx`. Regression found + fixed along the way: the PLIC's two MMIO
   megapages (`0x0c00_0000`, `0x0c20_0000`) must be inserted into `MmioRegions` in
   `kmain` — the higher-half MMIO mid table only leaf-maps inserted pages, so
   `plic::init` was faulting on an unmapped VA.

**Done when**: gate green (host + snemu itest incl. `tx-irq-delivers` + `--scramble`),
board boots with the ring + interrupt drain in the telemetry path. ✅

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

### Step 10a: Prefactor — one source, chosen explicitly — ✅ DONE

**Split out 2026-08-25, before 10b.** Adding `--serial` to a *precedence* chain makes
an existing footgun worse: with three live transports, `--udp 9000 --replay boot.bin`
silently ignores one. Cheaper to fix the shape before adding to it than after.

**Acceptance criteria**:
- `--replay` / `--udp` (and later `--serial`) are **mutually exclusive** — a clap
  `ArgGroup`; passing two is a usage error naming both, not a silent win.
- The default socket stays the fallback when no source flag is given.
- `Source::resolve` takes a **`SourceSelection` config struct**, not positional
  `Option`s (CLAUDE.md: config structs over long positional parameter lists — the
  4-arg version would put two transposable `Option`s side by side).
- Existing `resolve`/`policy`/`describe` behaviour is unchanged through the new shape.

`resolve` stays **total** — the conflict is enforced at the CLI layer, so the library
function keeps its documented precedence as defence-in-depth rather than returning a
`Result` the CLI can never trigger. The precedence tests then still pin behaviour the
type system does not.

**RED**: a `try_parse_from` test asserting `--replay x --udp 9000` is an
`ArgumentConflict`, plus the existing resolve tables rewritten against
`SourceSelection`.
**GREEN**: the `ArgGroup` + the struct.
**MUTATE**: `cargo xtask mutants -p collector` on `resolve`.
**KILL MUTANTS**: a mutant that drops the conflict must fail the parse test.
**REFACTOR**: assess whether `describe`/`policy` want to hang off `SourceSelection`.
**Done when**: two-source invocations are refused, existing tests pass through the new
shape, gate green, approved.

**Result**: `ArgGroup` over `replay`/`udp`/`serial`; `SourceSelection` carries the
flags and `resolve` takes it. `describe`/`policy` stayed on `Source` — they describe
the *resolved* transport, not what the operator asked for.

**Mutation caveat worth keeping.** `resolve`'s only mutant is `replace -> Self with
Default::default()`, which is **unviable** — `Source` has no `Default`, and
cargo-mutants does not mutate `match` arms individually. So the mutation pass says
nothing about whether the precedence tests are effective, and reporting "0 survivors"
without that would overstate it. Deriving `Default` to make it mutable would be
contorting production code for the tool; a default `Source` has no meaning.

### Step 10b: Collector `--serial` source — 🟡 CODE LANDED, BOARD-UNVERIFIED

**Acceptance criteria**: `cargo xtask reader --serial <dev> --baud N` decodes the
board's live stream; the board reaches Grafana. **⏳ The hardware half is
outstanding** — everything below is in-tree and gate-green, but the criterion above
is about a board, and no board has been involved.

**What landed**: `Source::Serial(SerialConfig { device, baud })` with `Resync`
policy and a `describe` naming both; `SerialReader`; `call_out_alternative`;
`--serial`/`--baud` (the latter `requires = "serial"`, so a stray `--baud` is a
usage error rather than a silent no-op). `cargo xtask reader` already forwarded
trailing args, so it needed no change. 111/111 in `collector`; mutants 13 caught,
3 unviable, 3 timeouts, no survivors.

**What is untested, precisely**: the `serialport::open` call. Everything above it is
covered by a scripted mock; that one line is glue whose first real exercise is the
board. Expect the failure modes there to be the ones no unit test can reach — wrong
device path, a port held by a stray `screen`, a baud mismatch delivering garbage
rather than silence.

**The real content is the read loop, not the enum variant.** `decode_stream`
(`protocol/src/stream.rs`) ends the session on *both* of the things a serial port
routinely does — any `Err` propagates, and `Ok(0)` is treated as clean EOF. A port with
a read timeout returns `TimedOut` during a quiet gap between heartbeats, so a **silent
board would look like end-of-stream** and the collector would exit mid-session. The
adapter that absorbs both is the testable core of this step, and it needs no hardware:
a mock `Read` scripting `bytes → TimedOut → bytes` proves it.

Also here: **refuse a `tty.*` device** with a message naming the `cu.*` alternative.
On macOS `tty.*` blocks in `open()` until carrier detect that a USB-TTL adapter never
asserts — an infinite hang, not an error. The collector is the first thing in the repo
to open a serial port, so the check lands here and
[board-bridge.md](board-bridge.md) step 1 reuses it rather than reimplementing it.

Policy is `OnDecodeError::Resync` (a physical line is lossy). `--baud` defaults to
**115200** — the measured choice from Step 6. The wasm core must still build
(`--no-default-features`); `serialport` goes under the existing `native` feature, which
is what Step 0 exists to protect.
**RED**: the source abstraction from step 3 is already tested; add serial-specific
config/parse tests only.
**GREEN**: a serial source implementing step 3's abstraction.
**MUTATE**: `cargo xtask mutants collector`.
**KILL MUTANTS**: address survivors.
**REFACTOR**: assess.
**Done when**: gate green, board telemetry in Grafana. *Should be small — step 3
did the design work.*

**Two findings worth carrying forward:**

- **The portability check that counts is the wasm32 one.** `cargo build -p collector
  --no-default-features --lib` on the *host* is not the same check as the gate's
  `--target wasm32-unknown-unknown`, and only the latter would catch a native-only
  dependency leaking into the core. Both pass; the point is that the weaker one was
  briefly mistaken for the stronger.
- **`serialport` is `default-features = false`.** We open a device path the operator
  named and never enumerate, so the `libudev` backend buys nothing and would add a
  Linux system dependency for a feature we do not call.

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
