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
  first few ms — *before* the first `kernel.heartbeat` span registers — and the
  kernel reboot-loops on the panic, faulting before a heartbeat every cycle
  (82575 frames, none reaching a heartbeat). `kernel.heartbeat` never registers.
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

## Secondary thread worth a look (not the cause of the FAIL)

snemu runs to the 150M **step limit** rather than halting, i.e. it keeps emitting
frames after the ~40M guard fault, whereas QEMU **reboot-loops** (82575 frames of
repeated boots). So there's a *second*, independent difference: **QEMU resets on a
kernel panic; snemu keeps emulating the panicked kernel.** That's panic/reset
fidelity, orthogonal to the guard MMU/scheduler, and it doesn't drive the FAIL
(the FAIL is purely the `kernel.heartbeat` vocabulary entry). Might be worth
modeling a reset-on-panic in snemu later so halting workloads converge to the same
terminal state — but it's cosmetic for the oracle today.

## What should change

1. **Oracle: stop failing halting workloads on a benign name.** Options, cheapest
   first:
   - Per-workload allowlist: `{stack-guard, stack-overflow-deep, boot-stack-guard}
     → benign only-snemu = {kernel.heartbeat}`. Gets the sweep to 43/43 honestly
     and documents *why*.
   - Or compare only the boot-prefix up to the crash for crash-defined workloads.
   - Or treat an only-snemu name that appears in QEMU's vocabulary on *some other*
     workload as non-invented (the kernel demonstrably emits `kernel.heartbeat`;
     it isn't fabricated telemetry).
2. **(Optional) snemu reset-on-panic**, so the terminal frame streams converge.

## Honesty trail — how I got it wrong twice before landing here

1. **Post 2's MMU story** — refuted: the walk faults correctly; the fault fires.
2. **My round-1 "benign timing" lean** — correct, but I then talked myself out of
   it.
3. **My round-2 "scheduler starvation / Heisenbug"** — **wrong, a measurement
   artifact.** I concluded the guard task was starved (never scheduled) because
   `cargo xtask snemu-boot --frames | grep …` returned 0 `ContextSwitch` / 0 `Log`
   frames at high step counts. Those were **empty pipes**, not real zeros: the
   `--frames` text dump through a shell pipe silently produced nothing at large
   `--max-steps` (yet worked at 3M). I read "no output" as "the task never runs."
   The in-process oracle (which decodes frames directly, no shell pipe) shows 8299
   frames *with* context switches *and* heartbeats, and the UART shows the guard
   faulting — so the task runs fine.

**Lessons:**
- Trust the **in-process** oracle (`snemu-diff`) over `snemu-boot --frames`
  piped through the shell. The latter's stdout is unreliable at high step counts
  — treat an empty pipe as "no measurement," never as "count is zero."
- A conclusion built on the *absence* of output needs the output path proven to
  work at that scale first. "0 frames" and "the pipe broke" look identical.

## Bottom line

Don't chase the guard-PTE encoding, the `remap`+shootdown path, **or** the
scheduler. The MMU walk is faithful, the guard faults in both emulators, and snemu
schedules fine. The 3 FAILs are the oracle's vocabulary rule being too strict
about a benign crash-vs-heartbeat **ordering** that the deterministic
instruction-clock induces. Fix it in the oracle (allowlist / prefix-compare), not
in snemu. Optionally model reset-on-panic so terminal streams converge.

**Debug edits used and reverted:** `snemu/src/cpu.rs` — a `translate_or_trap`
KSTACK-window `eprintln!`, a `dbg_timer_traps` field + timer-trap counter, a
guard-fault `eprintln!`; `kernel/src/sched/mod.rs` — `println!`s in
`touch_current_stack_guard`, `spawn_on_with_arg`, and `prepare_switch`. All
removed.
