# `glitch` — the userspace audio server (v1)

**Status (2026-08-27)**: ✅ **v1 COMPLETE — all eight increments done, and the in-kernel beep retired.**
`Object::AudioSink` + `Rights::AUDIO` + `authorize_audio`, the `AudioWrite` syscall +
cap-gated handler + `pwmdac::play_samples`, the `glitch-proto` `Play`/`Reply` codec, the
extracted `synth` crate (5a — `user/` no longer depends on `kernel-devices`), the server,
the `beep` client, the boot layout + AudioSink grant, and the `glitch-beep-plays`
acceptance itest all shipped, TDD'd and mutation-verified. See `## v1 COMPLETE` below for
the closing note.

**Work continues in [glitch-v2-async-ring.md](glitch-v2-async-ring.md)** (increments 1–5
shipped, 6–9 remain). This plan is kept in `plans/` rather than archived because v2 cites
it as its foundation; archive both together when v2 closes.

> *Stale-header note (2026-08-06): this block said "IN PROGRESS — kernel spine done
> (Increments 1–4). Next: Increment 5" long after every increment was marked ✅ DONE in
> the body below. `notes/loose-ends-2026-07-29.md` §9 read the header rather than the
> body and reported glitch as stalled with its `kernel-devices` layering violation still
> standing — 5a had in fact landed. A plan's header is the part everyone reads and the
> part nothing checks.*

`glitch` makes the PWMDAC a **capability** held
by a userspace server, so every source of sound is a *client* of one disciplined thing
rather than code racing `WDATA` directly. v1 establishes that spine — the DAC-as-cap
boundary — with the existing beep re-cast as glitch's first client. It is the foundation
the whole audio arc (sonification → modem → cross-machine `~>`) hangs off; design context
in [../docs/vf2-audio-design.md](../docs/vf2-audio-design.md). Name: `glitch` (snitch ·
stitch · glitch).

**Scope (v1): the discipline, not the features.** A single client, no mixing. Prove:
the DAC is an `Object::AudioSink` cap, `glitch` holds it, a `beep` client asks `glitch`
to play over IPC, `glitch` feeds samples through a cap-gated syscall, and it's all
observable. **Additive** — ships as a new workload *alongside* the hardware-validated
in-kernel beep, so nothing green breaks until glitch's own itest is green.

**Non-goals (explicitly v2+):**
- **Mixing** multiple simultaneous clients (v1 serves one play at a time).
- The **async RT ring** (kernel/IRQ feeds the DAC from a buffer glitch fills) — v1 paces
  kernel-side inside the syscall, which blocks for the play's duration. Fine for one
  client; the ring is what unblocks mixing + gapless.
- Arbitrary PCM streaming, a volume/rate-control protocol, sine (that's audio Tier-0 4b).
- **init-delegated** AudioSink — v1 brings glitch up via the boot `LAYOUTS`/`run_ipc`
  path where the *kernel* grants the AudioSink; routing it through init's delegation
  graph is v2.

---

## Architecture decisions

1. **The DAC is `Object::AudioSink`** — a payload-free authority cap (like
   `TelemetrySink`), added to the `Object` zoo (`kernel-proc/src/cap.rs:99-143`). The
   kernel keeps *all* MMIO (SYSCRG/IOMUX/CTRL bring-up + `WDATA`); the cap is the right
   to ask the kernel to emit samples.
2. **`AudioWrite(handle, samples)` syscall**, gated on the AudioSink cap. Validates the
   handle, copies the sample buffer from user memory, and writes it to `WDATA` **paced at
   the configured rate** (kernel-side, reusing the existing `now_ticks` spacing), then
   returns. Refusals snitch (`SyscallRefused` + `cap.denied_total`). Bring-up runs lazily
   on first call (a `Once`).
   - *Why kernel-side pacing in v1:* the DAC latches `WDATA` (proven on hardware), so
     correct pitch needs per-sample spacing, which userspace can't reliably hit. Blocking
     the syscall for the play duration is acceptable with one client; the async ring (v2)
     is what moves feeding off the syscall.
3. **glitch generates the samples; clients stay dumb.** Client→glitch IPC is a small
   `Play { freq_hz, duration_ms, gain, peak }` request (fits the 4-word inline message —
   no bulk-copy path needed). glitch turns it into samples (`Tone::square`) and calls
   `AudioWrite`. The larger sample buffer only crosses the *glitch→kernel* boundary, via
   `copy_from_user`.
4. **glitch is a boot IPC server (`LAYOUTS`/`run_ipc`).** The kernel pre-creates the
   shared endpoint and grants glitch `RECV | MINT` **plus** the AudioSink; the `beep`
   client gets a minted `SEND`. This avoids init needing to hold/delegate an AudioSink in
   v1. (`run_ipc` at `kernel/src/trap/user.rs:1048` is the exact grant site.)
5. **Observability rides metrics + spans, not CapEvents-per-play.** The AudioSink grant
   is already a `CapEvent::Transferred` at spawn (`user.rs:979`). Per-*play* visibility is
   a `glitch.play` span + a `snitchos.glitch.plays_total` metric (the `fs::serve` pattern:
   `user/fs/src/lib.rs:79` span + `:49` gauge), plus `SAMPLES_EMITTED` still climbing.

Each increment below is one RED→GREEN cycle (failing test first, in its own edit), then
MUTATE/REFACTOR. Host-testable logic leads; MMIO/IPC glue is verified by the Increment 8
itest. snemu already models the PWMDAC (Tier 0), so the whole path runs under snemu.

---

## Increment 1 — `Object::AudioSink` capability — ✅ DONE

Shipped: `Object::AudioSink` + `Rights::AUDIO` (a dedicated right — the "right to make
noise") + `authorize_audio` resolver + `describe` arm in `kernel-proc/src/cap.rs`;
`rights::AUDIO = 0b1000_0000` / `object_kind::AUDIO_SINK = 6` in `abi`;
`protocol::CapObject::AudioSink` (appended — wire-stability holds) + the kernel's
`cap_object_kind` arm. 6 host tests (accept + 3 refusal paths + describe), mutation 2/2
caught, clippy-clean, kernel builds. A faithful `authorize_telemetry` twin.

**RED** (`kernel-proc/src/cap.rs` tests): mirror `authorize_telemetry_refuses_*`
(`cap.rs:1269`). An `authorize_audio(table, handle)` accepts a cap with the AUDIO right,
and refuses `NoSuchCapability` / `MissingRight` / `WrongObject`. Assert `describe` emits
the new packed record.

**GREEN:** add `Object::AudioSink` (`cap.rs:~143`), a rights bit `AUDIO = 0b1000_0000`
(`abi/src/lib.rs:310`, mirror in `cap.rs:40`), `object_kind::AUDIO_SINK = 6`
(`abi:335`) + `describe` arm (`cap.rs:493`), the `authorize_audio` resolver (mirror
`authorize_telemetry`, `cap.rs:280`), and the `protocol::CapObject::AudioSink` +
`cap_object_kind` arm (`kernel/src/trap/user.rs:983`). Pure, fully host-tested.

## Increment 2 — `AudioWrite` syscall ABI — ✅ DONE (with Increment 3)

`AudioWrite = 32` + `from_usize` arm in `abi`, host-tested (RED→GREEN, the numbering
round-trip). **Coupled to Increment 3:** the kernel's `handle_user_ecall` match is
exhaustive (no `Some(_)` catch-all), so the ABI variant can't land without a dispatch
arm — 2 and 3 are build-joined.

**Wire-variant ripple (checklist for next time):** `protocol::CapObject::AudioSink`
(Increment 1) is matched exhaustively in *three* more places that don't fail the crate
you edit — `diagram/src/caps.rs`, `collector/src/caps.rs`, and the kernel's
`cap_object_kind`; all needed an arm. (`scenarios.rs` uses `matches!`, so it's immune.)

**RED** (`abi` tests): `Syscall::from_usize(32) == Some(AudioWrite)` and the variant
round-trips (mirror the existing syscall-numbering test).

**GREEN:** add `AudioWrite = 32` to the enum (`abi/src/lib.rs:26-259`) **and** the
`from_usize` arm (`:266-302`) — they must stay in sync. Small, host-tested.

## Increment 3 — `AudioWrite` kernel handler *(MMIO glue; itest-verified)* — ✅ DONE

Shipped `kernel/src/syscall/audio.rs::handle_audio_write` (cap-gate via `authorize_audio`
→ denial snitches `cap.denied` + `SyscallRefused`; `copy_from_user` the samples, bounded
to `MAX_SAMPLES = 256`; refuses over-long/bad ranges) + the dispatch arm + `mod audio;`.
The kernel half is `pwmdac::play_samples(bytes)` — lazy one-time `bringup`+`configure`
behind a `Once`, then paced `write_sample` + `SAMPLES_EMITTED.inc()` per LE i16. Kernel
builds clean; clippy-clean; the existing `audio-beep` itest stays green (additive —
`audio_beep_entry` untouched). Verified end-to-end by Increment 8's itest.

**Original plan notes below:**


`kernel/src/syscall/audio.rs` (+ `mod audio;` and the dispatch arm in
`kernel/src/syscall/mod.rs:33-77`). `handle_audio_write`: `current_process_or_refuse` →
`authorize_audio(caps, a0)` → on `Err` `refuse(...refusal_for(d))` + bump `cap.denied` →
`copy_from_user` the sample slice (mirror `metric.rs:77`) → lazy `bringup()`+`configure()`
(a `Once`) → paced loop of `write_sample` + `SAMPLES_EMITTED.inc()` (the current
`pwmdac.rs:146-156` body, relocated) → return. No new host logic beyond Increment 1's
resolver; covered by Increment 8.

## Increment 4 — `glitch_proto` (the `Play` wire type) — ✅ DONE

Shipped the `glitch-proto` crate (top-level, mirrors `fs-proto`): `Play { freq_hz,
duration_ms }` + `Reply { Played, Refused }` with tagged `[u64; MSG_WORDS]`
encode/decode, `WireError` for malformed tags/statuses. 6 tests (round-trips, locked
layouts, reject-unknown), mutation 5 caught / 2 unviable / 0 missed, clippy-clean.
**Decision resolving the plan's gain/peak tension:** v1 `Play` carries *only* freq +
duration — **volume is the server's policy** (consistent with the "no volume protocol"
non-goal); glitch picks a fixed low amplitude. Per-request gain is a v2 protocol add.

**RED** (new `user/glitch-proto` crate, host-tested): `Play { freq_hz: u32,
duration_ms: u32, gain_q16: u32, peak: i16 }` `encode`/`decode` round-trips through a
4-`u64` inline message (mirror `fs_proto`). Reject malformed.

**GREEN:** the pure codec. `#[lib] doctest = false`, workspace lints.

## Increment 5 — the `glitch` server

**Sub-decision — RESOLVED (user, 2026-07-25):** a `user/` crate must **not** depend on
`kernel-devices` (it's a kernel-side crate). And the kernel no longer needs `Tone`/`Gain`
at all — once glitch generates samples in userspace and the kernel just *writes* what it's
handed (`play_samples`), the kernel has no reason to synthesize tones. So:

### 5a — extract a shared synth crate *(do first)* — ✅ DONE

Shipped the dep-free `no_std` `synth` crate (`synth/`): `Tone` (+ `square`) and `Gain`
moved **out of** `kernel-devices` with their 7 tests (all green). No kernel *and* no
user-runtime deps — the clean boundary that keeps `user/` off `kernel-devices`. What
**stays** in `kernel-devices/src/pwmdac.rs` is the MMIO-layout logic
(`Ctrl`/`plan_rate`/`sample_interval_ticks`) + `syscrg`/`iomux` (117 tests still green).
The kernel binary imports `Tone`/`Gain` from `synth` (via `audio_beep_entry`) — a
transient dep that retires with the in-kernel beep after Increment 8; steady-state has
`synth` userspace-only. Verified: kernel riscv build (exit 0), workspace clippy (exit 0),
doc links resolve. **Name `synth` kept** (working name promoted to final — descriptive,
matches the pure-DSP role). Feedback-loop design fallout captured in
[../docs/sonification-feedback-design.md](../docs/sonification-feedback-design.md).

**Original plan notes:** Move the pure synthesis — `Tone` (+ `square`) and `Gain` —
**out of** `kernel-devices` into a new dependency-free `no_std` crate. Tests move with
it (already pure). Update `audio_beep_entry`'s import to `synth`.

### 5b — `user/runtime` `AudioWrite` wrapper — ✅ DONE

Shipped `runtime::audio_write(sink: usize, samples: &[i16]) -> Result<(), Denied>` +
`AUDIO_WRITE_MAX = 256` (`user/runtime/src/lib.rs`), mirroring `kill` over the `ecall`
helper: `a0` = handle, `a1` = ptr, `a2` = count; `usize::MAX` ⇒ `Err(Denied)`. Thin
ecall glue (unreachable on host, like every other syscall wrapper) — verified by
compiling `snitchos-user` for riscv; behaviourally exercised by Increment 8.

### 5c — the server — ✅ DONE

**Pure core split out (deviation from plan, forced + principled):** the plan put
`plan_play` in `user/glitch`, but a crate depending on `snitchos-user` is
`NOT_HOST_TESTED` (riscv-only asm), so `plan_play` couldn't be host-tested there. Split
exactly like `fs-core` vs `user/fs`: `plan_play` + the server's policy constants
(`FS_HZ = 8000` — *must match the kernel's `BEEP_RATE_HZ`* — and `PEAK = 4000`) live in a
new host-testable **`glitch-core`** crate (deps `synth` + `glitch-proto`); `serve()` lives
in the riscv-only **`user/glitch`**.

Shipped:
- **`glitch-core::plan_play(Play) -> Option<impl Iterator<Item = i16>>`** — TDD'd, 5 host
  tests (fs·dur count, duration scaling, server amplitude, zero-freq + supra-Nyquist
  rejection). A prerequisite `synth::Tone::sample_at(i)` was TDD'd first (the infinite
  square indexed by sample; `.cycle()` was unusable — `Clone` doesn't propagate through
  `Tone::samples`'s opaque return). `samples()` refactored to reuse it.
- **`glitch::serve() -> !`** (`user/glitch/src/lib.rs`, mirrors `fs::serve`): endpoint at
  `delegated_handle(0)`, AudioSink at `delegated_handle(1)`; registers
  `snitchos.glitch.plays_total`; loop `receive_with_reply` → decode `Play` → open a
  `glitch.play` span → chunk `plan_play(req)` into `runtime::audio_write(sink, chunk)`
  calls (≤ `AUDIO_WRITE_MAX = 256`/call) → bump the metric → `reply(Reply::Played)`;
  refusal path snitches `Reply::Refused`. Compiles clean for riscv; verified end-to-end by
  Increment 8.
- Bookkeeping: `synth`/`glitch-core`/`user/glitch` added to the workspace; `glitch` added
  to `NOT_HOST_TESTED`; the mutant-plan characterisation set updated (also picked up
  `glitch-proto`, stale since Increment 4). All 64 xtask plan tests green.

## Increment 6 — the `beep` client — ✅ DONE

Shipped `user/glitch/src/bin/beep.rs` (mirrors `fs-client`): `#[entry(needs =
[("glitch", ENDPOINT, SEND)])]`, `bootstrap().get::<Endpoint>("glitch")`,
`call(Play { freq_hz: 440, duration_ms: 1000 }.encode())` inside a `beep.request`
span, then decodes the reply and emits `snitchos.beep.played_total` on
`Reply::Played` (the client-side witness; the itest asserts on the server metrics).
Added the `hitch` dep (`default-features = false`) for the `#[entry(needs)]` note.
Compiles clean for riscv; exercised end-to-end by Increment 8.

## Increment 7 — boot layout, AudioSink grant — ✅ DONE

- **`kernel-boot`:** `WorkloadKind::GlitchBeep` added (sorted, between `Fs`/`HeapGrow`),
  TDD'd (`selects_glitch_beep` RED→GREEN; sorted-order + `every_workload_selects` cover
  it). 73 host tests green.
- **New `AudioSink` launch path:** rather than reuse fs's `RECV|MINT`, added a
  `Launch::IpcAudio { rights_bits }` variant + a `grant_audio: bool` param to `run_ipc`:
  after the endpoint it `insert_with_id`s an `Object::AudioSink`/`Rights::AUDIO` cap
  (kernel-minted root grant, `parent_cap_id: 0`) at slot 3 = `delegated_handle(1)`, and
  pushes it onto the (now `Vec`) snitched `grants`. `GLITCH_SERVER` = `IpcAudio { RECV }`
  — **least authority: RECV only, no MINT** (it never mints; the plan's `RECV|MINT` was
  fs-copy inertia). `BEEP` = `ipc_user(SEND)`.
- **Statics + build wiring:** `GLITCH_SERVER_ELF`/`BEEP_ELF` statics; new
  `user/glitch/src/bin/glitch-server.rs` (calls `glitch::serve()`); `build.rs`
  `build(&["glitch"])` phase + two `USER_PROGRAMS` rows. **No `SPAWNABLE` rows** —
  glitch-beep is LAYOUTS/`run_ipc`-launched, not `Spawn`-launched (skipped the plan's
  SPAWNABLE step as unneeded).
- **`kmain`:** `GlitchBeep` added next to `Fs` in the two userspace-workload match lists
  (exhaustive pre-secondary arm + `hart_1_probe`-suppression `matches!`). The `LAYOUTS`
  entry (server then client, `needs_endpoint: true`) is dispatched generically — no new
  arm. Kernel + userspace build clean (exit 0).

## Increment 8 — itest `glitch-beep-plays` *(acceptance)* — ✅ DONE

Shipped `scenarios::glitch_beep_plays` + registered `cpu "glitch-beep-plays" … {"glitch-beep"}`.
Asserts **both** `snitchos.glitch.plays_total >= 1` and `snitchos.audio.samples_emitted_total
>= 1` in one forward pass (two `Cell` flags — the metrics have no guaranteed wire order:
`plays_total` is a userspace `EmitMetric`, `samples_emitted` a heartbeat-drained kernel
counter). **`1/1 pass under snemu (100% fidelity)`.**

**Bug the itest caught (the whole point of Increment 8):** the first run FAILED with
`SyscallRefused { syscall: 32, reason: BadUserRange, task_id: glitch_server }` — every cap
grant and IPC hop was correct (the frame dump showed AudioSink `rights: 128` granted, the
`glitch.play` span, the Reply transfer), but `AudioWrite` was refused. Root cause:
`kernel_mem::mmu::user_range_ok` caps **any** single `copy_from_user` at `MAX_USER_STR_LEN =
256 bytes`; `MAX_SAMPLES = 256` samples = 512 bytes overran it. Fixed by tying
`MAX_SAMPLES = MAX_USER_STR_LEN / 2` (= 128) with a `const _: () = assert!(…)` compile-time
guard, and `AUDIO_WRITE_MAX = 128` in the runtime. Diagnosed via
`cargo run -p xtask-itest -- snemu boot --workload glitch-beep --frames`.

## v1 COMPLETE

All 8 increments shipped. `workload=glitch-beep` boots a userspace `glitch` server holding
the DAC as an `AudioSink` capability; a `beep` client asks it to play 440 Hz over IPC; the
server synthesizes (`glitch-core`/`synth`) and streams samples through the cap-gated
`AudioWrite` syscall; the kernel drives `WDATA`. The discipline is proved: the DAC is a
cap, every sound source is a client, and it's all observable on the wire.

## Acceptance

- **Automated (gate):** Increment 8 — `samples_emitted_total ≥ 1` **and**
  `glitch.plays_total ≥ 1` under snemu.
- **By ear / hardware:** `workload=glitch-beep` on the VF2 → the same tone as Tier 0,
  now sourced from a userspace server holding the DAC cap. (`--audio-out` still renders
  the WAV.)
- **Discipline proved:** a `SyscallRefused` when a non-holder calls `AudioWrite` (a
  negative-path scenario or unit assert on `authorize_audio`).

## Retiring the in-kernel beep — ✅ DONE

`glitch-beep` being green, the in-kernel beep was removed: `audio_beep_entry` (the boot
task) + the `AudioBeep` `WorkloadKind` + its `kmain` arm + the `audio-beep-emits-samples`
itest scenario. **The kernel's `synth` dependency dropped entirely** — `audio_beep_entry`
was its only user, so steady state is now what the plan promised: `synth` is
**userspace-only** (glitch generates; the kernel just writes bytes via `play_samples`).
`kernel-devices::pwmdac` keeps the MMIO-layout logic; `kernel/src/device/pwmdac.rs` keeps
the `unsafe` glue (`play_samples`/`write_sample`/`bringup`/`configure`/`SAMPLES_EMITTED`,
all used by the `AudioWrite` syscall). The `BEEP_RATE_HZ` constant became `DAC_RATE_HZ`
(must match `glitch_core::FS_HZ`). `glitch-beep` now covers the PWMDAC MMIO path (strictly
more than audio-beep did), so no coverage was lost. The `itest-matrix` generated diagram
was regenerated after the scenario removal.

## v2+ (deferred, in priority order)

1. **Mixing** — multiple clients, summed streams; forces the async feed.
2. **Async RT ring** — glitch fills a buffer, the kernel/IRQ feeds the DAC per tick;
   unblocks mixing + gapless, and is where the **XRun / sample-feed-deadline** observable
   lives.
3. **init-delegated AudioSink** — glitch under init's delegation graph instead of a boot
   grant.
4. **Richer protocol** — arbitrary PCM, per-client volume (`Gain`), rate select.

## References

- [../docs/vf2-audio-design.md](../docs/vf2-audio-design.md) — the audio arc; glitch is
  the discipline everything else builds on.
- [vf2-audio-tier0.md](legacy/vf2-audio-tier0.md) — the shipped DAC bring-up glitch sits on;
  `write_sample`/`bringup`/`configure` are what stay kernel-side.
- Seam refs inline above: `kernel-proc/src/cap.rs`, `abi/src/lib.rs`,
  `kernel/src/syscall/{mod,ipc,metric}.rs`, `user/fs/src/lib.rs`,
  `user/hello/src/bin/init.rs`, `kernel/src/trap/user.rs`.
