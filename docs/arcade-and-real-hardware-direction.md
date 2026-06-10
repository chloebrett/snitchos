# 🕹️ Direction: the arcade as SnitchOS's observability showpiece (+ real hardware)

> **Status: exploratory.** This captures a design conversation about where SnitchOS could go
> next — a physical arcade demo on real RISC-V hardware. It is *not* yet committed to
> [roadmap-and-milestones.md](roadmap-and-milestones.md); it's the reasoning and the
> recommended shape, to be folded into the roadmap once decided. Hedges are deliberate:
> prices, perf numbers, and tooling maturity are ballparks with wide error bars.

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

- **Tetris:** platform + **~1–3 days** of pure-logic game (host-tested). ≈ **2–2.5 weeks
  total in QEMU**, ~90% reusable platform.
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

## 10. Recurring principles (the through-lines)

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

## 11. Candidate sequencing & open questions

**Sequencing (smallest demoable first):**
1. Prove the arcade in QEMU: `Framebuffer`/`Input` ports → virtio-gpu-2D/ramfb + serial
   input → **Tetris on SnitchOS**, in a window.
2. Observability over it: frame-time + input-latency spans → Grafana.
3. Board bring-up: serial console → framebuffer (SPI panel first; display controller if you
   need to uncap fps) → GPIO/bridge input.
4. **2D before 3D; solo before multiplayer; Mac-host-as-P2 before 2-board.**
5. Network REPL (dwmac + smoltcp) when untethered interaction is wanted.

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
