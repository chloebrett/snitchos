# Plan: a native JH7110 display driver for the VisionFive 2

**Status:** Proposed — nothing built. Supersedes the "Not planned — framebuffer"
line in [visionfive2-port.md](visionfive2-port.md#dropped-for-now--framebuffer-fw_cfg--ramfb).

## Why this is in scope after all

The port plan dropped the framebuffer as "a whole project." It is — but the
[physics desktop](../docs/physics-desktop-design.md) and the arcade idea are
*fundamentally* about a self-contained machine driving a real panel. A QEMU-only
framebuffer can't be that. So the display driver is not a nice-to-have rider on
the port; it's the thing the port is for.

## The one-sentence thesis

> Do not port the vendor driver. **Capture what it does to the hardware, replay
> that, and use the vendor source as commentary.** The DC8200 bring-up is a few
> hundred MMIO writes; the difficulty is entirely in the U-Boot device-model /
> clock-framework indirection wrapped around them, and a trace dissolves that
> indirection for free.

This is the Panfrost / Asahi Linux methodology, applied to a case that is
strictly easier than either:

- **Panfrost** had to *understand* the hardware, because a GPU is programmable —
  a captured command stream generalises to nothing, so they captured across many
  workloads and diffed to isolate bit meanings. A display controller in a fixed
  mode has exactly **one** workload. The trace *is* the driver.
- **Asahi/Panfrost** were reverse-engineering blobs. We have **GPL source**
  (`sf_vop.c`, `sf_hdmi.c`). So this isn't reverse engineering at all — it's
  trace-assisted porting. The capture resolves the indirection; the source
  explains what the capture means.

The one technique worth stealing wholesale is the **diff**: capture two modes
(1080p60 and 720p60), and the registers that differ are the mode-dependent ones.
That converts "hardcoded single mode" into "parameterised by a handful of
registers" for the cost of one extra capture. Deferred past v1, but it's why the
capture harness should be reusable rather than one-shot.

## Ground truth (measured on the board, 2026-07-24)

The "let U-Boot bring up the display and inherit the framebuffer" shortcut is
**dead on this firmware.** Measured at the `StarFive #` prompt:

| Check | Result | Conclusion |
|---|---|---|
| `version` | `U-Boot 2025.10-0ubuntu0.24.04.1` | Ubuntu's build of *upstream* U-Boot |
| `cls` | `Unknown command` | `CONFIG_VIDEO` is off entirely |
| `printenv stdout` | `serial@10000000` | no `vidconsole` |
| `dm tree` | no `video` uclass, no DC8200, no HDMI | driver not compiled in |
| `dm tree` | clock controllers: sys@13020000, stg@10230000, aon@17000000 — **no VOUT CRG** | even the clock/power layer is absent |
| `dm tree` | `hdmitx0-pixel-clock` present as an unprobed `fixed_clock` | DT stub only, no driver behind it |

StarFive's `sf_vop.c` / `sf_hdmi.c` live on the vendor `JH7110_VisionFive2_devel`
branch and were never upstreamed, which is why this build has nothing. Reverting
SPI to vendor U-Boot is rejected: the 2025.10 update is what made the board boot
stock Ubuntu (see
[../notes/visionfive2-first-boot-and-firmware-update.md](../notes/visionfive2-first-boot-and-firmware-update.md)).

## The instrument

Chainloading vendor U-Boot was previously filed as a fallback. It isn't — **it's
the measuring device**, and it stays out of the SPI flash:

1. Leave upstream 2025.10 in SPI.
2. `load mmc` StarFive's `u-boot.bin` from SD, `go` it.
3. That second U-Boot runs the vendor display drivers and lights the panel.

Patch its `writel`/`readl` on the display path to log `addr = value` over serial,
and one boot yields the exact register sequence that drives *this* panel on *this*
board — with the HDMI PHY constants already resolved for the negotiated mode.

**The m1n1 lever.** Asahi's m1n1 is three things: a bootloader, a hypervisor that
traps macOS's MMIO to produce exactly this kind of trace, and — most useful here —
a **host-side Python REPL that pokes hardware live over USB**. The third is worth
copying early: a tiny poke server on the board (read/write a physical address,
over the existing UART) collapses the bring-up loop from
edit→rebuild→reflash→boot→squint into typing register writes at a prompt. Cheap
to build, and it pays for itself the first time a write hangs the bus.

## The oracle: model the DC8200 in snemu

We own an emulator and already ship a real ramfb device model in it
([legacy/snemu-ramfb-model.md](legacy/snemu-ramfb-model.md)). Model the DC8200
against the captured trace: the device accepts the sequence, asserts ordering
constraints, and exposes a scanout buffer that can be PPM-dumped for visual proof
— exactly the ramfb model's shape.

That makes the SnitchOS-side driver **host-testable and itest-able before it ever
touches hardware**, with the captured trace as the oracle. Same posture as the
JIT's byte-identical oracle and the itest suite generally. Without this, bring-up
is blind iteration over a serial cable; with it, it's a normal TDD loop that
happens to end on silicon.

## Milestones

Risk-ordered. Each leaves the QEMU/snemu gate green — none of D0–D3 touches the
kernel at all.

- **D0 — Go/no-go.** Chainload vendor U-Boot from SD; confirm it lights the
  monitor. Pure feasibility gate: if the vendor driver can't drive this panel,
  a port of it certainly can't. One evening. **Do this before anything else.**
- **D1 — Capture, reusably.** Instrument `writel`/`readl` in the vendor display
  path, dump the trace over serial, commit it as a fixture. Record the negotiated
  mode, resolution, pixel format, and framebuffer address. Note whether the trace
  touches I2C (see unknowns) and whether it does any cache maintenance.

  **Build the harness reusable, not one-shot** (decided). Concretely: the patched
  vendor U-Boot tree, the serial-capture script, and the trace→fixture parser are
  all kept and scripted, so re-capturing is a command rather than an
  archaeology session. Cheap now; it's what makes the two-mode **diff** possible
  later without a second bring-up — and re-capture is also the natural response to
  "the replay hangs at write #217 and I need to see what the vendor driver read
  back there." Assume ≥3 captures over the project's life, not one.
- **D2 — Poke REPL.** Host-side tool + minimal board-side MMIO read/write server
  over UART. Optional but strongly recommended before D4.
- **D3 — snemu DC8200 model.** Device model that accepts the D1 trace and exposes
  a scanout buffer; PPM dump for visual proof. TDD'd host-side like every other
  snemu device.
- **D4 — Driver as replay.** SnitchOS `kernel/src/device/dc8200.rs` replaying the
  captured sequence, green under snemu, with an itest asserting scanout contents.
- **D5 — Silicon.** Run it on the board. Success = a colour on the panel driven by
  SnitchOS with no bootloader help.
- **D6 — Factor.** Refactor the replay into named phases (PMIC → power domain →
  VOUT CRG → DC8200 → HDMI PHY) using the vendor source as commentary. Only now
  does understanding become necessary, and by then it's cheap.

Then the framebuffer work rejoins
[docs/framebuffer-design.md](../docs/framebuffer-design.md) Milestone 0 — the dumb
compositor is transport-agnostic and shouldn't care whether it's writing to ramfb
or a DC8200 scanout.

## Bring-up telemetry (do this from D4, not after)

Emit each bring-up phase as a span. A failed display init should show up in Tempo
as *"got through VOUT clocks, hung waiting for PHY lock"* rather than a black
screen and a dead console. The dominant failure mode in this whole class of work
is the silent hang, and this is the project whose entire thesis is that silent
failures are a design defect. Bring-up is the best possible demo of it.

## What got easier

Because *we* allocate the framebuffer rather than inheriting U-Boot's, place it in
the **first 1 GiB of physical RAM** (VF2 base `0x4000_0000`, so below
`0x8000_0000`). It's then covered by the existing linear-map gigapage —
`pa_to_kernel_va` just works, no extra `mmu::map`, and the only requirement is
reserving those frames in the bitmap. The "U-Boot's fb sits at top of RAM, far
outside your 1 GiB linear window" problem evaporates.

## Unknowns to de-risk in D0/D1 — not in D5

1. **PMIC rails.** Public JH7110 display writeups hit PMIC-over-I2C as a
   prerequisite for display power; the VF2 has its own PMIC on I2C (confirm the
   part — an AXP-family device is likely but unverified). `dm tree` shows i2c5
   probed for the EEPROM, so the bus works, but no PMIC driver exists. **Check
   whether the D1 trace touches I2C before any display register.** If it does,
   that's an extra device in scope.
2. **Cache coherency for scanout.** The display engine DMAs from DRAM while CPU
   writes may sit in the SiFive CCache (`cache-controller@2010000`, probed). Find
   out what the vendor code does — explicit flush, coherent path, or nothing.
   Cheap to learn from the trace; expensive to discover as "the screen shows stale
   garbage."
3. **Register bases.** DC8200 and VOUT CRG addresses quoted in public writeups
   should be **confirmed against the vendor DTS / the D1 trace**, not taken from
   secondary sources. Upstream's DT may not carry the display nodes at all.
4. **Mode.** Whatever D1 negotiates (likely 1080p60) is hardcoded for v1.

## Non-goals for v1

EDID/DDC probing · mode setting beyond the captured mode · multiple DC8200 planes
· hardware cursor · DSI output · 4K · double buffering / tear-free present ·
runtime hotplug. All deferrable; none blocks a lit panel.

## Sequencing (decided)

**This whole plan runs after M2** (telemetry over UART). M2 is the milestone that
makes the port *mean* something, it's a fraction of the work, and it is a genuine
*prerequisite* rather than merely a priority call: the bring-up telemetry above —
"hung waiting for PHY lock" instead of a black screen — only exists if frames
already reach the collector from the board. Doing the display first would mean
debugging the hardest driver in the port with the worst possible instrumentation.

So: **M2 → D0 → D1 → D2–D6.** The reusable capture harness is what makes that
ordering free; nothing is lost by not capturing while the chainload setup is fresh,
because re-capturing is a scripted command.

---
*On completion, `git mv` this file to `plans/legacy/` (repo convention) and run
`cargo xtask links`.*
