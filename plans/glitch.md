# `glitch` — the userspace audio server (v1)

**Status:** 📋 **PLAN — not started.** `glitch` makes the PWMDAC a **capability** held
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

## Increment 1 — `Object::AudioSink` capability

**RED** (`kernel-proc/src/cap.rs` tests): mirror `authorize_telemetry_refuses_*`
(`cap.rs:1269`). An `authorize_audio(table, handle)` accepts a cap with the AUDIO right,
and refuses `NoSuchCapability` / `MissingRight` / `WrongObject`. Assert `describe` emits
the new packed record.

**GREEN:** add `Object::AudioSink` (`cap.rs:~143`), a rights bit `AUDIO = 0b1000_0000`
(`abi/src/lib.rs:310`, mirror in `cap.rs:40`), `object_kind::AUDIO_SINK = 6`
(`abi:335`) + `describe` arm (`cap.rs:493`), the `authorize_audio` resolver (mirror
`authorize_telemetry`, `cap.rs:280`), and the `protocol::CapObject::AudioSink` +
`cap_object_kind` arm (`kernel/src/trap/user.rs:983`). Pure, fully host-tested.

## Increment 2 — `AudioWrite` syscall ABI

**RED** (`abi` tests): `Syscall::from_usize(32) == Some(AudioWrite)` and the variant
round-trips (mirror the existing syscall-numbering test).

**GREEN:** add `AudioWrite = 32` to the enum (`abi/src/lib.rs:26-259`) **and** the
`from_usize` arm (`:266-302`) — they must stay in sync. Small, host-tested.

## Increment 3 — `AudioWrite` kernel handler *(MMIO glue; itest-verified)*

`kernel/src/syscall/audio.rs` (+ `mod audio;` and the dispatch arm in
`kernel/src/syscall/mod.rs:33-77`). `handle_audio_write`: `current_process_or_refuse` →
`authorize_audio(caps, a0)` → on `Err` `refuse(...refusal_for(d))` + bump `cap.denied` →
`copy_from_user` the sample slice (mirror `metric.rs:77`) → lazy `bringup()`+`configure()`
(a `Once`) → paced loop of `write_sample` + `SAMPLES_EMITTED.inc()` (the current
`pwmdac.rs:146-156` body, relocated) → return. No new host logic beyond Increment 1's
resolver; covered by Increment 8.

## Increment 4 — `glitch_proto` (the `Play` wire type)

**RED** (new `user/glitch-proto` crate, host-tested): `Play { freq_hz: u32,
duration_ms: u32, gain_q16: u32, peak: i16 }` `encode`/`decode` round-trips through a
4-`u64` inline message (mirror `fs_proto`). Reject malformed.

**GREEN:** the pure codec. `#[lib] doctest = false`, workspace lints.

## Increment 5 — the `glitch` server

**RED** (host-test the pure core): factor request-handling into a pure
`plan_play(Play) -> impl Iterator<Item = i16>` (freq/dur/gain → samples via
`Tone::square`) and test it (count = `fs*dur`, amplitude respects gain, rejects bad
freq). *Open sub-decision:* reuse `kernel_devices::pwmdac::{Tone, Gain}` if a `user/`
crate may depend on `kernel-devices` (it's `no_std`, dep-free); else lift the ~10-line
square generator into a shared `no_std` crate. Confirm at this step.

**GREEN** (`user/glitch/src/lib.rs`, `serve() -> !` mirroring `fs::serve`
`user/fs/src/lib.rs:43`): resolve endpoint `delegated_handle(0)`, AudioSink
`delegated_handle(1)`; register `snitchos.glitch.plays_total`; loop
`receive_with_reply` → decode `Play` → open a `glitch.play` span → `AudioWrite(audiosink,
plan_play(req))` → bump the metric → `reply`. Refusal path snitches.

## Increment 6 — the `beep` client

**GREEN** (`user/glitch/src/bin/beep.rs`, mirror `fs-client.rs:48`):
`#[entry(needs = [("glitch", ENDPOINT, SEND)])]`, `bootstrap().get::<Endpoint>("glitch")`,
`call(Play { 440, 1000, gain, peak }.encode())`. Small; exercised by Increment 8.

## Increment 7 — boot layout, `SPAWNABLE`, AudioSink grant

- Add `glitch` + `beep` ELFs to `SPAWNABLE` (`kernel/src/trap/user.rs:609`) + the
  `include_bytes!(env!("SNITCHOS_GLITCH_ELF"))` statics + build-script env wiring
  (pattern at `user.rs:30-60`).
- A `UserLayout` (`user.rs:658`) for the workload: `run_ipc` (`user.rs:1048`) grants
  glitch its endpoint **and** a second `insert_with_id(Object::AudioSink, …)` cap, added
  to the `grants` array (`user.rs:1070-1074`) so it's snitched; `beep` gets a minted
  `SEND`.
- **RED (host):** add `WorkloadKind::GlitchBeep` (sorted) in `kernel-boot` — the
  `selects_*` + sorted-order tests cover it. **GREEN:** the `kmain` dispatch arm.

## Increment 8 — itest `glitch-beep-plays` *(acceptance)*

**RED:** an `xtask-itest` scenario booting `workload=glitch-beep` under snemu, asserting
`snitchos.audio.samples_emitted_total >= 1` **and** `snitchos.glitch.plays_total >= 1`
(proves the full client → glitch IPC → `AudioWrite` → cap-check → `WDATA` path, plus the
server's own observability). Register in the catalog (`xtask-itest/src/itest.rs`).

**GREEN:** wiring from Increments 1–7. snemu's PWMDAC device (Tier 0) accepts the writes;
the AudioSink `CapEvent` and the `glitch.play` span appear on the wire. Optional stronger
assert: the AudioSink `CapEvent::Transferred` frame is present.

## Acceptance

- **Automated (gate):** Increment 8 — `samples_emitted_total ≥ 1` **and**
  `glitch.plays_total ≥ 1` under snemu.
- **By ear / hardware:** `workload=glitch-beep` on the VF2 → the same tone as Tier 0,
  now sourced from a userspace server holding the DAC cap. (`--audio-out` still renders
  the WAV.)
- **Discipline proved:** a `SyscallRefused` when a non-holder calls `AudioWrite` (a
  negative-path scenario or unit assert on `authorize_audio`).

## Retiring the in-kernel beep (optional, after Increment 8)

Once `glitch-beep` is green, the `audio_beep_entry` kernel task + the `AudioBeep`
workload can be removed (or kept as a lower-level MMIO smoke). Deferred — keeping both
green through the build is the point of the additive approach.

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
- [vf2-audio-tier0.md](vf2-audio-tier0.md) — the shipped DAC bring-up glitch sits on;
  `write_sample`/`bringup`/`configure` are what stay kernel-side.
- Seam refs inline above: `kernel-proc/src/cap.rs`, `abi/src/lib.rs`,
  `kernel/src/syscall/{mod,ipc,metric}.rs`, `user/fs/src/lib.rs`,
  `user/hello/src/bin/init.rs`, `kernel/src/trap/user.rs`.
