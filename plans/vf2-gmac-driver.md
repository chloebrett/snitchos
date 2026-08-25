# Plan: the JH7110 GMAC driver — `NetDevice` on real hardware

**Branch**: main (this repo works directly on main; the user commits)
**Status**: 📐 **NOT STARTED — scoping plan.** Phase 0 and steps 1–4 are sized and
TDD-decomposed; steps 5–7 are deliberately left as sketches, because they depend on
what Phase 0 measures. Per the planning skill: do not write steps you cannot yet size.
**Design**: [docs/network-telemetry-design.md](../docs/network-telemetry-design.md)
(Decision 3's JH7110 row, and open questions 4–5)
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

## Phase 0 — reconnaissance, before any code

The unknowns are the schedule. Answer these first; several can invalidate later steps.

- **DTB inventory.** The `ethernet@16030000` / `ethernet@16040000` nodes: MMIO base
  and size, interrupt numbers, `clocks`/`resets` phandles and their indices, `phy-mode`
  (expect `rgmii-id`), `phy-handle` and the PHY's MDIO address, and any
  `starfive,tx-use-rgmii-clk` style vendor properties. Source of truth is the board's
  own DTB plus mainline `jh7110.dtsi` / `jh7110-visionfive-2.dtsi`.
- **Which MAC.** The JH7110 has two GMACs; the VF2's RJ45 is wired to one of them.
  Confirm which, and do not assume the first node is the connected one.
- **The megapage question.** Is the GMAC's base inside a 2 MiB megapage `kmain`
  already maps, or does it need its own `insert`? SYSCRG needed one; the PWMDAC did
  not. This is a two-line answer that must be known before the first MMIO read.
- **Reference sequences to transcribe.** Mainline `dwmac-starfive.c` +
  `stmmac_main.c`, and U-Boot's StarFive dwmac support. The syscrg/iomux precedent is
  the model: read the mainline driver, transcribe the *register model* into a pure
  host-tested module, keep the volatile writes in `kernel/`.
- **Does U-Boot leave anything usable behind?** It brings the PHY up for TFTP, so the
  link is provably working seconds before `booti`. Worth **measuring** whether the MAC
  is left in a usable state — but assume nothing: the display work already found that
  inheriting U-Boot's framebuffer was a dead end
  ([vf2-display.md](vf2-display.md)). Even if no state is inheritable, U-Boot's
  *source* is a known-good sequence **for this exact board**, which is worth more than
  a generic datasheet reading.
- **Open question 4, answered early: does the config we need force an RX ring?** The
  design note defers this to bring-up. Resolve it in Phase 0 instead, because it
  changes two things: the size of this plan, and whether the network-console path
  (see the addendum in
  [docs/network-telemetry-design.md](../docs/network-telemetry-design.md)) gets its RX
  ring almost for free.

**Done when**: the above are written up as a short findings note (follow
[v0.4-memory-findings.md](v0.4-memory-findings.md)'s shape), and steps 5–7 below can
be decomposed with real acceptance criteria.

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
logic — no board, no MMIO — and are where most of the *code* lives. Steps 5–7 are
board bring-up, where most of the *time* lives; each is one observable bit, per the
port's "cheapest possible board increment" discipline.

---

### Step 1: GMAC clock + reset bring-up as a pure `Op` sequence

**Acceptance criteria**: `syscrg::gmac_bringup(...)` returns the exact ordered
sequence of clock-gate, divider, and reset assert/release operations for the connected
GMAC, matching the mainline sequence, with reset release expressed as
"poll status until the bit reads 1" — the model `syscrg.rs` already encodes.

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

**Note**: this step may be **empty** — the RJ45 pins may be fixed-function rather than
muxed, in which case the delay configuration lives in a GMAC/syscon register instead
and belongs to step 1. Phase 0 decides. If it is empty, say so and delete the step
rather than inventing work for it.

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

### Step 5 (sketch): PHY bring-up on the board — link up

The first board increment, and the first place the emulator cannot help. Reset the
YT8531 via its board GPIO, configure RGMII delay, read the PHY ID registers over MDIO
(a known constant — the cheapest possible "am I talking to the right thing?" oracle),
then poll link status.

**The observable bit**: a telemetry breadcrumb over the UART carrying the PHY ID and
the link state. Nothing else. Do not attempt to transmit in this step.

Decompose after Phase 0 — the reset GPIO, the delay configuration location, and the
YT8531's register specifics are exactly what Phase 0 establishes.

---

### Step 6 (sketch): first datagram on the wire

MAC init, install the TX ring built in step 3, transmit one hand-built frame from
`kernel-net::build_udp_datagram`. **Oracle: `tcpdump` on the host**, not the collector
— at this stage the question is "did well-formed bytes leave the board", and a raw
capture answers it without involving the decode path.

---

### Step 7 (sketch): `net=` end to end

Wire the `NetDevice` impl into the existing boot-time sink selection — which already
exists and already works for virtio-net, so this should be small. Boot the board with
`net=…`, run `cargo xtask collect`, watch the board reach Grafana.

At this point M2.5 is complete and
[network-telemetry.md](network-telemetry.md) can move to `plans/legacy/`.

---

## Open questions

1. **RX ring necessity** (design note Q4) — pulled forward into Phase 0; see above.
2. **PHY specifics** (design note Q5) — YT8531 reset GPIO, RGMII clock/delay location,
   MDIO addressing. The genuinely unknown-shaped part.
3. **Interrupt or poll?** The telemetry path is fire-and-forget and TX-only, so a
   polled reclaim in the heartbeat may be sufficient and avoids a PLIC route entirely.
   Decide once step 3's reclaim is written; prefer the poll unless something forces
   otherwise.
4. **Does the second GMAC matter?** Almost certainly not — one RJ45 — but confirm
   rather than assume, since a wrong-node bring-up looks identical to a broken one.
5. **What happens to `net=`'s static MAC on real hardware?** The board has an assigned
   MAC (often from OTP/eFuse). Does the bootarg keep overriding it, or should the
   driver read the hardware address? Bootarg-wins is simpler and matches the existing
   config story; note it as a deliberate choice rather than an oversight.

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
