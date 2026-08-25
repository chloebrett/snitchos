# B3 — Telemetry over UART (M2 design note)

> Status: **design, unbuilt.** Prerequisite for M2 in
> [plans/visionfive2-port.md](../plans/visionfive2-port.md). The board boots,
> runs userspace, and hosts a Stitch REPL, but emits **zero telemetry** —
> virtio-console is a QEMU device and the JH7110 hasn't got one. This note is
> the decision record for putting the frame stream on a physical wire.

## What this is actually about

SnitchOS exists to talk about itself. Every other board blocker was a
portability chore; this one is the point. On QEMU the `Frame` stream rides a
virtio-console into a Unix socket. On hardware there is no such device, so the
frames need a wire — and a wire has properties a virtqueue doesn't: it is slow,
it is lossy, it has no backpressure signal, and nothing on the other end is
guaranteed to be listening.

## Two premises the original plan got wrong

The B3 sketch in the port plan says the subtleties are *"framing (postcard frames
need length-delimiting or a COBS-style boundary on a raw byte pipe — virtio gave
us message boundaries for free), backpressure, and flow control."* Reading the
code, the first half is wrong and the omission matters.

**Framing is already solved.** `protocol::stream::try_decode_frame` is
`postcard::take_from_bytes`: it decodes one frame from a byte slice and returns
how many bytes it consumed, or `DeserializeUnexpectedEnd` if the buffer is
short. `decode_stream` is generic over `R: Read` and drives exactly that loop
over a growing buffer. **The wire has been an unframed byte stream all along** —
virtio's message boundaries were never load-bearing, and the collector would
work unchanged over a serial port today.

**The real problem is resynchronisation.** A byte stream that never loses bytes
needs no boundaries. A UART loses bytes:

- the reader attaches mid-stream (the board doesn't wait for us — see below)
- the board resets mid-frame
- RX overrun on the host side, or a dropped byte on a marginal cable
- the kernel itself drops a frame under backpressure (by design, below)

Postcard is self-delimiting but not self-*synchronising*: given a buffer that
starts halfway through a frame, there is no way to find the next boundary. It
will decode garbage or error. And today a decode error is **fatal** —
`decode_stream` returns `Err` and the collector exits. On a socket that is
correct (a malformed frame is a bug). On a UART it is a guaranteed death on the
first glitch, and "the collector dies whenever you reset the board" is not a
telemetry system.

So B3's genuinely new design problem is: **how does the decoder find its footing
again after losing bytes?**

## Decision 1 — framing for resync: COBS

Wrap each encoded frame in [COBS](https://en.wikipedia.org/wiki/Consistent_Overhead_Byte_Stuffing)
and delimit with a zero byte.

COBS's property is exactly the one we need: the encoded payload is guaranteed to
contain **no zero bytes**, so `0x00` is an unambiguous frame boundary that can
never appear inside a frame. After any loss, the decoder discards bytes until
the next `0x00` and is immediately back in sync, losing at most the frame in
flight. Overhead is ~1 byte per 254, and `postcard` already ships a COBS flavour
(`postcard::to_slice_cobs`), so this is a flavour change at the encode site
rather than a new format.

Rejected alternatives:

- **Length-prefix.** A corrupt length is indistinguishable from a valid one, so a
  single lost byte can make the decoder skip an arbitrary distance. Needs a sync
  word and a checksum to recover, which is COBS with extra steps.
- **Sync word + CRC.** More bytes, and the sync word can occur inside a payload,
  so it still needs escaping. COBS *is* the escaping, done minimally.
- **Nothing (status quo).** Works right up until the first dropped byte, then
  never works again.

The cost is a real one and worth stating: **the wire format changes.** COBS
framing must be applied on the virtio path too, or the two transports diverge and
`itest` stops testing what the board runs. Prefer changing both — one wire
format, two transports — over a board-only variant.

## Decision 2 — ring buffer, TX-interrupt drain, still drop-and-count

`UartFrameSink` must never block:

- A UART at 115200 moves ~11.5 KB/s. The kernel can generate frames far faster
  than that (see the throughput section), so blocking on TX means the heartbeat
  stalls behind the wire and the kernel's timing is dictated by a serial cable.
- With no reader attached, blocking is *permanent*.

**Buffer, yes — but a buffer is not a fix for sustained overrun.** A ring absorbs
*bursts* (the pre-init flush, the ~70-send metric-registration spike at boot),
which is real value. It cannot absorb a rate mismatch: if frames arrive faster
than the wire drains, the queue grows without bound and frames are dropped
anyway — later, staler, and having spent memory to delay the inevitable. So the
ring changes *where* loss happens, not *whether*. Drop-and-count stays.

**Draining on the heartbeat does not work.** The arithmetic:

| quantity | value |
|---|---|
| fast tick (`timebase_hz / TICKS_PER_HEARTBEAT`, = 20) | 20 Hz |
| 8250 / DesignWare TX FIFO | 16 bytes |
| non-blocking bytes per second | 20 × 16 = **320 B/s** |

That is ~36× slower than even a 115200 line, so a tick-driven non-blocking drain
is not a transport. The alternative — drain until empty inside the tick — is
blocking with extra steps: 12 KB at 115200 stalls the heartbeat for a full
second, which is exactly the failure this decision exists to prevent.

**So: TX-interrupt-driven drain.** The 8250's THRE interrupt fires whenever the
FIFO has room; the ISR refills from the ring. The ring then drains at precisely
wire speed, with no polling, and kernel timing is decoupled from the cable — the
only design here where that is actually true. It is symmetric with the RX side
that already exists (`console::drain_rx`, timer-driven), and it is why the ring
is worth building at all: without an interrupt the ring has no drainer that can
keep up.

Cost, stated plainly: `kernel/src/device/uart.rs` is **polled-output-only by
explicit design** ("No interrupts, no ..."), so this adds an interrupt path to a
driver that deliberately hasn't got one. That is the price of decoupling; the
alternative is capping telemetry at 320 B/s.

Discipline unchanged from the allocator and IRQ paths: never emit from a context
that can't afford to wait; bump a counter, let a later pass drain it. The dropped
count goes on the wire (`Frame::Dropped`, already used for pre-init overflow), so
loss is **observable** — the only honest way for an observability system to lose
data.

## Decision 3 — no wait-on-collector; reset is the sync point

`cargo xtask boot` passes QEMU `socket,…,server=on,wait=on`: the hypervisor
*freezes the guest* until the collector connects, giving lossless capture from
frame 0. **Hardware has no such lever.** The U74 runs on power-up and cannot
block on a host consumer.

So the board model inverts: **start the collector first, then reset the board.**
The reset is the synchronisation point, not a socket connect. Consequences:

- Early frames are only captured if the reader is already attached. The pre-init
  buffer flushes once the sink is up, so a reader attached before reset gets
  everything; one attached later gets a `Dropped(N)` and a mid-stream resync
  (which Decision 1 makes survivable).
- If a demo genuinely needs frame 0 losslessly, an opt-in handshake — the sink
  waits for a byte on RX before flushing the pre-init buffer — buys it for ~10
  lines and an RX dependency at boot. **Deferred**; it trades "always boots" for
  "boots only when watched", which is the wrong default for a board.

## Decision 4 — who owns the wire (the console-ownership problem)

This is the question today's session surfaced and it is more interesting than it
looks. The kernel's `println!` and userspace's `ConsoleWrite` both write UART0
with **no arbitration**. Running the Stitch REPL on the board, the kernel's `hb`
pulse landed mid-token and shredded the prompt. We "fixed" it by silencing the
kernel behind a `board-heartbeat-print` feature — a workaround, not an answer.

In a capability system this is a design smell, not a formatting bug: **writing
the console is authority**, and the kernel holds an ambient copy that outranks
the process holding the cap. B3 has to answer "who may write this wire" for the
telemetry line anyway, so answer it for both.

**Decision: one UART, and the human log becomes frames.** A second USB-serial
adapter means two cables to the laptop for every session, which is a permanent
ergonomic tax to dodge a problem we can dissolve instead.

The dissolution: stop putting *two kinds of content* on the wire. Emit human log
lines as `Frame::Log` — a variant that already exists and that the panic path
already uses. Then the stream is **only frames**, there is nothing to demux, and
the collector's existing `--text` mode renders the log. One writer, one format,
one cable.

This is also the honest version of the project's own thesis: if the system is
supposed to talk about itself through structured telemetry, a second unstructured
side-channel that only a human with a terminal can read is the anomaly, not the
baseline. The `hb`-shreds-the-REPL bug was that anomaly surfacing.

### Input stays raw — the directions are independent

Framing exists to solve *resync after loss in a continuous stream*. The RX
direction has neither property: there is exactly one host-side writer (a human at
a keyboard, or the collector relaying one), and a lost byte is a lost keystroke,
not a protocol desynchronisation. So the wire is **asymmetric — frames out, raw
bytes in** — and `console::drain_rx` / `ConsoleRead` are untouched. The Stitch
REPL keeps working unchanged.

### The cost is `screen`, and it is bigger than it looks

`screen /dev/tty.usbserial-0001 115200` stops working, and that is not merely an
inconvenience:

- **The collector must become the terminal.** Only one process can own the
  serial port. If that is the collector, it has to relay raw-mode stdin to serial
  TX and render `Frame::Log` to stdout — a real feature (raw mode, echo, Ctrl-C
  handling), not a footnote. `cargo xtask reader` is the natural home.
- **Bootstrapping hazard.** Frames-only means that when the frame path is broken
  you have *nothing*, and you are debugging the very thing you need in order to
  debug. This session is the argument: a silent boot cost two flashes and a wrong
  diagnosis, and what fixed it was **plain `println!` markers**.
- Also note the baud change (below) moves the number: `screen … 921600`.

The first two are mitigated at the edges — the two moments you most need raw text
are already outside the frame path: **pre-init** (before the sink exists) and
**panic** (the emergency UART path, which must not depend on a working sink).
Both stay raw text regardless. But that only covers the ends of a boot, not a
board that comes up and misbehaves in the middle.

### So: console mode is a boot decision, not a build-time commitment

- **`console=text` (default).** Today's behaviour. Human-readable, `screen`
  works, no collector required. The bring-up and "something is wrong" mode.
- **`console=frames`.** Frames only, collector renders. The telemetry mode.

One content type on the wire at any moment, so there is still nothing to demux —
the elegance is preserved where it pays, without betting the board's only
diagnostic channel on the frame path being healthy. It also rides the bootarg
mechanism that now works in **every** build (the `itest-workloads` gate came off
this session), so it costs a `WorkloadKind`-style parse arm and nothing else.

Defaulting to `text` is the conservative choice and probably right for a while:
the board is new, and the day telemetry breaks is the day you need the console
most. Flip the default once the frame path has earned it.

### Where this is going: the collector as the terminal

Treating collector-as-terminal as the *price* of frames-only undersells it. The
intended direction is for `collector/` to grow into a **server** backing custom
React dashboards, hosting the terminal alongside them — taking over `screen`'s
role rather than merely replacing it.

In that world the console mode is not a concession, it is the point: **routing is
free because the wire is typed.** "Which frames are telemetry and which are text"
is a `match` on the `Frame` variant — `Log` to the terminal pane, everything else
to dashboards. No parsing, no heuristics, no demux, because the kernel already
said what each frame *is*.

Two things this converges with, both already built:

- **snemu compiles to wasm32 unmodified** (`docs/snemu-wasm-design.md`). A
  collector serving a React app, with the guest running in the same tab, is
  "SnitchOS in a browser" with real dashboards.
- **The diagram folds are already dashboard logic** — `cargo xtask diagram`'s
  telemetry targets reconstruct views by folding `OwnedFrame`s ("a diagram is a
  collector", `docs/diagrams-design.md`). That projection code is host-tested and
  is exactly what a dashboard consumes.

Questions to settle before building it, not after:

1. **Replace Grafana, or only its UI?** Grafana brings storage, query and alerting
   for free. Keeping Prometheus as the store and building custom UI on top is a far
   smaller commitment than owning retention. Custom UI wins for what Grafana is
   *bad* at — cap-derivation trees, span trees, the physics desktop, a terminal —
   not for line charts.
2. **The collector becomes stateful and bidirectional**: persistent process, push
   channel (SSE/WebSocket), retained history, and a write path from browser to
   board.
3. **That write path is an authority question.** A browser tab that can inject
   keystrokes into a REPL holding real caps is a capability boundary. For this
   project of all projects it should not be ambient — design it rather than
   inherit it.

None of this blocks B3; it argues that step 3b (collector as terminal) is an
investment rather than a workaround, and that `console=frames` is where the board
ends up.
- **Demux is not needed, but if it ever is**, COBS makes it nearly free: frames
  contain no `0x00`, so a `0x00` starts frame mode. Ambiguity only arises for a
  run of non-zero bytes that is valid as both text and a COBS-encoded `Frame`,
  which the prefix/suffix structure above makes a non-issue. Recorded as a
  fallback, not the plan.

Rejected: **two UARTs.** Cleanest in the abstract — exactly one writer per line,
independent bauds, no shared-wire reasoning at all — but it needs the second UART
wired out of the 40-pin header to a second adapter. Worth revisiting only if the
single line proves too slow *after* the baud lever below is exhausted.

Also noted: **Ethernet (M2.5) supersedes this whole question.** Frames over
UDP have bandwidth to spare and no cable-sharing problem, and the port plan
already scopes it. UART-now is not throwaway — the sink is behind a trait and the
framing/resync work is transport-independent — but the one-cable constraint is a
temporary one, and that is an argument against paying for a second adapter to fix
it.

Whichever transport, the *ownership* rule holds: **one writer per stream.** The
kernel's ambient `println!` is the anomaly, and should eventually be a capability
the kernel holds like anyone else.

## Throughput — measured, and 115200 suffices

**Measured 2026-07 under snemu (Step 6).** The earlier "~60 KB/s" estimate was
wrong — it was **boot-transient-dominated**, taken over ~5 heartbeats where the
one-time registration + spawn burst swamps the sample. Measured properly, as a
two-point delta that excludes the transient — bytes emitted between ~13 and ~53
heartbeats, `steady = (bytes_B − bytes_A) / (heartbeats_B − heartbeats_A)`, with
`heartbeats = timer_fires / TICKS_PER_HEARTBEAT(20)` and one heartbeat ≈ one guest
second:

| workload | Δbytes | Δheartbeats | **steady-state** |
|---|---|---|---|
| `init` (the board's default boot) | 221,809 | 40 | **≈ 5.5 KB/s** |
| `demo` (scheduler + producer/consumer) | 434,977 | 160 | **≈ 2.7 KB/s** |

`init` is the *heavier* of the two (it runs userspace + the FS server/client, so
per-heartbeat it adds IPC frames, cap events, and more per-task metrics — the rate
*ramps* as it spawns children, it doesn't decay). Both are **single-digit KB/s**,
comfortably under a **115200 UART's ~11.5 KB/s**.

**So the baud lever is optional, not required.** 115200 carries `init`'s
steady-state with ~2× headroom; the boot transient is front-loaded (registration
+ pre-init flush) and absorbed by the existing pre-init buffer + drop-and-count,
and even the boot-window *average* is ~3.3 KB/s. This flips the earlier
conclusion: one 115200 cable is enough for the workloads that exist today.

Levers, in the order they now matter:

- **Emit less / accept sampling — the honest defaults, and already sufficient.**
  Drop-and-count makes any overflow observable (`Frame::Dropped`); at 5.5 KB/s vs
  11.5 KB/s there is nothing to drop for `init`. Per-task metrics are the bulk and
  scale with task count, so a genuinely task-heavy board workload is the only thing
  that would approach the ceiling — and a metric subset / sampling is a policy knob
  if it ever does.
- **Raise the baud — headroom, not a fix.** Kept in the back pocket for a
  future task-heavy workload or a tighter no-drop guarantee, *not* needed for the
  telemetry that exists. Standard 8250 work: `LCR.DLAB`, divisor to `DLL`/`DLM`,
  `divisor = uart_clock / (16 × baud)`, clock from the DTB `clock-frequency` — the
  manual-decode path B4 built for `reg-shift`/`reg-io-width`. At 921600 → ~92 KB/s.
  Caveats if pursued: the CP2102 adapter's ceiling (~1 Mbaud) and that the console
  must reconnect at the new rate (switch **early and once**, document next to
  `setenv bootargs`).

**Consequence for the plan:** Step 7 (baud programming) drops from prerequisite to
*optional headroom*. Chosen baud for the board: **115200** (inherited from OpenSBI,
no divisor programming needed). Revisit only if a measured workload exceeds it.

Do not design around 115200 by default. **Measure first**, then pick a baud.

## Decision 5 — the collector side is nearly free

**Built 2026-08-25 (Step 10b), and the prediction mostly held — with one correction
worth recording.** A serial source *was* a new `R` and a new variant, not a rewrite.
What the "nearly free" framing missed is that a physical port expresses "no bytes
yet" in two ways `decode_stream` reads as end-of-stream — it propagates any `Err`
(and a port with a read timeout returns `TimedOut` during a quiet gap) and treats
`Ok(0)` as clean EOF. So an idle board would have ended the session. The fix is a
`Read` adapter (`SerialReader`) that absorbs exactly those two and propagates
everything else, so a *dead* port never masquerades as a quiet one. Small, but not
free, and not visible from the seam — the generic-over-`R` design was right and still
did not imply the transports were interchangeable.

`decode_stream` is already generic over `R: Read`. A serial source is a new
`R`, not a rewrite:

- Add a `--serial <dev> [--baud N]` source alongside the Unix socket.
- Make decode errors **non-fatal** on a lossy transport: on error, skip to the
  next `0x00` delimiter, count a resync, continue. Today's fail-fast behaviour
  stays correct for the socket path; the two transports differ in their error
  policy, and that difference should be explicit in the type, not a flag buried
  in a loop.
- A `Resyncs` counter is itself telemetry about the transport — worth exporting.

## What stays unchanged

- `FrameSink` is already a trait (`kernel_obs::sink::FrameSink`, one method).
  `UartFrameSink` is a new impl, not a kernel change.
- The `Frame` enum, the intern table, the span registry, the pre-init buffer, the
  batch ring: all transport-agnostic already.
- The virtio path stays for QEMU/snemu, so `itest` keeps working. **Both
  transports must carry the same wire format** (see Decision 1) or the gate stops
  testing the board's reality.

## Open questions

1. **Actual throughput.** The two data points below disagree by more than the
   workload difference explains. Measure before picking a baud.
2. **UART input clock** on the JH7110, for the divisor. Should be on the DTB UART
   node; if absent, it has to be derived from the clock tree (more work).
3. **Adapter ceiling.** Does the CP2102 in use sustain 921600?
4. **TX interrupt plumbing.** The UART driver is polled-only today, and the PLIC
   routing for the console UART isn't wired (the port plan lists PLIC under
   M2.5). This may be the largest single piece of B3.
5. **COBS on the virtio path** touches `itest`'s golden expectations — sequence
   it as its own increment, before any UART work.
6. **Does the telemetry path need RX?** Only if the reader-ready handshake
   (Decision 3, deferred) is ever built. The console needs RX regardless — the
   Stitch REPL uses it, and RX stays raw bytes in both console modes.
7. **When does `console=frames` become the default?** Not until the frame path
   has proven itself on the board, and probably not until the collector-as-
   terminal experience is at least as good as `screen`. Worth revisiting rather
   than leaving `text` the default forever by inertia.

## Increment sketch

Host-first: everything that can be proven under snemu is proven before the board
is involved, because a board round-trip is expensive (a stale image cost two
flashes and a wrong diagnosis in one session).

1. **COBS the wire format**, both transports. Gate green. Pure protocol +
   collector work, no board.
2. **Recoverable decode** in the collector: on error, skip to the next `0x00`,
   count a resync, continue. Socket path only; still verifiable under snemu.
3. **`console=` mode + `Frame::Log`** — route kernel `println!` through the frame
   path when `console=frames`, keeping `text` as the default and pre-init/panic on
   raw text always. This is Decision 4, and it is *also* host-testable: snemu's
   UART capture shows exactly what the board would emit. Retires the
   `board-heartbeat-print` workaround (the mode subsumes it).
3b. **Collector as terminal** — `xtask reader` puts stdin in raw mode, relays
   bytes to serial TX, renders `Frame::Log` to stdout. Needed before `console=frames`
   is usable interactively; the Stitch REPL is the test case. Can be built and
   tested against the QEMU socket path first.
4. **Measure** real telemetry throughput under snemu; pick a baud.
5. **Baud programming** (`DLL`/`DLM` from the DTB clock) — small, board-verifiable
   on its own by just watching the console come back at the new rate.
6. **TX ring + THRE interrupt** in the UART driver. The big one.
7. **`UartFrameSink`** on top of the ring; board brings up.
8. **Collector `--serial` source**; board into Grafana. *(Code landed 2026-08-25 and
   gate-green; the "board into Grafana" half is outstanding — no board has been
   attached.)*

Steps 1–4 are host-side. Step 5 is the cheapest possible board increment (one
observable bit: does the console still print). Steps 6–8 are where the real
hardware risk lives, and they arrive with everything else already proven.
