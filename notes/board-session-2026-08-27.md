# Board session — 2026-08-27

Running the agenda in [../docs/next-board-session.md](../docs/next-board-session.md).
Captured live; ordered as it happened, not as the agenda lists it.

## Setup

- Adapter: `/dev/cu.usbserial-0001`, 115200. Opened first time, no contention.
- Image: `cargo xtask image --workload gmac-probe`, `--opt max` (default),
  1778208 bytes, 21:18.
- Board found **already at the U-Boot prompt** (`StarFive # `), powered.

**The sandbox blocks `/dev/cu.*`.** First `board exec` returned
`OpenFailed { kind: Other }` purely from the harness sandbox, not the board.
Every board command in this session needed the sandbox override. Worth an
allowlist entry — the failure is indistinguishable from a real open failure at
the taxonomy level, which is the one thing the `Ended` split exists to prevent.

## Item 1 — netboot provisioning: **already done, and the doc was wrong**

The agenda says "⏳ Written down, never run. Nobody has typed this at the board."
That is false. `printenv` shows it provisioned, and in a *better* form than the
doc records:

```
bootcmd=dhcp; setenv serverip 192.168.0.7; tftpboot 0x40200000 snitchos.img; cp.b ${fdtcontroladdr} 0x46000000 0x100000; booti 0x40200000 - 0x46000000
bootdelay=2
serverip=192.168.0.7
bootargs=workload=stitch-repl
```

It copies the DTB to `0x46000000` and passes that address literally, rather than
passing `${fdtcontroladdr}` through to `booti`. Both of the doc's predicted
silent-failure traps were already avoided: `${fdtcontroladdr}` is stored
**literally** (unexpanded), and the semicolon chain **survived as one string**.

So the doc's warnings were sound and the status line was stale. The real failure
was a layer below anything it anticipated.

### The actual failure: `serverip` had silently drifted

`tftpboot` failed with repeated `ICMP destination unreachable (port unreachable)
from 192.168.0.8`.

Root cause, verified rather than inferred:

- The Mac is **192.168.0.8**, not `.7` (`ifconfig`, DHCP `yiaddr`, routing table
  all agree). Nothing on this machine holds `.7`.
- `en0` is **Wi-Fi** (`networksetup -listallhardwareports`). No ethernet
  interface has a LAN address, so the Mac reaches the board's LAN over Wi-Fi —
  the "long ethernet cable" carries the *board*, not the Mac.
- `en0`'s permanent MAC is `bc:d0:74:08:62:fd`, but the address **in use** is
  `96:5f:c7:25:91:03` — locally-administered, i.e. **Private Wi-Fi Address is on**
  for home wifi ssid.

That is the whole mechanism: a DHCP reservation cannot pin a rotating MAC, so
`serverip=192.168.0.7` was correct when written and became wrong without any
change to the repo, the board, or the doc. **The hardcoded IP in `bootcmd` is a
standing trap, not a one-off.**

Second, independent fault: `dnsmasq` had been running since **11 Aug** bound to
addresses that no longer existed — the board's ICMP *port unreachable* proves the
Mac's kernel received the datagram and had no listener. Restarted by hand
(needs sudo, so not automatable from the agent):

```
sudo pkill dnsmasq
sudo dnsmasq --enable-tftp --tftp-root=/Users/chloe/c/snitchos --port=0 --no-daemon --log-queries
```

After the restart, with `serverip` set to `.8` in the live environment:

```
Bytes transferred = 1778208 (1b2220 hex)
```

— an **exact** match for the local `snitchos.img`, so item 0's staleness check is
satisfied by byte count rather than by faith. TFTP path confirmed end to end.

**Not yet saved.** `saveenv` of the corrected `bootcmd` did not happen before the
board went quiet (below).

## Item 4c — partially answered for free

ARP: `192.168.0.60 → 6c:cf:39:0:56:ca`, matching U-Boot's `ethaddr` and
`ethact=ethernet@16030000`. So **the currently-cabled RJ45 is GMAC0**, the one
U-Boot uses for DHCP/TFTP. GMAC1 (`16040000`) is therefore the *other* jack —
still to be confirmed by plugging it.

`chipa_gmac_set` in the environment independently confirms the node mapping:
`/soc/ethernet@16030000/ethernet-phy@0` and `/soc/ethernet@16040000/ethernet-phy@1`.

## Board went quiet — unexplained

After the successful `tftpboot`, a well-formed one-shot

```
setenv bootcmd '…'
printenv bootcmd
```

produced **no echo at all**, and the board has been silent since (Ctrl-C, bare
newlines, and a 15 s pure read all returned nothing).

Ruled out:

- Shell quoting — the exact bytes were dumped and verified correct.
- Cable/adapter — `/dev/cu.usbserial-0001` still enumerated, unchanged inode.

Leading candidate, **unconfirmed**: U-Boot's console has no flow control, and a
~160-byte pasted line can be dropped or wedge it. If that is real it is a
constraint on the board bridge generally — `script.rs` must feed U-Boot a line at
a time and wait for the prompt between lines, never paste a block.

Awaiting a power cycle.

## Tooling gaps this session found

1. **`exec` takes its input as a positional argv string.** A U-Boot command chain
   is semicolon-separated by nature, so driving one from a shell collides with the
   repo's atomic-Bash rule. Escaping semicolons is a workaround, not a fix. Wanted:
   `exec --input-file <path>` (and `-` for stdin), so a sequence lives in a file —
   reviewable, diffable, re-runnable, and the natural input for `script.rs`.
2. **Sandbox denies `/dev/cu.*`**, and the denial surfaces as `OpenFailed{Other}`,
   which looks like a board fault.
3. **No line-at-a-time pacing** when driving U-Boot (see above).

## Still to do

- `saveenv` the corrected `bootcmd` (blocked on power cycle).
- §3a proper boot-log capture, §3c exit-code taxonomy, §3d unplug test,
  §3e quiescence tuning.
- Item 2 — `cargo xtask reader --serial`, the milestone-closing one.
- Item 4a/4b — `gmac-probe` boot and U-Boot descriptor ground truth.

## Follow-ups for a desk session (no board needed)

- ~~Turn **off** Private Wi-Fi Address for network, then pin the
  DHCP reservation to `bc:d0:74:08:62:fd` and restore `serverip=192.168.0.7`.~~
  **Done 2026-08-28, by a better route than this note proposed.** Private Wi-Fi
  Address was set to **Fixed** rather than off, and the DHCP reservation pinned to
  the resulting stable per-SSID MAC (`96:5f:c7:25:91:03`). `en0` holds
  `192.168.0.7/24` — a real lease, not the `/32` alias, since only one `inet` line
  is present.

  **Correcting this session's own diagnosis:** the notes above conclude "Private
  Wi-Fi Address is on" and treat that as the fault. The fault was **rotation**, not
  privacy — macOS offers Off / Fixed / Rotating, and Fixed gives a locally-
  administered MAC that is stable per SSID, which a reservation *can* pin. So the
  fix keeps the privacy feature and needs neither the permanent MAC nor turning
  anything off. (A first pass at this update mis-read `ifconfig` still showing a
  locally-administered MAC as "the change hasn't applied yet" — under Fixed that is
  the expected steady state.)

  Residual, and genuinely small: the MAC is per-SSID, so another network needs its
  own reservation; and `serverip` is still hardcoded in `bootcmd`, so a network
  change still drifts. The `ifconfig alias` remains the fallback that depends on
  neither router nor UART.
- ~~Correct item 1's status in `docs/next-board-session.md` — it is provisioned.~~
  **Done 2026-08-28.** Item 1 now records the saved environment verbatim, the three
  U-Boot facts (`dhcp` overwrites `serverip`; `dhcp` does not set `ipaddr`; `md.l`
  counts are hex), and the drift hazard with its ICMP signature.
- Add input echo-to-stderr (below), plus optional `exec --input-file`.

## Board liveness settled by tcpdump — the UART is the only fault

With the UART dead and ping useless (SnitchOS has no network stack; idle U-Boot
ignores ICMP), liveness was settled from the **broadcast** side instead. `bootcmd`
opens with `dhcp` and then TFTPs, so a living board must emit a DHCP DISCOVER and
an ARP for `serverip`. Broadcasts flood every switch port including the AP, so the
Wi-Fi-attached Mac sees them even though the board is on the cable:

```
21:46:44  DHCP Request from 6c:cf:39:00:56:cb
21:46:47  ARP Announcement 192.168.0.61
21:46:47  Request who-has 192.168.0.7 tell 192.168.0.61   <- the stale serverip
21:46:53  (gives up; silent thereafter)
```

**The board is fine.** Boot ROM, U-Boot, DHCP and ethernet all work. The serial
link is the sole failure, and every "silent board" reading earlier in this session
was that one fault.

Generalisable: *a board with no working console is not necessarily a dead board,
and its network broadcasts are an independent liveness oracle.* Worth building
into the board bridge — `wire::capture` reporting `Timeout` cannot distinguish
"hung" from "console unplugged", but a concurrent BPF watch can.

## `serverip`: interface alias beats DHCP reservation

Since the board asks for `192.168.0.7` and nothing answers, the fix is to **make
the Mac answer there** rather than to edit `bootcmd`:

```
sudo ifconfig en0 alias 192.168.0.7 255.255.255.255
```

Better than the doc's DHCP-reservation plan on three counts: it needs no working
UART, no router access, and is immune to the Private-Wi-Fi-Address MAC rotation
that broke the reservation in the first place. The saved `bootcmd` becomes correct
as written, so **item 1 needs no `saveenv`**. Make it permanent with a
`networksetup` secondary address so it survives a reboot.

## Item 4c — RETRACTED and reopened

The earlier "the cabled RJ45 is GMAC0" claim was read off a **stale ARP entry** and
is not supported:

| When | MAC | U-Boot | Address |
|---|---|---|---|
| Manual `dhcp`, pre-cycle | `…56:ca` (`ethaddr`) | `Using ethernet@16030000` | .60 |
| Auto `bootcmd`, post-cycle | `…56:cb` (`eth1addr`) | — | .61 |

### ...then ANSWERED — RJ45 #2 is GMAC1

The "both jacks cabled" guess was also wrong. There is **one** cable, and it was
moved from port #1 to port #2 between the two observations. That makes the pair a
controlled A/B across a single variable:

| Cable in | DHCP MAC | U-Boot device |
|---|---|---|
| port #1 | `…56:ca` = `ethaddr` | `ethernet@16030000` (stated explicitly) |
| port #2 | `…56:cb` = `eth1addr` | `ethernet@16040000` |

**Physical RJ45 #2 is GMAC1 (`0x1604_0000`)** — the controller the driver targets,
and currently the cabled one. Item 4c closed, without the jack-at-a-time procedure
the agenda proposed, and without `net list`.

Method note: this was answered *by accident*, from telemetry captured for an
unrelated reason. The tcpdump was run to settle board liveness; the MAC in it
turned out to answer a question from a different item entirely. Cheap broad
capture beat the targeted procedure.

### ...and it probably explains the dead console

The cable swap means reaching across the board, which is exactly how the UART
header's dupont jumpers get nudged. The console died inside that same window. That
is a far better explanation than the long-`setenv`-line hypothesis, which was never
more than a guess and does not explain why a *power-cycled* board stayed silent.

## §3c exit-code taxonomy — one row confirmed on hardware

Not staged; it happened. After the adapter was unplugged from the Mac, the device
node was **absent** (not renamed — no `cu.usbserial-*` at all):

```
board: NoSuchDevice { device: "/dev/cu.usbserial-0001" }   exit 2
```

Correct classification, and instant — no hang, no timeout. This is the row that
matters most to an unattended loop: exit 2 means *fix the host*, not *reboot the
board*. Remaining rows: `CallInDevice` (point at `tty.*`), `PortHeld` (hold it with
`screen`), and `--until NEVER_APPEARS` → exit 1.

Note also that the harness **sandbox** denial surfaces as `OpenFailed{Other}`,
which is a *different* failure wearing similar clothes — a host-side permission
problem that is neither of the above. Arguably its own variant.

## ✅ Zero-touch netboot works — SnitchOS runs on the board

`run bootcmd` → DHCP → TFTP (1778208 bytes, exact) → `booti` → banner, every `ph:`
marker, **4 U74 harts up**, 85+ heartbeats. `saveenv` persisted `serverip=192.168.0.7`
and the deleted `ipaddr`, so this is now hands-off. Item 1 closed for real.

Two U-Boot facts that cost time and are worth keeping:

- **`dhcp` overwrites `serverip`** with the DHCP server (the router). That is why the
  saved `bootcmd` orders it `dhcp` *then* `setenv serverip` — the sequence is load-bearing.
- **`dhcp` does NOT set `ipaddr`** in this U-Boot (2025.10, lwIP-based); it binds the
  lease internally. A stale `ipaddr` therefore never refreshes and must be *deleted*.
  But it is **cosmetic** — `our IP address is <NULL>` still transferred fine, because
  lwIP sources from its bound address. It looked like the fault and was not.
- **`md.l <addr> 16` dumps 22 words** — U-Boot parses counts as hex.

## 🔴 THE BLOCKER: the kernel has no UART telemetry sink

**Item 2 cannot pass with this tree, and item 4a is blocked by the same fault.**

- `kernel/src/obs/tracing.rs` module doc: *"All frames go out the virtio-console."*
- `KernelSink` routes to exactly two sinks: the UDP batcher (`net=`, needs virtio-net)
  and virtio-console.
- **`UartFrameSink` appears nowhere in `kernel/src`** — it lives only in
  `kernel-obs/src/uart_sink.rs`.

The VF2 has neither virtio-console (`init failed: NotFound`) nor virtio-net, so
telemetry frames have **no sink at all**. B3 step 9 landed the sink *implementation*
and its host tests (8/8 mutants killed — genuinely done); nothing ever installed it.
The plan's status is true of the crate and misleading about the system, which is
exactly the risk its own header names: *"No byte of this has crossed a real UART."*

**`console=frames` would make it worse, not better** — it routes `println!` into the
same dead sink, costing the human log as well.

### The probe cannot speak on the hardware it was written for

`device::gmac::probe` reports via `crate::tracing::emit_log`, so it ran and emitted
into the dead sink. `kernel/src/device/console.rs` states the rule three lines from
the bug:

> early output must never depend on the frame sink (the stale-image / `ph!`-markers
> lessons)

Same lesson, unlearned in a new place. **Fix: the probe should `println!`** — it is
reconnaissance output for a human, on a board whose only working channel is raw UART.
That is a small change and it unblocks item 4a independently of the sink work.

## ✅ TELEMETRY CROSSES A REAL UART

```
ContextSwitch { from: 3, to: 6, t: 58553396, reason: Yield, hart_id: 1 }
ContextSwitch { from: 0, to: 1, t: 58557212, reason: Yield, hart_id: 0 }
```

Decoded off the serial line, both harts represented, timestamps monotonic. The
sink work above was necessary but **not sufficient** — it fed a ring that never
drained. The actual blocker was the PLIC.

### The PLIC context is not a constant — the boot hart moves

`plic.rs` hardcoded QEMU `virt`'s numbers, with a standing `// board: derive from
DTB` note. Two things were wrong:

- **UART source**: QEMU `virt` is 10; the JH7110 is **32**.
- **S-mode context**: contexts follow `interrupts-extended` order. QEMU `virt` is
  symmetric (every hart contributes M then S) so hart `m` owns `2m+1`. The JH7110
  is one S7 + four U74s and the **S7 contributes only an M context**, so the
  missing `cpu0 S` shifts everything down one: U74 `m` owns S-context `2m`.

**And the boot hart is not fixed.** Two consecutive power cycles reported
secondaries `mhartid 1, 2, 4` then `1, 3, 4` — so the kernel booted on mhartid 3,
then on 2. OpenSBI hands off to whichever hart wins the race. A hardcoded context
is right only by luck; my first fix hardcoded `2` and failed for exactly this
reason. It is now computed from `LOGICAL_TO_MHARTID[0]`, which `kmain` fills from
the DTB before `plic::init`.

**This failure mode is silent by construction.** The source gets enabled in some
other context's bitmap: every MMIO write succeeds, nothing faults, and the
interrupt simply never arrives. It presents as a working UART with a TX ring that
fills and never drains — indistinguishable from "the sink isn't wired".

**The oracle was already in the tree.** `main.rs` pushes `tx-irq-ok` through the
ring right after enabling external interrupts, precisely to prove PLIC + SEIE +
THRE + drain end to end. Its *absence* from the board's boot log was the whole
diagnosis, and the `tx-irq-delivers` itest passes under snemu — a genuine
emulator/hardware divergence that only a board could find.

### Remaining: `console=frames`

Text and frames now interleave and corrupt each other on the one wire:

```
ph��:� postr-eilpeaise
snitchos.time.tickpsh	: p�o��s�t-f%r	a!msnei-taclholso.cirq
```

That is Decision 4 of the design doc arriving on schedule — *"one UART, and the
human log becomes frames."* Setting `console=frames` in the saved bootargs is the
last step before `cargo xtask reader --serial` gives a clean stream. Blocked only
on getting back to the U-Boot prompt (autoboot interrupt, still untested).

## The input-echo design (supersedes `--input-file` alone)

`--input-file` on its own is a **regression in observability**: an agent writes a
temp file, passes a path, and the transcript records the path instead of the
commands. For this repo that is the wrong trade.

So `exec` should **echo the exact bytes it is about to write, verbatim, to
stderr**, beside the capture summary it already prints. Then the input mechanism
stops mattering — argv, file or stdin all leave the same record. It also captures
something argv never did: what was *actually* sent after all quoting, which is
precisely what had to be hand-checked with `cat -v` tonight when the escaping fell
under suspicion. And it lets the tool express a case it currently cannot — input
sent, nothing echoed back.

