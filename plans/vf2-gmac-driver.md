# Plan: the JH7110 GMAC driver — `NetDevice` on real hardware

**Status**: 📐 **NOT STARTED.** Phase 0's *desk* half is now done — see the design
note — which closes three open questions, retires a risk nobody had asked about
(DMA coherency), and replaces the step 5–7 sketches with a six-rung tracer-bullet
ladder. What remains of Phase 0 is board-side and cheap.
**Design**: [docs/vf2-gmac-design.md](../docs/vf2-gmac-design.md) — the register map,
the GMAC1-over-GMAC0 decision, the module boundary, the ladder, and the failure-mode
table. Read it before this plan. Its parent is
[docs/network-telemetry-design.md](../docs/network-telemetry-design.md) (Decision 3's
JH7110 row, and open questions 4–5).
**Owes its existence to**: [network-telemetry.md](network-telemetry.md) PR 8, which
says to write this file once PR 7 landed. PR 7 landed; this is that file.
**Milestone**: completes M2.5 in [visionfive2-port.md](visionfive2-port.md)

## Goal

A `NetDevice` impl for the VisionFive 2's Synopsys DesignWare GMAC, so that booting
the board with `net=…` streams telemetry to the collector over Ethernet.

## The honest sizing

**This is the monster.** The design note's own table rates its reuse below the trait
as "**None**" and its cost as "**the monster — weeks**", against "small–moderate" for
virtio-net. The port plan calls getting the first frame out "bigger than the rest of
the port combined." Nothing in this plan should be read as talking that down.

What *is* different from a from-scratch NIC, and worth holding onto so the work does
not get over-feared:

- **Everything above the trait is already proven.** `kernel-net`'s packet layer,
  `UdpFrameSink`, the `net=` bootarg, the collector's `--udp` source, and a
  deterministic itest all shipped in PRs 1–7 and are exercised on every gate run. This
  is a driver swap behind a working interface, not a rewrite.
- **DMA-descriptor discipline is not new here.** The kernel already knows devices see
  physical addresses, and has the `TX_STAGING` + `mmu::va_to_pa` staging pattern and
  the linear map to hand a device a PA. The NIC is the most DMA-heavy device this
  codebase would have written — not the first.
- **The JH7110 register-model pattern is established and host-tested.**
  `kernel-devices/src/syscrg.rs` (clock gates, dividers, the assert/release/poll reset
  model) and `kernel-devices/src/iomux.rs` (pad routing) already exist, transcribed
  from mainline, with `kernel/src/device/pwmdac.rs` as the thin `unsafe` glue that
  applies their pure `Op`s. The GMAC's clock and reset bring-up is **the same shape
  with different indices**, and `syscrg.rs`'s own doc comment already anticipates
  other consumers.

So the risk is concentrated in exactly two places: the **DMA rings** and the
**PHY/MDIO bring-up**. The design note names the latter "the classic bare-metal NIC
time sink, and the one genuinely unknown-shaped part of the whole design."

## Prerequisites

1. **[board-bridge.md](board-bridge.md), both phases — build these first.** This is
   step 3 of that plan's ladder, and the sequencing was a deliberate decision
   (2026-08-25), not an accident of what got written when. Two reasons:
   - **The loop.** This is weeks of flash → reset → read breadcrumbs → edit on a
     peripheral with many silent failure modes (PHY never links, wrong RGMII delay,
     MDIO reads garbage, ring misconfigured). Doing that by hand, power-cycling a
     board, is the expensive way to spend those weeks. Phase 2 additionally makes it
     *unattended-overnight* capable.
   - **An independent diagnostic channel.** The thing being debugged here *is* the
     network path. Having telemetry arrive over a different physical link — UART
     pins, a separate radio — is what lets a broken `net=` boot still explain itself.
     If the only channel were the one under construction, every failure would look
     the same: silence.

   **What the bridge does not do is shrink this plan.** It retires zero lines of the
   driver below. It is tooling, and the estimate below already assumes it exists.
2. **M2 UART telemetry (shipped)** — the breadcrumb channel. Everything this driver
   reports during bring-up arrives over the serial line, because the thing being
   debugged *is* the other channel.
3. **`cargo xtask image` → TFTP netboot (shipped)** — a reset is the whole re-flash.

## Phase 0 — reconnaissance

**The desk half is done**: [docs/vf2-gmac-design.md](../docs/vf2-gmac-design.md) has
the DTB inventory, the clock/reset indices, the full register map, the descriptor
layout, and the reference sequences. It also settles three things this section used to
ask:

- **Which MAC** — *both* are wired on v1.3B and both are YT8531/`rgmii-id`, so the
  choice is free and is made on cost: **GMAC1**, because it hangs off SYSCRG (the
  controller `syscrg.rs` already models) rather than AONCRG, and its PHY-mode syscon
  sits in a megapage SYSCRG already needs.
- **RX ring** (open question 4) — **not needed.** TX and RX are separately configured
  and separately started in dwmac4; program only the TX half.
- **DMA coherency** — a risk this plan never named. **JH7110 is IO-coherent**; no
  cache-maintenance layer is in scope. Had it gone the other way this would be a
  different plan, because the U74 has no `Zicbom`.

**The method for the board half is a probe workload, and it runs first** — before the
board-bridge, before step 1. `workload=gmac-probe`: a read-only boot that dumps what
U-Boot already configured and stops. See the design note's "Rung −1".

The board is delivered over TFTP, so U-Boot brought a GMAC and PHY up and moved
megabytes over them seconds before `booti` — a working configuration for this exact
board is sitting in the register file, and it answers three of the four open questions
below as *measured values* rather than derivations.

That state is destroyed by step 5's first reset assert, but re-established by every
TFTP boot — so this is a constraint on **where the reads live** (their own read-only
workload, never a preamble bolted onto bring-up) rather than a deadline.

It needs no bridge — one human-attended boot — so it is off the prerequisite chain
entirely, and it can be built **in parallel with the bridge**. The two improve each
other: bridge step 6b (`board boot --workload X`) is what makes the probe zero-touch,
and the probe is the only workload that can legitimately hang the board, which is what
the bridge's hang watchdog needs to be tested against.

What it must answer, and each item is minutes not days:

- **Confirm `dma-noncoherent` is absent from the board's own live DTB**, not just from
  mainline. One grep. If present, stop and re-scope.
- **Which physical RJ45 is GMAC1** — plug one jack, see which PHY links; or read
  U-Boot's `ethact`.
- **The megapage question.** `0x1600_0000` is expected to need its own `insert`.
  Two-line answer, needed before the first MMIO read.
- **Is there a PHY reset GPIO?** Not in the mainline VF2 DTS; check the board's DTB.
- **Does U-Boot leave anything usable behind?** Worth *measuring*, worth nothing to
  plan around — the display work already found inheriting U-Boot's framebuffer was a
  dead end ([vf2-display.md](vf2-display.md)). Its *source*, though, is a known-good
  sequence for this exact board.

**Done when**: the five above are answered against the live board and any correction to
the design note is folded back into it.

## Acceptance Criteria

- [ ] SYSCRG clock + reset bring-up for the GMAC is a pure, host-tested `Op` sequence,
      in the same shape as `syscrg::pwmdac_bringup`.
- [ ] TX DMA descriptor-ring logic is host-tested over a mock MMIO transport:
      descriptor layout, ownership bits, ring index advance and wrap.
- [ ] MDIO read/write register sequencing is host-tested, including the busy-poll
      timeout path (a PHY that never answers must return an error, not spin forever).
- [ ] The PHY reports link-up on the board, observable as a telemetry breadcrumb over
      the UART.
- [ ] One valid UDP datagram from the board is captured by `tcpdump` on the host.
- [ ] Booting the board with `net=…` streams decodable telemetry into the collector,
      and thence to Grafana.
- [ ] The gate is unchanged: `cargo xtask test && cargo xtask itest && cargo xtask
      itest --scramble`, with the QEMU/snemu virtio-net path untouched.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. Steps 1–4 are pure host
logic — no board, no MMIO — and are where most of the *code* lives. Steps 5–10 are the
tracer-bullet ladder on the board, where most of the *time* lives; each is one
observable bit, per the port's "cheapest possible board increment" discipline.

The board should be met with code already believed correct — every pure layer TDD'd and
mutation-tested against a `FakeGmac` double before it is powered on — so that a failure
is information about hardware rather than about us.

---

### Step 1: GMAC clock + reset bring-up as a pure `Op` sequence

**Acceptance criteria**: `syscrg::gmac1_bringup(...)` returns the exact ordered
sequence of clock-gate, divider, and reset assert/release operations for GMAC1,
matching the mainline sequence, with reset release expressed as
"poll status until the bit reads 1" — the model `syscrg.rs` already encodes.

Indices are known (design note): clocks `GMAC1_AHB` 97, `GMAC1_AXI` 98, `GMAC1_PTP`
102, `GMAC1_TX_INV` 106, `GMAC1_GTXC` 107; resets `GMAC1_AXI` 66, `GMAC1_AHB` 67.
**No AONCRG module** — that is GMAC0's cost, and GMAC0 is not on this path.

**RED**: a golden-sequence test asserting the ops in order against the transcribed
mainline sequence, plus a test that reset release polls status rather than assuming.
**GREEN**: the new indices + the sequence builder in `kernel-devices/src/syscrg.rs`.
**MUTATE**: `cargo xtask mutants -p kernel-devices`. Order and bit indices are targets.
**KILL MUTANTS**: a mutant that reorders release-before-gate must fail a test.
**REFACTOR**: assess whether `pwmdac_bringup` and `gmac_bringup` share a shape worth
extracting — two consumers is the threshold, not one.
**Done when**: golden sequence passes, mutation reviewed, gate green, approved.

---

### Step 2: RGMII pad / pinmux configuration (pure)

**Acceptance criteria**: the pad routing and any RGMII clock/delay configuration the
board needs is a pure `FieldWrite` sequence in the `iomux::route_output` shape,
asserted against the board DTS's pin group.

**Note**: this step is **very likely empty.** No pinctrl group for the GMAC pads
appears in the board DTS. What exists instead is the `sys_syscon` PHY-mode field
(offset `0x90`, shift `2`, 3-bit `phy_intf_sel`) and the `GMAC1_TX_INV` clock
selection — both of which belong to step 1. Confirm against the board's own DTB, then
**retire this step with a note rather than inventing work for it.**

**RED/GREEN/MUTATE/KILL/REFACTOR**: as step 1.
**Done when**: the sequence matches the DTS, or the step is retired with a note.

---

### Step 3: TX DMA descriptor ring (pure, over a mock transport)

The largest genuinely-new chunk of logic, and it is fully host-testable.

**Acceptance criteria**:
- Building a descriptor for a frame produces the correct layout: buffer physical
  address, length fields, first/last segment flags, and the **ownership bit set last**
  (the device must never see a half-written descriptor).
- Submitting a frame advances the ring index by one; submitting `N = ring_len + 1`
  frames wraps correctly and reuses only descriptors the device has returned.
- A full ring returns `TxFull` — never overwrites an unreclaimed descriptor.
- Reclaim: descriptors the device has released are returned to the free pool.

**RED**: tests over a mock MMIO transport (`kernel-devices`'s existing
`MmioTransport` mock is the precedent) asserting descriptor bytes, the index advance,
the wrap, and the full-ring refusal.
**GREEN**: the ring in `kernel-devices` — pure `core`, no alloc, no MMIO.
**MUTATE**: `cargo xtask mutants -p kernel-devices`. The wrap arithmetic, the
ownership-bit ordering, and the full-ring comparison are the targets.
**KILL MUTANTS**: a mutant that sets the ownership bit before the length must fail —
that is a real, hard-to-debug hardware race, and the test is what makes it a
compile-time-visible mistake instead of a two-day board hunt.
**REFACTOR**: assess what, if anything, is shared with the virtqueue layer. Probably
nothing — different ring discipline — and forcing a shared abstraction over two
dissimilar rings would be worse than two clear ones.
**Done when**: all four behaviours tested, mutation reviewed, gate green, approved.

---

### Step 4: MDIO transaction sequencing (pure)

**Acceptance criteria**: an MDIO read builds the correct address/control register
write and extracts the data field from the result; a write does likewise. The
busy-poll has a **bounded** iteration count and returns an error when the PHY never
clears busy — it does not spin forever.

**The bound is not optional.** An unbounded poll against an unresponsive peripheral is
exactly the `PollUntilSet` hang that motivated the whole watchdog design
([docs/board-agent-bridge-design.md](../docs/board-agent-bridge-design.md)), and MDIO
against a PHY that is held in reset is the most likely place to meet it on this
device. Bound it here, in the pure layer, where a test can prove it.

**RED**: register-sequence tests over the mock; a "busy never clears" test asserting
the error return within N iterations.
**GREEN**: the sequencer in `kernel-devices`.
**MUTATE**: the bound comparison and the data-field mask/shift are targets.
**KILL MUTANTS**: a mutant that removes the bound must fail (the test must not merely
time out — it must assert the error value).
**REFACTOR**: assess.
**Done when**: sequences and the timeout path pass, mutation reviewed, gate green,
approved.

---

### Steps 5–10: the tracer-bullet ladder

The design note's ladder, one rung per step. Each is one observable bit with an oracle
that does not depend on the rungs above it being right — that independence is the whole
point, and it is why this is six steps rather than three. Full table, including what
each rung *cannot* prove and the failure-mode lookup, is in
[docs/vf2-gmac-design.md](../docs/vf2-gmac-design.md).

Breadcrumbs are **telemetry `Frame`s over the M2 UART channel**, not ad-hoc `println!` —
so the bring-up log is decodable, diffable between attempts, and assertable by the
board-bridge unattended.

| Step | Rung | Observable |
|---|---|---|
| 5 | **T0 — the MAC answers.** Clock ungate + reset release, then read a register with a known reset value. | breadcrumb with the raw word |
| 6 | **T1 — the PHY answers.** MDIO read of PHY ID1/ID2; YT8531's OUI is a known constant. | breadcrumb with both words |
| 7 | **T2 — link up.** Advertise **100-full** (not gigabit — see the design note), restart autoneg, poll BMSR. | breadcrumb `link up 100/full` |
| 8 | **T3 — the DMA engine moves.** One descriptor, contents deliberately irrelevant, poll until the MAC clears `OWN`. | breadcrumb `own cleared, tdes3=…` |
| 9 | **T4 — bytes leave the board.** One broadcast frame, custom ethertype. Oracle is `tcpdump -XX`, **not** the collector. | host capture |
| 10 | **T5/T6 — `net=` end to end.** One decodable `Frame`, then sustained heartbeat-paced telemetry. | collector, then Grafana |

Step 8 is the one most easily skipped and the most valuable: `OWN` clearing proves the
engine read the descriptor *and* fetched the buffer, validating the entire address-
translation story with a frame that does not have to be correct. Folded into step 9,
"nothing on tcpdump" acquires a dozen causes instead of three.

**Optional, and only after step 9: a snemu dwmac device model** as a deterministic
regression guard. Not before — it would prove the driver matches *our model of* the
hardware, which is the document that produced the driver.

---

## Open questions

Closed by the design note: **RX ring necessity** (none — TX and RX start separately);
**interrupt vs poll** (poll; no PLIC route); **which MAC / does the second one matter**
(both are wired and identical on v1.3B, so the choice is free and GMAC1 wins on cost);
and **static MAC vs hardware MAC** (bootarg wins, matching the existing config story —
a deliberate choice, not an oversight). One risk was added and retired: **DMA
coherency**.

Genuinely still open:

1. **The MDIO CSR clock-range constant** — depends on the AHB (`pclk`) rate, hence on
   what U-Boot left the clock tree in. Bounded: surfaces at step 6, found by trying
   values.
2. **YT8531 delay configuration** — `rgmii-id` puts the delay in the PHY, but the VF2
   vendor tree carries `motorcomm,*` delay and clock-inversion properties. Which this
   board needs is the genuinely unknown-shaped part; it surfaces between steps 7 and 9.
3. **PHY reset GPIO** — absent from mainline's VF2 DTS. Resolve against the board's DTB
   in Phase 0.
4. **Ring depth.** Start at 8. A tuning knob, not a design decision — `TxFull` is
   already the trait's contract.

## Pre-PR Quality Gate

Before each commit:

1. Mutation testing — `cargo xtask mutants -p kernel-devices` (kernel/ excluded; its
   coverage is the board).
2. Refactoring assessment — run the `refactoring` skill.
3. `cargo xtask clippy` (host + riscv; never blanket `--fix` the kernel —
   `deref_addrof`).
4. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
5. `cargo xtask links` if any `.md` moved or gained links.

## Notes

- **Do not regress the virtio-net path.** QEMU/snemu keep working; this is a third
  `NetDevice` impl beside them, and the deterministic `net-telemetry-over-udp` scenario
  stays the gate for everything above the trait.
- **A stale board image is the first hypothesis for any hardware "regression."** Output
  that does not match source means a missed `cargo xtask image` until proven otherwise.
- **The board image is a release build.** Every codegen-sensitive lesson the port has
  already paid for applies here — no cached address or stack frame spanning the
  trampoline, and no assuming debug behaviour carries over.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md's override of the
planning skill's delete step) and merge any learnings via the `learn`/`adr` agents.*
