# The next board session — an ordered agenda

**Written 2026-08-27**, from a verification sweep of every in-flight plan against
the tree.

The VisionFive 2 is across the room, not far away — but in its current state it
needs wiring up each time, including a long ethernet cable, and that setup is
enough of a nuisance that you don't do it on a whim. So the cost is not *reaching*
the board, it is the fixed overhead of standing it up. That overhead is paid once
per session regardless of how much you do once it's running, which is the whole
argument for batching: the marginal question is nearly free, the first one is not.

The failure mode this file prevents: wire it all up, do the one thing you sat down
for, tear down — and only afterwards notice that four other questions needed the
same board and could have ridden along in the same session.

Everything here is board-*required*. Anything that could be answered at a desk has
already been answered at a desk — the items below are exactly the residue.

Board facts already measured (LMA, boot hartid, timebase, UART shift, no `sstc`)
live in [../plans/visionfive2-port.md](../plans/visionfive2-port.md) under *Ground
truth*. Don't re-measure them; do trust them.

---

## 0. Before you plug anything in

**Build a fresh image.** `cargo xtask image`.

This is not ceremony. A VF2 "regression" is a missed `cargo xtask image` until
proven otherwise — the board boots whatever is in the TFTP root, which is
whatever you built last time, and a stale image reproduces a bug you already
fixed with total conviction. Confirm the image's timestamp before believing any
result in this document.

**Have the CP2102 adapter and the device path ready.** On macOS the device is
`/dev/cu.*`, **never** `/dev/tty.*` — `tty.*` blocks in `open()` until a carrier
detect that a USB-TTL adapter never asserts, so it hangs forever rather than
erroring. The collector already refuses `tty.*` and names the `cu.*` alternative
(`collector::serial::call_out_alternative`); if you see that message, it is
working as designed.

**Check nothing else holds the port.** A stray `screen` from a previous session
is exclusive and will make a working setup look dead.

---

## 1. Provision zero-touch netboot — ✅ DONE 2026-08-27

**This item used to say "written down, never run". That was false**, and finding
out cost the first hour of the 2026-08-27 session. The environment was already
provisioned, in a *better* form than this file recorded — and both parser traps
warned about below had already been avoided.

The saved environment, as `printenv` reports it:

```
bootcmd=dhcp; setenv serverip 192.168.0.7; tftpboot 0x40200000 snitchos.img; cp.b ${fdtcontroladdr} 0x46000000 0x100000; booti 0x40200000 - 0x46000000
bootdelay=2
serverip=192.168.0.7
```

Verified end to end: `run bootcmd` → DHCP → TFTP (1778208 bytes, an exact match for
the local image) → `booti` → banner, 4 U74 harts, 85+ heartbeats. `saveenv`
persisted. **`cargo xtask image` → reset → fresh kernel now needs no typing.**

Three U-Boot facts that cost time and are worth keeping:

- **`dhcp` overwrites `serverip`** with the DHCP server (the router). That is why
  the saved `bootcmd` orders it `dhcp` *then* `setenv serverip` — the sequence is
  load-bearing, not stylistic.
- **`dhcp` does not set `ipaddr`** in this U-Boot (2025.10, lwIP-based); it binds
  the lease internally, so a stale `ipaddr` never refreshes and must be *deleted*.
  It is cosmetic — `our IP address is <NULL>` still transfers fine. It looked like
  the fault and was not.
- **`md.l <addr> 16` dumps 22 words** — U-Boot parses the count as hex.

### ⚠ What can still break it: `serverip` drift

The one hazard that remains, and it is not hypothetical — it *did* break, silently,
between sessions. `serverip` is hardcoded in the saved `bootcmd`, so it is only
correct while the Mac actually holds that address.

**Fixed 2026-08-28.** Private Wi-Fi Address is set to **Fixed** — a
locally-administered MAC that is *stable per SSID* (`96:5f:c7:25:91:03` here) — and
the router's reservation is pinned to that. Verified: `en0` holds `192.168.0.7/24`,
matching the saved `serverip`, and it survives rejoins without exposing the
permanent MAC.

The distinction is **Fixed vs Rotating**, not private vs not: a locally-administered
MAC in `ifconfig` is the expected steady state here, not a warning sign. Rotation
was what broke the original setup; Fixed removes it.

Still true, and the reason to keep checking: the MAC is **per-SSID**, and `serverip`
is hardcoded in the saved `bootcmd`, so a *different network* still drifts.

**Before booting, check `ipconfig getifaddr en0` agrees with the board's saved
`serverip`.** If it does not, the fix that needs no router and no working UART:

```
sudo ifconfig en0 alias 192.168.0.7 255.255.255.255
```

Make the Mac answer where the board already looks, rather than editing `bootcmd`.

**How the drift presents**, so it is recognisable rather than mysterious: TFTP fails
with repeated `ICMP destination unreachable (port unreachable)`. That message is
itself evidence the Mac *received* the datagram and had no listener — so it also
catches the second failure this session hit, a `dnsmasq` left running since 11 Aug
bound to addresses that no longer existed. `sudo pkill dnsmasq` and restart it.

Full context: [../plans/visionfive2-port.md](../plans/visionfive2-port.md),
*Making netboot zero-touch*.

---

## 2. Verify uart step 10b — the blocking item

**This is the one that closes a milestone.** Everything in
[../plans/uart-telemetry.md](../plans/uart-telemetry.md) is landed and
gate-green; the only outstanding criterion is about a board, and no board has been
involved. Until it passes, **B3 and M2 are not done** — and M2 is the milestone
the port plan calls "the milestone that makes the port *mean* something."

```
cargo xtask reader --serial /dev/cu.<adapter> --baud 115200
```

**Success:** the board's live frame stream decodes; spans and metrics reach
Grafana.

**What is actually untested is one line** — the `serialport::open` call. Everything
above it is covered by a scripted mock. So expect failures no unit test could
reach, and distinguish them, because they look alike:

| Symptom | Likely cause |
|---|---|
| Immediate error naming `cu.*` | you passed a `tty.*` path — working as designed |
| Port busy | a stray `screen`/`cu` holds it |
| Garbage rather than silence | baud mismatch (115200 is the measured choice from step 6) |
| Clean "end of stream" mid-session | **see below** |

**The trap worth knowing in advance:** `decode_stream` ends the session on both
`Err` *and* `Ok(0)`. A serial port with a read timeout returns `TimedOut` during a
quiet gap between heartbeats — so **a silent board looks exactly like a clean
end-of-stream**, and the collector exits mid-session. If you see the reader exit
cleanly while the board is plainly still running, that is this, not a dead board.

**If it passes:** mark step 10b verified, B3 done, M2 achieved. Then step 5b (the
interactive relay) becomes the next uart item.

---

## 3. First light for the board bridge — it is now a runnable command

**Updated 2026-08-27**: this item used to say "steps 1–2 are done". Steps 1–4 are
now code-complete and gate-green (`reach`, `stop`, `split`, `script`, `wire`,
`outcome`, and a `cargo xtask board exec` CLI). None of it has touched a board.
Everything from step 4b onward — interrupting the autoboot countdown — has **no
host-side oracle at all**, so this session is the gate on all of it.

### 3a. The one-command first light

Nothing unbuilt is required. Empty input, quiescence stop — a pure read:

```
cargo xtask board exec "" --device /dev/cu.<adapter> --until-quiet 500
```

Power-cycle the board and it captures the boot log. That single command exercises
`check_device`, `serialport::open`, the read loop, the text/frame split, and the
exit code in one shot.

**Success:** the boot log on stdout, decoded frames on stderr, exit `0`.

### 3b. Run it *before* item 2, and here is why

Items 2 and 3 put opposite idle-handling disciplines on the same physical setup,
which makes them a natural A/B rather than two chores:

| | idle read (`TimedOut` / `Ok(0)`) | so a quiet board… |
|---|---|---|
| `cargo xtask reader --serial` (item 2) | absorbed by `SerialReader` | keeps the session alive |
| `cargo xtask board exec` (item 3) | **seen**, advances the quiescence clock | is the point |

If item 2's clean-EOF trap fires but item 3 works, the fault is in the collector's
idle handling. If **both** fail, the problem is lower down — the port, the baud, the
cable — and you have saved yourself debugging the wrong layer. Running the cheaper,
more-instrumented one first is the better experiment.

### 3c. Provoke each failure deliberately — the exit codes are the interface

The taxonomy's whole claim is that these need *different actions*, so check the
codes actually discriminate rather than all meaning "it didn't work":

| Provoke | Expect | Exit |
|---|---|---|
| Point at `/dev/tty.*` | `CallInDevice`, naming the `cu.*` path, **instantly** | 2 |
| Hold the port with `screen` | `PortHeld`, naming the holder's pid via `lsof` | 2 |
| Point at a nonexistent path | `NoSuchDevice` | 2 |
| Board running, `--until "NEVER_APPEARS"` | reached it, marker never came | **1** |
| Board running, `--timeout 2000` only | capture ran its window | **0** |

The 1-vs-2 split is the one that matters: an unattended loop reboots the board on
`1` and fixes the *host* on `2`. If they collapse, the loop chases the wrong fix
every iteration.

### 3d. ⚠ The hypothesis only hardware can settle

**Unplug the adapter mid-capture.** Expect `Unreachable` and exit `2`, from
`wire::Ended::TransportFailed`.

This is a genuine guess. The code assumes a yanked USB-serial adapter makes
`read()` return an **error**. If `serialport` instead returns `Ok(0)` forever, that
path reads as *idle* — the capture spins to its deadline and reports `Timeout`
(exit `1`, "the board went silent") for what was actually a dead cable (exit `2`,
"fix the host"). That is precisely the conflation the `Ended` split was introduced
to prevent, so it failing here would mean the split is right and its *input* is
wrong.

Nothing at a desk can answer this: it depends on what the macOS driver does to a
blocking read when the device node vanishes. **If it reports `Timeout`, the fix is
in `wire::capture`** — treat a repeated zero-length read on a port that once had
bytes as transport death, not idleness.

### 3e. Tune the quiescence window against real timing

`--until-quiet` is the one constant no mock got to choose: the gaps between real
heartbeats are a property of the board. Try 200 ms, 500 ms, 1 s and note which
first stops fragmenting a boot log into pieces. Record the answer — Phase 2 needs
it, because WiFi jitter will widen whatever serial wants.

---

## 4. GMAC Phase 0 — boot the probe, then three DTB questions

From [../plans/vf2-gmac-driver.md](../plans/vf2-gmac-driver.md). The desk half of
Phase 0 is done ([vf2-gmac-design.md](vf2-gmac-design.md) settles the register map,
GMAC1-over-GMAC0, no RX ring, and IO-coherency), and **most of what used to be five
manual checks is now one boot**: `workload=gmac-probe` reads the GMAC1, SYSCRG and
`sys_syscon` registers and reports each over the UART.

### 4a. Run the probe

```
setenv bootargs 'workload=gmac-probe'
```

then reset. (Until board-bridge step 6b lands, the workload rides `bootargs` and is
typed by hand — `cargo xtask image` prints the line to retype. A bare reset picks up
a fresh *image* but the *old* bootargs.)

**Once step 6b lands this whole item collapses to one command** —
`cargo xtask board boot --workload gmac-probe` — and could be run unattended.
If you drive it from the bridge, `gmac-probe: done` is the natural stop marker, and
its *absence* after a trailing `read …` line is the hang.

**Read the version line first.** The dump opens with

```
gmac-probe: core version 0x52 — expected dwmac-5.20; the rest of this dump can be believed
```

and if it says `NOT dwmac-5.20`, stop reading the rest — the base or the offset is
wrong and every later word is noise. The `0x0110` version offset is transcribed from
mainline and **has never been confirmed against a datasheet**, so a mismatch is at
least as likely to be our transcription as the board's state.

**⚠ This code has never run anywhere.** Nothing answers at `0x1604_0000` under QEMU
or snemu, so unlike every other kernel path there is no emulated counterpart and no
itest — its first execution is its first test. Two failure shapes to expect:

| What you see | What it means |
|---|---|
| `read gmac1.version @ 0x16040110` and then **nothing** | Bus hang reading a gated peripheral. That is itself the answer to "is the clock ungated / megapage mapped" — and it is why the breadcrumb precedes the read. |
| `version = 0x00000000` or `0xffffffff` | Reset still asserted, or wrong base. Both are refused by the self-check, so the dump will say so. |

What it answers without any further typing: whether the megapage needs its own
`insert` (old item 3), whether U-Boot ungated the clocks and released the resets,
the `phy_intf_sel` field, the MDC divider in the MDIO CSR field, the station MAC
address, and whether U-Boot left a TX descriptor ring behind (old item 5).

The probe reads the **device tree first**, before any MMIO — none of it can hang the
bus, and its answers say whether the register reads are even worth attempting. So the
dump opens with:

```
gmac-probe: dtb dma-noncoherent=false (true means STOP — no Zicbom on this core)
gmac-probe: dtb /soc/ethernet@16040000 status=okay phy-mode=rgmii-id phy-handle=yes reset-gpios=no
```

**`dma-noncoherent=true` ends the session's GMAC work.** The whole driver estimate
rests on this being false; if the board disagrees with mainline, the plan needs
re-scoping around a cache-maintenance layer the kernel has no primitives for.

### 4b. Ground-truth the descriptor layout from U-Boot's own ring

**Done at the U-Boot prompt** — either before you boot the probe, or on any later
return to it; you are there anyway for item 1. U-Boot has a *working*
GMAC driver for this exact board, so after any network operation its descriptor ring
is real, correctly-encoded silicon-accepted data — which is the only independent
check available on a descriptor layout otherwise transcribed from mainline headers
by hand.

```
dhcp                          # or any tftp op, so the ring is live
md.l 0x16040110 1             # version — expect low byte 0x52
md.l 0x16041114 1             # DMA_CHAN_TX_BASE_ADDR: the ring's physical base
md.l <that value> 16          # the first four descriptors, 4 words each
```

Read the descriptors against `kernel_devices::gmac`'s TDES encoding: word 0/1 are the
buffer address low/high, word 2 has the buffer size in bits `[13:0]`, word 3 has
`OWN`(31) `FD`(29) `LD`(28) and the frame length in `[14:0]`. **A mismatch here
invalidates desk work rather than costing a debugging session** — which is the whole
point of taking the reading before writing more of the driver.

`md.l 0x16040110 1` also answers the version question without booting our kernel at
all, so it is a free cross-check on the probe: if U-Boot's `md` says `0x52` and the
probe says otherwise, the fault is ours, not the board's.

### 4c. Still manual — one question

**Which physical RJ45 is GMAC1?** Plug one jack and see which PHY links, or read
U-Boot's `ethact`. The DTB report says which *nodes* are enabled, not which *socket*
is which — that mapping isn't in the device tree.

---

## Why this order

Item 1 makes 2–4 cheap, and 3 is cycle-heavy. Item 2 is the only one that closes a
milestone, and it is a single boot. Items 3 and 4 are both "validate assumptions
before building more on them," and both are cheap *per question* but only
answerable here.

Item 4 got much cheaper since this file was written — it used to be five manual
checks and is now one boot plus three DTB greps. That is the probe doing its job:
it exists to move questions off the board, and it did so before ever running on
one. Which also means its own correctness is now the thing riding on this session.

**Run §3a before item 2** (updated 2026-08-27). It is one command and about thirty
seconds, and it makes item 2's result *interpretable*: the two put opposite
idle-handling disciplines on the same cable, so if item 2 dies at a quiet gap you
immediately know whether that is the collector's `SerialReader` behaviour or the
port itself. Item 2 still closes the milestone; §3a just stops you debugging the
wrong layer to get there.

If the session is cut short, the priority is **1, then §3a, then 2**. Item 1 pays
for every future session; §3a is nearly free and de-risks item 2; item 2 unblocks
the port's headline milestone.

## What to capture before you unplug

Board sessions end abruptly. Capture enough that the following desk session does
not need the board again:

- **The console transcript**, whole. Not the interesting part — the whole thing.
- **The `gmac-probe` dump specifically**, even if it looks like a hang. A dump that
  stops after one breadcrumb is a *result*, and the last line names the register
  that stopped it.
- **The live DTB**, so any *further* device-tree question never needs the board twice.
  The probe already reports the two that Phase 0 asked; this is insurance against the
  next one.
- **`printenv`** in full, once provisioned.
- **The exact device path and baud** that worked.
- **Which RJ45 linked**, and its PHY.
- **The `--until-quiet` value that stopped fragmenting the boot log** (§3e). It is
  the one constant no host test could choose, and Phase 2 needs it as its starting
  point before WiFi jitter widens it.
- **What `read()` returned when you unplugged the adapter mid-capture** (§3d) —
  error or `Ok(0)`. One bit, and it decides whether `wire::capture` needs a fix.
  Note it even if the answer is the boring one; "confirmed as assumed" is a result
  that stops the question being asked again.

Then fold the answers back into the plans they came from, and re-date those
`**Status (YYYY-MM-DD)**:` headers — `cargo xtask plan-status` prints them
oldest-first, so a session's worth of updates is visible immediately.
