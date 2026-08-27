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

## 1. Provision zero-touch netboot (one U-Boot session)

⏳ **Written down, never run.** Nobody has typed this at the board — do not assume
the environment is provisioned. A downstream plan once assumed it was, and the
wrong assumption propagated.

Do this **first**, before anything else, because it makes every later item in this
list cheap: after it, `cargo xtask image` → reset → fresh kernel, with no typing.
Items 3 and 4 below are many flash-reset cycles each, and they are the reason this
goes first rather than last.

With the Mac's IP pinned by DHCP reservation (`192.168.0.7`):

```
setenv serverip 192.168.0.7
setenv bootdelay 3
setenv bootcmd 'dhcp; tftpboot 0x40200000 snitchos.img; booti 0x40200000 - ${fdtcontroladdr}'
saveenv
```

**Then verify with `printenv bootcmd` before trusting it.** Two parser-dependent
things fail silently here, and both look like a successful `setenv` right up until
the next reset:

- **`${fdtcontroladdr}` must be stored literally, not expanded.** It resolves at
  boot time, not at `setenv` time. If `printenv` shows an address where the
  literal should be, the quotes did not protect it — re-set with
  `\${fdtcontroladdr}`.
- **The semicolons must survive as one string.** Whether `'…'` groups the chain
  depends on whether this U-Boot was built with the hush parser or the simple one.
  `printenv` shows whether the whole chain landed in one variable.

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

## 3. Reality-check the board bridge's host logic

[../plans/board-bridge.md](../plans/board-bridge.md) steps 1–2 are done and
**neither has touched a board**. Both are guesses about how real hardware
misbehaves, and both are about to have steps 3–9 built on top of them. Validating
them costs one session; discovering they are wrong after step 6 costs a rewrite.

- **`reach.rs` — the failure taxonomy.** Its whole purpose is that "could not
  reach the board" must never look like "reached it, and it said nothing." Provoke
  each variant deliberately: unplug the adapter, hold the port with `screen`, point
  it at a wrong path, and boot a kernel that reaches the prompt but emits nothing.
  Does each produce the variant that tells you what to *do* differently? That is
  the axis the module claims to partition on.
- **`stop.rs` — the stop-condition evaluator.** Exercise all three against a live
  stream: a quiescence window, a marker match, a timeout. The quiescence window is
  the one most likely to be mistuned against real timing, because the gaps between
  real heartbeats are the thing no mock got to choose.

Note the interaction with item 2: a silent board reading as EOF is a *collector*
behaviour, and the bridge sits on the same serial handle. Check the bridge does
not inherit it.

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
`cargo xtask board boot --workload gmac-probe` — and at that point the item stops
being an instruction to a human and becomes a script, or something an agent runs
unattended. That is worth designing *for* rather than discovering later, so:

> **The probe's output lines are a parse contract, not prose.** Every register
> report is `gmac-probe: <region>.<label> = 0x<8 hex digits>`, every access is
> preceded by `gmac-probe: read <region>.<label> @ 0x<addr>`, and the run is
> bracketed by `gmac-probe: start` / `gmac-probe: done`. A missing `done` with a
> trailing `read` line is the hang signature. Reword those lines and you break
> whatever the bridge asserts on — the verdict sentences after them are free text
> and safe to edit, the `key = value` lines are not.

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

### 4b. Still manual — three DTB questions

The probe reads MMIO, not the device tree, so these stay hand-work this session:

1. **Is `dma-noncoherent` absent from the board's *live* DTB?** One grep. Mainline
   says it is; confirm on the actual board. **If present, stop and re-scope** — the
   U74 has no `Zicbom`, so a cache-maintenance layer would be a different plan.
2. **Which physical RJ45 is GMAC1?** Plug one jack and see which PHY links, or read
   U-Boot's `ethact`.
3. **Is there a PHY reset GPIO?** Not in the mainline VF2 DTS; check the board's
   own DTB.

Capturing the live DTB (see below) answers 1 and 3 at the desk afterwards, so they
never need the board twice.

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

If the session is cut short, the priority is **1, then 2**. Item 1 pays for every
future session; item 2 unblocks the port's headline milestone.

## What to capture before you unplug

Board sessions end abruptly. Capture enough that the following desk session does
not need the board again:

- **The console transcript**, whole. Not the interesting part — the whole thing.
- **The `gmac-probe` dump specifically**, even if it looks like a hang. A dump that
  stops after one breadcrumb is a *result*, and the last line names the register
  that stopped it.
- **The live DTB**, for items 4b.1 and 4b.3, so DTB questions never need the board twice.
- **`printenv`** in full, once provisioned.
- **The exact device path and baud** that worked.
- **Which RJ45 linked**, and its PHY.

Then fold the answers back into the plans they came from, and re-date those
`**Status (YYYY-MM-DD)**:` headers — `cargo xtask plan-status` prints them
oldest-first, so a session's worth of updates is visible immediately.
