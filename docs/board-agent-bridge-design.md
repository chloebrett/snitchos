# Driving the real board from an agent — design

**Status:** 📐 **DESIGN — not started.** A scoping/design analysis for letting an
agent (or a human at a REPL) drive the physical VisionFive 2: build an image, boot it,
send input, read back *structured* output, decide, and repeat — closing the iteration
loop on real silicon instead of a human relaying the serial console. This is a design
(the shape and the rationale); the TDD-decomposed increments become a
`plans/board-agent-bridge*.md` plan later.

The thesis: the same observability that makes SnitchOS worth building — structured
frames, per-task telemetry — is exactly what an agent needs to iterate on hardware. If
the OS narrates its own state (including its own hangs) over a wire the host can decode,
the loop drives itself. This converges with the [collector-as-server](../plans/uart-telemetry.md)
direction: the board becomes just another frame source.

---

## What already exists (the head start)

Most of the hard parts landed with the UART-telemetry work
([plans/uart-telemetry.md](../plans/uart-telemetry.md)):

- **COBS wire framing (done).** Every telemetry frame on the UART is COBS-encoded and
  `0x00`-delimited, so the serial byte stream is **self-framing and resyncable** — the
  usual pain of reading frames off a noisy line is already solved (`protocol::wire_encode`,
  `decode_stream`).
- **`console=frames` mode (done).** A bootarg routes every post-init kernel `println!`
  through `Frame::Log`, so the whole kernel-phase UART stream becomes structured frames —
  no text/binary demux. `console=text` (default) keeps the human log.
- **A reusable decoder** — `decode_stream` / `try_decode_frame` / `OwnedFrame`, already
  shared by the collector, snemu, and the itest harness.
- **Image delivery + netboot.** `cargo xtask image --workload X` drops `snitchos.img` in
  the TFTP root; the board **already netboots the latest image** on reset. So there is no
  U-Boot scripting to automate — a reset *is* the whole re-flash.
- **A source seam.** The collector's `run_source` is explicitly where a serial source
  slots in.

What's left is a host-side serial bridge, a way to trigger reboots, and — the interesting
part — a way to recover from and *observe* hangs.

## Piece 1 — the host serial bridge

The command the loop is built on. `cargo xtask board`:

- Opens the board UART (`/dev/cu.usbserial-*` @ 115200; Rust `serialport` crate).
  **`cu.*`, never `tty.*`.** On macOS the `tty.*` node is the call-in device: `open()`
  blocks until carrier detect is asserted, and a USB-TTL adapter typically never asserts
  it. The open then hangs forever with no error — which for an unattended bridge is
  indistinguishable from a board that never booted. `cu.*` is the call-out node and skips
  the DCD wait. (Measured on the VF2 dev host: a `screen` on the `tty.*` node sat in a
  blocked open indefinitely; the same adapter on `cu.*` worked immediately.)
- **Port contention must be reported as itself, not as silence.** The port is exclusive —
  a leftover `screen`, a previous bridge run, or an editor's serial monitor holds it, and
  the next open fails with `EBUSY` (or, via a pending `tty.*` open, blocks). The bridge
  must distinguish *cannot open the port* from *opened the port and the board said
  nothing*, because on hardware those have the same downstream symptom and completely
  different fixes. Report the holder if it can be identified (`lsof` on the device node
  names the pid), and never leave the port held on exit — restore-on-drop, including on
  panic, the same discipline as snemu's raw-mode guard.
- **`exec "<text>"`** — writes input, then captures output until a stop condition:
  a **quiescence window** (N ms with no new bytes), a **marker** (regex, e.g. U-Boot's
  `=>` prompt or a kernel span), or a **timeout**. Returns `{ io_text, frames[] }`.
- **Two-phase decoding.** U-Boot speaks plain line-oriented text; the kernel speaks COBS
  frames (under `console=frames`). The bridge captures raw bytes and switches decoding at
  the `booti` handoff — or, more simply, try-COBS-decode with a text fallback per line.

This alone unblocks ~80% of hardware iteration: script the (rare) U-Boot interaction, read
the boot log, watch for a driver's breadcrumbs, and diff runs — with a human needed only
to recover a true hang. It pays for itself on the current audio bring-up (reading whether
`audio: PWMDAC up` prints and tuning `CORE_DIVIDER` without a human in the loop).

## Piece 2 — the recovery ladder

The board must return to a known state between runs, and — critically — recover when a run
wedges, without a human. Three levels, most-common first; the design goal is that each
lower level fires an order of magnitude less often than the one above.

| Level | Trigger | Mechanism | Catches |
|---|---|---|---|
| **L0 — agent reboot** | kernel responsive | console `"reboot"` line → **SBI SRST** cold reboot → netboot | the normal "next iteration" case |
| **L1 — kernel watchdog** | one task hung, kernel core alive | autonomous detect → emit a "task hung" frame → self-reboot (SRST) | *partial* hangs (our `PollUntilSet` case) that emit no silence |
| **L2 — relay backstop** | whole kernel dead (trap handler wedged, all harts gone) | host sees total silence past a deadline → **reset-header relay pulse** | deep hangs the board can't self-recover |

**L0 — software reboot.** RISC-V's **SBI System Reset extension** (SRST, EID `0x53525354`,
`sbi_system_reset(type = cold_reboot, reason)`) resets the harts back through the boot flow.
The kernel already has `sbi.rs` wrappers (`set_timer`, `hart_start`), so this is one more
ecall. Trigger it off the existing polled-RX path — a magic console line the kernel turns
into an SRST. Because the board netboots, `reboot` = "pick up the freshest image." This is
the primary mechanism; the relay is a fallback, not the default.

**L2 — reset backstop.** A relay pulsed across the VF2 reset header (hardware already on
hand): the host arms a watchdog and, if the board emits *nothing* for T seconds during a
run that should be producing frames, pulses reset. **A cheaper backstop may be on-chip:**
the JH7110 very likely has a hardware watchdog timer (a DesignWare WDT is standard on these
SoCs). Petted by the healthy kernel, an on-chip WDT resets the SoC when the kernel dies
*entirely* — shrinking the relay's role to near-zero (only a wedged pet-path would need it).
The catch is that a hardware reset can't name the task, so software still detects-and-reports
first (L1); the WDT only *guarantees recovery*. Either way this level almost never fires —
the design is arranged so L0/L1 handle nearly everything.

**L1 is the interesting one** and gets its own section — because our exact failure mode
(a task spinning on `PollUntilSet` while heartbeats keep coming) is invisible to L0 (the
agent sees frames, not silence) and to L2 (the board isn't silent). Only something *inside*
the kernel, watching *per task*, catches it.

## Piece 3 — liveness introspection as the shared primitive

The tempting framing is "add a hang-check command." But an agent-invoked hang-check whose
only outcome is a reboot **is just a reboot command** — the check adds nothing. The value
isn't the reboot; it's *knowing what wedged*. So the primitive to expose is not "check for
a hang" but **"dump what every task is doing right now"** — a liveness snapshot — with
reboot kept orthogonal.

**The snapshot** (per task): id/name, scheduler state (Running / Ready / Blocked-on-what),
CPU time consumed, ticks since it last voluntarily yielded or blocked, and its current PC /
open span. Emitted as frames (on-thesis: task state is already telemetry).

One primitive, two callers:

- **Autonomous (the watchdog):** the heartbeat/monitor runs the snapshot on a timer,
  applies a *conservative* policy (a task Running and burning CPU but un-progressed past a
  generous threshold = runaway), emits a "task X hung @ PC" frame, and self-reboots via
  SRST. No agent involvement — this is what catches partial hangs while the agent is blind.
- **On-demand (the agent command):** the agent asks for the snapshot whenever it wants —
  "is it wedged, or just slow?" The *policy lives agent-side*: it reads the snapshot and
  decides to wait, reboot, or move on. This is useful precisely **when it does *not*
  reboot** — which is what distinguishes it from a bare reboot command.

Separating introspection from action is what resolves the "check ≈ reboot" collapse: the
snapshot is a product in its own right, and both a kernel timer and an agent consume it.

### The agent supplies the deadline (this dissolves most of the hard part)

The kernel should not *guess* what "too long" means — the agent knows, per run. So the
watchdog deadline is a **boot parameter**: `watchdog=<duration>` (parsed in `kernel-boot`
alongside `workload=`, default **off** so normal boots are unaffected). A quick smoke gets
`watchdog=2s`; a heavy sweep gets `watchdog=60s`. The kernel policy then collapses to
something dead-simple — "any runnable task on-CPU past the configured budget without
yielding → hang" — *because the agent picked the budget*. Policy lives with the caller who
has the context, via a parameter, instead of as a kernel heuristic.

On breach: **emit the "task X hung @ PC" frame, then reset.** Two care points:
- **Flush before reset.** The reset wipes state, so the hang frame must be *out the UART*
  before SRST fires, or a lossy serial line yields a truncated frame and the agent just
  sees an unexplained reboot. Borrow the panic-emits-telemetry shape (emit → bounded TX
  drain → halt): emit, spin until the UART TX FIFO drains, *then* SRST.
- **Report-only vs. auto-reboot** is a bootarg variant. Since the agent set the deadline,
  auto-reboot-on-breach is a fine default for an unattended loop; `watchdog=2s,report-only`
  instead emits the hang frame + snapshot and lets the agent decide (introspect, wait, or
  reboot via L0) — the same introspection-before-action split, chosen at boot.

### The residual judgement call

The agent-supplied deadline handles the common case; a little judgement remains at the
edges. Two failure modes to weigh:

- **False negatives are safe-ish** — a missed hang just falls through to L2 (silence → relay).
- **False positives are the risk** — rebooting a task that was legitimately busy or blocked.

Mitigations:
- **Blocked tasks are off the runqueue** — trivially excluded (they're *supposed* to be
  parked). The idle task's `wfi` likewise.
- **Long genuine work vs. a spin** is the ambiguous case. Options, in increasing
  cooperation: (a) a generous time threshold + leave the final call to the agent via the
  on-demand snapshot (kernel only auto-acts on egregious cases); (b) a **petting counter**
  a task bumps at safe loop boundaries, so "Running but un-petted for T" is unambiguous, at
  the cost of the task opting in. For a dev/agent loop, (a) is probably enough — the
  autonomous policy stays conservative, and the agent's judgement (with the full snapshot)
  handles the grey zone.
- **Action on detection:** reboot-and-report is blunt but deterministic and gives the agent
  the diagnostic it needs. Kill-and-restart-the-task is surgical but assumes the task is a
  clean unit of work; it composes with the shipped `Kill` primitive
  (see [supervision](../plans/supervision-v2.md)) and is a natural follow-on, not v1.

## The loop

```
cargo xtask image --workload X          # fresh img → TFTP root
  → xtask board: send "reboot" (L0)     # or relay pulse (L2) if wedged
  → board netboots the new image
  → xtask board: capture frames until quiescence/marker
  → agent reads {task snapshot, samples_emitted, logs, hang frames}
  → agent edits, rebuilds, repeats
  ‖ kernel watchdog (L1) self-reboots + reports any partial hang mid-run
```

## Relationship to the current audio work

The systemic watchdog is a broad safety net; the *targeted* fix for the audio bring-up is
smaller and independent: a **bounded `PollUntilSet`** in the PWMDAC driver (give up after N
iterations, log "reset never released," continue) so that one call can't wedge regardless
of any watchdog. The bounded poll is the local seatbelt; the watchdog is the systemic one.
Do the bounded poll with the driver; treat the watchdog as its own subsystem.

## Safety and guardrails

- **Physical side effects.** Serial writes and a reset relay act on real hardware outside
  the command sandbox. The bridge runs through the normal permission gate and needs
  explicit per-session authorization.
- **Thrash guard.** An autonomous loop must carry a hard iteration cap and a minimum
  inter-reboot interval, so a bad build (boot-loop) can't hammer the board.
- **The relay is the true backstop** — it recovers a board that even the kernel watchdog
  can't, so it must be wired to *reset*, not just power, and be independently triggerable by
  the host when all frames stop.
- **The `reboot` magic line is unauthenticated** — anyone on the UART can reboot the board.
  Fine for a bench dev board; noted so it isn't shipped to anything that matters.

## Open questions

- **Does SBI cold-reboot re-netboot on the VF2?** SRST cold reset should re-enter the boot
  flow (ZSBL → SPL → U-Boot → autoboot netboot); confirm the board actually re-fetches over
  TFTP rather than warm-restarting the payload. If not, warm-vs-cold reset type needs a look.
- **Frame/text handoff** at `booti` — is a per-line try-COBS-with-text-fallback robust
  enough, or is an explicit phase switch cleaner?
- **Watchdog default** — with an agent-supplied `watchdog=<dur>` deadline, does v1
  auto-reboot on breach or default to `report-only`? (Leaning: auto-reboot for unattended
  loops, report-only opt-in.)
- **Does the JH7110 have a usable on-chip WDT?** If yes, it can be the L2 backstop and the
  relay becomes near-vestigial — confirm the peripheral, its MMIO base, and that it survives
  the boot flow (SPL/U-Boot don't disable it out from under the kernel).
- **Snapshot transport** — a new `Frame` variant for the task snapshot, or reuse existing
  per-task metric frames plus a "yielded-ticks-ago" gauge?

## References

- [../plans/uart-telemetry.md](../plans/uart-telemetry.md) — COBS framing, `console=frames`,
  the `run_source` seam this builds on.
- [../plans/visionfive2-port.md](../plans/visionfive2-port.md) — board delivery (`xtask
  image` → TFTP → `booti`), the netboot flow, the SBI-call gotchas.
- [vf2-audio-design.md](vf2-audio-design.md) — the first driver this loop would iterate on;
  the `PollUntilSet` hang is L1's motivating case.
- [../plans/supervision-v2.md](../plans/supervision-v2.md) — `Kill` + supervision, which a
  kill-and-restart watchdog action would compose with.
