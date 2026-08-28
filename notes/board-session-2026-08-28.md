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

### One smaller thing the decode surfaced

`BuildInfo` arrives **before** `Hello`, and the collector drops it with a warning
whose remedy ("Stop QEMU and restart the kernel after the collector connects") is
QEMU-shaped advice that cannot apply to a serial board — there is no connect-first
on a physical line. Either the kernel's frame order is wrong or the collector's
expectation is; either way the message needs a serial-aware branch.

## Still to do

- The `console=frames` hang — the blocker for item 2 / M2.
- A reset path that works on this board (WARM_REBOOT, or the JH7110 watchdog),
  without which the bridge's unattended loop cannot close.
- Item 4bb — `workload=gmac-tx`, T3/T4.
- §3c's remaining exit-code rows (`CallInDevice`, `PortHeld`, `--until NEVER_APPEARS`).
- §3d — the unplug-mid-capture test.
