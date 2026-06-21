# 🕹️ Direction: the arcade as SnitchOS's observability showpiece (+ real hardware)

> **Status: committed as the post-v1.0 north-star** (2026-06-21) — the arcade is now the
> headline post-v1.0 real-time workload in [roadmap-and-milestones.md](roadmap-and-milestones.md).
> This doc remains the full reasoning + recommended shape (sequencing, hardware, the novel
> capability-OS game primitives). Hedges are deliberate: prices, perf numbers, and tooling
> maturity are ballparks with wide error bars. The v1.0 line itself is the interactive
> shell + a basic editor; the arcade rides *after* it.

**One-line thesis:** build a small physical arcade machine (display + controller + sound,
running a Tetris clone and eventually a simple software-rendered Minecraft, eventually
multiplayer) on a RISC-V board — framed as *the killer real-time workload that proves the
observable OS*, not as a pivot to building a game console.

---

## 1. The framing that makes this work (read this first)

An arcade is the **best possible observability workload**, not a genre switch. Games have
hard, legible, real-time requirements a CRUD/metrics-ingestion server simply doesn't: frame
deadlines, input→photon latency, audio buffer underruns, netcode jitter. "Watch the
frame-time histogram in Grafana while you play Tetris," "here's the trace of
input→simulate→render→scanout for this dropped frame," "watch the multiplayer lag spike
show up as a span" — *that* is what the observability thesis is for, and it's far more
compelling than request counters.

**The identity guardrail:** hold the line that the arcade is the *showpiece workload for the
observable OS*, not a new project. The distinctive identity is "the OS that snitches on
itself, running a multiplayer arcade where you can see exactly why a frame dropped or a
packet lagged." If it drifts into "I'm building a game console and forgot the snitching,"
the unique identity is lost (there are many hobby OSes; there's one that does this).

**It rides the existing roadmap rather than replacing it:** a game is a natural *first
userspace process* (v0.7); the FS (v0.10) holds assets/high-scores/saves; the network
(v1.2) is multiplayer. The arcade is the connective demo that gives each milestone a
concrete, fun reason to exist. Candidate roadmap change: **the arcade replaces/augments the
v0.11 metrics-ingestion server as the v1.0 headline workload** — it demonstrates "real
workload, fully observable" far more vividly.

---

## 2. Hardware: a RISC-V board (VisionFive 2), explicitly **not** a Pi 5

### The decisive lever: stay on RISC-V

The single biggest win is **not porting to aarch64.** Staying RV64GC keeps the entire arch
layer — asm, CSRs, Sv39, trap model, context switch — as-is. So the recommendation is a
RISC-V application-class SBC.

### Why not the Raspberry Pi 5 (even though one is on hand)

- **aarch64** → a full arch port (page tables, GIC, generic timer, exception levels,
  `kernel8.img` boot). Big.
- **The RP1 southbridge.** Pi 5 moved USB *and* Ethernet (and GPIO) onto a separate chip,
  **RP1**, reached over an *internal PCIe link*. So both USB and networking sit behind a
  **PCIe root-complex bring-up + RP1** wall — and RP1 is sparsely documented (you'd read
  Raspberry Pi's Linux drivers, not a clean datasheet). This *erodes* the usual "network is
  easier than USB" escape: on Pi 5 both share the PCIe/RP1 prerequisite.
- **No emulator** for BCM2712 PCIe/RP1 — you lose the fast QEMU loop exactly where it hurts
  most (link training, DMA landing in the wrong page — silent failures, the worst kind; cf.
  the higher-half DTB saga).
- **RP1-free tiers that *do* work on Pi 5:** the debug UART (serial console, early-boot, not
  behind RP1) and the VideoCore framebuffer (display, not behind RP1). So a Pi 5 could do
  serial + HDMI framebuffer, but USB/network are the steep cliff.

### Board comparison (both are the same SoC)

Both candidates use the **StarFive JH7110**: quad **SiFive U74, RV64GC**, OpenSBI boot. The
SoC is what matters for bare metal, so the *kernel work is identical* on either.

| | **Milk-V Mars** | **VisionFive 2** |
|---|---|---|
| SoC / cores | JH7110 / 4× U74 (identical) | JH7110 / 4× U74 (identical) |
| Boot | OpenSBI (same as QEMU `virt`) | OpenSBI (same as QEMU `virt`) |
| Peripherals | 1× GbE, HDMI, USB, 40-pin | 2× GbE, PCIe/M.2, 4× USB, HDMI, 40-pin |
| **Docs / community** | smaller corpus | **reference JH7110 board — most worked examples** |
| Price (4GB) | ~$55 USD | ~$75 USD |

**Recommendation: VisionFive 2, 4GB.** With flexible budget the ~$20 premium buys the one
relevant thing — the **deepest bare-metal worked-example trail** on the JH7110, which
directly mitigates the silent-failure risk that's the real hazard on metal. Guardrails:
- **Stop at 4GB.** A microkernel can't use 8GB; that's paying for DRAM you'll never fill.
- **Don't go more exotic/powerful** (e.g. TH1520/C910 boards). The U74's *boringness* —
  in-order, standard RV64GC, closest to QEMU `virt` — is the feature. Staying boring is
  staying de-risked.

### Why the JH7110 is the right fit for SnitchOS specifically

1. RISC-V → zero arch port.
2. **Boots via OpenSBI — same S-mode/SBI environment as your QEMU `virt`.** The mental model
   transfers directly; no firmware glue from scratch.
3. **Quad U74 harts** → the SMP showpiece (cross-hart producer/consumer, IPIs, TLB
   shootdown) runs on real silicon. Bump `MAX_HARTS` 2→4.
4. **DesignWare 8250/16550-style UART** → your `ns16550a` driver nearly transfers (`println!`
   *and* the serial console).
5. **Documented + trodden** — TRM, upstream U-Boot/OpenSBI/Linux, active community.

### Essential accessory people forget

A **USB-to-TTL 3.3V serial adapter** (CP2102/FTDI, ~$10). This single cable is the entire
I/O story for SnitchOS-on-hardware: it carries both the serial console *and* the telemetry
stream to the host. Budget it. (Plus a microSD + USB-C PSU.) Whole setup fits ~$100 AUD on
a Mars, a bit more on a VF2.

### Minor port work
- MMIO addresses differ from QEMU `virt` — **hardcode JH7110 addresses to start** (same
  pattern already used for `virt`); un-park DTB later.
- 8GB → bump the frame allocator's `MAX_RAM_BYTES` (currently 4 GiB cap / 128 KiB bitmap).
  Trivial.

---

## 3. Portability / HAL principles (apply now, regardless of when hardware happens)

These make the codebase better even if it never leaves QEMU, and keep the door open:

- **virtio is the QEMU *port*, not the thing.** Real hardware has no virtio. The virtio
  console/gpu/input/RX work is "the QEMU implementation of a device-class port."
- **Design against device-class traits:** `Framebuffer`, `Input`/`PointerEvent`, `Console`,
  `FrameSink`, `Timer`, `InterruptController`. Logic (renderer, game, dispatch) sits above;
  device drivers are swappable impls. (Exactly the kernel-core extraction discipline, one
  level out.)
- **The reuse litmus test:** *does a crate ask for a buffer + a trait impl, or does it ask
  for an OS (std, async runtime, driver framework)?*
  - Reuses cleanly: **smoltcp** (TCP/IP), **embedded-graphics** (no_std 2D), **xhci** (xHCI
    register/type bindings), RustCrypto.
  - Does **not** reuse: **Mesa** (needs Linux + DRM), **russh** (needs std + tokio),
    TinyUSB-into-your-kernel (chip/substrate-specific C).
- **Fix DTB discovery.** The parked `collect_mmio_regions` (pre-MMU/higher-half silent crash)
  becomes load-bearing for real hardware. **Un-park and harden it on QEMU first**, where you
  can see what's happening — don't discover it's broken for the first time on a board with
  only a serial cable.
- **Transport-agnostic telemetry.** `FrameSink` with a virtio-console impl (QEMU) and a UART
  impl (board). On the board, frames leave over UART → USB-serial → the collector reads a
  serial port instead of a unix socket; Grafana is identical.
- **QEMU-first, board-as-additive-port.** Almost everything is logic and develops in the fast
  loop; only physical drivers (SPI panel, GPIO, dwmac) need the board.
- **Offload hostile peripherals to coprocessors** (see USB/controllers below).

---

## 4. Peripheral reality: easy paths vs behemoths

Happily, the *cheap* paths are also the more arcade-authentic ones.

| Need | Easy / authentic path | Behemoth (defer/skip) |
|---|---|---|
| **Display** | **SPI TFT panel** (ILI9341-class) — SPI pixel writes, bare-metal-trivial | HDMI + JH7110 display controller (bounded but real); GPU 3D engine (non-starter) |
| **Controls** | **Arcade buttons/stick → GPIO**; or a **controller bridged via MCU** (§7) | USB/Bluetooth host stack in-kernel |
| **Sound** | **PWM → amp → speaker** (chiptune/square waves) | I2S PCM + codec + DMA (the "over-engineered audio" milestone) |
| **2D game** | Pure Rust + `embedded-graphics` — the fun part | — |
| **3D** | **Software rasterizer / voxel raycaster** (CPU → framebuffer) | GPU acceleration (not happening bare-metal) |
| **Networking** | smoltcp + JH7110 **dwmac** ethernet (bounded; *not* behind anything RP1-like) | — |

---

## 5. Graphics & the GPU question

### Scanout ≠ 3D acceleration — you only need the easy one

"GPU" conflates two jobs: **scanout** (get a framebuffer onto a display — a display-controller
job) and **3D acceleration** (triangles/shaders on dedicated hardware). You need scanout
(SPI panel = easy; display controller = bounded). 3D acceleration is the non-starter, and you
sidestep it with a software rasterizer.

### Why 3D GPU acceleration is a non-starter (any GPU)

1. **No documentation — the programming interface is a trade secret.** Unlike a UART
   (datasheet), a NIC (documented GMAC), or even xHCI (public spec), a GPU's command
   interface and **shader ISA** are secret. Open drivers (nouveau, etnaviv, the PowerVR
   effort) are *reverse-engineered*.
2. **Minimum-viable driver is Mesa-scale** — a kernel DRM driver (GPU memory mgmt, command
   rings, fences, the GPU MMU, firmware loading) *and* a userspace driver including a
   **shader compiler**. Hundreds of thousands of lines.
3. **Firmware gates everything** (signed blobs; on newer NVIDIA the GSP firmware mediates all
   access).

An **old/discrete card (e.g. GTX 750) is worse, not better:** NVIDIA is the most closed
vendor (nouveau is pure RE; Maxwell needs signed firmware even Linux struggled with); it adds
PCIe-root-complex bring-up + ~55 W power/mechanical hassle; and the "dumb framebuffer via
VBIOS" trick is dead on non-x86 (the VBIOS is x86 real-mode; you'd have to *emulate x86* to
POST it — absurd effort for a worse result than a $15 SPI screen). "Old = documented" is true
for UARTs/NICs and uniquely false for GPUs.

### Asahi Linux — existence proof *and* price tag

Asahi genuinely reverse-engineered the Apple GPU, which confirms it's *possible* — and
measures the cost. It took: world-class GPU-RE specialists (Alyssa Rosenzweig on Mesa/compiler,
Asahi Lina on the Rust DRM kernel driver); years; a priceless methodological advantage —
**tracing Apple's own working driver live** via the m1n1 hypervisor running macOS and logging
every MMIO/memory transaction; reverse-engineering a versioned firmware ABI; and **building on
Mesa** for all of userspace. None of those advantages apply to a solo bare-metal kernel. It
confirms "non-starter," it doesn't refute it.

### Can Mesa be reused? No (but the right thing can)

Mesa = millions of lines assuming a Linux/POSIX userspace + a kernel DRM driver underneath.
Reusing accelerated Mesa ≈ becoming Linux. **The reusable thread is software rendering, not
Mesa's heavy implementations:**
- **2D (Tetris): `embedded-graphics`** — Rust, no_std, built for SPI TFTs. Direct drop-in.
- **3D (Minecraft): a small custom software rasterizer** — a few hundred lines, and *more*
  observable than a GPU (you can trace per-stage timing).

### If acceleration were a hard requirement

Beat the GPU problem by **refusing undocumented hardware** — choose a public ISA or hardware
you own:
- **Route A — RISC-V Vector (RVV):** SIMD on the CPU, a *ratified public ISA*, no driver/
  firmware. Matrix multiplies and per-pixel fill loops map onto it. **Caveat: the VF2's U74
  has no RVV** — you'd need an RVV-1.0 board (e.g. **SpacemiT K1** / Banana Pi BPI-F3; *not*
  the TH1520/C910's pre-ratification RVV 0.7.1). Still RISC-V → no arch port. Tooling honesty:
  Rust RVV intrinsics are unstable; realistically inline-asm/`.S` kernels or autovectorization.
- **Route B — FPGA:** synthesize an open GPGPU (**Vortex**) or build a small systolic-array
  matmul accelerator yourself; define your own MMIO interface (documented by construction);
  ideally on a **PolarFire SoC** (hard RV64GC quad + FPGA fabric on one chip). Bigger detour
  (HDL toolchain), but you own — and can instrument at RTL level — the whole accelerator.

**Principle:** the only thing that ever made GPUs impossible was *secrecy*. Restore
documentation (public ISA or hardware you authored) and acceleration becomes tractable *and*
more observable. Never RE a commercial GPU driver — that's the Asahi-scale tar pit.

### virtio-gpu (pedagogically interesting, mostly QEMU-only)

The one "GPU" you can learn by *building* (open spec) rather than RE-ing. **2D scanout** mode
is simple (reuses your virtqueue machinery) and gives a framebuffer in a QEMU window — useful
arcade infra with mild pedagogy. **3D (virgl)** teaches the real driver-side mental model
(resource model, command-stream encoding, fences, shipping shaders as IR) but is Mesa-shaped,
QEMU-only, and teaches the *interface* not the *silicon*. Keeper insight: virtio-gpu is the
readable Rosetta Stone for the **command-ring + resource + fence archetype** that real GPUs,
xHCI, and NVMe all instantiate in opaque/huge form. If pursued: a bounded "draw a triangle"
spike, not a usable driver.

---

## 6. Software renderer & performance expectations

3D on this hardware = **software rasterizer on the CPU → framebuffer → scanout.** Mental
model: **mid-90s-to-2000 software 3D.**

- **VisionFive 2 (4× U74 @ 1.5 GHz, scalar, no SIMD):** Quake-1 / early-PlayStation /
  Minecraft-classic class. **QVGA (320×240) 30–60 fps comfortably**; VGA (640×480) ~30 fps
  with care. Anchor: Quake (1996) ran smooth on a Pentium ~133; four U74s are ~60–120× that,
  swamping a 3–5× renderer-inefficiency factor. Real perf < theoretical because in-order
  cores punish cache misses (texture-access locality matters).
- **Banana Pi K1 (8 cores + RVV 1.0):** ~2× cores × ~3–6× RVV on hot fill loops, minus
  Amdahl ≈ **~5–8× effective** with good (non-trivial) optimization → a generation up
  (VGA@60 / ~720p@30, fancier scenes).

**"Without matmuls" clarification:** the vertex transform (4×4 × vec4 per vertex) is *not*
the bottleneck — per-pixel fill/shade dominates. What SIMD/RVV buys is vectorized *per-pixel*
throughput, not matrix math.

**The sneaky real bottleneck — the display transport.** An SPI TFT caps framerate by *bus
bandwidth*: a 320×240 RGB565 frame ≈ 1.2 Mbit; SPI @ ~50 MHz → **~30–40 fps max just to ship
the buffer**, regardless of CPU. Mitigate with **DMA + double-buffering** (render N+1 while
DMA ships N). To exceed it, use the display controller (harder bring-up). So display choice
may bound you before the CPU does. You'd *measure* all of this — per-stage spans + a
frame-time histogram in Grafana are the on-brand way to find the real numbers.

---

## 7. USB & controllers — bridge it, don't build it

### Why USB is a behemoth

A layered stack of independent specs over a real async tree-shaped bus: a complex host
controller (**xHCI ~600pp / dwc2**, and the JH7110/Pi differ), stateful timing-sensitive
**enumeration**, multiple **transfer types** (control SETUP/DATA/STATUS, interrupt), a
**hub tree with hot-plug**, a **HID report-descriptor** bytecode parser, and on real silicon
**DMA cache-coherency** + SoC power/clock/PHY bring-up. Contrast virtio-input: one spec, one
virtqueue, ready-parsed evdev events. (Boot-protocol HID — fixed 8-byte keyboard / 3-byte
mouse reports — is the saving grace, but the controller + enumeration + DMA floor remains.)

### What's reusable

Host-side Rust is thin (device-side — `usb-device`/`embassy-usb` — is mature but wrong side).
The reusable bits are *bindings* (`xhci` register/type defs), not a driver. The genuinely
reusable *seam* — if you ever did want USB — is the **protocol engine (enumeration, transfers,
HID) over an HCD trait, fake-HCD-testable** — the exact virtio-handshake decomposition. The
controller drivers (xHCI/dwc2) are the per-platform impls.

### The controller answer: a coprocessor bridge

Using an 8BitDo *as designed* (USB or Bluetooth) needs the USB stack — or, for BT, an even
*bigger* Bluetooth stack. **Don't put either in the kernel.** Offload to a $5 MCU that speaks
the hostile protocol and streams decoded `{buttons, axes}` over UART/SPI/I2C (which SnitchOS
already drives):

- **Wireless 8BitDo (sweet spot): ESP32 + Bluepad32** (Bluepad32 explicitly supports 8BitDo)
  → UART to SnitchOS.
- **Wired: Raspberry Pi Pico (RP2040) + TinyUSB host** → UART/SPI.
- **No bridge at all:** a **PS2/DualShock** controller over SPI/GPIO (dual analog sticks,
  documented simple protocol), or a **Wii Nunchuk/Classic** over I2C (analog stick(s),
  trivial protocol).
- **DIY cabinet:** arcade microswitch stick + buttons → **GPIO**; analog sticks → an
  **external I2C/SPI ADC** (ADS1115) since the JH7110 likely lacks a usable ADC.

### The "MCU has a USB stack but isn't Linux" reconciliation

Yes the MCU runs a USB stack (TinyUSB / Bluepad32 — a *library* in its firmware); no it
doesn't run Linux (bare-metal Pico SDK, or **FreeRTOS** on ESP32 — a kilobyte-scale
scheduler, not an OS). This doesn't contradict "USB is a behemoth." The MCU wins because:
(1) the stack was already **written and battle-tested for that exact chip** — you `#include`
it; (2) its USB controller is **far simpler** (full-speed, fixed-function vs xHCI/dwc2);
(3) it does **one narrow job** (host this gamepad), not a general robust stack; (4) the
substrate has **no MMU/cache/SMP integration tax**. None hold for "general USB host in a
custom Rust kernel on the JH7110." **The bridge doesn't shrink the behemoth — it relocates it
to where it's already solved on friendly hardware, and you consume the output over a trivial
wire.** Bonus: the bridge's fixed input latency is itself traceable (input→render spans).

---

## 8. Networking & remote interaction

Splits into three very different costs:
- **IP stack: easy — smoltcp** (mature, no_std, designed to embed; the reuse win USB lacks).
- **NIC driver: bounded on VF2** — the JH7110 **dwmac** (DesignWare GMAC) ethernet, *not*
  behind anything RP1-like. On QEMU it's virtio-net (reuses your virtio machinery).
- **SSH: overshoots — skip it.** `russh`/`thrussh` are std + tokio (fail the litmus test);
  porting one into a no_std microkernel is a major project. You don't need it.

**The cheap interaction path is a raw TCP "network REPL"** (`nc`/`telnet` → type commands)
over smoltcp — which is just the control-plane/REPL with the transport swapped to a socket.
Ladder for "interact with SnitchOS on the board":
1. **Serial console** (UART RX + a line loop + dispatch) — needs no network; the trivial
   first step (your `Uart16550` already does TX; RX is ~10 lines: poll LSR bit 0 / read RBR).
2. **Network REPL** (dwmac + smoltcp + raw TCP) — untethered, reuses virtio-net on QEMU.
3. **USB keyboard / SSH** — only if they're the point, never as a means to interaction.

---

## 9. Effort estimates (focused-work, wide error bars)

The game is the cheap part; the **platform it sits on** is the real cost — a one-time,
shared investment.

**Shared platform (QEMU-first), ~1.5–2 weeks:** framebuffer in a QEMU window (virtio-gpu-2D
or ramfb — the bulk) + serial-console input (~days) + a fixed-timestep game loop (~days) +
2D drawing via `embedded-graphics` (~free). Board port (SPI panel + GPIO) is a separate,
board-only phase (+1–2+ weeks with the silent-failure tax).

- **Tetris (first — deliberately):** platform + **~1–3 days** of pure-logic game
  (host-tested). ≈ **2–2.5 weeks total in QEMU**, ~90% reusable platform. It's first because
  it needs *zero art* — colored cells + bitmap-font score text, all shipped by
  `embedded-graphics` — so it proves the real-time platform (framebuffer + input + loop,
  under the SPI display-transport/double-buffer constraint) with no asset production.
- **Slay the Spire port (the first real *app* / userspace flagship — *after* Tetris):**
  turn-based 2D, so GUI-light in *compute* (no rasterizer, no framerate pressure) but
  **graphics-heavy in *assets***. The terminal version leaned on **emoji**, which are free as
  terminal text but on a framebuffer become **sprites you must produce** (`embedded-graphics`
  does monochrome bitmap fonts, *not* color emoji). Cost: (a) de-`std` the well-factored core
  to `no_std + alloc` + `hashbrown`, **injecting clock/RNG/IO behind traits** (the existing
  `WallClock` pattern) and discarding the TUI; (b) a **sprite/atlas pipeline** — curate the
  finite emoji set into pre-rendered sprites (e.g. from open Twemoji/Noto Emoji) + a blitter;
  (c) the 2D layout (easy). Comes after Tetris *because* Tetris needs no art while StS needs
  the sprite pipeline. Variable: how `std`-entangled the core is (well-factored → core port
  is days-to-weeks; tangled → de-`std` grind). **Killer synergy:** a run is decisions + RNG,
  so OS-owned RNG/time make runs **deterministically replayable + tamper-evident** for free
  (primitives #5/#12) — the best possible fit for the replay/provenance story.
- **Simple Minecraft (huge variance):** adds a **software 3D renderer** — the one genuinely
  novel subsystem (~1–3 weeks to "decent," more to optimize for playable fps — the
  high-variance item) — plus known voxel algorithms (greedy meshing, raycast place/break,
  noise worldgen, basic lighting; days each).
  - Tier 0 (fly around a voxel chunk): ~3–5 weeks on top of the 2D platform.
  - Tier 1 (place/break, worldgen, a few blocks, lighting): ~1.5–2.5 months.
  - Tier 2 (saves→FS, audio, streamed worlds, physics) and Tier 3 (multiplayer→dwmac+smoltcp
    +netcode): further multi-week-to-month tails.

**Leverage:** QEMU-first means almost all of it develops in the fast loop; game logic +
rasterizer as pure host-tested crates behind `Framebuffer`/`Input` traits; the board is an
additive driver-port phase. Budget generously for the software renderer + perf tuning and the
board bring-up; the rest is trivial or known-algorithm grind.

---

## 10. Novel OS-level "wow factor" primitives

What makes these novel — vs commercial consoles *and* hobby game-OSes — is one inversion:
**SnitchOS is the platform/console layer itself, but transparent, capability-secured, and
OS-owned, instead of a closed, app-mediated, cloud-locked service.** Two corollaries no
console can match:
- **Honesty by construction** — the platform *measures* games from the outside; a game can't
  fake its playtime or hide that it stutters (the snitch is the OS, not the app).
- **Safety by construction** — capability-confined games mean *untrusted* code runs safely.

And because the OS owns the I/O boundary (input, time, randomness, render, score, identity),
it gets cross-cutting superpowers no game could have alone. The catalog (Tier = novelty +
strategic value; Rides = the milestone it depends on):

1. **Sessions/host-join as a capability; leaderboards & saves *above* games.** A game is a
   process handed a "play session" cap; the session (identity, score sink, input routing,
   render surface) is an OS object that **outlives the game** — crash a game and the session
   survives, relaunching it into the same lobby slot. Cross-game profiles. *Tier 1. Rides
   v0.7b caps + v0.10 FS.*
2. **Honest, OS-measured playtime & accounting.** Playtime, per-game CPU/mem/energy, lifetime
   dropped-frame counts — measured from outside, uniform, un-fakeable. *Cheap. Rides
   observability.*
3. **Observable multi-tenancy / native split-screen.** N games as capability-confined
   tenants; a compositor tiles the screen; the scheduler time-slices; you *watch the OS
   arbitrate* in Grafana (who's starving, fairness, per-tenant frame budgets). Input is a cap
   you route to a tenant — hot-swap which game your controller drives. The best stress-test
   *and* the best demo. *Tier 1. Rides v0.9 preemption/priorities.*
4. **Untrusted games run safely.** A game gets exactly a render cap + input cap + score cap,
   nothing else — a stranger's downloaded game can't escape, and denials are traced. *Tier 1.
   Rides v0.7b caps.*
5. **Record-and-replay / determinism as a primitive.** The OS mediates all nondeterminism
   (input, clock, RNG) → bit-exact replay that *every game inherits for free*: instant
   replays, ghosts, tamper-evident speedrun verification (caps + trace = provenance), "replay
   the exact dropped frame." *Tier 1. Rides the I/O boundary + caps.*
6. **Live QoS/fairness as a visible, tunable knob.** Tune scheduler priority live (control
   plane); see the effect in Grafana ("give Minecraft more CPU, watch Tetris's budget
   shrink"). *Stretch. Rides v0.9.*
7. **Cross-game OS mechanics.** Meta-currency earned in any title spendable in another;
   achievements defined by the OS *observing* behavior (not game-reported); a global
   aggregate score. The platform has game-mechanic agency. *Stretch. Rides sessions + FS.*
8. **Physical & internet effects.** Cabinet LED/7-seg/rumble driven by *OS* events (hart
   load, GC pressure); physical *OS* buttons ("snapshot this trace", "spawn another game").
   Internet **spectate-by-trace** — stream the *telemetry*, not video; reconstruct the
   session in a viewer/Grafana (bandwidth-tiny, infinitely inspectable); a public "what's
   playing now" trace dashboard. *Cheap (physical) → Stretch (internet). Rides GPIO / v1.2
   net + control plane.*
9. **The self-quantifying cabinet.** The machine displays its own lifetime stats — uptime,
   games played, frames rendered, hottest hart, biggest OOM survived, longest session. It
   snitches on itself. *Cheap. Rides observability.*

**Throughline:** these aren't bolted-on game features — they're OS primitives that emerge
from *owning the I/O boundary, securing it with capabilities, and observing everything*, and
they're directly downstream of milestones already planned. Tier-1 picks (1, 3, 4, 5) are the
"nobody else does this": consoles can't (opaque/closed platform, single-title, trust-the-app)
and hobby game-OSes don't (one game, no platform layer).

### The compositor as a creative primitive — arbitrary-angle & dynamic split

A deeper take on #3. Once the *OS* owns compositing, split-screen stops being axis-aligned
rectangles (which is all any console/game does). Each tenant renders to its own surface and
the compositor decides per-pixel which tenant owns it — a 50/50 vertical split is just
`x < W/2`; an **arbitrary angle** is "which side of line `ax+by+c=0`", one cheap test per
pixel (nearly free, since software rendering already touches every pixel). That generalizes:

- **Animated / dynamic splits** — the dividing line *moves or rotates* live, tied to game
  state (the leader's region grows; the boundary swings as the match shifts). The *layout
  itself becomes feedback*, provided uniformly to every game.
- **Arbitrary regions** — circular PiP, hexagonal tiles, a **Voronoi split for N players**
  (cells grow/shrink), a tenant rendered into *any* shape.
- **Cross-tenant compositor effects** — the OS holds all surfaces, so it can blend/fade
  between games, warp a tenant (rotate/scale/perspective), or open a "portal" where one
  region is a window into another game.
- **Layout-as-mechanic** — players fight over screen real estate; the OS arbitrates the
  boundary from their inputs. The split *is* the game, provided to any title.

Great "only here" demo: *"watch the screen split rotate as the match swings."*

### Further primitives (round 2)

Tier / ⚡ cheap-ish / 🌋 ambitious, as before:

10. **Observability overlay as a toggleable in-game "debug vision" HUD** ⚡ — a key toggles
    the live OS traces *on top of* the running game (frame timing, which hart, heap pressure,
    the scheduler juggling tenants). Observability as a player-toggleable AR layer, not a
    separate tab. *Tier 1.*
11. **Live game migration between cabinet and browser tab** 🌋 — move a *running* game (its
    session + state are OS objects) from the physical cabinet into a browser tab mid-play and
    back. Live process migration across a hardware↔WASM boundary; ties sessions + caps + §11.
    *Tier 1, ambitious.*
12. **Tamper-evident, replayable leaderboards (anti-cheat by construction)** — since the OS
    mediates input/time/RNG and traces everything, each high score ships with a verifiable,
    replayable trace the OS attests to. The snitch thesis *as* anti-cheat. *Tier 1.* (Rides
    #5.)
13. **OS-level universal mods/filters via capability shims** — insert an OS shim between a
    game and the surfaces it owns. **The honest line: the OS can transform any *signal it
    sits on* without understanding the game, but not *meaning it has no access to*.**
    - *Generic (works on any title):* render filters (CRT, colorblind, post-fx — OS owns the
      framebuffer); input/accessibility remaps (rebind, hold→toggle, repeat — OS owns input);
      **slow-mo / time-scaling** — the real generic *difficulty* lever: the OS owns the
      clock, so it decouples game-time from wall-time (slow the game → you react in real time
      → easier; speed up → harder), effective for any *timing-bound* game; score handicap (OS
      owns the score sink); RNG reroll/seed control (OS owns the stream — gives
      reroll/seed-scum, semantically blind).
    - *NOT generic:* semantic difficulty (enemy health, hitboxes, AI) needs the game's
      internal model — the OS can't touch it on an arbitrary title.
    - *Middle path:* a **difficulty *contract*** games opt into (a cap/param) for unified UI +
      handicap-adjusted leaderboards; non-adopters still get the generic boundary transforms.
    *Tier 1 for the generic transforms; the contract is opt-in.*
14. **Rollback netcode as an OS primitive** 🌋 — the GGPO-style rollback fighting games
    hand-roll falls out for free if the OS owns determinism + input; any game inherits real
    netcode. *Tier 1, ambitious.* (Rides #5 + v1.2 net.)
15. **"Twitch-plays-anything"** ⚡ — the OS multiplexes input from many session participants
    into one single-player game (everyone controls the one Tetris), or fans one input to N
    synced instances. Crowd co-op the game never coded for. *Fun, cheapish.*
16. **Time-travel / rewind any game** 🌋 — extend record/replay to rewind *live* (state
    snapshots via the cap/IPC model): a universal "rewind" button the OS gives every title.
    *Ambitious.* (Rides #5.)
17. **Synesthetic kernel — A/V reactive to OS internals** ⚡ — chiptune tempo tracks the
    scheduler tick; the LED strip pulses on heap growth; the screen glitches on a TLB
    shootdown. Internal state *as* performance. Demoscene-flavored, on-brand. *Cheap, high
    charm.*
18. **Crash as narrated spectacle** ⚡ — with crash-survivable sessions + observability, a
    game dying becomes a feature: the OS catches it, shows the autopsy (last frames + trace),
    and resurrects it into the session. Robustness made showable. *Cheap, distinctive.*
19. **Energy/thermal as a visible game stat + "eco mode"** — the OS measures per-game joules;
    expose "X J/hr", cap a power budget, watch the framerate/power tradeoff live.
    *Niche but on-thesis.* (Needs real power telemetry on hardware; modeled in the emulator.)
20. **Games that span multiple cabinets/screens** 🌋 — networked sessions let N cabinets
    side-by-side render one continuous world (split-screen *across machines*); browser tabs as
    extra panes. *Ambitious, great visual.*

Lead picks: the **debug-vision overlay (#10)** and **arbitrary/dynamic split** are cheap,
immediately striking, and scream "observable OS." **Live migration (#11)** and
**tamper-evident leaderboards (#12)** are the ambitious "nobody else *can* do this" flagships
once caps + sessions + WASM land. The **synesthetic kernel (#17)** and **crash-as-spectacle
(#18)** are cheap charm reinforcing the identity.

## 11. In-browser portability (WASM): SnitchOS in a tab

The portability thesis's ultimate payoff: **the same OS on a real cabinet *and* in a browser
tab, sharing sessions.** "Run SnitchOS in WASM" forks three ways:

- **(A) Compile the kernel to wasm — nonsensical.** The kernel *is* hardware mediation (CSRs,
  MMU, traps, MMIO); wasm removes the hardware (it's a sandboxed VM, and the browser is
  already its supervisor). Stubbing all that out isn't running the OS.
- **(B) Run the *unmodified* kernel in a wasm RISC-V emulator.** Precedent: Bellard's
  TinyEMU/jslinux runs riscv Linux in-browser. The kernel thinks it's on QEMU `virt`; the
  emulator wires virtio-gpu → `<canvas>`, virtio-input → keyboard + the **Gamepad API**,
  virtio-net → a **WebSocket**, telemetry → an in-page view. **The browser becomes another
  board** (QEMU / VF2 / K1 / browser = four targets, identical kernel) — the HAL thesis
  validated. *Catch: emulation overhead — 2D/Tetris fine, software 3D likely a slideshow.*
- **(B′) Compile the *portable upper half* (platform layer + games, above the HAL traits) to
  `wasm32`** with browser-native port backends (canvas / Gamepad / WebSocket). Native wasm
  speed → 3D viable. Requires the clean HAL split you're already building.

**The crucial distinction — B ports the *guarantees*, B′ ports the *experience*.** B′ runs in
one wasm sandbox: you get the split-screen compositor + session-joining, but *not* real
capability isolation / crash-survival / true multi-tenancy (there's no microkernel
underneath — it's one module). Only **B** (full emulation) preserves the OS-guarantee
features (#1/#4/#5 above) in-browser, at the cost of speed.
- Want a *real* SnitchOS in the tab (caps, isolation)? → **B**, accept the 2D/perf ceiling.
- Want it to *look and play* like one, fast, incl. 3D? → **B′**.
- You can do both: B′ for playable clients, B as the "boot the genuine OS in your tab"
  showpiece.

**"Same sessions" works because sessions are a *network protocol*, not a local thing.** Each
instance (cabinet or browser) runs the OS features locally and joins shared sessions over the
net. Browsers can only WebSocket/WebRTC, so bridge through a **session-relay server** both the
browser (WS) and the physical arcade (UDP/ethernet via dwmac + smoltcp) speak to. Split-screen
is a local compositor capability everyone has; the *session* is the shared, networked thing.
So a browser can run its own local split-screen *and* be in a multiplayer session with the
cabinet.

**Why it's a huge win:** shareability. Most hobby OSes need build + QEMU; this one becomes a
**URL** — "click, SnitchOS boots in your tab, you're in the session in seconds." Transformative
for the blog/video (a reader plays a round *inline*) and for multiplayer reach. Combined with
spectate-by-trace: watch live, then click to jump in. And it's observable *in the browser*.

**Don't conflate with the roadmap's "WASM userspace"** — that's the *inverse* (SnitchOS
*hosts* wasm apps; capabilities map onto wasm imports). This is *a browser hosts SnitchOS*.
Opposite nesting.

**Status:** far-future north-star — needs the network stack, sessions, and the game platform
first. It's the portability payoff you design *toward*, not a next step. Costs: a wasm-rv
emulator + device bridges (B) or the HAL split + browser port backends (B′), plus a
session-relay server; perf ceiling for emulated 3D; transport via WebSocket/WebRTC.

## 12. Recurring principles (the through-lines)

1. **Documentation/ownership beats reverse-engineering.** Choose public ISAs (RVV) or
   hardware you authored (FPGA); never RE a commercial GPU.
2. **Reuse litmus test:** buffer + trait reuses (smoltcp, embedded-graphics, xhci-types);
   needs-an-OS doesn't (Mesa, russh, TinyUSB-in-your-kernel).
3. **QEMU-first, board-as-port;** HAL device-class traits make the swap clean.
4. **Offload hostile peripherals to coprocessors** (USB/BT → MCU bridge → UART).
5. **The command-ring + resource + fence archetype recurs everywhere** (virtio, xHCI, NVMe,
   GPUs); virtio is its readable exemplar.
6. **Everything observable — even the cheats.** Bridge latency, renderer stages, link state
   → telemetry. The "shortcuts" become demo material rather than hidden.
7. **Silent, un-isolatable failure is the real hazard on metal** (cf. higher-half DTB). No
   emulator for board peripherals → develop logic in QEMU, harden discovery (DTB) in QEMU,
   keep the board phase small and serial-cable-debuggable.

---

## 13. Candidate sequencing & open questions

**Sequencing (smallest demoable first):**
1. Prove the arcade in QEMU: `Framebuffer`/`Input` ports → virtio-gpu-2D/ramfb + serial
   input → **Tetris** (needs *zero art* — the platform-prover).
2. Observability over it: frame-time + input-latency spans → Grafana.
3. **Sprite/atlas pipeline → port the Slay the Spire clone** (de-`std` core + emoji→sprite
   atlas) as the first real userspace-flagship *app*. Tetris first because it needs no art;
   StS second because it needs the sprite pipeline.
4. Board bring-up: serial console → framebuffer (SPI panel first; display controller if you
   need to uncap fps) → GPIO/bridge input.
5. **2D before 3D; solo before multiplayer; Mac-host-as-P2 before 2-board.** (Both Tetris and
   StS are 2D; Minecraft is the 3D step.)
6. Network REPL (dwmac + smoltcp) when untethered interaction is wanted.

**Far-future application — the portfolio homepage (an application of §11).** "Boots when you
open the website": the *real* unmodified kernel in a wasm RISC-V emulator (Option B — fine
here, since a portfolio is GUI-light, so the emulated-3D slideshow caveat doesn't bite), with
the **SnitchOS boot log + live span tree as the loading screen** (latency-as-spectacle). Lands
in a hardcoded portfolio shell; projects are launchable tiles (incl. playable Tetris / the StS
clone). Self-quantifying (#9): the page shows its own live heap/scheduler. Caveats — bundle
size, and a canvas-rendered OS is invisible to screen readers / search / awkward on mobile —
resolved by **progressive enhancement**: a normal accessible/indexable HTML portfolio that
*embeds the OS as the hero demo*, not the only path to content. Composes with the StS port
into a recursive flex: *a game you built, on an OS you built, in a browser tab, fully traced,
one click away.*

**Open questions:**
- **VF2 vs an RVV board.** If hardware-accelerated compute / fast 3D matters, the SpacemiT K1
  (RVV 1.0) reframes the board choice — at the cost of a different SoC's MMIO bring-up (still
  RISC-V, no arch port).
- **Display:** SPI panel (easy, ~30–40 fps QVGA ceiling) vs display controller (harder,
  uncapped). Decide early.
- **Games as userspace processes vs kernel tasks** — MVP can be a kernel task (cooperative
  scheduler exists); userspace rides v0.7.
- **Which controller path** (ESP32+Bluepad32 / Pico+TinyUSB / PS2 / Nunchuk / DIY panel).
- **Whether RP1-tier work ever happens** — almost certainly never for this project; serial +
  framebuffer + GPIO/bridge cover the arcade without it.

---

*Cross-references: [roadmap-and-milestones.md](roadmap-and-milestones.md) (the
virtio-RX → kernel-shell → control-plane bullet + its insertion-point open question relate
directly), [observability-design.md](observability-design.md), and the kernel-core extraction
philosophy this whole direction leans on.*
