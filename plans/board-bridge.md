# Plan: the board bridge — driving the VisionFive 2 from the host

**Branch**: main (this repo works directly on main; the user commits)
**Status**: 📐 **NOT STARTED.** Two phases — the host bridge (steps 1–6b, including the
U-Boot layer that makes netboot zero-touch) and the ESP32 transport (steps 7–9). Blocked on one prerequisite:
[uart-telemetry.md](uart-telemetry.md) **Step 10** (collector `--serial`), which is
already written and is the last item on B3/M2's critical path.
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
- [ ] The port is released on every exit path including panic.
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

### Step 1: Port-open policy — device kind and failure classification

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

### Step 2: The capture loop's stop conditions

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

### Step 3: Two-phase decoding — U-Boot text, then kernel frames

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

### Step 4: `cargo xtask board exec` — the CLI over steps 1–3

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
**MUTATE**: `cargo xtask mutants` on the exit-code and flag-mapping logic.
**KILL MUTANTS**: a mutant that returns success when the stop condition never fired
must fail — that is the difference between a working loop and one that silently
believes a wedged board.
**REFACTOR**: consider whether `Source::Serial` (Step 10's collector source) and this
bridge should share one serial handle type. They should, if Step 10 has landed.
**Done when**: the command works against the real board, gate green, approved.
Requires the permission gate — this writes to physical hardware.

---

### Step 4b: Reach and drive the U-Boot prompt

**Acceptance criteria**: `cargo xtask board uboot "<command>"` interrupts autoboot,
lands at the `=>` prompt, runs the command, and returns its output. Failing to reach
the prompt within the countdown is an error that says *that*, distinct from a board
that never printed anything.

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

**✅ Done 2026-08-25 — the Mac holds a DHCP reservation at `192.168.0.7`.** That was the
complementary fix, and it was the right one to do first: it costs minutes and no code,
and it removes the failure *class* (a `serverip` that drifts with the lease) rather
than automating around it.

**This changes what this step is for.** With a stable `serverip`, a one-time manual
`saveenv` at the U-Boot prompt already delivers zero-touch netboot — the exact commands,
and the two parser gotchas the read-back check catches, are written up in
[visionfive2-port.md](visionfive2-port.md) ("Making netboot zero-touch", **not yet
applied** at time of writing). So `provision` is
no longer the daily-loop fix, it is the **residue** handler: a wiped environment, a new
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

### Step 6: `cargo xtask board reboot` + the thrash guard

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

1. **Does SBI cold reset re-netboot on the VF2?** SRST *should* re-enter ZSBL → SPL →
   U-Boot → autoboot → TFTP, but if it warm-restarts the payload instead, then
   "reboot = pick up the freshest image" — the premise the whole loop is built on —
   is false and the reset type needs revisiting. **Resolve this before step 5**; it is
   one experiment and it invalidates a lot if it goes the wrong way.
2. **Does snemu need the SRST model?** See step 5. Leaning yes: without it the reboot
   path has no deterministic oracle, and this repo's discipline is that the board
   confirms what the emulator already proved.
3. **Marker syntax** — literal substring or regex? Start with a literal (the two real
   markers are U-Boot's `=>` prompt and a named span); add regex only when a case
   needs it.
4. **Does the JH7110 have a usable on-chip watchdog?** If yes it can be the L2
   backstop and the relay becomes near-vestigial. Confirm the peripheral, its MMIO
   base, and that SPL/U-Boot do not disable it out from under the kernel.
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
