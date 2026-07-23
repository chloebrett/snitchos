# Porting SnitchOS to the VisionFive 2 (StarFive JH7110)

**Status:** scoping only. No code. This maps the gap between what the kernel
assumes today (QEMU `virt`) and what a real StarFive JH7110 board needs, ranks
the blockers, and proposes risk-ordered milestones. Nothing here is committed
work yet.

## The one-sentence thesis

> The CPU is not the port. SnitchOS is already RV64GC / Sv39 / higher-half /
> SBI-driven, which is exactly what the JH7110's U74 cores are. The port is
> (a) swapping the handful of QEMU-synthetic devices — above all the
> **virtio-console the entire telemetry story rides on** — for real transports,
> and (b) fixing three hardware assumptions baked into boot: the timer
> extension, the RAM base, and how we even get bits onto the board.

Put differently: almost none of the *interesting* kernel (frame allocator,
scheduler, caps, userspace, IPC, RAMfs) is in scope. It's hardware-agnostic and
should run unchanged the moment the machine can keep time and emit a frame. The
work is concentrated at the metal boundary.

## The target, briefly

| | QEMU `virt` (today) | VisionFive 2 / JH7110 |
|---|---|---|
| Cores | `-smp 2`, U54-ish, hartids 0..N | 4× SiFive **U74** (RV64GC) + 1× S7 monitor; boot hart is a U74 |
| RAM base | `0x8000_0000` | **`0x4000_0000`** (2/4/8 GB parts) |
| Timer | **Sstc** (`rdtime`/`stimecmp`) | U74 has **no Sstc** — SBI `set_timer` or M-mode CLINT |
| Console UART | ns16550a @ `0x1000_0000` | `snps,dw-apb-uart` @ `0x1000_0000` (16550-compatible, DesignWare) |
| Telemetry | **virtio-console** (virtio-mmio) | **does not exist** |
| Framebuffer | fw_cfg + ramfb | **does not exist** (real display = JH7110 DC/HDMI driver) |
| PLIC / CLINT | unused (all I/O polled) | SiFive PLIC @ `0x0C00_0000`, CLINT @ `0x0200_0000` (only needed if we go interrupt-driven) |
| Firmware | OpenSBI `fw_dynamic`, S-mode handoff `a0=hartid a1=DTB` | **same** (OpenSBI → U-Boot → payload) — the one big thing that carries over cleanly |
| Delivery | `qemu -kernel elf` | build `Image`, U-Boot `booti` from SD/eMMC/TFTP |

## Ground truth (measured on the board, 2026-07-23)

Read off a live Ubuntu 24.04 boot + the `StarFive #` U-Boot prompt on the actual
**VF2 v1.3B** (JH7110, 4 GiB), post firmware-update to U-Boot 2025.10 / modern
OpenSBI. These retire most of the "measure on the board" unknowns; see
[../notes/visionfive2-first-boot-and-firmware-update.md](../notes/visionfive2-first-boot-and-firmware-update.md).

| Fact | Value | Feeds |
|---|---|---|
| RAM | base `0x4000_0000`, size 4 GiB → `0x1_4000_0000` | B2 |
| OpenSBI M-mode reserved | `[0x4000_0000, 0x4006_0000)` (384 KiB) — keep kernel above | B2 |
| **Kernel load address (LMA)** | **`0x4020_0000`** (`kernel_addr_r`; FIT loads U-Boot here too) = RAM base **+ 2 MiB, same offset as QEMU** | B2 |
| `fdt_addr_r` | `0x4600_0000`; U-Boot passes its DTB via `${fdtcontroladdr}` | handoff |
| **Boot hartid** | U-Boot on **hart 2**, handed Linux **hart 4** — arbitrary in **1..4, never 0** (S7 = hart 0, disabled) | B6 |
| ISA | `rv64imafdc` + `zba zbb zicntr zicsr zifencei zihpm zaamo zalrsc zca zcd` — **no `sstc`** | B1 |
| timebase-frequency | **4 MHz** (`0x3D0900`) | B1 |
| SBI | OpenSBI, spec v3.0; ext: **TIME, IPI, RFENCE, SRST, DBCN, FWFT, HSM, PMU** | B1, M0.5, SMP |
| Console UART0 | `serial@10000000`, `snps,dw-apb-uart`, **`reg-shift=2`, `reg-io-width=4`** (32-bit regs, 4-byte stride), clock 24 MHz | B4 |
| CLINT / PLIC | `0x0200_0000` / `0x0C00_0000` (both < `0x4000_0000`, inside identity MMIO gigapage; same as QEMU) | B5 |
| NIC (future) | 2× `snps,dwmac-5.20` @ `0x1603_0000`/`0x1604_0000`, PHY **Motorcomm YT8531**, RGMII-id, MDIO | M2.5 |
| QSPI layout | `spl@0` `[0,0xf0000)`, `uboot-env@0xf0000`, `uboot@0x100000` | firmware |

**Identity-map consequence for B2:** the RAM identity gigapage moves from QEMU's
root entry 2 (`[0x8000_0000, 0xC000_0000)`) to **entry 1** (`[0x4000_0000,
0x8000_0000)`); the linear-map base shifts to `0x4000_0000`. The MMIO identity
gigapage `[0, 0x4000_0000)` is unchanged and already covers UART/CLINT/PLIC. (Full
4 GiB needs more than one leaf, but first-boot only touches the low 1 GiB.)

## Gap analysis, by subsystem

Ordered by how much it blocks a first boot. Each cites where the assumption
lives so the eventual plan has anchors.

### B1 — Timer: Sstc → SBI `set_timer` *(hard blocker)*

`SstcClock` reads `rdtime` and arms `stimecmp` (CSR `0x14d`) directly
(`kernel/src/trap/mod.rs:95-113`). That's the Sstc extension. QEMU implements
it; the JH7110 U74 cores do not. Without a timer there is no heartbeat, so the
OS has no pulse on the board at all.

- **Work:** a `Clock` impl behind the same trait that arms the timer via SBI
  `sbi_set_timer` (legacy EID `0x0`, or TIME extension). `rdtime` for *reading*
  the clock is fine (U74 has the `time` CSR); only the *arm* has to change.
- **Nice property:** the timer is already trait-shaped (`SstcClock` is one impl),
  and timebase-frequency already comes from the DTB, not a constant
  (`kernel/src/main.rs:153-156`, `kernel/src/dtb.rs:29-43`). So this is a
  swap-in, not a rework.
- **Risk:** low-medium. SBI timer is the oldest, most universally-supported SBI
  call. Main unknown is whether we keep `Sstc` for QEMU and pick at runtime, or
  just move everything to SBI (SBI works on QEMU too — probably simplest to go
  SBI-only and delete the Sstc path, keeping one code path tested on both).

### B2 — RAM base `0x8000_0000` → `0x4000_0000` *(hard blocker)*

The load address, the identity gigapage, and the linear map all hardcode RAM at
`0x8000_0000` (`kernel/linker.ld:3,10`; `kernel/src/mem/mmu.rs:194-262`). On the
JH7110, DRAM starts at `0x4000_0000`, so the S-mode payload lands there and every
one of those constants is wrong.

- **Work:** parameterise the RAM base. Cleanest is a single `RAM_BASE` constant
  the linker script, `pa_to_kernel_va`, and the identity/linear gigapage setup
  all derive from. The higher-half offset math is base-relative already; it's the
  *identity* mappings and the LMA that pin `0x8000_0000`.
- **Risk:** medium. Touches the most delicate boot code (pre-MMU, higher-half
  trampoline). Read `plans/v0.4-memory-findings.md` **first** — this is exactly
  the address-translation minefield it documents.
- **Open question:** what physical address does VF2's U-Boot actually `booti` the
  payload to? That fixes the LMA. Needs to be measured on the board (or read from
  the U-Boot `booti` convention), not assumed.

### B3 — Telemetry transport: virtio-console → UART framing *(the real design work)*

This is the port's *point*. The postcard `Frame` stream is SnitchOS's entire
reason to exist, and today it rides a virtio-console over virtio-mmio
(`kernel/src/device/virtio_console.rs`). The address is DTB-discovered, so it's
portable *in principle* — but the JH7110 has no virtio-console device to
discover. The frames need a physical wire.

- **Chosen direction:** frame the telemetry over a **physical UART**. The board
  has multiple UARTs; dedicate a second one to the frame stream (keep UART0 for
  the human `println!` log), or multiplex if only one serial line is wired out.
- **Why this is clean:** the sink is already a trait (`FrameSink` in
  `kernel-obs`). A `UartFrameSink` that writes postcard bytes to a UART is a new
  impl, not a kernel rewrite. The host side changes too: `collector` reads a
  **serial port** instead of a Unix socket — a new source adapter behind the
  existing decoder.
- **Risk:** medium. The subtleties are framing (postcard frames need
  length-delimiting or a COBS-style boundary on a raw byte pipe — virtio gave us
  message boundaries for free), backpressure (UART is slow; the heartbeat emits a
  lot), and flow control. This is where the genuinely new design lives.
- **Deferred alternative:** keep virtio-console for QEMU and add UART framing for
  hardware, selected at boot. Probably worth it — don't regress the QEMU/snemu
  path that the whole test gate depends on.

### B4 — UART discovery: match `snps,dw-apb-uart` *(easy, but blocks console)*

DTB lookup hardcodes `compatible = "ns16550a"` (`kernel/src/dtb.rs:12-18`); the
comment there already flags that boards reporting `snps,dw-apb-uart` won't match
— which is exactly the JH7110. The *driver* is fine: it's polled and
16550-register-compatible (`kernel/src/device/uart.rs`), and it deliberately does
no baud/divisor init, relying on OpenSBI's config — which holds on VF2 too.

- **Work:** add `"snps,dw-apb-uart"` to the accepted compatible strings **and
  handle the register stride** — the board reports `reg-shift=2` / `reg-io-width=4`
  (32-bit registers spaced 4 bytes apart), vs QEMU's byte-spaced ns16550a. The
  driver hardcodes the QEMU stride, so it must parameterize the shift (from the DTB
  `reg-shift`) or it pokes the wrong offsets. So: compatible string **+ register
  stride**, not a one-liner.
- **Risk:** low (measured, mechanical) — but not zero; the stride is easy to get
  wrong and produces garbage or silence.

### B5 — DTB-before-MMU crash / MMIO hardcoding *(latent blocker for portability)*

MMIO regions are hardcoded (`mmu::QEMU_VIRT_MMIO_BASE = 0x1000_0000`,
`kernel/src/main.rs:72-76`) because DTB *iteration* pre-MMU crashes under the
higher-half link — the parser exists but is parked (`collect_mmio_regions`,
`kernel/src/mem/mmu.rs:36-55`, `#[expect(dead_code)]`).

- **Good luck:** the JH7110 UART is *also* at `0x1000_0000`, and it's inside the
  identity MMIO gigapage `[0, 0x4000_0000)`, so the hardcoded MMIO window
  probably still covers what we need for a first boot. We may not have to fix the
  DTB crash on day one.
- **But:** relying on hardcoded QEMU addresses on real hardware is exactly the
  fragility that bites later. Reviving `collect_mmio_regions` (or parsing the DTB
  in the physical identity window *before* the higher-half switch) is the
  principled fix and the thing that makes the port generalise beyond this one
  board. Flag as a fast-follow, not a milestone-0 gate.
- **Risk:** medium, and annoying to debug (silent pre-MMU crash).

### B6 — hart topology: `MAX_HARTS=2` / `1 - hart_id` → real ids *(now a FIRST-BOOT blocker)*

Bringup is proper SBI HSM (`sbi::hart_start`, `kernel/src/main.rs:453-472`) which
is portable — but `MAX_HARTS = 2` (`kernel/src/smp/percpu.rs:80`) and the
`secondary = 1 - boot_hart` arithmetic (`main.rs:139,456,461`) hardwire exactly
two harts with ids `{0,1}`.

**The board disproves the "deferrable" assumption.** Measured boot hartid is **2
or 4** (never 0; U74s are harts 1–4, S7 is 0). `percpu::init` indexes
`PER_HART_DATA[hartid]` — so a boot hartid of 2–4 with `MAX_HARTS=2` reads/writes
**past the array before the kernel prints anything**. This gates M1 (single-hart
first light), not just SMP.

- **Work (minimum, for M1):** size the per-hart arrays for physical hartid ≤ 4
  (`MAX_HARTS ≥ 5`, S7 at 0 included) **or** remap physical hartid → logical index
  before the first per-hart access. Boot only the boot hart.
- **Work (full, later):** iterate the U74 harts from the DTB; drop the `1 -
  hart_id` "other hart" arithmetic. Per-hart arrays, runqueues, exception stacks
  all sized by `MAX_HARTS`.
- **Risk:** medium. The M1 slice (array sizing / remap) is small but **mandatory**
  — a silent OOB at boot is exactly the no-output failure M0.5 exists to avoid.

### Dropped for now — framebuffer (fw_cfg / ramfb)

fw_cfg is a QEMU invention at a hardcoded port (`kernel/src/device/fwcfg.rs:18`,
`0x1010_0000`) and ramfb rides it (`kernel/src/device/ramfb.rs`). Neither exists
on hardware. A real display means writing a JH7110 display-controller + HDMI
driver — a whole project. **Out of scope.** The framebuffer / physics-desktop
work stays QEMU-only until someone wants to write that driver. Guard it so the
board build simply doesn't call `ramfb::init` (`kernel/src/main.rs:286`).

## Proposed milestones (risk-ordered)

Each leaves the tree in a known-good state and — critically — **does not regress
the QEMU/snemu test gate**, which everything else in the repo depends on.

- **M0 — Get bits onto the board, get a serial console.** No kernel code.
  Establish the physical loop: USB-serial adapter on the console UART, U-Boot
  prompt reached, a known-good `Image` booted to prove the board + toolchain +
  delivery path. This de-risks everything: without it, every kernel bug looks the
  same (silence). *This is the real first task and it's logistics, not Rust.*
  Full mechanics in **[Bring-up mechanics](#bring-up-mechanics-m0-in-detail)**
  below.

- **M1 — First light: boot to the human UART log.** B4 (match
  `snps,dw-apb-uart`) + B2 (RAM base) + B1 (SBI timer). Success = the NS16550
  boot log and a heartbeat *tick* over the human UART on real silicon. No
  telemetry frames yet. This proves MMU, higher-half, trampoline, and time all
  work on hardware — the scariest 20%.

- **M2 — Telemetry on hardware.** B3: `UartFrameSink` + collector serial source.
  Success = spans and metrics from the real board decoded by the collector and
  landing in Grafana. **This is the milestone that makes the port *mean*
  something** — SnitchOS observing real hardware is the whole pitch.

- **M2.5 (optional, big) — Ethernet-native telemetry.** Replace/augment the UART
  frame sink with a real JH7110 GMAC driver + minimal ARP/IP/UDP, streaming
  frames to the collector over the network. See
  **[Ethernet: three different asks](#ethernet-three-different-asks)** — this is
  its own project (NIC driver + PHY bring-up + DMA rings), *not* a rider on M2.
  Thematically the strongest milestone (an observability OS streaming telemetry
  over the wire = what real production nodes do), but scoped as a deliberate,
  separate step. Boot-over-Ethernet (TFTP) is unrelated to this and comes for
  free at M0.

- **M3 — Multi-hart.** B6: generalise `MAX_HARTS` and hart iteration, bring up
  all four U74s. Success = the SMP itest scenarios' spirit (cross-hart
  producer/consumer) running on real cores.

- **M4 (deferred) — Portability hardening.** B5: real DTB-driven MMIO discovery
  so the kernel isn't leaning on QEMU addresses that happen to coincide.

- **Not planned — framebuffer.** Separate project; needs a native display driver.

## Bring-up mechanics (M0 in detail)

The practical loop, worked out. **No soldering** — the VF2's 40-pin GPIO header
ships pre-populated with pins; you push jumper wires straight on.

**Hardware you need:**

- **USB-to-TTL serial adapter — 3.3V** (CH340 / CP2102 / FTDI, ~$5–10). Your
  console; without it a failed bare-metal kernel is just a dark board. **Must be
  3.3V logic** — the header is 3.3V-only and a 5V adapter can *damage the board*.
- 3× female-female dupont jumper wires (usually bundled with the adapter).
- microSD card (8 GB+) + a host SD reader — for the one-time known-good image and
  as a fallback boot source.
- USB-C power supply (5V/3A).
- Ethernet: **either** a LAN cable from the board to your home router (Mac stays
  on WiFi — same subnet, works; see topology note), **or** a USB-C Ethernet
  adapter for a direct Mac↔board cable. See the Ethernet section for the choice.

A labeled schematic board map + 40-pin UART pinout lives at
[`docs/visionfive2-board-map.html`](../docs/visionfive2-board-map.html) (open in a
browser).

**Serial console wiring** (verified against the VF2 Quick Start): board
**pin 6 → GND**, board **pin 8 (UART0 TX) → adapter RX**, board **pin 10 (UART0
RX) → adapter TX** (TX/RX cross over). Terminal at **115200 8N1**.

**Boot chain:** BootROM → SPL → OpenSBI → **U-Boot** (all in onboard SPI-NOR
flash, preloaded on current boards) → *U-Boot loads SnitchOS*. You live at the
U-Boot prompt; that last arrow is the only part we drive. One-time exception: an
early board rev that didn't ship U-Boot in flash needs it written once via the
UART-recovery path (boot-mode jumpers **Switch_2 = UART = `RGPIO_1,RGPIO_0 =
1,1`**, run StarFive's recovery tool, set jumpers back). Most current boards skip
this.

**The dev loop (TFTP boot — the good one):** U-Boot has its own network stack, so
booting over Ethernet is *free and needs zero kernel code* — it happens before
SnitchOS runs. Mac runs a **TFTP server** (built-in `tftpd`, or `brew install
tftp-hpa` / `dnsmasq`) pointed at a folder; the kernel image lives in that folder.
At the U-Boot prompt:

```
dhcp                                    # or static: setenv ipaddr <board-ip>
setenv serverip 192.168.1.50            # the Mac's IP
tftpboot 0x40200000 snitchos.img        # pull kernel into RAM
booti 0x40200000 - ${fdtcontroladdr}    # boot it; ${fdtcontroladdr} = U-Boot's own DTB
```

`${fdtcontroladdr}` handing over U-Boot's DTB is a freebie: it satisfies
SnitchOS's `a1 = DTB` handoff with the board's *real* devicetree, no DTB of our
own to supply. Save these into U-Boot's `bootcmd` and the board auto-fetches on
reset — rebuild on the Mac → power-cycle → fresh kernel, no SD shuffling.

**SD vs TFTP:** SD (`fatload mmc … ; booti`) means popping the card each build —
fine for the first known-good boot, slow to iterate. TFTP is the iteration loop.

## Ethernet: three different asks

"Use Ethernet for everything" splits into three asks at very different layers —
one is nearly free, two are a real project. Don't conflate them.

1. **Boot the kernel over Ethernet — FREE, works today.** Pure U-Boot TFTP (see
   above). Happens *before* our kernel runs; no SnitchOS code. Do this now.
2. **Read logs / telemetry from the running kernel over Ethernet — needs a NIC
   driver.** U-Boot's network stack evaporates at `booti`; the running kernel has
   the bare machine. For SnitchOS to speak Ethernet it must drive the JH7110
   **Synopsys DesignWare GMAC** (`dwmac`): MAC init, **PHY bring-up over MDIO**
   (board-specific reset GPIO + clock config in the JH7110 syscon — where
   bare-metal NIC bring-up eats days), **DMA descriptor rings**, then a **minimal
   ARP/IP/UDP** to address frames at the Mac. This is M2.5 — bigger than the rest
   of the port combined to get the *first* frame out.
3. **Send messages *to* the kernel over Ethernet — NIC driver + RX path + a
   command protocol.** Same driver dependency plus new inbound surface the kernel
   has no notion of today. Furthest out; thematically aligned with the
   actor-model / typed-processes design (processes reachable as network
   endpoints).

**Recommendation: don't couple them.** UART carries telemetry first (M2 — the
`FrameSink` is already a swappable trait, the collector already speaks network on
the host side), Ethernet-native is the M2.5 upgrade once you want speed +
bidirectionality. UART-now isn't throwaway: the sink is abstracted and the B3
framing design mostly carries over.

**One codebase tailwind for the GMAC path:** the DMA-descriptor discipline isn't
foreign — SnitchOS already learned "devices see physical addresses" (the
`TX_STAGING` / `va_to_pa` staging in `virtio_console`) and has the linear-map
machinery to hand PAs to a device. The NIC is just the most DMA-heavy device it'd
have written.

**Pragmatic shortcut for "network logs" before the driver exists:** bridge on the
host — run a serial-to-TCP bridge (`ser2net`) on the Mac so the board's UART
telemetry is reachable over a TCP socket by the collector. "Read logs over the
network" with zero kernel work; just not the board speaking IP natively.

**Host↔board network topology (either works, no crossover cable — modern GbE is
auto-MDI-X):**

- **Board→router by cable, Mac on WiFi.** Simplest, *no USB-C adapter needed* — a
  home router bridges WiFi + wired into one subnet, so they can talk. Router does
  DHCP. Caveat: use your **main** WiFi, not a guest network (guest/"AP isolation"
  blocks device-to-device traffic).
- **Direct Mac↔board cable.** Needs the USB-C Ethernet adapter. No DHCP, so set
  **static IPs** both ends (e.g. Mac `192.168.2.1`, board `192.168.2.2`, `/24`;
  Mac adapter → *Configure IPv4: Manually*, gateway blank; board U-Boot `setenv
  ipaddr` / `serverip`). Mac keeps WiFi for internet on a separate interface —
  macOS routes board traffic out the cable, everything else over WiFi. Fully
  self-contained + deterministic (works with no router anywhere); the cost is the
  adapter + a one-time IP config.

## Biggest unknowns to de-risk early

1. **Delivery + serial loop (M0).** Everything downstream is undebuggable
   without it. Do this before writing a line of kernel code. *(Board boots stock
   Ubuntu — the hardware/firmware half is proven; the serial adapter is en route.)*
2. ~~**The `booti` load address on VF2.**~~ **Resolved:** `0x4020_0000` (= RAM
   base + 2 MiB, same offset as QEMU). See ground-truth table.
3. ~~**Does OpenSBI hand off S-mode identically?**~~ **Mostly resolved:** standard
   `a0=hartid`, `a1=DTB` handoff, modern OpenSBI (SBI v3.0, DBCN/TIME/HSM present).
   The *live* wrinkle is the **non-zero, non-deterministic boot hartid (1–4)** —
   see B6, now a first-boot blocker.
4. **How does U-Boot launch a bare kernel?** `booti` wants a RISC-V *Image*
   (64-byte header) we don't emit; options are `bootelf` on the ELF or adding an
   Image header. Now well-scoped (load addr known); resolve before M1.
5. **UART framing design (B3).** The one genuinely new design problem —
   boundaries, backpressure, flow control on a raw byte pipe. Worth a small
   design note of its own before M2.

## What this port is *not*

It is not a rewrite, and it is not "make RISC-V run on RISC-V." The kernel's hard
parts already run on the U74's exact ISA. The port is a thin, sharp shell of
metal-boundary work — a timer call, a base address, a compatible string, and one
real transport design — wrapped around a body of code that doesn't need to
change. The risk is concentrated, not spread; M0 and M1 hold almost all of it.

## Reference docs

StarFive's docs are unusually good for an SBC vendor. Mapped to the blockers:

- **JH7110 TRM** — <https://doc-en.rvspace.org/JH7110/PDF/JH7110_TRM_StarFive_Preliminary_V2.pdf>
  — the main one. System memory map (B2 RAM base, B5 MMIO), UART registers (B4),
  CLINT + timer (B1), PLIC, GMAC (M2.5).
- **JH7110 Datasheet** — <https://doc-en.rvspace.org/JH7110/PDF/JH7110_Datasheet.pdf>
  — hart topology (B6), which ISA extensions the U74 actually implements (confirms
  *no Sstc* → B1).
- **VF2 Datasheet** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_Datasheet.pdf>
  — board pinout (which pins the console UART is on).
- **VF2 Quick Start Guide** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_QSG.pdf>
  — debug-pin wiring + boot-mode jumper table (M0).
- **VF2 40-Pin GPIO Header UG** — <https://doc-en.rvspace.org/VisionFive2/40-Pin_GPIO_Header_UG/VisionFive2_40pin_UG/gpio_pinout%20-%20vf2.html>
- **Recovering the Bootloader** — <https://doc-en.rvspace.org/VisionFive2/SDK_Quick_Start_Guide/VisionFive2_SDK_QSG/recovering_bootloader%20-%20vf2.html>
  — the one-time U-Boot-to-flash path.
- **VF2 SDK Quick Start Guide** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_SDK_QSG.pdf>
  — boot chain + flashing.

**What the manuals *can't* tell you** (measure on the board / read the board DTS):
the actual `booti` load address (B2's LMA), the exact DTB layout + non-zero boot
hartid (M1 handoff). SBI semantics (B1 `set_timer`, B6 HSM `hart_start`) are the
[RISC-V SBI spec](https://github.com/riscv-non-isa/riscv-sbi-doc), not StarFive's;
the board just ships an OpenSBI that implements them.
