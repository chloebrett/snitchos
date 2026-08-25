# Driving the VisionFive 2 audio-out jack (JH7110 PWMDAC)

**Status:** 📐 **DESIGN — not started.** A scoping/design analysis, not an
implementation plan: it collapses the build-vs-defer uncertainty around the 3.5mm
analog audio-out jack on the VisionFive 2, establishes the hardware facts, and stages
the work into tiers. The TDD-decomposed increments become a `plans/vf2-audio*.md`
plan once Tier 0 is greenlit. The headline finding: **a first beep needs no DMA
engine** — the DAC's sample register is plain MMIO the CPU can write directly, so
Tier 0 is CPU-PIO and a weekend-sized win; the DMA engine is a separate, larger
sub-project needed only for gapless streaming.

Prereq: this builds on the hardware port
([../plans/visionfive2-port.md](../plans/visionfive2-port.md), M1 first light
achieved 2026-07-24). It is peer to, and shares the SoC-integration gap with,
[../plans/vf2-display.md](../plans/vf2-display.md) — both want peripheral clock/reset
(SYSCRG) bring-up the kernel does not have yet.

---

## What the jack actually is

The VF2's 3.5mm analog out is driven by the JH7110's on-chip **PWMDAC** — a
PWM/sigma-delta DAC, **not** an I2S codec. This matters: there is **no external
codec** to configure over I2C, no MCLK/BCLK/LRCLK wiring, no ALSA-codec dance. The
whole path is one on-chip peripheral, its two clocks + one reset, and whatever feeds
its sample register. (I2S on this SoC is the HDMI-audio and mic/PDM path — a
different, heavier animal we are *not* touching here.)

## Hardware facts (mainline-driver + datasheet confirmed)

Sourced from mainline Linux `sound/soc/starfive/jh7110_pwmdac.c`,
`arch/riscv/boot/dts/starfive/jh7110.dtsi`,
`drivers/clk/starfive/clk-starfive-jh7110-sys.c`, and the JH7110 Datasheet v1.63.

**The block is tiny — two registers.** Base `0x100b0000`, size `0x1000`.

| Offset | Name | Meaning |
|---|---|---|
| `0x00` | `WDATA` | Sample-data write port (fixed-address, non-incrementing) |
| `0x04` | `CTRL` | Control |

`CTRL` (0x04) bitfields:

| Field | Bits | Notes |
|---|---|---|
| `ENABLE` | `[0]` | master enable |
| `SHIFT` | `[1]` | 8-bit vs 10-bit resolution (driver default: 8-bit) |
| `DUTY_CYCLE` | `[3:2]` | PWM duty mode (default: center) |
| `CNT_N` | `[12:4]` | sample-count / clock divider — sets output rate |
| `DATA_CHANGE` | `[13]` | |
| `DATA_MODE` | `[14]` | inverter / MSB (default: inverter-MSB) |
| `DATA_SHIFT` | `[17:15]` | data left-shift (default: 0) |

**No stereo-select bit.** Mono = write one 16-bit sample to `WDATA`; stereo = write
a packed 32-bit `L|R` word. Channel count in Linux only sets the DMA word width
(2 vs 4 bytes) — interleaving is purely *what word you write*.

**Clocks + reset (all in SYSCRG @ `0x13020000`):**

- `pwmdac_apb` — gate; parent `apb0`.
- `pwmdac_core` — gate **+ divider** (÷ up to 256); parent chain
  **PLL2 → audio_root (÷ up to 8) → pwmdac_core**. This is the clock whose *rate*
  selects the sample rate (e.g. 6.144 MHz → 8 kHz, 11.2896 MHz → 44.1 kHz,
  12.288 MHz → 48 kHz).
- `pwmdac_apb` **reset** — deassert before use.

**The feed, in Linux:** DMA-only. The driver registers a dmaengine PCM and points
its slave address at `WDATA`; it never polls a status bit. The DMA controller is the
**Synopsys DesignWare AXI DMAC** (`dma-controller@16050000`,
`compatible = "starfive,jh7110-axi-dma"`, request line **22**) — a full
scatter-gather block-LLI engine with a global block + per-channel banks and in-memory
linked-list descriptors. Large. JH7110's variant notably needs *two* resets.

> ### 🔑 The finding that collapses the Tier 0/1 fork
>
> `WDATA` is an **ordinary MMIO register**. The DMA path merely uses it as a
> fixed-address write target with no back-pressure register the driver ever reads.
> Nothing in the hardware description says the DAC only advances on DMA bursts.
> **Therefore the CPU can write `WDATA` itself at the sample cadence and get the same
> result the DMA would** — no DMA engine required for a beep.
>
> **The one caveat, and the only real hardware unknown:** no FIFO / not-full / level
> status bit is documented in *any* source (the driver defines exactly two
> registers). So a PIO loop **cannot poll "is there room?"** — it must **rate-pace in
> software**, writing one sample every `1/fs` using the existing `mtime` busy-wait.
> For a *fixed-frequency* tone, where we control both the sample rate and the emit
> cadence, that is completely adequate. Whether `WDATA` has any input latch/FIFO
> depth (which would forgive small timing jitter) is the thing to confirm on
> hardware or in the datasheet's register chapter — but it does not block Tier 0.

## What the kernel has to lean on (from the current tree)

- **MMIO region mapping — likely a zero-line change for the DAC.** `0x100b0000`
  aligns to the 2 MiB megapage `0x10000000`, which is the *same megapage as UART0*
  (`0x10000000`) that `kmain` already inserts (`kernel/src/main.rs:94-95`). So the
  DAC registers are probably already mapped. **SYSCRG** at `0x13020000` (megapage
  `0x13000000`) is *not* — that's one new `mmio_regions.insert(0x1300_0000)`, well
  within the 16-megapage `CAP`.
- **Register-access idiom:** the `base: usize` + inline `read_volatile`/`write_volatile`
  pattern from the ns16550a UART (`kernel/src/device/uart.rs`), with the host-testable
  layout/bitfield half living in a `kernel-devices` module (mirror
  `kernel-devices/src/uart.rs`). No shared ioremap/`Volatile<T>` helper exists; don't
  invent one for this.
- **Board-constant selection:** the `vf2` cargo feature +
  `kernel/src/device/console.rs:97-104` pattern for compile-time board constants.
- **Timing:** the existing `mtime`/timer path (`kernel/src/sbi.rs`, the `Clock`
  abstraction) is the sample-cadence source for PIO pacing.

**Everything else is greenfield.** No SYSCRG clock/reset code exists anywhere in the
tree (confirmed: zero `crg`/`clk` hits outside the *timer*). No DMA engine of any
kind (virtio's descriptor rings are the virtio spec's own shared-memory DMA — *not* a
reusable SoC DMA engine, and carry over to nothing here). No audio/PWM/I2S/PDM code.

## The tiers

### Tier 0 — first beep (CPU PIO). *~1 weekend, gated on nothing external.*

Deliver: the board emits a tone from the jack. New work:

1. **SYSCRG bring-up (the new infra):** ungate `pwmdac_apb` + `pwmdac_core`, ensure
   `apb0` / `audio_root` / PLL2 upstream are running (they should be, post-U-Boot),
   set `pwmdac_core` to a low rate (target 8 kHz), deassert `pwmdac_apb` reset. Model
   the SYSCRG gate/divider/reset registers host-testably in `kernel-devices`; the
   kernel side is the MMIO writes + the one `insert(0x1300_0000)`.
2. **PWMDAC config:** program `CTRL` (`cnt_n`, `shift`, `duty_cycle`, `data_mode`,
   `data_shift`), then set `ENABLE`.
3. **PIO tone:** a precomputed square/sine LUT; a timed loop writes one sample to
   `WDATA` every `1/fs` off `mtime`.

Risk is concentrated in (1) — first-ever CRG code — and in the documented unknown
(no FIFO status; if `WDATA` has a one-deep latch, expect audible artifacts on a
mistimed write, never a hang). Both are acceptable for a beep.

**This is the milestone that proves "the board makes a sound."** Everything above
here is deferrable.

### Tier 1 — gapless streaming (dw-axi-dmac). *Several × Tier 0. The real sub-project.*

The AXI DMAC driver: channel setup, the per-channel register banks, in-memory LLI
descriptor chains, the memory→`WDATA` fixed-address slave mode, request line 22, the
two-reset quirk, and a ring-refill interrupt. This is a driver in its own right and
dwarfs the DAC. **Build it generically** — the display port
([../plans/vf2-display.md](../plans/vf2-display.md)) and anything else DMA-fed reuses it, so it's not
throwaway. Only start it once Tier 0 has proven the DAC and you actually want clean,
continuous audio.

### Tier 2 — the SnitchOS payoff. *Small on top of Tier 1 (or even Tier 0).*

Audio as an **observability output channel**, which is the only reason this belongs
in *this* OS rather than being a toy:

- **Sonify the telemetry:** heartbeat → tick, context-switch → click, OOM → falling
  tone, panic → buzz. A new `Frame` sink that happens to be analog.
- **Hear a deterministic replay:** teach [snemu](snemu-design.md) a PWMDAC model (it already
  folds frames and models MMIO devices), and you can *hear* a boot replay — or diff
  two boots by ear. Converges with the snemu-wasm + collector-as-server direction.

> **Emulator audio — snemu yes, QEMU no.** The testing-useful slice of this (capture
> the `WDATA` write stream, render a `.wav`, listen) is pulled *forward* into Tier 0
> as [../plans/legacy/vf2-audio-tier0.md](../plans/legacy/vf2-audio-tier0.md) Increment 9: it gives
> by-ear confirmation without hardware and an exact itest oracle. It works because
> snemu is a deterministic MMIO-device emulator and the guest's `mtime`-paced writes
> already encode the sample rate — so snemu need model no analog clock. **QEMU can't
> do this**: it has an audio subsystem (`-audiodev`) but its audio device models are
> PCI/virtio (`intel-hda`, `virtio-sound`), never a StarFive PWMDAC, and our itests
> run on `virt`/snemu where `0x100b0000` isn't mapped at all. Modelling it in QEMU
> would mean a custom C device model for no gain over the snemu route.

This reframes the whole thing from "make the board beep" to "a new telemetry
channel," which is on-thesis. The roadmap already gestures at it:
`docs/roadmap-and-milestones.md:78` ("over-engineered audio: RT deadlines, XRun
forensics") and `docs/arcade-and-real-hardware-direction.md:189` (the PWM-vs-I2S-DMA
fork).

> ### Far-future capstone: `Frame` as a network protocol over an acoustic PHY
>
> The north star the whole audio arc points at (impractical today, noted so it
> isn't lost). The insight: **because SnitchOS's I/O is already `Frame`s, the
> transport is pluggable** — Frame-over-audio is a peer of Frame-over-UART (the
> `run_source` seam already abstracts "source"). Make it *bidirectional between two
> instances* and `Frame` stops being a telemetry format and becomes a **network
> protocol**; two acoustically-coupled microkernels gossiping their own
> observability is just the most delightful demo of that general truth (Frame-over-
> IR / -socket / -QR would work too; audio's the one that sounds like 1996).
>
> Built from scratch, the stack is: **acoustic PHY** (PWMDAC out, PDM/I2S mic in) →
> **FSK modem / data-link** (mod/demod; clock recovery is the hard DSP) → **COBS
> frame transport** (already done) → an app protocol where instances stream
> telemetry at each other or RPC over sound. No new wire protocol — reuse `Frame`.
>
> A ladder, not a cliff, each rung reusing the last:
> 1. **TX → host** — SnitchOS FSKs frames out, a host demodulator recovers them.
>    Reachable *today* on the Tier-0 work (`glitch` + an NCO + FSK); no board input
>    needed. Deterministic: demodulate a recorded snemu boot, get the frames back.
> 2. **host → board RX** — needs the JH7110 mic/ADC driver + a fixed-point
>    demodulator (Goertzel + bit-timing recovery) — a motivating first consumer of
>    userspace DSP (hence lazy-FP-context-switch).
> 3. **board ↔ board** — full duplex, acoustic-coupler style (speaker→mic across
>    air, à la 1970s handset cups) or a line-out→line-in cable.
>
> Everything before this in the audio arc (beep → `glitch` server → sonification →
> modem TX) is a rung toward it.
>
> **What a Frame-network buys — distributed primitives, reimagined honestly.** The
> acoustic PHY is a forcing function against the two biggest lies in the Unix model:
> - **Remote access as capability delegation, not ambient login.** "ssh into another
>   instance" = the host *delegates a specific cap bundle* to the remote principal
>   over the link; every remote invocation is a `CapEvent` the host sees. A live cap
>   can't teleport (it's a local slot/generation), so the mechanism is a **local
>   proxy** holding the real caps and mediating invocation-*requests* carried as
>   frames (CapTP / E-lineage). Extends the explicit-authority shell across the gap.
> - **Cross-machine `~>` — the shipped typed/cap-checked pipe over a new transport.**
>   `~>` is *already* Stitch's cross-process pipe: `a ~> b` is legal iff `a.out`
>   marshals into `b.in` (structural — the "hitch") *and* passes the cap-compatibility
>   check, rejected pre-spawn. The acoustic link is just a new **transport** for it —
>   extend the boundary to a Frame-over-audio link to another instance and you get
>   cross-*machine* `~>` (the design already lists cross-*language* `~>` as pending;
>   cross-machine is its sibling). "ssh" above and `~>` here are the **same
>   mechanism** — one interactive-session flavour, one typed-pipe flavour — both
>   proxy-mediated. The transport's lossiness is orthogonal: the COBS Resync policy
>   (`DecodeError.consumed`, `0x00`-delimited) absorbs a dropped frame, independent of
>   `~>`'s typed/authority meaning.
> - **Reliability is a transport policy, chosen per channel (TCP-vs-UDP over sound).**
>   Extends the Lossless/Resync policy pair with a third: **Reliable (ARQ)** — CRC per
>   frame + retransmit-on-NAK, ordered. But *telemetry doesn't want retry* (a dropped
>   heartbeat is stale — retransmitting is worse than useless), so sonification/gossip
>   channels stay best-effort (Resync); only *data* channels (`~>` bytes, an ssh
>   session) opt into Reliable. Same acoustic PHY, policy per channel. The maximalist
>   version is **literal PPP-over-the-modem → TCP/IP** (actual dial-up; builds on the
>   `kernel-net` crate), vs. the lighter frame-native ARQ.
> - **Half-duplex turn-taking = `Call`/`Reply` IPC.** Acoustic is naturally
>   half-duplex (one speaker/mic, shared air), so it needs turn-taking — a "walkie-
>   talkie" *over* protocol, whose per-turn **line-turnaround** delay is the source of
>   the lag. But that's not bolted on: it *is* the kernel's shipped `Call`/`Reply`
>   rendezvous — the message boundary is the "over," the `Reply` hands the line back.
>   Consequences: interaction is **line-mode** (char-at-a-time would pay a turnaround
>   per key — and line-mode is historically authentic + fine for a REPL); a lost
>   turn-token deadlocks/collides, fixed by a **turnaround timeout** — which is the
>   *same* machinery as ARQ (a lost "over" is a lost ACK). Escape hatch for smooth
>   char-mode: **full-duplex** via frequency-division (each direction its own tone
>   band, à la Bell 103) or a stereo/2-wire cable (full-duplex free).
> - **The modem is parameterizable — build both duplex modes, don't pick one.**
>   Duplex isn't a mode, it's *band allocation*: `bands: Shared(b)` → directions
>   collide → must turn-take (half); `Split{tx,rx}` → simultaneous (full/FDD). So
>   turn-taking is a *consequence* of shared-band, not a switch. The modem factors
>   into orthogonal axes — a `ModemConfig { modulation (FSK freqs → PSK/QAM), baud,
>   bands (=duplex), reliability (BestEffort/ARQ), COBS }` — layered: shared
>   modem-core (NCO + demod) → duplex layer → reliability → frames. The modem is a
>   **`glitch` client** (glitch plays samples, never learns what a modem is; all
>   config lives userspace).
> - **You can TDD a modem, because the wire is deterministic.** mod/demod are pure
>   round-trips; and the *whole path* is testable since snemu captures `WDATA`
>   bit-for-bit: guest modulates → snemu captures samples → host demodulates → assert
>   the bytes returned. Every config point is a host-testable round-trip, turning the
>   config space into an A/B experiment platform (half vs full throughput, FSK vs PSK)
>   — the allocator-/snemu-backend-A/B measurement culture, aimed at a modem.
> - **Two snemu instances, audio-linked digitally — the test strategy for the whole
>   arc.** No real audio need be harmed: run machine A and machine B, pipe A's
>   PWMDAC sample-output into B's audio-*input* and vice versa (a deterministic
>   sample-shuttle where speaker→air→mic would be). Everything runs and is tested —
>   FSK mod/demod, framing, protocol, cross-machine `~>` — *except* the analog PHY,
>   which was never the interesting part. It's **deterministic end-to-end** (two
>   deterministic machines + a deterministic shuttle), so it's a *stronger* claim
>   than single-instance TDD: a reproducible two-machine acoustic-protocol test with
>   zero hardware. Real acoustic audio becomes the **moneyshot demo**, not a
>   dependency. Needs: an audio-*input* device in snemu (mirror of `pwmdac_samples()`
>   — and *also* rung-2's missing mic path, so building it for the test builds it for
>   real), a `snemu link a.elf b.elf` harness, and **deliberate channel impairment**
>   (`--loss`, `--snr`) as a *knob* to exercise Resync/ARQ on purpose. Generalizes:
>   two instances + any digital link = a deterministic distributed-systems test
>   harness; audio is just the first, most-charming instance.
> - **Throughput reality:** a from-scratch FSK modem is ~300–1200 bps; the Shannon
>   ceiling of a voiceband channel is ~35 kbps (why V.34 stopped at 33.6k), and
>   over-the-air acoustic is ~100 bps–1 kbps. At ~1200 bps an ssh session is a
>   usably-slow vintage terminal and telemetry is a few frames/sec — enough for the
>   demos, and reliability/ARQ spends some of that budget on ACKs + retransmits.

## Floating point: deliberately none (a kernel-wide invariant)

The kernel has emitted **zero floating-point instructions** to date. That is
load-bearing, not incidental: FP register state (`f0–f31` + `fcsr`) is **not** in
`TaskContext` and is **not** saved on trap entry, so `sstatus.FS` can sit at `Off`.
The first FP instruction in kernel code either traps (FS=Off) or silently corrupts FP
state across context switches. Audio *feels* float-heavy, but Tier 0 keeps the
invariant intact:

- **Sine/square LUT** = a `const [i16; N]` baked into the binary, generated at *build
  time* (`build.rs`, where floats are free) or via a `const fn` integer approximation.
  Zero runtime FP.
- **Gain** = fixed-point (Q0.16 multiply + shift), not a float.
- **Rate/divider + PIO pacing** = integer.

**Decision: FP lives in userspace, never the kernel.** The kernel's zero-FP property
is now a *permanent invariant*, not a Tier-0 convenience. Real DSP (resampling,
filters, an FFT for XRun forensics, softvol in dB) is float-natural, and all of it
belongs in userspace. The rejected alternatives, for the record: fixed-point DSP
in-kernel (hand-rolled Q-format, keeps the invariant but pushes float-natural work
into the wrong place), and `kernel_fpu_begin/end`-style regions (adds FP-save cost and
risk to the kernel for no benefit once userspace can do it).

**What this makes prerequisite.** Userspace FP requires a discrete new kernel feature:
**lazy FP context switching** — `sstatus.FS` Dirty-tracking plus save/restore of
`f0–f31` + `fcsr` across the switch (they are not in `TaskContext` today). This gates
the **DSP**, not raw streaming: Tier 1's DMA PCM path is integer and needs no FP, so
this feature can land independently, whenever the first userspace float-consumer
does. **On-thesis payoff:** the `FS`-Dirty bit tracked for *correctness* is exactly
the signal for a *snitch* — "task went FP-dirty," lazy-FP trap counts, first-FP-use
spans. FP state becomes observable for free as a side effect of supporting it.

## Recommendation

Do **Tier 0 now** — it is the cheapest path to a real "hardware makes a sound" win
and the SYSCRG code it forces is needed by the display port anyway. Defer Tier 1
until gapless audio is actually wanted, and build the DMAC generically when you do.
Tier 2 is the reason to bother at all; it's cheap once either feed path exists.

**Before writing a line:** the highest-value hardware check is confirming the
`WDATA` FIFO/latch depth and any status bit against the datasheet's PWMDAC register
chapter (or by ear on the board) — it's the only unresolved unknown, and it only
affects how forgiving the PIO pacing must be, not whether Tier 0 is possible.

## Open unknowns

- **`WDATA` FIFO/latch depth + any status bit** — undocumented in the driver;
  verify on hardware/datasheet. Affects PIO pacing tolerance only.
- **Upstream clock state after U-Boot** — assumed `apb0`/`audio_root`/PLL2 are
  already running; confirm, else PLL/root bring-up is added Tier 0 scope.
- **SYSCRG base** — datasheet register chapter says `0x13020000`; the clock *IDs* are
  authoritative regardless. Confirm the exact base before the first write.
- **Analog path enable** — whether the jack needs any board-level mux/amp-enable GPIO
  beyond the DAC itself (the port's `visionfive2-port.md:560` "reset GPIO + clock
  config in the syscon" note hints board glue may exist).

## References

- [../plans/visionfive2-port.md](../plans/visionfive2-port.md) — the hardware port; M1 first light.
- [../plans/vf2-display.md](../plans/vf2-display.md) — peer sub-project; shares the SYSCRG gap and
  would reuse the Tier 1 DMA engine.
- Mainline `sound/soc/starfive/jh7110_pwmdac.c`, `jh7110.dtsi`,
  `clk-starfive-jh7110-sys.c`, `drivers/dma/dw-axi-dmac/` — the authoritative feed
  model, register map, and clock chain.
- JH7110 Datasheet v1.63 — register chapter for the FIFO/status unknown.
