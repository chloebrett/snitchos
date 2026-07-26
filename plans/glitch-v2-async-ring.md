# `glitch` v2 — the async RT ring (the keystone)

**Status:** 📐 **PLANNED — not started.** Depends on glitch v1 (shipped;
[glitch.md](glitch.md)). This is the "A" milestone: it turns the DAC from a
*syscall that blocks for the whole play* into a *ring the kernel drains on a timer*,
which is the enabler under sonification (B) and the modem (C), **and** the place the
first real-time deadline observable in the OS lives (XRun / missed sample-feed
deadline). Everything here is snemu-testable end to end (snemu models the PWMDAC and
time deterministically); the VF2 is the by-ear garnish, never a correctness gate.

Design context: [../docs/vf2-audio-design.md](../docs/vf2-audio-design.md) (the arc),
[../docs/sonification-feedback-design.md](../docs/sonification-feedback-design.md)
(why the enqueue path must be generic, cap-gated PCM — the modem and the sonifier are
just other producers).

## Goal

glitch fills a bounded kernel ring; a timer-driven drain feeds the DAC one sample at a
time; the enqueue syscall returns immediately (non-blocking, back-pressured); a drain
tick that finds the ring empty mid-stream emits a structured **XRun** frame + counter.

## Why this is first (and not the flashy B/C work)

`AudioWrite` today spins per-sample inside the syscall for the play's whole duration
(`pwmdac::play_samples`, `kernel/src/device/pwmdac.rs:142-150`). That is fine for one
beep and fatal for everything else: a sonifier can't map dozens of events/sec to sound
if each `Play` blocks; a modem can't stream a continuous FSK waveform. The ring is the
single unblocking move under **both** B and C. It also introduces a *category* of
telemetry the OS lacks — a hard deadline with a wall-clock (audible) consequence.

## Scope

- A bounded PCM sample ring (pure, `kernel-devices`).
- A generic, cap-gated, **non-blocking** `AudioEnqueue` syscall (copy-to-ring, return
  accepted count = back-pressure).
- A timer-driven drain that feeds `WDATA` at the sample rate.
- The **XRun observable**: a structured `Frame` + `snitchos.audio.xruns_total`, forced
  deterministically by an under-feeding workload.
- **Mixing** — two concurrent plays summed (userspace, in glitch), which is what proves
  the async feed actually decouples refill from drain.
- **init-delegated AudioSink** — glitch's DAC cap comes from init's delegation graph,
  not the boot `run_ipc` kernel grant.
- **snemu PCM-out for the async path** — a dev/demo affordance (WAV, later a live
  window), explicitly outside the deterministic gate.

## Non-goals (deferred past this milestone)

- **Arbitrary-PCM *client→glitch* bulk IPC.** The mixing demo here uses overlapping
  freq+duration `Play`s that glitch synthesizes and sums — no bulk path needed. Bulk
  client→glitch PCM lands when the sonifier/modem need to hand glitch pre-rendered
  audio. (The modem's own path is cheaper: it holds its *own* AudioSink and enqueues
  directly — see Architecture decision 2.)
- **DMA-fed drain.** The drain here is timer-IRQ-driven. On the VF2 an 8 kHz feed IRQ
  is affordable; a DMA ring is the later hardware-perf increment.
- **Per-client rate select.** One DAC rate (`glitch_core::FS_HZ` / `DAC_RATE_HZ`) as today.
- **The sonifier (B) and modem (C)** — separate milestones that consume this one.

---

## Architecture decisions

1. **The ring lives in `kernel-devices` (pure, dep-free, host-buildable).** It is
   exactly the crate's charter — "device protocol logic, no MMIO … the console ring."
   A single SPSC ring of `i16`, bounded to a few ms of audio. Producer = the enqueue
   syscall (copy_from_user → tail). Consumer = the timer drain (head → `WDATA`).
   Fully host-tested; loom-modellable there (kernel-devices owns the `--cfg loom`
   checks) since it's a genuine SPSC hand-off.

2. **The enqueue path is generic, cap-gated PCM — not glitch-specific.** Any process
   holding `Object::AudioSink` can be a producer. This is *the* forward-looking
   decision that makes C cheap: the modem gets its own `AudioSink` and enqueues its
   FSK waveform through the same syscall, never routing PCM through glitch's
   freq+duration protocol. glitch is just the first (and, for note-shaped clients, the
   sharing) producer — not a mandatory chokepoint for all sound.

3. **Drain cadence — RESOLVED (user, 2026-07-26): multiplex `mtimecmp`.** The JH7110
   PWMDAC is software-paced (no hardware FIFO — v1 spins between `WDATA` writes; see
   `pwmdac.rs:147`), so samples must reach `WDATA` *at* the sample rate. The single
   per-hart timer (`mtimecmp`; VF2 has no `sstc`) already drives the scheduler/heartbeat.
   The drain shares that one timer via a **soonest-deadline 2-entry timer wheel**:
   `mtimecmp = min(next_audio, next_sched)`; on fire, whichever deadline(s) are due run
   (feed a sample + rearm `next_audio`; and/or the scheduler tick), then re-arm to the
   next minimum. This is the correct driver design and a reusable primitive (any future
   periodic device wants it), at the cost of a small timer-wheel abstraction now.
   Rejected alternatives: **(a)** raising the whole tick to the sample rate (couples
   audio to an 8 kHz sched tick), and **(c)** burst-draining a spin-paced batch per
   coarse IRQ (reintroduces bounded spin). Both are deterministic under snemu too, but
   the wheel is the one that generalizes. **The wheel is the first sub-step of Increment 4.**

4. **XRun = drain-tick-finds-empty while a stream is active.** A pure
   `drain_tick(ring, active) -> DrainOutcome::{Fed(n), Underrun, Idle}` isolates the
   deadline decision from timers. `Underrun` (empty but glitch hasn't signaled
   end-of-stream) → a structured `Frame::AudioXRun` + `snitchos.audio.xruns_total`.
   `Idle` (empty, no active stream) is silent. Forced deterministically by an
   under-feeding workload (the OOM-workload pattern).

5. **Mixing is userspace, in glitch.** Multiple concurrent `Play`s → glitch
   synthesizes each and **saturating-sums** them per sample into its one enqueue
   stream; the kernel stays single-ring. Consistent with v1's "glitch generates
   samples, the kernel just writes bytes." The sum is pure DSP → `synth`/`glitch-core`,
   host-tested. (Per-stream `Gain` lands here — mixing is the first place it's needed.)

6. **init-delegated AudioSink.** init holds `AudioSink` and delegates it to glitch
   (copy semantics, as today for other caps), retiring the `Launch::IpcAudio`
   kernel-mint grant from v1 (`kernel/src/trap/user.rs`, glitch.md Increment 7). This
   also positions init to delegate `AudioSink` to a future modem directly.

7. **snemu PCM-out stays outside the gate.** `--audio-out` must capture the
   *async-drained* path to WAV; a live native window is optional (the snemu
   native-window direction). The **gate asserts on `samples_emitted_total` and
   `xruns_total`**, never on sound qualitatively — hearing is for humans.

Each increment is one RED→GREEN→MUTATE→KILL→REFACTOR cycle (failing test first, in its
own edit). Pure logic leads and is host-tested; MMIO/IPC/timer glue is verified by the
Increment 9 itests. All work on `main`; the user commits.

---

## Increment 1 — the PCM sample ring (pure, `kernel-devices`)

**Acceptance:** a bounded SPSC `SampleRing` accepts a slice up to remaining capacity
(returns accepted count; a full ring accepts 0 — back-pressure), drains in FIFO order,
and reports `len`/`is_empty`/`is_full`/`capacity`.
**RED:** ring tests — push N into a cap-M ring returns `min(N, M-len)`; drain returns
samples in push order; full ring's `push_slice` returns 0; wrap-around across the
buffer end preserves order.
**GREEN:** the ring over a fixed `[i16; CAP]` + head/tail. No `unsafe`.
**MUTATE / KILL:** off-by-one on accepted-count and wrap indices are the live mutants.
**REFACTOR:** consider the loom SPSC model (`tests/loom_*`) if the drain/enqueue split
warrants it — defer the actual loom test to after Increment 4 wires the two sides.
**Done when:** ring host tests + mutation report green.

## Increment 2 — the drain-tick outcome (pure, `kernel-devices`)

**Acceptance:** `drain_tick` maps (ring non-empty) → `Fed(n)`, (empty & stream active)
→ `Underrun`, (empty & inactive) → `Idle`.
**RED:** the three arms, plus "active but non-empty never reports Underrun."
**GREEN:** the pure function over the ring + an `active: bool`.
**MUTATE / KILL:** the empty/active predicate flips.
**Done when:** host tests + mutation report green; no timer or MMIO involved yet.

## Increment 3 — `AudioEnqueue` syscall: non-blocking enqueue (kernel; itest-verified)

**Acceptance:** an `AudioSink`-holder calling `AudioEnqueue(handle, samples)`
copies the samples into the ring and returns the accepted count **without blocking**
for playback; a non-holder is refused (`SyscallRefused` + `cap.denied_total`); a full
ring returns 0 accepted (caller retries — back-pressure).
**RED:** ABI numbering round-trip for the new `Syscall` variant (mirror glitch.md
Increment 2); the cap-refusal path is a `kernel-proc` `authorize_audio` assert (already
exists). The enqueue-vs-block behavior is proven by the Increment 9 itest.
**GREEN:** `Syscall::AudioEnqueue` + `from_usize` arm; a handler that
`authorize_audio` → `copy_from_user` (bounded, reuse the `MAX_SAMPLES = MAX_USER_STR_LEN/2`
guard from glitch.md Increment 8) → `SampleRing::push_slice` → return accepted count.
**Retire the blocking drain:** `play_samples`' spin-loop moves to the Increment 4 timer
drain; the syscall no longer paces.
**MUTATE / KILL:** covered by the resolver's existing tests + the itest.
**Done when:** kernel builds, clippy clean, ABI tests green; behavior confirmed in Inc 9.

## Increment 4 — timer-driven drain + XRun frame + counter (kernel; itest-verified)

**Acceptance:** the drain runs on the timer, feeds `WDATA` at the sample rate,
`samples_emitted_total` climbs *from the drain* (not the syscall); a mid-stream empty
ring emits a `Frame::AudioXRun` + increments `snitchos.audio.xruns_total`; the kernel
keeps heartbeating across an XRun.
**Sub-step 4a — the timer wheel (pure, host-tested).** Per Architecture decision 3, a
soonest-deadline 2-entry wheel: given `(next_audio, next_sched, now)` compute the next
`mtimecmp` and, on fire, which deadline(s) are due + their re-armed nexts. Pure logic
in a kernel-testable crate (`kernel-boot` or a small new home) — RED: `min` selection,
both-due-simultaneously, re-arm past `now` when a deadline was missed. This is the
reusable primitive; the audio drain is its first client. **Sub-step 4b** wires it to
the real `mtimecmp` + the drain below.
**RED:** wire-format ripple for the new `Frame::AudioXRun` variant — add it (appended,
positional-encoding rule holds), then the compile breaks that force the matching arms:
`OwnedFrame::from_borrowed` (`protocol/src/stream.rs`), and any exhaustive `Frame`
match in `collector`/`diagram` (the glitch.md Increment 2 checklist). protocol
roundtrip test for the new variant.
**GREEN:** the timer hook calls `drain_tick`; `Fed(n)` → `write_sample` ×n +
`SAMPLES_EMITTED`; `Underrun` → deferred XRun emit (bump an atomic, emit from the
heartbeat — **never emit a frame from IRQ context**, same rule as the alloc path).
**MUTATE / KILL:** the Fed/Underrun dispatch and the deferred-emit counter.
**Done when:** protocol tests green; kernel builds; XRun path exercised by Inc 9's
`glitch-xrun` scenario.

## Increment 5 — glitch fills the ring in chunks (userspace)

**Acceptance:** `glitch::serve` renders a `Play` into PCM and feeds it via repeated
`AudioEnqueue` calls, pacing refills off the accepted-count back-pressure (retry when
0 accepted) instead of one blocking call; glitch stays responsive to new IPC while a
play is in flight.
**RED:** `glitch-core` gains a pure "refill plan" step (given ring free-space +
remaining samples, how many to hand this call) — host-tested; the serve-loop wiring is
riscv-only (verified by Inc 9).
**GREEN:** replace the single `audio_write` chunk loop with the back-pressure-aware
feed; keep the `glitch.play` span + `plays_total`.
**Done when:** `glitch-core` tests green; glitch compiles for riscv; Inc 9 confirms.

## Increment 6 — mixing: two concurrent plays summed (userspace)

**Acceptance:** two clients each sending a `Play` (different freq) overlapping in time
produce a **summed** stream in the ring (saturating add, per-stream `Gain`); neither
starves the other.
**RED:** `synth`/`glitch-core` `mix(streams, gains) -> i16` — saturating sum,
gain-scaled, clipping at the extremes; host-tested.
**GREEN:** glitch tracks active plays and sums them per sample into its enqueue feed.
**MUTATE / KILL:** the saturation boundary + the gain scaling.
**Done when:** mix host tests + mutation green; Inc 9's `glitch-mix` confirms both
contribute on the wire.

## Increment 7 — init-delegated AudioSink

**Acceptance:** glitch launched under init's delegation graph plays exactly as under
the v1 boot grant; the AudioSink reaches glitch as an init→glitch `CapEvent::Transferred`,
not a kernel-mint root grant.
**RED:** the delegation wiring is kernel/init glue; the host-testable slice is any
`kernel-boot`/`kernel-proc` change to the launch layout (mirror glitch.md Increment 7's
`WorkloadKind` test). Behavior proven by re-running the async-plays scenario under the
init path.
**GREEN:** init holds `AudioSink`, delegates it to glitch; retire `Launch::IpcAudio`'s
kernel mint.
**Done when:** the async-plays itest passes with glitch under init; the AudioSink's
`parent_cap_id` chains to init, not 0.

## Increment 8 — snemu PCM-out for the async path (dev affordance, outside the gate)

**Acceptance:** `snemu … --audio-out foo.wav` on an async workload renders the
*drain-fed* samples to a playable WAV; (optional) a live window plays them.
**RED/GREEN:** verify/extend snemu's PWMDAC model to capture the timer-drained `WDATA`
writes (not the retired syscall spin). No gate assertion — this is for ears.
**Done when:** a WAV of `glitch-async` contains the expected tone; documented as
outside the deterministic gate.

## Increment 9 — acceptance itests

**Acceptance:** three deterministic snemu scenarios:
- **`glitch-async-plays`** — `samples_emitted_total ≥ 1` from the drain **and** glitch
  services a second IPC request while a play is in flight (proves non-blocking).
- **`glitch-xrun`** — an under-feeding workload forces `xruns_total ≥ 1` and a
  `Frame::AudioXRun`; the kernel keeps heartbeating after (the deadline observable, and
  that a missed deadline is survivable).
- **`glitch-mix`** — two overlapping plays; assert both frequencies' contribution is
  present (e.g. both clients' `plays_total` and a mixed `samples_emitted` climb).
**RED→GREEN:** one `Result<(), String>` fn each in `scenarios.rs` + registration in
`SCENARIOS`; new `workload=` arms (`audio-async`, `audio-xrun`, `audio-mix`) in
`kernel-boot` (host-tested parse) + `kmain`/heartbeat dispatch.
**Done when:** all three pass under snemu (`itest` and `itest --scramble`); the
`itest-matrix` generated diagram regenerated.

---

## Acceptance (milestone)

- **Automated (gate):** Increment 9 — async-plays (non-blocking), xrun
  (`xruns_total ≥ 1` + `AudioXRun` frame + survives), mix (two streams) under snemu.
- **By ear / hardware:** `workload=audio-async` on the VF2 → gapless tone from the
  ring; `audio-mix` → a chord; `audio-xrun` → an audible glitch where the deadline slips.
- **Discipline preserved:** a non-holder calling `AudioEnqueue` snitches
  (`SyscallRefused`); the enqueue path is generic PCM (a non-glitch holder can produce).

## What this unblocks

- **C (modem):** reuses the generic PCM `AudioEnqueue` directly (its own AudioSink);
  needs only "arbitrary PCM streaming," no new tracing infra. Shortest path to the
  flashy capstone.
- **B (sonifier):** is a glitch client that can now map a live event stream to sound
  (no blocking Play). Its real prerequisites (FrameSubscribe + cross-process span
  propagation) are a separate tracing-correctness milestone; see
  [../docs/sonification-feedback-design.md](../docs/sonification-feedback-design.md).

## Pre-PR quality gate (per increment)

1. Mutation testing (`mutation-testing` skill) on the pure crates.
2. Refactoring assessment.
3. `cargo xtask clippy` + `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
4. `cargo xtask links` after any `git mv` of a doc.

## References

- [glitch.md](glitch.md) — v1, the DAC-as-capability spine this builds on; its "v2+
  (deferred)" list is exactly this milestone.
- [../docs/vf2-audio-design.md](../docs/vf2-audio-design.md) — the audio arc.
- [../docs/sonification-feedback-design.md](../docs/sonification-feedback-design.md) —
  why the enqueue path is generic PCM, and what B really needs.
- [vf2-audio-tier0.md](vf2-audio-tier0.md) — the DAC bring-up + software-paced `WDATA`
  (why the drain cadence is a real decision).
- Seam refs: `kernel/src/device/pwmdac.rs` (blocking drain to retire),
  `kernel-devices/src/pwmdac.rs` (rate/pacing logic), `kernel/src/syscall/audio.rs`
  (enqueue handler), `kernel/src/trap/user.rs` (`run_ipc`/`Launch::IpcAudio` grant),
  `protocol/src/stream.rs` (`OwnedFrame` ripple), `user/glitch/`, `glitch-core/`, `synth/`.

---
*On completion, `git mv` this to `plans/legacy/` (per CLAUDE.md), don't delete.*
