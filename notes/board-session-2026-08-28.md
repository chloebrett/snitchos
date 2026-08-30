# Board session — 2026-08-28

Running the agenda in [../docs/next-board-session.md](../docs/next-board-session.md),
picking up the "Still to do" list from
[board-session-2026-08-27.md](board-session-2026-08-27.md). Captured live.

## Setup

- Adapter: `/dev/cu.usbserial-0001`, 115200. Nothing held the port.
- Mac holds `192.168.0.7` (`ifconfig en0`), matching the saved `serverip` — the
  Fixed-private-MAC + reservation fix from 2026-08-28 held.
- `dnsmasq` still running from the previous session, TFTP root at the repo.
- Image: `cargo xtask image`, `--opt max`, **1839528 bytes**, 12:23.
- The sandbox still denies `/dev/cu.*`; every board command needed the override.
  Unchanged from last session, still worth an allowlist entry.

## The first hour was a broken UART — and the board was fine the whole time

Symptom: zero bytes in an 8 s window, no echo to a bare `\r`, and the same
silence from a completely independent read path (`stty` + `cat`), so not a tool
bug. Meanwhile ARP showed `192.168.0.61 → 6c:cf:39:00:56:cb`, i.e. the board had
DHCPed.

The diagnosis that mattered: **the silence covered U-Boot's own banner**, and
nothing in our kernel can suppress that. So the fault had to be below the kernel,
in the physical link.

Split with a loopback — adapter RX and TX dupont ends touched together, board
disconnected entirely:

```
board: > LOOPBACK-TEST-12345
LOOPBACK-TEST-12345board: capture ended (Quiet)
```

Adapter, USB cable, driver, baud, `serialport`, and the whole `board exec` path:
all good. Fault was at the board end, and **replacing the dupont wires fixed it**
— reseating them twice did not. Wires, not seating.

Generalisable, and cheap enough to make the standing first move: *a loopback at
the adapter is one physical action and partitions the entire stack.* It should
arguably be a `cargo xtask board loopback` subcommand — it needs no board, has an
unambiguous pass/fail, and is exactly what you want before believing any "the
board is dead" reading.

Last session's "board went quiet — unexplained" and its long-`setenv`-line
hypothesis are almost certainly this same fault. The note already suspected the
cable swap; the answer is that the *wires* had failed, not their seating.

### `atime` is not a TFTP oracle

Mid-diagnosis I used `snitchos.img`'s access time as evidence the board had
fetched it. It worked once and then went stale across several real boots — APFS
does not update `atime` per read reliably. Don't build a liveness check on it;
the previous session's tcpdump-of-broadcasts oracle is the sound one.

## §3a / §3e — first light, and the input echo earns its keep

`cargo xtask board exec` works against real hardware. The 2026-08-28 echo
addition printed exactly what it promised:

```
board: > ^M
board: > (nothing)
board: > LOOPBACK-TEST-12345
```

`--until-quiet` fires correctly and reports `Quiet` (loopback, and again at the
U-Boot prompt); a genuinely silent line reports `Timeout`. Note the quiescence
clock appears not to start until the first byte arrives — an empty capture
against a dead line ends on the hard deadline, not on quiet. That is arguably
right, but it is undocumented behaviour and worth a test either way.

## 🔴 `board reboot` does not work on this board — SBI SRST hangs the platform

The new `reboot` subcommand did its half correctly. The kernel took the token:

```
board: > ~~~reboot^J
~~~reboot
parse error: 1:1: unexpected character `~`      <- the REPL also saw it
stitch> pmic_ops: cannot read pmic power register
```

`pmic_ops: cannot read pmic power register` is **OpenSBI**, failing to reach the
AXP15060 PMIC over I2C to perform the reset. The board did not restart, and it did
not come back — every later read was silent until a manual power cycle.

Two things follow, and the second is the sharper one:

- **`kernel/src/obs/heartbeat.rs`'s fallback never runs.** It is written for
  firmware that *refuses* SRST — "returns only if the firmware refuses", then
  prints `reboot: SBI SRST refused (error=…) — continuing`. That line never
  appeared. OpenSBI did not return an error; it **hung**. A refusal is a case the
  code handles; a hang is not, and on this platform the hang is what happens.
- **So `board reboot` is currently worse than not having it**: it costs a manual
  power cycle every time it is used, which is precisely the human-at-the-board
  cost the bridge exists to remove.

### Change made in-session: `RESET_TYPE`, warm on the board

`kernel/src/sbi.rs` now picks the reset type per platform — cold (1) everywhere,
**warm (0) under the `vf2` feature** — and `system_reset_cold_reboot` is renamed
`system_reset`, since it is no longer always cold. Scoped rather than swapped
globally on purpose: cold is correct under QEMU and snemu, and
`console-reboot-requests-srst` asserts a *halt*, which a `NOT_SUPPORTED` return
would not produce. Both feature paths build.

Warm is the cheapest untried alternative, not a known fix — OpenSBI's reset driver
may claim both types and hang identically. If it does, the next candidate is the
**JH7110 watchdog** at `watchdog@13070000`: U-Boot's banner says `WDT: Not
starting watchdog@13070000`, so it is present and idle, and a short timeout resets
the SoC with no firmware cooperation at all — immune to whatever is wrong with the
PMIC path.

Until one of those works, **the bridge's unattended loop cannot close on this
board**, because every iteration needs a human to power-cycle.

The doc comment on `system_reset` also gained the distinction this cost an hour to
learn: *a return is not the only failure mode, and the other one is worse.* The
function's contract was written for firmware that refuses; firmware that hangs is
unrecoverable from the caller's side and needs a different reset mechanism, not a
better fallback.

## Autoboot interrupt — solved, and the shape the bridge needs

Step 4b of the bridge plan ("interrupt the autoboot countdown") now has a working
recipe, and two failed attempts that name the constraints:

1. A shell loop doing `printf ' ' > /dev/cu.…` per keystroke **reopens the device
   every iteration**, which resets termios back to the default line rate. Result:
   a capture that looks exactly like a dead board.
2. Releasing the port between `run bootcmd` and attaching the reader loses the
   boot log — the adapter buffers it, and the *next* opener reads a stale burst
   and mistakes it for live output. This cost a full cycle to understand.

Both vanish under one rule: **one process holds one fd for the entire sequence.**
The working driver catches the prompt by spamming a space every 50 ms while
reading, then feeds commands **one line at a time, waiting for `StarFive #`
between them** — which is also the line-at-a-time pacing last session asked for.

```
Hit any key to stop autoboot:  2 ^H^H^H 0
StarFive #                <INTERRUPT>
StarFive # setenv bootargs console=frames
StarFive # run bootcmd
```

`bootdelay=2`, so being already-typing when the window opens is the whole trick.

## Item 4c — closed again, this time from U-Boot itself

`printenv` and a live `dhcp` both say it directly:

```
ethact=ethernet@16040000
ethernet@16040000 Waiting for PHY auto negotiation to complete...... done
DHCP client bound to address 192.168.0.61 (129 ms)
```

**The cabled RJ45 is GMAC1 (`0x1604_0000`)** — the controller the driver targets.
Confirms last session's inference by a second, independent route, and adds that
its PHY auto-negotiates in ~130 ms.

## Item 4b — descriptor ground truth ✅, and the version register ✅

At the U-Boot prompt, after a `dhcp` so the ring is live:

```
md.l 0x16040110 1
16040110: 00004152          <- low byte 0x52
md.l 0x16041114 1
16041114: ff73e5c0          <- DMA_CHAN_TX_BASE_ADDR
md.l 0xff73e5c0 10
ff73e5c0: ff73e840 00000000 0000015e 30000000
ff73e5d0: 00000000 00000000 00000000 00000000   (rest of the ring unused)
```

**Core version `0x52` — dwmac-5.20, confirmed.** The `0x0110` offset was
transcribed from mainline and had never been checked against a datasheet; it is
right. So is the `0x1604_0000` base, and the peripheral is ungated and out of
reset (U-Boot is using it).

The descriptor validates `kernel_devices::gmac`'s TDES encoding against
silicon-accepted data, which is the only independent check that layout can get:

| word | U-Boot | our encoding | verdict |
|---|---|---|---|
| 0/1 | `ff73e840` / `0` | buffer address low/high | ✓ |
| 2 | `0000015e` | `len & TDES2_BUFFER1_SIZE_MASK` — 350 bytes, a plausible DHCP frame | ✓ |
| 3 | `30000000` | `FD(29) \| LD(28)`, `OWN(31)` clear, length field cleared | ✓ writeback form |

Bit 15 (`TDES3_ERROR_SUMMARY`) is clear, so this is also a worked example of the
success case `had_error` is meant to distinguish. **No desk work is invalidated.**

## 🔴 `console=frames` boots, streams, and then the kernel hangs

The good half first, because it is real: **`console=frames` puts a decodable
frame stream on the board's UART.** Text runs to `ph: post-timer (interrupts on)`,
then the sink installs and the wire turns binary. Replayed through the collector
(`collector --replay`), every frame decodes:

```
BuildInfo { kernel_profile: "release", userspace_opt: "3" }
StringRegister { id: StringId(1), value: "kernel.boot" }
SpanStart { id: SpanId(2), parent: SpanId(0), name_id: StringId(1), t: 50195833, … }
SpanStart { id: SpanId(4), … name_id: StringId(3) /* telemetry_init */ … }
Dropped { count: 0 }
SpanEnd { id: SpanId(4), t: 50253737 }
MetricRegister { name_id: StringId(4) /* snitchos.heartbeat.count */, kind: Counter, … }
…
```

Timestamps monotonic, span nesting intact, `Dropped { count: 0 }`.

**Then it stops, ~500 bytes in, mid-metric-registration, and never resumes.** A
40 s stream afterwards captured nothing; two separate live collector attachments
captured nothing.

### It is a hang, not a drain failure

The tell is a *text* marker, not a frame. The boot path is:

```
ph!("post-timer (interrupts on)");
… enable_software_interrupts();
… enable_external_interrupts();
for &b in b"tx-irq-ok\n" { console::tx_push(b); }
ipi::send(…);
ph!("post-ipi");
```

`ph!` writes are polled and direct — they do not depend on the ring or the sink.
`ph: post-timer` appears. **`tx-irq-ok` never appears, and neither does
`ph: post-ipi`.** So the kernel does not merely fail to drain; it stops executing
somewhere in `enable_external_interrupts` / `tx_push` / `ipi::send`.

Note what that rules out. `tx-irq-ok` exists precisely as the PLIC + SEIE + THRE +
drain oracle, and last session's diagnosis turned on its absence — but its absence
is also consistent with never reaching it, and `ph: post-ipi` is what tells the two
apart. **A missing marker means "did not get here" before it means "the mechanism
under test is broken".**

### Why it looks volume-dependent

The same code path reaches the Stitch REPL fine under `console=text`, where the
ring carries almost nothing. Under `console=frames` every `println!` becomes a
`Frame::Log` and the pre-init backlog flushes through the ring at once. So the
hypothesis is a TX-interrupt path that livelocks or wedges under load — but that
is a hypothesis, and the text-mode control boot is the experiment that tests it.

**Item 2 therefore does not pass, and M2 stays open.** The collector's serial path
is not the problem: it decoded everything the board sent, and `--replay` of the raw
capture is a clean, complete decode. The fault is kernel-side.

### ✅ ROOT-CAUSED: the kernel inherits U-Boot's PLIC enable bitmap

Two more boots settled it, and both hypotheses above were wrong.

**First**, `console=text workload=gmac-probe` hangs in exactly the same place. So
it is not `console=frames`, not the frame sink, and not TX volume. In text mode
`println!` takes `UART.lock()` and writes *polled*, so a missing marker really is
"did not get here".

**Second**, three bisection markers between `post-timer` and `post-ipi` located it
precisely:

```
ph: plic boot mhartid 2 -> S-context 4
…
ph: post-timer (interrupts on)
ph: post-ssie                        <- last line. `post-seie` never printed.
```

The hang is *inside* `trap::enable_external_interrupts()` — the single CSR write
that sets `sstatus.SEIE`.

And the PLIC context is **right**: mhartid 2 is cpu2, whose S-context is 4 under
the JH7110's `interrupts-extended` order. So last session's suspicion of the
context formula is exonerated; the formula and the boot-hart-derived computation
both hold.

The actual mechanism:

- `plic::enable_source` **deliberately preserves the other bits** in the enable
  word, because a context may route several sources. Correct on a freshly-reset
  PLIC.
- **A bootloader is not a reset.** U-Boot has just run DHCP and TFTP over GMAC1,
  and leaves that source enabled in the same S-context the kernel adopts.
- So the first `SEIE` immediately takes an external interrupt for a device the
  kernel has no handler for. `handle_external` claims it, sees it is not the UART,
  completes it — and **completing an interrupt does not quiet its device**, so the
  `while let Some(source) = claim()` loop claims it again, forever.

A livelock inside the trap handler, with interrupts masked. No fault, no panic,
no output: indistinguishable from a dead board, which is why it read as "the UART
is broken" for most of the day.

**Fix**: `kernel_devices::plic::reset_context(plic, context, max_source)` clears
every enable bit the context holds before `enable_source` sets ours — the kernel
starts from a bitmap it owns rather than inheriting the bootloader's. Three tests
(TDD, RED confirmed first): clears inherited enables across *both* enable words,
leaves other contexts alone (the JH7110's M-contexts belong to OpenSBI, still
running), and covers the word holding `max_source` inclusively. `MAX_SOURCE` is
per-platform — 95 on QEMU `virt` (`VIRT_NDEV`), 136 on the JH7110
(`riscv,ndev = <136>`) — because clearing past the implemented range would write
registers that do not exist.

**Still worth doing separately**: `handle_external`'s claim loop is unbounded and
silent. An enabled source with no handler wedges the kernel with no diagnostic at
all. It should disable-and-snitch instead — "refusals snitch, never silent" is
this repo's rule everywhere else, and this is the one path where a surprise costs
the whole machine.

### Two lessons worth keeping

- **A missing marker means "did not get here" before it means "the mechanism under
  test is broken".** `tx-irq-ok` exists as the PLIC+SEIE+THRE+drain oracle, and
  last session read its absence as proof that path had failed. It was absent
  because execution stopped one instruction earlier. `ph: post-ipi`'s absence is
  what distinguishes the two, and it was already in the tree.
- **I built three successive mechanism stories** (volume-dependent TX livelock,
  `UART.lock()` deadlock, `TX_RING` deadlock) and the code had already defended
  against all three — `drain_tx` takes a fresh handle, `tx_push` wraps in
  `without_interrupts`. Each story was plausible and none was tested. The
  three-marker bisect cost one boot and beat all of them. *Instrument before
  theorising* is cheaper than it looks, even when the theory feels close.

### One smaller thing the decode surfaced

`BuildInfo` arrives **before** `Hello`, and the collector drops it with a warning
whose remedy ("Stop QEMU and restart the kernel after the collector connects") is
QEMU-shaped advice that cannot apply to a serial board — there is no connect-first
on a physical line. Either the kernel's frame order is wrong or the collector's
expectation is; either way the message needs a serial-aware branch.

## Where the session stopped

**The PLIC fix is written, host-tested and built into `snitchos.img`, but has
never run on the board.** The session ended before a power cycle could confirm it.
That is the one thing to do first next time, and it is a single boot:

```
cargo xtask image          # if the tree has moved
# arm a capture, power-cycle, and read the markers
```

Success looks like `post-ssie` → **`post-seie`** → `tx-irq-ok` → `post-ipi`,
followed by the `gmac-probe` dump. `post-seie` is the whole result: it is the
marker that has never printed on this board.

If it still stops at `post-ssie`, the diagnosis is wrong and the next suspect is
the claim loop itself rather than the bitmap — bound `handle_external` and have it
report the source id, which names the device instead of guessing at it.

Note the confirmation boot also delivers **item 4a** (the probe dump) for free,
since the fix has to be tested with *some* workload and `gmac-probe` is the one
still owed.

## Still to do

- **Confirm the PLIC fix on hardware** (above). Blocks everything else.
- Then item 2 / M2: re-run `console=frames` + `cargo xtask reader --serial`. The
  collector side is already proven — `--replay` decoded the captured stream
  completely — so this should be the boot that closes M2.
- `handle_external`: bound the claim loop and snitch on an unhandled source.
  Independent of whether the bitmap fix works; a silent livelock in the trap
  handler should not be reachable at all.
- Whether `WARM_REBOOT` actually helps. Tried once and got the same `pmic_ops`
  message, but that boot may have been the *old* image — untested, not disproven.
  The JH7110 watchdog remains the fallback.
- Item 4bb — `workload=gmac-tx`, T3/T4.
- §3c's remaining exit-code rows (`CallInDevice`, `PortHeld`, `--until NEVER_APPEARS`).
- §3d — the unplug-mid-capture test.

## Tooling: `cargo xtask board uboot` — ported, in-session

Driving the board this session needed a sequence `board exec` cannot express:
catch the autoboot prompt, feed commands one line at a time waiting for the prompt
between them, then stream — all on **one fd held open throughout**. That is
board-bridge step 4b, and it is what made the last four boots legible.

It was prototyped in a throwaway Python script in the scratchpad while `cargo`
rebuilds were costing minutes against a live board — a fair trade for a prototype
and a bad one for a deliverable, so it was **not** committed and has now been
ported:

```
cargo xtask board uboot --device /dev/cu.usbserial-0001 \
  --cmd 'setenv bootargs workload=gmac-probe' --cmd 'run bootcmd' --stream 45000
```

Most of it already existed. `script::run` was written for exactly this — one line
at a time, abandoning the rest if a step goes unanswered — and had simply never
been wired to a command. The genuinely new part is `xtask_board::knock` (6 tests,
RED first), which decides *what answered*.

Three properties it needs that `board exec` does not have, each learned by losing
a boot to its absence:

1. **One process owns the line for the whole sequence.** Releasing the port
   between `run bootcmd` and attaching a reader loses the boot log — the adapter
   buffers it and the next opener reads a stale burst as if it were live.
2. **Never reopen the device per keystroke** — that resets termios to the default
   line rate, and the result is indistinguishable from a dead board.
3. **Probe, don't wait passively.** Sending a CR every second and reading only
   what comes back after it distinguishes "already at a prompt printed before we
   opened" from "autobooted past the window" from "genuinely silent". Waiting for
   a prompt string to appear spontaneously fails the first two, silently.

Also worth having: a `board loopback` — short the adapter's RX and TX and echo a
string. One physical action, unambiguous pass/fail, and it partitions the entire
stack. It would have saved the first hour of this session.

And a caution the session earned twice: **U-Boot's console has no flow control.**
A `setenv` line came back as `bootargs le=text workload=gmac-probe` — the leading
`conso` simply dropped. Anything driving U-Boot should echo back what it sent and
compare, not assume the line arrived.
