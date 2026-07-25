# Tier 0 — first beep from the VF2 audio jack (CPU PIO)

**Status:** 🚧 **IN PROGRESS — the whole pure/host-testable layer is done.** Increments
1, 2, 4, 5 (`kernel-devices/src/pwmdac.rs`) and 3 (`kernel-devices/src/syscrg.rs`)
shipped — 33 host tests, clippy-clean, mutation-verified (the only survivors are the
documented `| → ^` disjoint-field equivalents). Next up is **9a** — the snemu PWMDAC
device model + a pure (kernel-independent) WAV decoder — which now sequences **before**
the kernel glue: snemu halts on unmapped MMIO writes, so its PWMDAC region is a
prerequisite for anything audio to run under snemu, and it's the harness that makes the
kernel work observable off-hardware. Full order: **9a → 6 → 7 → 8 → 9b**. Also
outstanding: **4b** (sine LUT via `build.rs`). TDD-decomposition of Tier 0 from
[../docs/vf2-audio-design.md](../docs/vf2-audio-design.md). Goal: **the board emits a
fixed-frequency tone from the 3.5mm jack**, fed by CPU PIO writes to the JH7110
PWMDAC — no DMA engine. Everything here is deferrable-free (gated on nothing
external); the design doc justifies every hardware fact.

**Non-goals (explicitly Tier 1+):** gapless/streaming audio, the dw-axi-dmac driver,
arbitrary PCM playback, mixing/resampling, telemetry sonification. This plan stops at
"a tone comes out, on demand, at a safe volume." (A snemu PWMDAC *capture* model is now
**in** scope — Increment 9a — because it's the emulator test harness; the full Tier-2
"sonify telemetry / hear any boot replay" experience stays out.)

## Volume, decided up front

No hardware volume register exists (the block is `WDATA` + `CTRL` only). Volume is
**digital**: scale sample values before writing `WDATA`. Tier 0 bakes a **gain
scalar into the tone generator**, defaulting **low** (a full-scale square into
headphones is painful). This makes volume a pure, host-tested function, not an MMIO
concern.

## Hard constraint: no floating point

The kernel has emitted zero FP instructions to date, and Tier 0 keeps it that way
(FP regs aren't in `TaskContext`; `sstatus.FS` stays Off — see the design doc's
"Floating point" section). Every increment below is **integer / fixed-point**: gain
is Q0.16, the tone LUT is a build-time-or-`const fn` `const [i16; N]` (floats allowed
in `build.rs`, never at runtime), rate/pacing are integer. A stray `f32` in a green
increment is a bug even if tests pass. The FP fork is Tier 1/2, not here.

## Testability boundary (why the increments split the way they do)

Per the repo rule (statics/`TrapFrame`/MMIO stays in `kernel/`; pure logic goes to a
host-tested `kernel-*` crate), Tier 0 splits into:

- **Pure, host-tested (`kernel-devices`):** every register-word computation, the
  sample-rate→divider math, the tone/LUT generation + gain, and the PIO pacing math.
  This is the bulk of the *logic* and gets real `cargo nextest` TDD coverage.
- **MMIO glue (`kernel/src/device/`), not host-testable:** the `read_volatile`/
  `write_volatile` calls driven *by* the pure layer, plus the one
  `mmio_regions.insert()`. Kept deliberately thin.
- **Acceptance:** manual, by ear, via a runtime `workload=audio-beep` (Increment 7).
  QEMU and snemu do **not** model the PWMDAC, so there is no automated audio
  assertion. But we *can* automatically assert **the driver code path ran** by
  emitting a `snitchos.audio.samples_emitted_total` metric and gating an itest on it
  climbing (Increment 8) — a regression guard that needs no audio model.

Each increment below is one RED→GREEN cycle: failing test first, in its own edit,
then the minimum code to pass. Assess refactor after each green.

---

## Increment 1 — `CTRL` register encoding — ✅ DONE

Shipped `kernel-devices/src/pwmdac.rs`: `Ctrl::to_bits()` + `CntN`/`DataShift`
newtypes + `Resolution`/`DutyCycle`/`DataMode` enums, 12 host tests, clippy-clean.
Mutation: 18/24 caught; the 6 survivors are the `| → ^` equivalent-mutant class
(disjoint fields), documented in `to_bits`. **Finding to carry to Increment 6:**
mainline's `PWMDAC_SAMPLE_CNT_512 = 512` overflows the 9-bit `CNT_N` field
(`GENMASK(12,4)`, max 511) — `CntN::new` rejects 512; resolve against the datasheet
before writing hardware.

**RED** (`kernel-devices/src/pwmdac.rs` tests): a typed `Ctrl` config
(`enable: bool`, `resolution: Resolution` {Bits8, Bits10} → `SHIFT`,
`duty_cycle: DutyCycle`, `cnt_n: u16`, `data_mode`, `data_shift`) encodes to the
expected `u32`. Table-driven asserts pin each field's bit position/mask against the
design doc:
`ENABLE[0]`, `SHIFT[1]`, `DUTY_CYCLE[3:2]`, `CNT_N[12:4]`, `DATA_CHANGE[13]`,
`DATA_MODE[14]`, `DATA_SHIFT[17:15]`. Include an out-of-range `cnt_n` (>9 bits)
test — the encoder must reject or mask, documented.

**GREEN:** the pure `Ctrl::to_bits(&self) -> u32` builder + field enums. Mirror the
layout/no-MMIO style of `kernel-devices/src/uart.rs`. `WDATA`/`CTRL` offsets as
consts here too.

## Increment 2 — sample-rate → (`pwmdac_core` rate, `cnt_n`) — ✅ DONE

Shipped `plan_rate(sample_rate_hz) -> Option<RatePlan>` in `pwmdac.rs`, transcribing
mainline `hw_params`' 7-row switch (8000/11025/16000/22050/32000/44100/48000 Hz).
3 tests: exact table, rejection of unsupported rates, and the property
`core_clk = fs × cnt_n × 256` (256 = 2⁸, the 8-bit PWM period). Mutation: all new
`plan_rate` mutants caught; only the pre-documented `| → ^` equivalents survive.
**Resolves the Increment 1 `cnt_n` worry:** the real field values are 1/2/3, so the
512 discrepancy never bites Tier 0 — but keep it flagged for a 10-bit/high-rate future.

**RED:** `plan_rate(target_fs_hz) -> RatePlan { core_clk_hz, cnt_n }` returns the
known-good pairs from the design doc for 8 kHz (6.144 MHz), 44.1 kHz (11.2896 MHz),
48 kHz (12.288 MHz). Assert the chosen `cnt_n` reproduces `target_fs` from
`core_clk_hz` within tolerance, and that an unsupported rate is a typed error, not a
panic.

**GREEN:** the pure rate-planning fn. This is where the fs↔divider relationship is
documented in code.

## Increment 3 — SYSCRG bring-up ops — ✅ DONE

Shipped `kernel-devices/src/syscrg.rs`: the generic JH7110 SYSCRG model
(`clock_reg_offset` = `index×4`, `reset_{assert,status}_offset`/`reset_bit` for the
`id/32`,`id%32` split, `CLK_ENABLE`=BIT(31), `CLK_DIV_MASK`=bits 23:0) plus
`pwmdac_bringup(core_divider) -> Option<[Op;4]>` — ungate core (divider+gate) → APB
(gate) → release reset (write 0) → poll status until set. 7 tests; clippy-clean;
mutation 34/38 caught, the 2 survivors the documented `| → ^` disjoint-field
equivalents. `core_divider` (`= audio_root_rate / core_clk_hz`) is a driver-supplied
runtime value, validated `1..=256`. The generic model is reusable by the display port.

**RED** (`kernel-devices/src/syscrg.rs`, new): a pure function that, given the
PWMDAC's clock/reset selectors, produces the ordered list of
`RegOp { offset, mask, value }` to (a) ungate `pwmdac_apb`, (b) ungate + set the
divider on `pwmdac_core`, (c) deassert the `pwmdac_apb` reset. Assert offsets/bits
against the datasheet SYSCRG register map (base `0x13020000`; confirm the exact base
per the design doc's open unknown before finalizing values). Assert ordering:
ungate-before-deassert-reset.

**GREEN:** the pure op-list builder. No MMIO. This crate becomes the home for future
SYSCRG consumers (the display port will want it too).

> **Blocking check — RESOLVED.** The offsets were confirmed not from the datasheet
> but from the mainline clock/reset drivers, which are authoritative for what runs on
> the silicon: SYSCRG base `0x13020000`; clock reg = `index×4` (`clk-starfive-jh71x0`),
> so PWMDAC_APB (157)→`0x274`, PWMDAC_CORE (158)→`0x278`; reset id 96 → assert `0x304`
> bit 0 / status `0x314` bit 0 (`reset-starfive-jh7110`, `assert_offset=0x2F8`,
> `status_offset=0x308`). **Still worth a hardware sanity-check** at bring-up (Increment
> 6), but no longer a blocker.

## Increment 4 — tone generator + gain (the volume knob) — ✅ DONE (square; sine → 4b)

Shipped in `pwmdac.rs`: `Gain` (Q0.16 fixed-point, `SILENCE`/`UNITY`, rejects
>unity) and `Tone::square(freq, fs, gain, peak) -> Option<Tone>` yielding one period
of signed PCM (DC-centred, `+high`/`−high` halves), rejecting 0 Hz and supra-Nyquist.
8 tests (period length, half-first/low-second, unity=peak, silence=0, half-gain,
DC-free, rejection). **No runtime FP** — pure integer/fixed-point. Mutation: all new
mutants caught. Deferred to **Increment 4b:** a sine wave via a `build.rs`-generated
`const [i16; N]` LUT (needs a build script; square is a fine beep meanwhile).

**RED:** `generate_tone(waveform, freq_hz, fs_hz, gain: Gain, resolution) -> impl
Iterator<Item = u16>` (or a fixed-capacity buffer — no alloc in this layer). Assert:
one period has `round(fs/freq)` samples; all samples lie within the resolution's
range (`< 2^bits`), DC-centered; `gain = 0.5` yields half the peak-to-peak of
`gain = 1.0`; `gain = 0.0` is silence (all mid-scale); a square and a sine both
satisfy the bounds. Fixed-point `Gain` (e.g. Q0.16) so it's `no_std`-clean.

**GREEN:** the pure generator. Default gain constant lives here, low. **No `f32`/`f64`
at runtime** — if the LUT is precomputed with float math, do it in `build.rs` and emit
a `const [i16; N]`; the runtime path only indexes + fixed-point-scales it. Add a test
(or a `#![deny]`/grep guard) asserting the runtime module is FP-free.

## Increment 5 — PIO pacing interval — ✅ DONE

Shipped `sample_interval_ticks(sample_rate_hz, timer_hz) -> Option<u64>` in
`pwmdac.rs` — round-to-nearest ticks between `WDATA` writes (8 kHz @ the VF2's 4 MHz
timebase = 500), rejecting a zero rate. 3 tests; mutation: all new mutants caught.

**RED:** `sample_interval_ticks(fs_hz, timer_hz) -> u64` — ticks between `WDATA`
writes. Assert 8 kHz @ the VF2's 4 MHz timebase = 500 ticks; assert a rate that
doesn't divide evenly is handled (round + documented drift), not silently truncated.

**GREEN:** the pure fn. Consumes the board timebase the port already reads from the
DTB.

## Increment 6 — kernel MMIO driver glue *(not host-tested)*

Wire the pure layers to hardware. `kernel/src/device/pwmdac.rs`:
- `insert(0x1300_0000)` for SYSCRG in `kmain` (the DAC block at `0x100b0000` is
  already in UART0's mapped megapage — confirm at bring-up, add its megapage only if
  not).
- Apply Increment 3's `RegOp`s to SYSCRG via `write_volatile`.
- Write Increment 1's `CTRL` word; set `ENABLE`.
- A `play_tone(freq, dur)` that drives Increment 4's samples to `WDATA` paced by
  Increment 5's interval off the existing `mtime` delay.
- Behind the `vf2` cargo feature; board-constant style per
  `kernel/src/device/console.rs`.

No unit tests (MMIO). Covered by Increment 7's manual acceptance. Keep it a thin
translation of the pure layer — no logic that isn't already tested above.

## Increment 7 — `audio-beep` runtime workload (acceptance)

**RED (host-tested part):** add a `WorkloadKind::AudioBeep` variant + `workload=`
parse arm in `kernel_boot::bootargs` (unit-tested there, per the runtime-workload
pattern). 

**GREEN:** dispatch in `kmain` on `boot_workload::selected()` to call
`pwmdac::play_tone(440, 1s)`. Then: `cargo xtask boot --workload audio-beep` on the
board → **hear a 440 Hz tone.** This is the acceptance gate; it is manual and
by-ear (documented — no automated audio oracle exists).

## Increment 8 — automated code-path guard (production metric)

*(Depends on 9a: snemu **halts the run** on a write to an unmapped MMIO address
(`bus.rs:216` → `main.rs:139`), so without 9a's PWMDAC/SYSCRG region this scenario
can't boot — my earlier "no audio model needed" was wrong. Given 9a exists, this adds
the cheap **production-metric** assertion: a real telemetry counter that also ships to
Grafana on hardware, so it's distinct from 9b's sample-stream oracle, not redundant.)*

**RED:** an itest scenario `audio-beep-emits-samples` that boots `workload=audio-beep`
under snemu (with 9a's PWMDAC region present) and asserts a
`snitchos.audio.samples_emitted_total` metric frame reaches the wire with value ≥ 1.
This proves the driver loop executed (clock bring-up → CTRL → the WDATA write loop ran).

**GREEN:** bump an atomic in `play_tone`'s write loop; emit it as a metric from the
heartbeat drain (the standard deferred-emission pattern — **never emit from inside
the tight sample loop**). Register the scenario in `xtask/src/itest.rs::SCENARIOS`.

This is the standing regression guard: it fails if clock bring-up or the driver path
breaks, without anyone needing to plug in headphones.

---

## Increment 9a — snemu PWMDAC device model + WAV decoder — ⚠️ SEQUENCED BEFORE 6

**Why it moves first (not last).** Two grounded findings flipped this from Tier-2
polish to a Tier-0 prerequisite:
1. **snemu halts on unmapped MMIO writes** — a write to `0x100b0000` falls through to
   RAM and returns `OutOfRange`, stopping the guest (`snemu/src/bus.rs:216`,
   `main.rs:139`). So *nothing* audio-related can run under snemu until snemu has a
   PWMDAC (+ SYSCRG `0x13020000`) region. The stub and the real capture model are the
   same work (snemu has no device trait — you add a `Bus` field + address branches in
   the read/write methods and fold state into `hash_state`; `bus.rs:37-53`), so build
   the capturing model once, here.
2. It's the **development harness**: with it, Increment 6 is observable in the emulator
   loop (and by ear) instead of hardware-only.

**Decision needed before 6 (virt-vs-VF2):** snemu is virt-only (RAM `0x8000_0000`),
while the PWMDAC is VF2-only and its driver would be `cfg(vf2)`-gated — so a virt-build
kernel wouldn't even compile the audio path. Resolve by either (a) snemu models the
PWMDAC as a **synthetic device in its virt machine** and the `audio-beep` workload
drives `0x100b0000` **address-driven, not hard `cfg(vf2)`** (mirrors how ramfb was
added to snemu's virt map), or (b) audio stays hardware-only for by-ear and snemu gets
the model purely to service the itest. **DECIDED (2026-07-25): (a)** — snemu models
the PWMDAC as a synthetic device in its virt machine and the `audio-beep` workload
drives `0x100b0000` address-driven (not hard `cfg(vf2)`), keeping the whole dev/test
loop off-hardware. Increment 6's driver is therefore address/region-driven, not
board-gated.

**What ships in 9a:**
- **snemu device model:** a `Pwmdac` bus device that accepts `WDATA` (`+0x00`)/`CTRL`
  (`+0x04`) and swallows the SYSCRG-region writes, wired into all `read_u*`/`write_u*`
  and `hash_state` (determinism). Timestamp each `WDATA` write on the guest clock
  (instret/`mtime`). Because the guest paces writes via `mtime` (Increment 5), the
  **inter-write timing already encodes the sample rate** — snemu reconstructs the
  waveform from real guest timing and need *not* model the PLL/core clock (may
  cross-check `CTRL.cnt_n`). Also sidesteps the open `WDATA`-FIFO unknown: it records
  what the guest wrote, when.
- **Pure WAV decoder (host-TDD):** `WDATA`-stream + timestamps → PCM → WAV bytes. A
  pure module with real unit tests fed synthetic writes — **no kernel dependency.**
  Follows the `--dump-framebuffer` precedent (`framebuffer.rs::render_ppm` + `fs::write`
  behind a flag; `main.rs:202`): `snemu … --audio-out beep.wav`.
  - ✅ **WAV container encoding done** — `snemu/src/audio.rs::encode_wav_mono_16`
    (canonical 44-byte mono/16-bit header + LE samples), 7 host tests, 13/13 mutants
    caught, clippy-clean.
  - ⏳ **Rate reconstruction** — timestamped `WDATA` writes → effective sample rate
    (pure, next sub-cycle).

QEMU can't do any of this (audio subsystem but no PWMDAC device model; virt map has
nothing at `0x100b0000`). Design home: `docs/snemu-design.md`.

## Increment 9b — by-ear WAV + itest oracle (after 6/7)

With 6/7 driving real `WDATA` writes: dump the boot's tone to `.wav` and listen (the
by-ear proof off the emulator), and upgrade the `audio-beep` scenario to assert snemu
captured N samples at the expected period/amplitude — an exact, analog-free oracle
**backing, not replacing,** Increment 8's production metric. Optional later: live
playback via a host audio crate (cpal/rodio); in wasm, Web Audio (converges with
snemu-wasm + collector-as-server).

## Sequencing

**9a → 6 → 7 → 8 → 9b.** 9a's pure WAV decoder has no kernel dependency and can start
immediately after the current pure layer; its snemu device model is the prerequisite
that lets everything downstream run under snemu at all.

## Acceptance summary

- **Automated (gate):** Increment 8 — `samples_emitted_total ≥ 1` under snemu (needs
  9a's region); Increment 9b — snemu-captured sample count/shape matches expectation.
- **Manual (the real proof):** `cargo xtask boot --workload audio-beep` on the VF2 →
  audible 440 Hz tone; or 9b's `--audio-out beep.wav` off snemu → listen without
  hardware.

## Risks carried from the design doc

- **Undocumented `WDATA` FIFO/latch depth + status bit.** If it's a one-deep latch,
  mistimed PIO writes produce audible artifacts (never a hang). Increment 5's pacing
  is the mitigation; verify by ear. This is the one thing that could make the beep
  *ugly* — it does not make it *fail*.
- **SYSCRG register offsets** (Increment 3's blocking check) — the only place we
  write raw datasheet offsets rather than mainline-confirmed values.
- **Upstream clock state after U-Boot** — assumes `apb0`/`audio_root`/PLL2 are live.
  If not, PLL/root bring-up joins Increment 3's scope.
- **Board analog-path GPIO** — if the jack needs a mux/amp-enable beyond the DAC,
  it surfaces as "code runs (Increment 8 green) but no sound (Increment 7 silent)."
  First thing to check if that split happens.

## References

- [../docs/vf2-audio-design.md](../docs/vf2-audio-design.md) — the design this
  decomposes; hardware facts, tiers, sources.
- [visionfive2-port.md](visionfive2-port.md) — board timebase, `vf2` feature,
  MMIO mapping, the runtime-workload mechanism.
