# Plan: the board bridge — driving the VisionFive 2 from the host

**Status (2026-08-31)**: 🚧 **STARTED — steps 1–4b done and board-verified; step 6
is blocked by firmware, not by us.** First light happened 2026-08-28
([../notes/board-session-2026-08-28.md](../notes/board-session-2026-08-28.md)):
`board exec` drove a real UART, the input echo earned its keep immediately, and
step 4b shipped as **`cargo xtask board uboot`**, which caught the prompt across a
power cycle and ran every command of that session.

**The one thing that did not survive hardware is reboot.** SBI SRST *hangs* this
board's OpenSBI in its PMIC reset path — it neither resets nor returns — so
`board reboot` costs a manual power cycle instead of saving one, and step 5's
"a return means failure" contract never gets to apply (step 6 has the detail and
the two remaining candidates). **Until that is solved the unattended loop cannot
close on this board**, which is the main thing standing between Phase 1 and the
value Phase 2 assumes.

Still true from the previous status: `serialport::open` and the live read loop have
now been exercised, so step 4's untested boundary is closed, but nothing here has an
itest — the transport is a physical port, and the pure halves (`reach`, `stop`,
`split`, `script`, `knock`, `thrash`) are what the gate covers. Two phases — the host
bridge (steps 1–6b, including the U-Boot layer that makes netboot zero-touch) and the
ESP32 transport (steps 7–9).

**Unblocked 2026-08-25**: its prerequisite, [uart-telemetry.md](uart-telemetry.md)
**Step 10** (collector `--serial`), has landed and is gate-green. **Run against a
board 2026-08-28**: it decoded a real capture completely via `--replay`, so the
collector's serial path is proven; M2 is held open by a kernel bug instead (see that
plan).
**Design**: [docs/board-agent-bridge-design.md](../docs/board-agent-bridge-design.md)
**Milestone**: follows M2 in [visionfive2-port.md](visionfive2-port.md)

## Goal

`cargo xtask board` — open the board's UART, drive it programmatically, and get
structured frames back, so a hardware iteration (flash → reset → run → read) needs
no human at the board. Then move that same wire onto WiFi, so it needs no laptop at
the board either.

## The ladder this sits in

Three steps, decided 2026-08-25. Each makes the next cheaper, and the ordering is
what keeps any one of them from being a big-bang project:

| | Step | What it buys | Cost |
|---|---|---|---|
| **1** | **Host bridge** (Phase 1 below) | `cargo xtask board exec` — the loop stops needing a human at the board | days, all host-side |
| **2** | **ESP32 as the UART transport** (Phase 2 below) | the laptop stops needing a cable to the board | a weekend, **no kernel code** |
| **3** | **Full IP on the VF2** ([vf2-gmac-driver.md](vf2-gmac-driver.md)) | the *board* speaks IP — M2.5 on hardware | weeks, all the hardware risk |

**How each step actually helps the next**, stated precisely so the claim can be
checked rather than assumed:

- **1 → 2 is code reuse.** The stop-condition evaluator, the two-phase decoder, the
  reboot logic and the CLI surface are all transport-agnostic. Step 2 is a second
  implementation of one seam — provided step 1 builds that seam rather than
  hardcoding a serial port, which is why it is called out in Step 4 below.
- **2 → 3 is tooling, not code.** The ESP32 retires **zero lines** of the GMAC
  driver. What it changes is the loop that driver gets built in: untethered,
  remotely resettable, and able to run unattended overnight. Over a multi-week
  bring-up on a peripheral with many silent failure modes, that is worth more than
  it sounds — but nobody should expect it to shrink step 3's diff.
- **2 → 3 has one genuine engineering payoff beyond the loop:** it gives the GMAC
  bring-up a **diagnostic channel on a physically independent link**. When the thing
  being debugged *is* the network path, having telemetry arrive over a different
  radio on different pins is exactly what you want. If UDP-over-GMAC comes up broken,
  the UART/ESP32 channel is still there to say why.
- **Bonus, off the arcade roadmap:** step 2 buys down the controller path. §7 of
  [docs/arcade-and-real-hardware-direction.md](../docs/arcade-and-real-hardware-direction.md)
  already plans ESP32 + Bluepad32 → UART for wireless gamepads. Doing the transparent
  bridge first means learning the firmware toolchain, the wire discipline and the real
  latency characteristics on a low-stakes project, before the arcade depends on them.

## Why this, and why now

**This is the development environment for every remaining hardware project**, not
a feature in its own right. Driver bring-up on real hardware *is* the loop: flash,
reset, read breadcrumbs, edit, repeat, with frequent wedges needing recovery. Two
projects are queued behind it and both are worse without it — the PWMDAC audio
bring-up ([vf2-audio-tier0.md](legacy/vf2-audio-tier0.md)), whose `PollUntilSet` hang is
the motivating failure, and the GMAC driver
([vf2-gmac-driver.md](vf2-gmac-driver.md)), which is weeks of exactly this loop on
a peripheral with many ways to hang silently.

**It is deliberately not a step toward the board speaking IP.** It retires none of
the GMAC's cost and exercises no new kernel network code. The relationship runs the
other way: it is the tool you would build the GMAC *with*. Anyone reading this plan
hoping it shortens the network-console path should read the addendum in
[docs/network-telemetry-design.md](../docs/network-telemetry-design.md) instead.

## What already exists (do not rebuild)

| Piece | Where |
|---|---|
| COBS framing — self-framing, resyncable serial stream | `protocol::wire_encode`, `decode_stream` |
| `console=frames` — kernel + userspace console output as `Frame::Log` | shipped (B3 Step 4) |
| `Frame::Log` → clean stdout rendering | shipped (B3 Step 5, `log_text`) |
| Source abstraction — `Source::open() -> Box<dyn Read>` | `collector/src/source.rs`; **a serial port is a `Read`** |
| The `cu.*`/`tty.*` refusal, naming the corrected path | `collector::serial::call_out_alternative` — **reuse it, step 1 does not rewrite it** |
| An idle port not reading as end-of-stream | `collector::serial::SerialReader` — the same hazard `exec` faces |
| …both reachable without the HTTP stack | `collector::serial` is **not** `native`-gated, so `xtask-board` depends on `collector` with `default-features = false` |
| Image delivery — `cargo xtask image` → TFTP root, board netboots on reset | shipped |
| Raw-mode TTY, keystroke injection, restore-on-drop | `snemu/src/interactive.rs` — crib it |
| SBI ecall wrappers (`set_timer`, `send_ipi`, `hart_start`) | `kernel/src/sbi.rs` — SRST is **not** among them |

## Scope

**In:** ladder steps 1 and 2 — **Phase 1**, Piece 1 (the bridge + `exec`) and Piece 2's
**L0** (software reboot via SBI SRST) from the design note; then **Phase 2**, the ESP32
transport that cuts the laptop's cable. They are one plan because Phase 2 is a second
implementation of one seam, not a second project.

**Out, each for a reason:**

- **Piece 3 — the liveness snapshot + kernel watchdog (L1).** Its own plan. It is
  observability work that stands on its own merits, it is the largest piece, and
  nothing here depends on it.
- **L2 — the reset backstop.** The recovery for a board even the kernel watchdog
  cannot save. Out of *this* phase, but note that Phase 2 changes its economics
  completely: as designed it meant procuring a relay board and wiring it to a laptop
  that has to be near the board, and with an ESP32 already sitting on the UART header
  it becomes one GPIO and a transistor, commanded over the same link — and the ESP32
  can watch for silence and pulse reset **autonomously**, with the laptop asleep. See
  Phase 2, step 9. Still check the JH7110's on-chip watchdog first; if it exists and
  survives the boot flow, even that becomes near-vestigial.
- **Step 5b of [uart-telemetry.md](uart-telemetry.md)** (raw stdin → the guest REPL,
  a *human* terminal). That is the interactive sibling of this plan's `exec`, and it
  is already planned there. Do it before or after; the two share the serial handle
  but not the surface, and this plan builds the **programmatic** one.
- **A snemu-backed transport — `board exec` against the emulator.** Out of Phase 1,
  but named here because it **constrains step 4's seam** and is cheap only if the
  seam is right the first time. See the follow-on section below.

## Follow-on: pointing the bridge at the emulator

**Sequence it directly after step 4, not "someday".** Recorded here because step 4
designs the seam it needs, and because the cost is small *only* if that seam is
right the first time.

**It is also the one backend that needs no hardware, no cable and no mutex on a
single board** — which makes it the one an agent can actually use, and the one that
works while somebody else is holding the VF2.

The bridge's transport is a seam because Phase 2 swaps a TCP socket behind it.
snemu is a natural third implementation, and the argument for it is *not* the
obvious one:

- **The first argument is that an agent cannot drive the emulator at all today**,
  and the gap is specifically **input**. Reading works on a pipe — `snemu boot
  --max-steps N` streams UART fine. Typing does not: keystrokes are read only while
  raw mode is active, raw mode requires a TTY (`snemu/src/interactive.rs`,
  `RawMode::enter`), and snemu says so itself when stdin is a pipe — *"streaming
  output, but nothing typed will reach the guest"*. That is deliberate and correct:
  a blocking read on a pipe would stall the emulator mid-run, and `O_NONBLOCK` on
  stdin would leak onto the parent shell's file description. `--interactive` also
  sets `max_steps = u64::MAX`, bounded by the person at the keyboard — a person an
  unattended agent does not have.

  The escape hatch snemu names — *"use `xtask itest` to script input"* — means
  authoring a Rust scenario, registering it in `SCENARIOS` and rebuilding. That is
  the right shape for a **regression test** and the wrong shape for a **question**.
  So "ask the running OS something" has no cheap path at all right now, on emulator
  *or* board, and `exec` is what a cheap path looks like.
- **The second argument** is that it closes a hole in *this* plan. Step 4 says the
  serial `open`/`read`/`write` is glue "exercised by hand against the board" — so
  the CLI assembly, the capture loop and the exit-code mapping have **no automated
  coverage at all**. Steps 1–3 are pure precisely because board round-trips are
  expensive, which means everything above them is untested until someone plugs in a
  cable. An emulator backend puts `board exec` in the gate.
- **The third argument** is that a seam with one implementation is a guess. Phase 2
  is weeks out; snemu would make the abstraction real while it is still cheap to
  change.
- **The architectural pull** is the one in
  [docs/board-agent-bridge-design.md](../docs/board-agent-bridge-design.md): one
  thing a browser talks to, backed by either a real board or a host emulator.

**Three constraints, all of which make this smaller than it sounds — or larger:**

1. **It covers `exec` and decode, not the U-Boot layer.** snemu has no U-Boot, so
   steps 4b/4c/6b — reaching the `=>` prompt, `provision`, `boot --workload` — have
   no emulated counterpart and stay board-only. That is half of Phase 1.
2. **It would not faithfully exercise step 3.** Under snemu, telemetry and console
   are *separate* channels (`virtio_tx_output()` vs `uart_output()` in
   `xtask-itest/src/itest/harness.rs`), so the mixed-stream problem step 3 exists to
   solve does not arise there. Exercising the splitter needs snemu run in the VF2's
   UART-telemetry mode, or the merge is a fiction that tests nothing.
3. **It cannot live in `xtask-board`.** That crate deliberately pulls `collector`
   without `native`; linking snemu would recompile the emulator on every bridge
   edit — exactly what the `xtask-itest` and `xtask-cram` splits exist to prevent.
   A separate crate is the established answer here.

**The one decision that must be made now, in step 4:** keep the seam a plain byte
stream. An emulator *could* hand back `OwnedFrame`s directly, skipping step 3's
`split` and the COBS decode entirely — and that would be a bypass wearing a test's
clothes, green while the real path rots. Emulated bytes must travel the same road
as board bytes.

## Acceptance Criteria (Phase 1)

Phase 2 has its own, below.

- [ ] `cargo xtask board exec "<text>"` writes input to the board UART and captures
      output until a stop condition, returning both the raw text and the decoded
      frames.
- [ ] Three stop conditions work and are distinguishable in the result: a quiescence
      window (N ms with no new bytes), a marker match, and a timeout.
- [ ] A mixed stream — U-Boot's plain text, then the kernel's COBS frames — is split
      correctly without the caller declaring which phase it is in.
- [ ] Opening a `tty.*` device is **refused with a message naming the `cu.*`
      alternative**, rather than blocking forever.
- [ ] A port already held by another process fails with an error that says so (and
      names the holder if it can be identified) — never reported as board silence.
- [ ] The port is released on every exit path. Panic is covered by ownership (see
      Step 1's result); the live risk is `std::process::exit`, which runs no
      destructors — step 4 returns an `ExitCode` instead.
- [ ] `cargo xtask board reboot` reboots the board via SBI SRST and returns once the
      fresh image is booting; the "task hung"/reboot line is flushed before the reset
      takes effect.
- [ ] **Netboot is zero-touch**: after one `cargo xtask board provision`, a reset boots
      the current image with nobody at the U-Boot prompt — and `provision` discovers
      this Mac's IP rather than hardcoding it, so moving networks is a re-run, not a
      debugging session.
- [ ] `cargo xtask board boot --workload X` selects the workload without a human
      retyping `setenv bootargs`.
- [ ] A stale `snitchos.img` and a drifted `serverip` are each caught by preflight and
      reported as themselves, before they become a mysterious TFTP timeout or a
      phantom regression.
- [ ] An unattended loop cannot hammer the board: a hard iteration cap and a minimum
      inter-reboot interval, both enforced and both reported when they fire.
- [ ] The full gate stays green: `cargo xtask test && cargo xtask itest && cargo
      xtask itest --scramble`.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test. Host tests run with `cargo nextest run` (never plain `cargo
test`); mutation via `cargo xtask mutants`. **Present acceptance criteria and get
confirmation before writing any code for each step.**

The ordering is deliberate: steps 1–3 are pure host logic with no board and no
serial hardware, step 4 is the CLI that assembles them, and only steps 4b onward
involve the kernel or a physical board. Same host-first discipline as the rest of the
port — a board round-trip is expensive.

Steps **4b, 4c and 6b are the U-Boot layer** — reaching the prompt, provisioning a
saved environment, and per-run bootargs plus preflight. They exist because the netboot
loop is *not* zero-touch today, contrary to what the design note claimed until it was
corrected: `serverip` names a Mac whose IP moves, and the workload rides U-Boot's
`bootargs` rather than the image. Both are hand-typing today, and hand-typing is what
this whole plan exists to delete.

## Phase 1 — the host bridge (steps 1–6b)

---

### Step 1: Port-open policy — device kind and failure classification — ✅ DONE

**Result** (`xtask-board/src/reach.rs`, 9 tests, mutants 5 caught / 1 unviable / 0
survivors): `check_device` refuses a `tty.*` path *before* opening it — delegating
to `collector::serial::call_out_alternative` rather than restating the rule — and
`classify_open_failure` maps an `ErrorKind` to an `Unreachable`. `holder_pid` reads
the pid from `lsof` output, tolerating a malformed row rather than panicking on an
external tool's output.

**The variants partition by what the operator must do differently** — kill a
process (`PortHeld`), fix permissions (`NoPermission`), plug the adapter in
(`NoSuchDevice`) — with `OpenFailed` keeping the kind of anything unrecognised. A
single catch-all would have been honest about the failure and useless about the
remedy, which is the whole point of the step.

**Two things changed during the work:**

- The `cu.*`/`tty.*` half was already done — it shipped with Step 10b of
  [uart-telemetry.md](uart-telemetry.md), so this step reused it.
- **The release-on-drop criterion was cut as ceremony, and replaced with a sharper
  constraint on step 4.** A guard type was written, then deleted: `serialport`'s
  handle closes on drop and Rust runs destructors while unwinding, so a *panicking*
  bridge already releases the port, and a wrapper testing that would have been
  testing the language. The case ownership does **not** cover is
  `std::process::exit`, which runs no destructors — and that is not hypothetical:
  `xtask-itest/src/itest.rs` already installs a Ctrl-C handler calling
  `process::exit(130)`, and Ctrl-C is exactly how a person ends an interactive
  bridge session. **Step 4's rule: return an `ExitCode`, never `process::exit`
  while the port is open; a signal handler must release first.**

### Step 1 (as originally written): Port-open policy — device kind and failure classification

The two documented ways this goes wrong, as pure functions, before any I/O exists.

**Acceptance criteria**: a device-path classifier reports `/dev/tty.usbserial-X` as
the call-in node and `/dev/cu.usbserial-X` as call-out, and the bridge refuses the
former with a message naming the latter. An open-failure classifier maps a
permission/busy error to a distinct "port is held" variant carrying the path, so
"cannot open the port" and "opened it, board said nothing" are different values —
not the same silence.

**Why this is step 1**: on hardware these two have identical downstream symptoms and
completely different fixes, and the `tty.*` failure mode is an *infinite block*, not
an error — the worst possible thing for an unattended bridge. Both were measured on
the VF2 dev host, so the tests encode observed behaviour, not speculation.

**The specifics are serial-only; the principle is not.** Phase 2 puts this same
bridge on a TCP socket, whose failure set is different (connection refused, the ESP32
rebooted, the AP dropped) but whose hazard is identical: every one of them must report
as *itself* and never as board silence. Write `BridgeError` so a transport contributes
variants rather than so it owns the enum.

**RED**: a table of `(path, expected kind)` cases including both node flavours and a
non-serial path; a table of `(io::ErrorKind, expected BridgeError)`.
**GREEN**: the two classifiers, no I/O.
**MUTATE**: `cargo xtask mutants -p xtask` (or wherever the bridge module lands).
**KILL MUTANTS**: a mutant that collapses the two error variants into one must fail a
test — that collapse is precisely the bug being designed out.
**REFACTOR**: assess whether holder identification (`lsof` on the device node) belongs
here or in the glue; it shells out, so probably the glue.
**Done when**: both tables pass, mutation reviewed, gate green, approved.

---

### Step 2: The capture loop's stop conditions — ✅ DONE

**Result** (`xtask-board/src/stop.rs`, 18 tests, mutants 15 caught / 4 unviable / 0
survivors): one `StopCondition` value carries all three conditions and `Capture`
reports which fired. The plan's REFACTOR question — do they want to compose? — was
answered yes up front rather than at the end, so there is no per-condition call site
to unify later.

**Three decisions the tests now pin, each of which could have been the quiet bug
this step exists to prevent:**

- **The timeout is mandatory; the other two are optional.** That asymmetry makes "a
  capture that never returns" unrepresentable, which is the property an *unattended*
  bridge needs most. Consequence for step 4: `--until-quiet` and `--until` are not
  alternatives to `--timeout`, they are additions to a default one.
- **Quiescence is armed by the first byte, never by capture start.** A 300 ms window
  measured from `t=0` cuts off a board that takes 800 ms to answer — the "clock
  starts at the wrong moment" failure the step names. Total silence is the
  *timeout's* answer, and it reports as itself.
- **A zero-byte read is a tick, not data.** A capture loop polling a quiet port sees
  far more empty reads than arrivals; refreshing the quiet clock on each would stop
  quiescence from ever firing. `observe(t, &[])` and `tick(t)` are the same call.

Precedence when two come due on the same event is `Marker` > `Timeout` > `Quiet`: a
marker found in bytes that genuinely arrived is a success, and reporting it as a
timeout would make a working round-trip look like a wedged board.

**Two deviations from this step as written, both approved before coding:**

- **No `WallClock`; `observe` takes elapsed-since-start as a parameter.** More
  deterministic than a fake with hidden state, and it keeps the layer a pure
  function of its arguments. The single real `Instant::now()` lands in step 4's
  capture loop, where the I/O already is.
- **`retained_bytes()` is public.** The memory bound — straddle context, never
  history — is a requirement for a capture that may run for minutes, and an
  invariant nothing can observe is an invariant nothing checks.

**What mutation testing actually caught**, since it was not the predicted target:
the `>=` boundaries were all killed by the tests as written, but a
`retained_bytes -> 0` mutant survived. The retention test asserted a *ceiling*
(`< marker.len()`), which retaining nothing satisfies — a different bug wearing the
same number, costing no memory and silently breaking every split match. Fixed by
asserting the exact value. **A bound needs both of its sides.**

**REFACTOR — the tail buffer is *not* shared with step 3's resync scanner.**
Assessed and declined: they retain for incompatible reasons. Step 2 holds a bounded
window (`marker.len() - 1`) because the marker is the only thing it looks for and
everything older is provably useless. Step 3 must hold *every* byte since the last
delimiter, unbounded, until a frame either completes or is abandoned. Same word,
different lifetime — sharing them would mean the looser bound wins.

### Step 2 (as originally written): The capture loop's stop conditions

**Acceptance criteria**: given a sequence of `(timestamp, bytes)` arrivals and a
`StopCondition`, the evaluator reports stop-or-continue and **which** condition
fired. Quiescence fires only after N ms with no new bytes (and never on a stream
still producing); a marker fires on the first match, including one split across two
arrivals; a timeout fires from the start of capture regardless of traffic.

**Why a pure evaluator**: this is the heart of `exec` and the part most likely to be
subtly wrong (quiescence that starts its clock at the wrong moment; a marker missed
because it straddled a read boundary). Time is injected, per the existing
`WallClock` pattern in the collector, so the tests are deterministic and instant.

**⚠ Quiescence does not survive the move to WiFi unchanged.** Over a direct UART,
silence means the board is silent. Over Phase 2's TCP transport it may mean a
few-hundred-millisecond WiFi stall, and a quiescence window tuned for serial will
false-fire and report a wedged board that is fine — the exact failure this tool exists
to prevent. Make the window a parameter of the *transport*, not a constant, and prefer
marker-based stops over quiescence when on TCP. Encode a "stall shorter than the
window must not stop the capture" case in the tests now, while it is free.

**RED**: arrival-sequence tables driving a `FakeWallClock` — including the
split-marker case, a "still producing, must not stop" case, and the stall case above.
**GREEN**: the evaluator over an injected clock.
**MUTATE**: the comparison boundaries (`>=` vs `>`) on the quiescence window and
timeout are the targets.
**KILL MUTANTS**: a mutated quiescence comparison must break the "still producing"
test.
**REFACTOR**: assess whether the three conditions want to compose (stop on
*whichever* fires first) — they do; make that a value, not a special case.
**Done when**: tables pass, mutation reviewed, gate green, approved.

---

### Step 3: Two-phase decoding — U-Boot text, then kernel frames — ✅ DONE

**Result** (`xtask-board/src/split.rs`, 11 tests, mutants 9 caught / 3 unviable /
0 survivors): `split(&[u8]) -> Split { io_text, frames, resyncs }`.

**The specification collapsed to one sentence: *text is the bytes that aren't
frames*.** Extract every frame, concatenate the leftovers. That rule is uniform —
it does not care whether the leftover is U-Boot's banner, a kernel `println!`
between two frames, or a second boot after a mid-capture reset.

**This step was designed wrong twice before it was designed right**, and both
errors are worth keeping because both are easy to repeat:

1. **Emitting one item per text *line*.** A line is a display concept. Committing
   to it meant finding line boundaries in a half-binary stream — and COBS strips
   `0x00` **and only** `0x00`, so `0x0A` rides through a frame body freely.
   Measured: `SpanId(10)` *is* a newline byte, and 2.7% of `SpanEnd` frames carry
   one from the timestamp varint alone. A newline-delimited splitter cuts those
   frames in half and reports both halves as text, silently, correlated with the
   data rather than randomly. **The plan's own "try-COBS-with-text-fallback per
   line" says to do this. Don't.**
2. **Using `\n` to enumerate candidate frame starts.** Less wrong, still the wrong
   layer. Swept over every offset in a real handoff chunk: the schema alone
   identifies the true start, and the newline candidates *miss it entirely* when
   the text before a frame has no newline (`=> booti …`), which the U-Boot log
   happens to avoid by luck of its formatting.

**What the frame-start search actually is.** A `0x00`-delimited chunk that fails
to decode is searched backwards from its terminator, bounded by `MAX_FRAME_BYTES`
(520, from `ENCODE_SCRATCH` in `kernel-obs/src/uart_sink.rs` — the sink refuses to
emit anything larger, so no frame can start further back). Two filters: it must
decode, and everything before it must still look like text. Where several
candidates survive — `CapEvent`'s NUL-padded name admits a few — **the latest
wins**: swept over 96 mixed chunks (16 frame shapes × 6 text prefixes),
latest-wins was wrong 0 times and earliest-wins 3.

**⚠ Why the search is not optional — and a bug it found in shipped code.** U-Boot's
log contains no `0x00`, so the first terminator on the wire belongs to the
kernel's *first frame*: the whole boot log and that frame land in one chunk. Drop
it and you lose `Hello`, which `open_stream` sends exactly once per boot
(`kernel/src/obs/tracing.rs`) and which carries the `timebase_hz` every later
timestamp is relative to.

**`collector --serial` does exactly that today.** Measured by feeding a realistic
mixed stream through the collector's own decoder and policy
(`Source::Serial → OnDecodeError::Resync`, `collector/src/source.rs:106`):

```
sent:    Hello, BuildInfo, StringRegister, SpanStart
decoded: BuildInfo, StringRegister(kernel.boot), SpanStart
resyncs: 1
Hello reached the host: false
```

and `collector/src/state.rs:240` then drops every frame that arrives without an
anchor, after one warning, exiting 0. So a real `--serial` session would report
success having recorded nothing. It is latent rather than observed — `--serial` is
marked board-unverified in [uart-telemetry.md](uart-telemetry.md), and it cannot
happen over virtio-console, where telemetry has its own channel. **Not fixed here:
`collector/` is out of this plan's lane.** The cheapest real fix is probably a
`Hello`-seeking resync inside `protocol::stream`, which would fix the collector,
this bridge, and anything else on a UART at once.

**REFACTOR — the collector's resync policy is *agreed with*, not shared.**
`decode_stream` owns its own read loop and has no notion of text; there is nothing
to reuse but `try_decode_frame`, which this does. The policy names match
(`OnDecodeError::Resync`) because they are the same idea, not the same code.

**Two corrections mutation testing forced**, both worth reading before writing the
next step:

- **A guard documented as "the load-bearing half" was tautological.** It compared
  `consumed` against the chunk length to reject a partial-span decode — but
  `try_decode_frame` derives `consumed` from the *delimiter's position*, not from
  what postcard read, so it is always equal. The check was deleted and the doc
  comment corrected to say what the layer genuinely cannot know.
- **An index loop advancing past each terminator carried a `+ 1` whose mutant
  hangs the splitter.** Rewritten over `split_inclusive`, which expresses the same
  chunking with no arithmetic to mutate. A splitter that hangs on a capture is a
  worse failure than one that mis-splits it, and the mutant count fell 19 → 12.

**Process note, recorded rather than glossed:** the split tests were written before
the implementation but the RED run picked up the new module mid-flight, so they
were never *observed* failing. Mutation testing stood in as that evidence. Writing
tests first is not the same as watching them fail.

### Step 3 (as originally written): Two-phase decoding — U-Boot text, then kernel frames

**Acceptance criteria**: a byte stream containing U-Boot's line-oriented text,
then the `booti` handoff, then COBS-framed kernel telemetry, splits into an ordered
sequence of text lines and decoded frames — **without the caller declaring which
phase the stream is in**. Undecodable bytes resync at the next `0x00` and are
counted, never fatal.

**Design note's open question, answered here**: try-COBS-with-text-fallback per line
versus an explicit phase switch. Build the former (it needs no handoff detection and
degrades gracefully if the board reboots mid-capture, which is the whole point of
this tool); the tests are the evidence for whether it is robust enough.

**RED**: a crafted stream — text lines, then frames, then garbage, then frames again —
asserting the exact ordered output plus the resync count. A "text line that happens to
contain a `0x00`" adversarial case.
**GREEN**: the splitter over the existing `decode_stream` / `try_decode_frame`.
**MUTATE**: `cargo xtask mutants` on the splitter; the resync-skip loop is the target.
**KILL MUTANTS**: a mutant that swallows the frame after a resync must fail.
**REFACTOR**: if this duplicates the collector's resync policy, share it — they are
the same policy (`OnDecodeError::Resync`).
**Done when**: the stream splits correctly, mutation reviewed, gate green, approved.

---

### Step 4: `cargo xtask board exec` — the CLI over steps 1–3 — 🟡 CODE LANDED, BOARD-UNVERIFIED

**Result**: `xtask-board` gained a binary; lean `xtask` forwards via `delegate_to`.
Exit codes are the interface — `0` ended as asked, `1` reached the board but the
awaited event never came, `2` never reached it — and `outcome::exit_code` shares
its rule with `script::run` through `StopCondition::satisfied_by`, so the CLI and a
script can never disagree about whether a step succeeded.

Two modules fell out that the plan did not anticipate, both because **mutation
testing found design flaws, not just missing tests**:

- **`wire::Ended`.** `capture` originally returned `StopReason::Timeout` for *both*
  a deadline and a dead transport, so no assertion could tell them apart — the
  mutants covering that branch were unkillable, which is what exposed it. That is
  the same conflation `reach` and `script` each exist to prevent, thrown away at
  the bottom of the stack. A mid-capture transport death is now an
  `Outcome::Unreachable` and exits `2`, so an unattended loop retries the *host*
  rather than rebooting a board that was answering fine.
- **`reach::refine_with_holder`.** The `NoDevice` disambiguation was inline in glue
  and deletable without a test noticing.

**Coverage**: 60 tests, 48 mutants caught, 17 unviable, 0 timeouts, **7 missed —
all in `main.rs`** (the entry point, `emit`/`report` formatting, the `consult_lsof`
shell-out whose *decision* is tested in the lib). That layer's real coverage is a
board, the same argument CLAUDE.md makes for excluding `kernel/`.

**⏳ Outstanding, and it is the acceptance criterion**: none of this has touched a
real UART. `serialport::open` and the live read loop are the untested boundary.
Verified by hand only that a bad path yields `NoSuchDevice` and a `tty.*` path is
refused *instantly* — the failure that otherwise blocks forever.

### Step 4 (as originally written): `cargo xtask board exec` — the CLI over steps 1–3

**Acceptance criteria**: `cargo xtask board exec "<text>" [--until-quiet Nms |
--until <marker> | --timeout Nms]` opens the port, writes the input, captures to the
stop condition, and prints the result. Data on stdout, diagnostics on stderr,
`--json` emits `{ io_text, frames[] }` for the agent-facing path. A non-zero exit
distinguishes "stop condition never met" from "could not open the port". The port is
released on every exit path including panic.

**RED**: the pure parts are already tested; add CLI-surface tests (flag parsing, exit
code mapping) in the style of `xtask`'s existing CLI characterisation tests. The
serial `open`/`read`/`write` is glue and is exercised by hand against the board.
**GREEN**: the `serialport`-backed handle plus the assembly. **First new host-side
dependency in this area** — fine for a host tool, worth naming in the commit.

**Build the transport as a seam, not a serial port.** The handle is
`Box<dyn Read + Write>` chosen by flag, because Phase 2 swaps a TCP socket in behind
it and step 3 of this ladder may want a third. This costs nothing now and is annoying
to retrofit, and the collector's `Source::open() -> Box<dyn Read>` already proved the
shape one milestone ago. The serial *specifics* (baud, the `cu.*` check) belong to the
serial branch, not to `exec`.

**`exec` is single-shot; the rest of Phase 1 is not — `script.rs` is the shape.**
✅ Built (`xtask-board/src/script.rs`, 6 tests, mutants 4 caught / 2 unviable / 0
survivors). `exec` sends one thing and captures one answer, which is the right
primitive and the wrong shape for steps 4b, 4c and 6b — each of those is a
send/expect *conversation*, and written as three hand-rolled loops they are three
chances to get the same thing wrong:

| command | the conversation it is |
|---|---|
| `uboot "<cmd>"` | keystroke → until `=> ` → cmd → until `=> ` |
| `provision` | N × (`setenv …` → until `=> `), then `saveenv` → until `=> ` |
| `boot --workload X` | setenv bootargs → until `=> ` → `boot` → until a boot marker |

`Step { send, until }` plus `run(&[Step], perform)` makes those *configurations*.
Same move step 2 already made one level down — composing the three stop conditions
into one value meant there was never a special case to unify later. Three
decisions in it:

- **A step that never saw what it awaited abandons the rest, unsent.** The safety
  property, not an optimisation: if the prompt did not come, the board is booting
  or wedged or mid-`saveenv`, and the next command is not a failed step but an
  *unpredictable* one. A `provision` that writes half an environment is worse than
  one that writes none. The test asserts on what did **not** reach the wire.
- **`Interrupted` is a distinct variant from `Abandoned`.** A dead transport is not
  a silent board. This layer says *where* it stopped; the caller, holding the
  `io::Error` or step 1's `Unreachable`, says why.
- **The I/O is a closure, not a port** — which is what makes "which steps never
  reached the wire" observable at all, since a test can then stand where the wire
  does. The real driver passes a one-line write-then-capture closure.

**A third backend is already identified — snemu — and it constrains this seam, so
read the follow-on below before finalising the handle type.** The short version:
keep the seam a plain byte stream even for an emulator that could hand back
structured frames directly. Feeding emulated bytes through the same decode path is
what makes the emulator a *test* of the bridge instead of a bypass of it.

**MUTATE**: `cargo xtask mutants` on the exit-code and flag-mapping logic.
**KILL MUTANTS**: a mutant that returns success when the stop condition never fired
must fail — that is the difference between a working loop and one that silently
believes a wedged board.
**REFACTOR**: consider whether `Source::Serial` (Step 10's collector source) and this
bridge should share one serial handle type. They should, if Step 10 has landed.
**Done when**: the command works against the real board, gate green, approved.
Requires the permission gate — this writes to physical hardware.

---

### Step 4b: Reach and drive the U-Boot prompt — ✅ DONE 2026-08-30

**Shipped as `cargo xtask board uboot`** and used for every boot of the 2026-08-28
session. Catches the prompt across a power cycle, runs `--cmd`s one at a time, then
streams — see [../notes/board-session-2026-08-28.md](../notes/board-session-2026-08-28.md).

```
cargo xtask board uboot --device /dev/cu.usbserial-0001 \
  --cmd 'setenv bootargs workload=gmac-probe' --cmd 'run bootcmd' --stream 45000
```

Most of it was already here: `script::run` had been written for exactly this
send/expect sequencing and simply never wired to a command. The new part is
`xtask_board::knock` (6 tests), which decides **what answered** — and the shape of
that decision is the step's real lesson, so it is worth recording against the
criteria as written.

**Three constraints hardware imposed that the plan did not anticipate**, each of
which cost a boot before it was understood:

1. **One process must own the port for the whole sequence.** Releasing it between
   `run bootcmd` and attaching a reader loses the boot log — the USB adapter
   buffers it, and the *next* opener reads a stale burst as though it were live.
   This is why `uboot` is one command rather than `board exec` called three times.
2. **Never reopen the device per keystroke.** Doing so resets termios to the
   default line rate, and the result is indistinguishable from a dead board.
3. **Ask again rather than scanning a buffer.** A prompt already in the capture
   proves the board *was* at a prompt, not that it is now. `knock` sends a bare CR
   each second and judges only what arrives after it — which also separates
   "autobooted past the window" (SnitchOS answers) from "genuinely silent",
   two states a rolling-buffer scan reports identically.

**Deviation from the criteria as written**, deliberately: the prompt is
`StarFive #`, not `=>`, and the race is not modelled against a `FakeWallClock`.
The countdown turned out not to be the hard part — knocking every 50 ms from
*before* the board powers on wins it every time, so there is no window to miss and
no timing logic to table-test. The pure logic worth testing was the classification
instead, and that is what `knock_tests` covers. **`bootdelay` stays non-zero** for
the reason below, and the knock cadence is what makes a short delay survivable.

**Not done**: no `--engine`-style verification against a booted kernel, and the
`uboot` command has no itest — its transport is a real serial port, and the pure
half is covered by `knock_tests` + `script_tests`.

**Why this is its own step**: it is the one interaction in the whole plan with a hard
real-time constraint — the autoboot countdown is a couple of seconds, and a keystroke
sent late means the board boots instead of stopping. Everything after it (steps 4c and
6b) is ordinary command/response at a prompt.

**⚠ Keep `bootdelay` non-zero, and treat it as the way back in.** Once step 4c saves a
`bootcmd`, the temptation is `bootdelay=0` for a faster loop. Don't: the countdown is
the only unprivileged way to interrupt a board whose saved environment is wrong, and
without it, recovering a bad `bootcmd` needs the boot-mode jumpers. A comfortable
delay is the cost of always being able to intervene. This also interacts with Phase 2 —
WiFi jitter eats into a tight window, so set the delay generously enough that the
network transport can still win the race.

**RED**: the prompt-detection and countdown-race logic is pure, given step 2's
evaluator — table-test `(arrival sequence, delay budget) → Reached | MissedWindow`
against a `FakeWallClock`, including a "prompt arrived one tick late" case.
**GREEN**: the interrupt sequence plus the `uboot` subcommand over step 4's transport.
**MUTATE**: `cargo xtask mutants` on the window logic.
**KILL MUTANTS**: a mutant that reports success when the window was missed must fail —
otherwise every later step silently talks to a booted kernel instead of U-Boot.
**REFACTOR**: this is step 2's marker stop condition with a deadline; if it is not
reusing it, find out why before duplicating.
**Done when**: the command round-trips against the board, gate green, approved.

---

### Step 4c: `cargo xtask board provision` — a saved env that netboots zero-touch

The step that answers "why do I keep typing U-Boot commands."

**Acceptance criteria**: one invocation writes a complete, persistent netboot
environment and **verifies it by reading it back** — board IP via `dhcp`, `serverip`
**discovered from this Mac at run time** (`ipconfig getifaddr en0`, already the
documented incantation in [visionfive2-port.md](visionfive2-port.md)) rather than
hardcoded, a `bootcmd` that fetches `snitchos.img` and `booti`s it with
`${fdtcontroladdr}`, a non-zero `bootdelay`, then `saveenv`. After it runs, a bare
reset boots the current image with no human at the keyboard.

**Discovering `serverip` instead of hardcoding it is the whole point.** The reason the
env goes stale is that it names a machine whose address moves; re-running `provision`
after switching networks is a one-liner, whereas a hardcoded value is a bug that
surfaces as a mysterious TFTP timeout.

**❌ Corrected 2026-08-28. The 2026-08-25 claim here — "a DHCP reservation removes
the failure *class*" — was wrong, and the board session of 2026-08-27 spent most of
an evening proving it.** It removed the *instance* and left the class untouched.

macOS **Private Wi-Fi Address** was *rotating* `en0`'s MAC, and **a reservation
cannot pin a MAC the client changes.** So `serverip=192.168.0.7` stayed in the saved
`bootcmd`, the Mac quietly became `.8`, the board ARP'd for `.7`, nothing answered,
and TFTP failed with ICMP port-unreachable. Nothing in the repo, the board, or the
docs changed — which is precisely what makes it a *class*. See
[../notes/board-session-2026-08-27.md](../notes/board-session-2026-08-27.md).

**Resolved 2026-08-28** by setting Private Wi-Fi Address to **Fixed** (stable per
SSID) and pinning the reservation to that MAC — so a reservation *is* the right
tool, once the address it names holds still. The 2026-08-25 claim was wrong about
the mechanism, not about reservations.

**This strengthens the case for `provision`, rather than the reverse.** The whole
point of the step as designed is that it **discovers `serverip` at run time**
(`ipconfig getifaddr en0`) instead of baking it into `bootcmd` — which is exactly
the defect the session found. I demoted this step on the strength of the wrong
belief; the correct reading is that a hardcoded `serverip` is a standing trap and
re-deriving it is the fix.

**What is genuinely true:** netboot *is* zero-touch today — verified end to end on
2026-08-27, `run bootcmd` → DHCP → TFTP (exact byte match) → `booti` → 4 harts up,
`saveenv` persisted. So this step is not urgent. But it is not merely a residue
handler either: it is the thing that stops the next drift, and there **will** be a
next drift until the address is re-derived rather than remembered. Until then, the
mechanism that depends on neither router nor UART is
`sudo ifconfig en0 alias 192.168.0.7 255.255.255.255` — make the Mac answer where
the board already looks.

Beyond that, `provision` still handles the **residue**: a wiped environment, a new
network, a second dev machine, or a board someone else is bringing up. Still worth
building (it is small, and it is what makes the loop reproducible rather than
remembered), but it dropped out of the critical path. Sequence it accordingly.

**RED**: the env-script builder is pure — `(board config) → ordered setenv commands` —
so golden-test it, plus a read-back verifier that fails when a variable did not stick.
**GREEN**: the builder, the `provision` subcommand over step 4b.
**MUTATE**: `cargo xtask mutants`. The verifier is the target.
**KILL MUTANTS**: a mutant whose verifier passes when `saveenv` silently failed must
fail a test. "I set it and assumed" is exactly the bug this step is fixing.
**REFACTOR**: assess whether `provision` and `boot --workload` (step 6b) share an env
model. They should.
**Done when**: a wiped environment is restored by one command, a bare reset then
netboots unattended, gate green, approved.

---

### Step 4d: `cargo xtask board loopback` — split the stack in one physical move

**Acceptance criteria**: with the adapter's RX and TX jumpers pulled off the board
and touched together, `cargo xtask board loopback --device …` writes a known string,
reads it back, and reports pass/fail. On pass it says explicitly that the adapter,
cable, driver, baud and host path are all good and **the fault is at the board end**.

**Why this earns a step.** 2026-08-28 lost its first hour to a silent board while I
theorised about console modes and firmware. The cause was **failed dupont wires** —
and reseating them twice did not fix it, so "check the connections" is not the same
advice. A loopback is one physical action with an unambiguous verdict that partitions
the *entire* stack: everything host-side on one side, everything board-side on the
other. The 2026-08-27 session's "board went quiet — unexplained" was almost certainly
the same fault, written up as a mystery for want of this test.

It also pairs with the cheaper question that costs nothing: **does the silence cover
output our code cannot possibly control?** U-Boot's banner is the ideal probe — if
that is missing too, no amount of kernel debugging will help, and the loopback is the
next move rather than the tenth.

**RED**: the logic is thin but not nothing — "wrote N bytes, read back M, compare" has
a real timeout case and a real partial-read case, and both must be distinguishable
from a mismatch. Table-test against the existing `wire::capture` seam.
**GREEN**: the subcommand over step 4's transport; a `StopCondition` on the echoed
string with a short deadline.
**MUTATE**: `cargo xtask mutants xtask-board`.
**KILL MUTANTS**: a mutant that reports pass on a *partial* echo must fail — a
half-echo is a real symptom (wrong baud, marginal wire) and reporting it as success
would send the operator to the board end for a host-side fault, which inverts the
command's entire purpose.
**REFACTOR**: if this duplicates `exec`, make it `exec` with a preset condition.
**Done when**: gate green, and it has been run once against a deliberately
disconnected adapter (which needs no board — that is the point).

---

### Step 5: SBI SRST — the kernel side of reboot

**Acceptance criteria**: a magic console line (exact token decided at CONFIRM)
triggers `sbi_system_reset(cold_reboot)`. The kernel emits a reason frame **and
drains the UART TX FIFO before the reset fires**, so the host sees why it rebooted
rather than an unexplained silence. Absent the magic line, console input behaviour is
byte-identical to today.

**The flush is the whole difficulty.** Borrow the shape the panic path already
proved: emit → bounded TX drain → act (see
[legacy/panic-emits-telemetry.md](legacy/panic-emits-telemetry.md)). A reset that
wipes state before the frame is on the wire yields a truncated frame and a
diagnostic-free reboot.

**RED**: host-test the line detector in `kernel-boot`/`kernel-devices` (pure: a byte
stream in, "reboot requested" out — including the token split across two reads, which
a 256-byte ring makes possible). The SRST ecall itself is kernel glue.
**GREEN**: `sbi::system_reset` (EID `0x53525354`) beside the existing wrappers, plus
the trigger on the detector.
**MUTATE**: `cargo xtask mutants` on the detector; the split-token case is the target.
**KILL MUTANTS**: a mutant that only matches a token arriving in one read must fail.
**REFACTOR**: assess whether the detector belongs beside `drain_rx` or in the console
module proper.
**Done when**: detector tests pass, mutation reviewed, gate green, approved.

**⚠ Two things to resolve during CONFIRM, both of which can reshape this step:**

- **snemu does not model SRST.** `service_sbi` (`snemu/src/cpu.rs`) handles IPI,
  HSM `hart_start`, and TIME `set_timer`; everything else returns
  `ERR_NOT_SUPPORTED`. So the kernel side is **not** gate-testable as written.
  Cheapest fix: model SRST in snemu as a halt with a distinguishable reason (snemu
  already carries halt reasons, and *keeping* the reason is a lesson this repo has
  already paid for), then an itest asserts the reason. That is a small extra step and
  it makes the trigger deterministic; without it, step 5's only oracle is the board.
- **The magic line is unauthenticated** — anyone on the UART can reboot the board.
  Correct for a bench dev board, recorded here so it never ships to anything that
  matters. It is the same authority question the network console faces, at a much
  smaller blast radius.

---

### Step 6: `cargo xtask board reboot` + the thrash guard — 🔴 BLOCKED ON HARDWARE 2026-08-30

**The command is built and its guard is host-tested. It cannot work on this board**,
and the reason is below step 5's floor rather than in anything this step does.

Measured twice, 2026-08-28. The kernel side works exactly as designed — the token is
detected, the reason frame goes out, the TX drains — and then the SBI call reaches
OpenSBI's JH7110 reset driver, which prints

```
pmic_ops: cannot read pmic power register
```

— it cannot reach the AXP15060 PMIC over I2C — and **hangs**. The board does not
reset and does not come back. So `board reboot` currently *costs* a manual power
cycle rather than saving one, which inverts the point of the step.

**Step 5's contract does not survive contact with this firmware.** It is written for
firmware that *refuses* SRST: "callers should treat a return as reboot unavailable
and say so". `kernel/src/obs/heartbeat.rs`'s fallback (`reboot: SBI SRST refused
(error=…) — continuing`) never ran, because OpenSBI never returned. **A return is not
the only failure mode, and the other one is unrecoverable from the caller's side** —
no amount of better fallback fixes it; it needs a different reset mechanism.

Tried: `RESET_TYPE` is now per-platform (`kernel/src/sbi.rs`), warm (0) under `vf2`
and cold (1) elsewhere — scoped rather than swapped, since cold is correct under QEMU
and snemu and `console-reboot-requests-srst` asserts a halt a `NOT_SUPPORTED` return
would not produce. **Untested**: the one boot that showed the same `pmic_ops` message
may have been running the previous image. Not disproven, not confirmed.

Next candidate: the **JH7110 watchdog** at `watchdog@13070000`. U-Boot reports
`WDT: Not starting watchdog@13070000`, so it is present and idle, and a short timeout
resets the SoC with no firmware cooperation at all — immune to whatever is wrong with
the PMIC path.

**Until one of these works, the unattended loop cannot close on this board**: every
iteration needs a human at the power switch. That is the single biggest constraint on
Phase 2's value, and it is a hardware/firmware fact rather than a missing feature.

**Acceptance criteria**: the command sends the magic line, waits for the boot marker
on the fresh image, and returns success only once the board is booting. An iteration
cap and a minimum inter-reboot interval are enforced, and when either fires it is
**reported as itself** — a bad build that boot-loops must not be able to hammer the
board, and the operator must be told that is what happened.

**RED**: the guard is pure — `(history of reboot times, now, policy) -> Allow |
Denied{reason}` — so table-test it against a `FakeWallClock`, including the
back-to-back case and the cap-exhausted case.
**GREEN**: the guard plus the command assembling steps 4 and 5.
**MUTATE**: the interval comparison and the cap boundary.
**KILL MUTANTS**: an off-by-one that permits one extra reboot past the cap must fail.
**REFACTOR**: assess whether the guard is reusable by the eventual autonomous loop —
it should be the same value.
**Done when**: a reboot round-trip works against the board, the guard denies a
too-fast second reboot, gate green, approved.

---

### Step 6b: per-run bootargs, and a preflight that catches the stale-image class

Two small things that between them make the loop actually zero-touch.

**Acceptance criteria**:
- `cargo xtask board boot --workload X` sets U-Boot's `bootargs` for this run and boots
  — **because the workload is not in the image.** It rides `bootargs`, which is why
  `cargo xtask image` prints `setenv bootargs 'workload=X'` for a human to retype and
  why a bare reset picks up a fresh *image* but the *old workload*. This is the step
  that closes that gap.
- `cargo xtask board preflight` checks, and reports each as itself: the TFTP server is
  running and serving the expected root; `snitchos.img` there is newer than the last
  build; U-Boot's saved `serverip` matches this Mac's current IP; the board answers at
  the prompt.

**Preflight earns its place against a *recorded, repeated* failure.** "A VF2 regression
is a missed `cargo xtask image` until proven otherwise" is a lesson this project has
already paid for more than once — output not matching source means a stale artifact.
Preflight turns that from a debugging session into a line of output, and it adds the
sibling this analysis surfaced: a `serverip` that no longer names your Mac fails as a
TFTP timeout, which looks nothing like its cause.

**RED**: each check is a pure predicate over injected inputs — `(tftp root listing,
build timestamp) → Fresh | Stale{age}`, `(saved serverip, current ip) → Match |
Drifted{from, to}` — so table-test them; the I/O that gathers the inputs is glue.
**GREEN**: the predicates, the two subcommands.
**MUTATE**: `cargo xtask mutants`. The freshness comparison is the target.
**KILL MUTANTS**: a mutant that reports Fresh for an image older than the build must
fail. That mutant *is* the bug preflight exists to catch.
**REFACTOR**: assess folding preflight into `boot` as an on-by-default check with an
`--no-preflight` escape, rather than a command people forget to run. Leaning yes — a
check you have to remember is a check that does not fire on the day it matters.
**Done when**: a deliberately-stale image and a drifted `serverip` are each reported
correctly, `boot --workload` switches workload with no typing, gate green, approved.

---

## Phase 2 — the ESP32 transport (steps 7–9)

**Cuts the laptop's cable to the board. Contains no kernel code whatsoever.**

An ESP32 sits on the same three UART pins the USB-TTL adapter uses and bridges them
to WiFi. SnitchOS sees an identical 115200 line, the same COBS frames, the same
`console=frames`, the same `ConsoleRead` — it never learns it is on a network, which
is exactly why this is cheap. Every item in the RX gap list in
[docs/network-telemetry-design.md](../docs/network-telemetry-design.md)'s addendum is
sidestepped rather than solved, and the kernel's trust model is unchanged (it is still
"whatever is on the UART"; the exposure moves to the ESP32's WiFi, which is a password
rather than a capability redesign).

**Wiring** — no soldering; the VF2's 40-pin header carries UART0, already documented
in [visionfive2-port.md](visionfive2-port.md): **pin 6 → GND**, **pin 8 (UART0 TX) →
ESP32 RX**, **pin 10 (UART0 RX) → ESP32 TX**. Both sides are 3.3V, so no level shifter
— confirm against the specific dev board before wiring rather than trusting this line.

**Two things to get right up front:**

- **Power the ESP32 independently** (its own USB supply), *not* from the VF2's header,
  if it is also to be the reset backstop. A recovery mechanism powered by the thing it
  is meant to recover is not one.
- **Keep the ESP32 dumb** — transparent bytes in both directions, no COBS decoding, no
  buffering cleverness. The host already owns the decoder; a smart bridge becomes a
  second thing to debug at precisely the moment the board is misbehaving.

**What this does *not* remove: the Ethernet cable.** TFTP netboot happens in U-Boot,
which talks to its own NIC; the ESP32 cannot serve it. The end state is a board across
the room on power and a cable to the router, with the **laptop** untethered — which is
the stated goal, but is not "no cables to the board." SD boot would remove that one too,
at the cost of the fast reflash loop; not worth it.

### Acceptance criteria (phase)

- [ ] `cargo xtask board --tcp <host>:<port> exec "…"` behaves identically to the
      serial transport — same frames, same stop conditions, same exit codes.
- [ ] A capture over WiFi and a capture over the cable, of the same boot, are
      **byte-identical** after decode.
- [ ] Losing the WiFi link mid-capture reports as a transport failure, never as a
      quiescent board.
- [ ] The whole loop (`image` → `reboot` → `exec`) runs with no USB cable between the
      laptop and the board.

---

### Step 7: a TCP transport behind the Phase 1 seam

**Acceptance criteria**: `--tcp <host>:<port>` selects a socket transport; everything
above the seam is untouched. Connection refused, a mid-capture disconnect, and a DNS
failure each produce a distinct transport error — never an empty capture that looks
like board silence. The quiescence window is transport-defaulted (see step 2's warning)
and overridable.

**RED**: the transport is a `Box<dyn Read + Write>`, so point the existing step 2/3
tests at a loopback `TcpListener` fixture and assert identical results to the serial
fixture — one table, two transports. Plus a disconnect-mid-capture test asserting the
error, not an empty success.
**GREEN**: the TCP branch plus its error mapping.
**MUTATE**: `cargo xtask mutants` on the error mapping.
**KILL MUTANTS**: a mutant that maps a disconnect to "capture complete" must fail. That
is the whole point of the step.
**REFACTOR**: if the serial and TCP branches have diverged in anything but their error
sets and their defaults, the seam is in the wrong place — fix it here.
**Done when**: both transports pass one shared test table, gate green, approved.

---

### Step 8: the ESP32 firmware + wiring

**Acceptance criteria**: with the ESP32 in place of the USB-TTL adapter, a full boot
captured over `--tcp` decodes to the same frames as the same boot captured over the
cable. Throughput is verified under the worst real load — `init` steady-state telemetry
already consumes about half of 115200 (measured: ≈5.5 KB/s against ~11.5 KB/s), so a
naive bridge that drops bytes under sustained load will show up here, in the diff.

**Not a TDD step** — it is firmware selection and wiring. Its oracle is the parity
capture, which is why step 7 lands first.

**Verify, don't assume, the firmware.** A transparent TCP↔UART bridge is a well-trodden
ESP32 use case with several off-the-shelf options; pick one deliberately and record
which, rather than inheriting whatever a search returns first. Requirements: transparent
8N1 at 115200 (ideally higher, see open questions), no line-ending translation, no
local echo, and a documented reconnect behaviour.
**Done when**: the parity capture is byte-identical after decode, and the loop runs with
no laptop cable.

---

### Step 9 (optional): reset GPIO + autonomous silence watchdog

The L2 backstop, at a fraction of its designed cost. An ESP32 GPIO plus a transistor
across the VF2's reset header, exposed over the same link — and, because the ESP32 sees
the byte stream, able to pulse reset on prolonged silence **without the laptop being
involved at all**.

**Acceptance criteria**: `cargo xtask board reset --hard` pulses the line and the board
reboots. The autonomous watchdog is **off by default** and, when armed, honours the same
thrash guard as step 6 — an unattended reset loop must be as hard to start accidentally
here as it is there.

**Do this last, and only once L0 has been measured in anger.** If the software reboot
handles everything in practice, this is a solution to a problem you do not have.
**Done when**: a hard reset recovers a deliberately-wedged board, the guard holds, and
the default-off behaviour is verified.

---

## Open questions

Carried from the design note, plus what this plan surfaced:

1. ~~**Does SBI cold reset re-netboot on the VF2?**~~ **ANSWERED 2026-08-28, and the
   answer is worse than either branch this question imagined.** SRST does not
   re-netboot *and* does not warm-restart: OpenSBI's JH7110 reset driver cannot reach
   the AXP15060 PMIC (`pmic_ops: cannot read pmic power register`) and **hangs**. The
   platform neither resets nor returns. So "reboot = pick up the freshest image" is
   not merely false, it is unavailable — see step 6. The question assumed the failure
   mode would be a *wrong reset*; it was *no reset and no return*, which is the case
   `sbi::system_reset`'s "a return means failure" contract cannot express.
2. **Does snemu need the SRST model?** See step 5. Leaning yes: without it the reboot
   path has no deterministic oracle, and this repo's discipline is that the board
   confirms what the emulator already proved.
3. **Marker syntax** — literal substring or regex? Start with a literal (the two real
   markers are U-Boot's `=>` prompt and a named span); add regex only when a case
   needs it.
4. **Does the JH7110 have a usable on-chip watchdog?** **Promoted 2026-08-28 from
   "nice if it exists" to the leading candidate for the reset path itself**, because
   question 1's answer removed SRST. Partly confirmed for free: U-Boot's banner says
   `WDT: Not starting watchdog@13070000`, so the peripheral **exists, has a known
   MMIO base, and is left idle rather than disabled**. What is unconfirmed is whether
   a short timeout produces a full SoC reset that re-enters ZSBL (the thing SRST was
   supposed to do), and whether anything re-disables it. Its appeal now is exactly
   that it needs **no firmware cooperation** — it cannot be defeated by whatever is
   wrong with the PMIC path. If it works it is also still the L2 backstop, and the
   relay becomes near-vestigial.
5. **Which ESP32 firmware?** See step 8 — pick deliberately and record the choice.
6. **Does the bridge hold up at higher baud?** `init` already uses ~half of 115200,
   so the line has less headroom than it looks. If Phase 2 goes well, the deferred
   Step 7 of [uart-telemetry.md](uart-telemetry.md) (programming the baud) becomes
   more attractive — an ESP32 handles 921600 comfortably, whereas WiFi bridging at
   high baud is where naive firmware starts dropping bytes. Measure before assuming
   either.
7. **What is the WiFi latency distribution, really?** It sets the transport-default
   quiescence window in step 7. Measure it rather than guessing a round number — the
   tail matters more than the median here, because the tail is what false-fires.
8. **When does `--until-quiet`'s clock start?** Observed 2026-08-28, undocumented and
   untested: a capture against a **totally silent** line ends on the hard deadline
   (`Timeout`), not on quiescence — so the quiet clock appears not to run until the
   first byte arrives. There is a defensible argument for that (you cannot call a
   board "gone quiet" if it never spoke), and an equally defensible one against
   (a board that says nothing for 500 ms is quiet by the stated definition). Either
   way **the behaviour is currently decided by accident rather than by a test**, and
   the two readings give opposite exit codes for a dead board — which is precisely
   the distinction [`outcome`](../xtask-board/src/outcome.rs) exists to keep sharp.
   Pick one, write the test, say why in the doc comment.

## Pre-PR Quality Gate

Before each commit:

1. Mutation testing — `cargo xtask mutants -p <crate>` (kernel/ excluded; its
   coverage is the itest).
2. Refactoring assessment — run the `refactoring` skill.
3. `cargo xtask clippy` (host + riscv; never blanket `--fix` the kernel —
   `deref_addrof`).
4. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
5. `cargo xtask links` if any `.md` moved or gained links.

## Notes

- **Physical side effects.** Serial writes act on real hardware outside the command
  sandbox; the bridge runs through the normal permission gate and needs explicit
  per-session authorization.
- **Do not regress `screen`.** Until step 4 is trusted, the manual path must keep
  working — which mostly means: never leave the port held.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md's override of the
planning skill's delete step) and merge any learnings via the `learn`/`adr` agents.*
