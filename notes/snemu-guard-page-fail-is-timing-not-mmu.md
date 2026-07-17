# The 3 snemu-diff guard FAILs are a timing artifact, not an MMU bug

**Date:** 2026-07-04 (corrected 2026-07-05)
**Context:** Post 2 (`posts/snemu-02-...`) ended by handing the next investigation
"gift-wrapped": the three stack-guard workloads (`stack-guard`,
`stack-overflow-deep`, `boot-stack-guard`) FAIL the differential oracle, each
diverging on one only-in-snemu name — `kernel.heartbeat`. Post 2's parting
hypothesis: *"snemu's page-table walk allows the guard-page access that QEMU
rejects"* — an MMU-fidelity gap.

**That hypothesis is wrong, and so were two of mine along the way** (see the honesty
trail at the bottom). The mechanism is **timing**, not translation, and not
scheduling.

## The verdict (what the authoritative oracle shows)

`cargo xtask snemu-diff --workload stack-guard`:

```
snemu 8299 frames (step limit 150M), qemu 82575 frames
structural agreement on the first 171 frame(s)
first divergence at frame 171:
  snemu: ContextSwitch { from: 3, to: 4, reason: Yield, hart_id: 1 }
  qemu:  StringRegister "snitchos.task.stack_guard_smoke.cpu_time_ticks"
vocabulary — 83 shared, 0 only-qemu, 1 only-snemu:  ["kernel.heartbeat"]
FAIL
```

Read that carefully:

- snemu **does** context-switch (frame 171 is literally a `ContextSwitch`).
- The guard task **does** run and **does** fault — the snemu UART prints
  `kernel stack overflow: task 5 guard fault at 0xffffffc04000f000 (slot 3)`
  around ~40M instructions, and page-fault-as-trap delivers it, the handler names
  the guard region, exactly as designed. The MMU walk is faithful.
- The **only** vocabulary difference is `kernel.heartbeat`, only-in-snemu. The
  first *structural* divergence (frame 171) is a benign cross-hart `ContextSwitch`
  ordering wobble.

So the mechanism is the same one `canonical()` was built for, just at the
vocabulary layer:

- **QEMU** (real 10 MHz wall-clock, fast boot): the guard task faults within the
  first few ms — *before* the first `kernel.heartbeat` span registers (first
  heartbeat isn't due until 1 s). So `kernel.heartbeat` never registers on the
  QEMU side. (The high QEMU frame count — ~78k for `panic-now` over the 6 s window
  — is **not** reboots; it's hart 1's probe spinning and emitting spans while
  hart 0 idles post-panic. Measured: exactly one `kernel.boot` registration, no
  reboot. See the secondary-thread section.)
- **snemu** (deterministic instruction-count clock): the same fault lands *after*
  several heartbeat spans have already opened (heartbeat period = 10M
  instructions; boot-to-fault ≈ 40M). So `kernel.heartbeat` is in snemu's
  vocabulary but not QEMU's.

Same kernel, same correct behavior (guard fault → named overflow). The two clocks
just order "first heartbeat" vs "guard crash" differently. `kernel.heartbeat` is
only-in-snemu because of that ordering — **not** because snemu invented telemetry.

The oracle's `canonical()` normalization absorbs this class for timestamps and
metric values, but the **vocabulary rule** (`only_snemu.is_empty()` ⇒ faithful) is
too strict for a workload that **deliberately halts**: whether a terminal crash
lands before or after the first heartbeat is a clock artifact, and both orderings
are legitimate.

## Secondary thread — investigated, and there is NO reset gap

I had claimed (unverified, from the raw frame count) that QEMU **reboot-loops** on
the panic while snemu keeps emulating — a "panic/reset fidelity" difference. That
was a third bad inference. **Measured it:** in QEMU's `panic-now` stream, there is
exactly **one** `kernel.boot` registration and 19363 `SpanStart`s out of 77619
frames. QEMU boots **once** and does not reboot — the kernel panic handler is just
`loop { wfi }` (no SBI reset), and QEMU has no auto-reset. The big frame count is
hart 1's probe task spinning and emitting spans for the whole 6 s capture window
while hart 0 idles post-panic. snemu does the *same* thing (hart 0 idles, hart 1
runs on); it just gets fewer hart-1 turns because its budget is 150M instructions,
not 6 real seconds. So the frame-count gap is benign wall-time-vs-instruction
volume, `canonical()`/vocabulary already absorbs it, and **there is no reset-on-
panic gap to model.** Both emulators boot once and idle after the crash.

## CONFIRMED via minimal repro (`panic-now`)

Built the smallest possible workload to test the thesis: `workload=panic-now` —
a kernel task that calls `panic!()` immediately on first run, **no guard page, no
MMU, no fault** (`kernel/src/workloads/storms.rs::panic_now`, dispatched in
`kmain`, swept by the oracle). `cargo xtask snemu-diff --workload panic-now`:

```
snemu 8297 frames (step limit 150M), qemu 77920 frames
first divergence at frame 171: ContextSwitch{from:3,to:4,Yield,hart 1}  (same wobble)
vocabulary — 83 shared, 0 only-qemu, 1 only-snemu: ["kernel.heartbeat"]
FAIL
```

**Byte-for-byte the same signature as `stack-guard`** (8297 vs 8299 frames, same
frame-171 divergence, same only-snemu name). So the guard page / MMU / scheduler
are conclusively *not* involved — a bare panic reproduces the whole FAIL. The
minimal form is: *a kernel that crashes at a fixed post-boot point emits N>0
heartbeats before dying under snemu's instruction-clock, 0 under QEMU's
wall-clock.*

Quantified: boot-to-crash ≈ 40M instructions. snemu's `rdtime = instret` + the
DTB's 10 MHz means a heartbeat period ("1 second") = 10M instructions, so snemu
reads boot as ~4 s → ~4 heartbeats. QEMU runs the same 40M instructions in ~0.2 s
real; its first heartbeat isn't due until 1 s → 0 heartbeats.

**Corollary — there's nothing on the snemu side to fix.** Two dead ends: (a) I
briefly imagined "make snemu reset-on-panic like QEMU" — but QEMU doesn't reset
(measured above), so there's nothing to mirror. (b) Even if it did, snemu's first
heartbeat (10M instr) precedes the crash (40M instr), so a post-crash reset can't
erase heartbeats already emitted. Both emulators boot once and idle after the
crash; the divergence is purely how many heartbeats fit before the crash in each
clock. The only real fix is oracle-side.

## FIX LANDED (oracle-side)

`xtask/src/snemu_diff.rs`: added `BENIGN_ONLY_SNEMU = ["kernel.heartbeat"]` and
`invented_names(only_snemu)` = only-snemu minus those recurring-infra names.
`Comparison::faithful()` now checks `invented_names(&self.only_snemu).is_empty()`
instead of `only_snemu.is_empty()`. So an only-snemu set of exactly
`{kernel.heartbeat}` (what `panic-now` and the stack-guard family produce, already
observed) is PASS; any *other* only-snemu name still FAILs (a genuine invention).
Reporting distinguishes the two cases. Unit-tested:
`kernel_heartbeat_alone_in_only_snemu_is_not_an_invention` (benign) and
`a_workload_specific_only_snemu_name_is_still_an_invention` (still caught).

**Robustness tightening — SHIPPED (was the one open limitation).** The benign
pass used to forgive `kernel.heartbeat`-only-in-snemu *unconditionally*, which
would mask a hypothetical snemu bug that fails to halt and over-emits heartbeats.
Closed by [plans/legacy/panic-emits-telemetry.md](../plans/legacy/panic-emits-telemetry.md): the kernel
panic handler now emits a `Log("kernel panic …")` on the wire, so `invented_names`
takes a `snemu_crashed` flag (`snemu_reached_crash` = a panic Log is present) and
forgives `kernel.heartbeat` **only when snemu is proven to have reached the crash**.
A snemu that ran past where it should have died — heartbeats but no panic frame —
now correctly FAILs. Verified: `panic-now` and `stack-guard` PASS "…snemu reached
the crash too (panic frame present)". (Every crashing workload emits the frame:
the stack-guard family's `report_stack_guard_fault` calls `panic!()`, which runs
the same handler.)

## Remaining / follow-ups

1. **~~Oracle robustness~~ — RESOLVED** (was the one real open item). The
   "clean fix" below shipped: the panic handler now emits a `kernel panic` `Log`
   (panic-safe: no alloc/intern, non-blocking bounded-retry send —
   [plans/legacy/panic-emits-telemetry.md](../plans/legacy/panic-emits-telemetry.md)), and the oracle
   forgives `kernel.heartbeat`-only-in-snemu **only when that panic frame is
   present** (`snemu_reached_crash`). A fail-to-halt snemu — heartbeats but no
   panic — now FAILs. The fragile heartbeat-count threshold was avoided entirely.
2. **~~snemu reset-on-panic~~** — dropped. Investigated (measured one `kernel.boot`
   in QEMU's stream): QEMU does **not** reset on panic, so there was never a gap to
   close. See the secondary-thread section.

## Honesty trail — how I got it wrong twice before landing here

1. **Post 2's MMU story** — refuted: the walk faults correctly; the fault fires.
2. **My round-1 "benign timing" lean** — correct, but I then talked myself out of
   it.
3. **My round-2 "scheduler starvation / Heisenbug"** — **wrong, a measurement
   artifact.** I concluded the guard task was starved (never scheduled) because
   `cargo xtask snemu-boot --frames … 2>/dev/null | grep …` returned 0
   `ContextSwitch` / 0 `Log` frames. I *thought* the cause was scale (empty pipes
   at large `--max-steps`, since a 3M run had shown frames). **The real cause was
   dumber and now fixed:** `snemu-boot`'s `--frames` dump was written to
   **stderr**, and I was silencing build noise with `2>/dev/null` — so I discarded
   the very data I asked for. The 3M run that "worked" used `2>&1`; every empty run
   used `2>/dev/null`. Step count was a pure confound. (Fix: `snemu/src/main.rs`
   `report_frames` now prints the dump to **stdout** — the requested data belongs
   there; diagnostics stay on stderr. Verified: the same 60M run now yields 976
   `ContextSwitch` frames through `2>/dev/null`.) The in-process oracle decodes
   frames directly (no CLI, no stream to misroute), which is why it was right all
   along: 8299 frames *with* context switches *and* heartbeats.

**Lessons:**
- A conclusion built on the *absence* of output needs the output path proven to
  work first. "0 frames" and "I threw the frames away with `2>/dev/null`" look
  identical on a terminal. Check *which* stream your data is on before trusting a
  zero.
- Prefer the **in-process** oracle (`snemu-diff`) over the `snemu-boot` CLI for
  measurement: no stream to misroute, no shell in the middle.

## Bottom line

Don't chase the guard-PTE encoding, the `remap`+shootdown path, **or** the
scheduler. The MMU walk is faithful, the guard faults in both emulators, and snemu
schedules fine. The 3 FAILs are the oracle's vocabulary rule being too strict
about a benign crash-vs-heartbeat **ordering** that the deterministic
instruction-clock induces. Fixed in the oracle (benign-name filter), not in snemu.
There is no reset-on-panic gap (QEMU boots once; measured). The one real follow-up
is oracle robustness (make the crash observable in telemetry so the benign pass can
be conditioned on snemu actually reaching it) — see Remaining.

**Debug edits used and reverted:** `snemu/src/cpu.rs` — a `translate_or_trap`
KSTACK-window `eprintln!`, a `dbg_timer_traps` field + timer-trap counter, a
guard-fault `eprintln!`; `kernel/src/sched/mod.rs` — `println!`s in
`touch_current_stack_guard`, `spawn_on_with_arg`, and `prepare_switch`;
`xtask/src/snemu_diff.rs` — a `QEMU-DBG` boot/span counter. All removed.
