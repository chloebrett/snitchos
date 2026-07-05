# The 3 snemu-diff guard FAILs are a timing artifact, not an MMU bug

**Date:** 2026-07-04
**Context:** Post 2 (`posts/snemu-02-...`) ended by handing the next investigation
"gift-wrapped": the three stack-guard workloads (`stack-guard`,
`stack-overflow-deep`, `boot-stack-guard`) FAIL the differential oracle, each
diverging on one only-in-snemu name — `kernel.heartbeat`. Post 2's parting
hypothesis: *"snemu's page-table walk allows the guard-page access that QEMU
rejects"* — an MMU-fidelity gap.

**That hypothesis is wrong.** This is the third time an appealing story about
these workloads didn't survive contact with the oracle (post 2 already recounts
two). The mechanism is timing, not translation.

## What the reproduction actually showed

Ran `cargo xtask snemu-diff --workload stack-guard`. Confirmed FAIL:
`only-snemu = ["kernel.heartbeat"]`, snemu stops at the 150M step limit, QEMU
emits 70760 frames (it's reboot-looping on the fault).

Instrumented two layers:

1. **snemu `translate_or_trap`** — logged every access into the KSTACK window
   (`0xffff_ffc0_4000_0000+`). Across a full 150M-step run: **no MMU bug is even
   reachable** in the sense the post meant, but the walk *does* fault when the
   store finally happens (see below). The only early KSTACK accesses are the
   heartbeat's high-water *scan* reading the bottom of a live task's stack.
2. **kernel `touch_current_stack_guard`** — logged task id + resolved guard VA.

At 150M steps the smoke task **does** run and **does** fault, cleanly:

```
GUARD-DBG touch id=5 guard_va=Some(0xffffffc04000f000)   (slot 3)
kernel stack overflow: task 5 guard fault at 0xffffffc04000f000 (slot 3)
```

So in snemu: the guard store executes → `translate` faults → page-fault-as-trap
delivers scause=STORE_PAGE_FAULT / stval=the guard VA → the trap handler
recognizes the guard region → reports the named overflow. **The MMU walk, the
guard PTE encoding, and trap delivery are all correct.** The `ff253ba`
page-fault-as-trap fix works.

## The real mechanism: instruction-clock reorders the crash vs. heartbeats

`kernel.heartbeat` is registered the first time a heartbeat span opens. The
divergence is *when the guard fault happens relative to that*:

- **QEMU** (real 10 MHz wall-clock timer, fast boot): the smoke task preempts and
  faults within the first few ms — **before the first heartbeat span**. The
  kernel then reboot-loops on the panic, faulting before a heartbeat every cycle.
  So `kernel.heartbeat` **never registers**. 70760 frames = many boot cycles,
  none reaching a heartbeat.
- **snemu** (deterministic instruction-count clock, round-robin harts): the smoke
  task isn't scheduled until ~150M instructions in — **after several heartbeats
  have already ticked**. So snemu registers `kernel.heartbeat`, *then* faults.

Same kernel, same correct behavior (deliberate guard fault → named overflow →
halt). The two clocks just order "first heartbeat" and "guard crash" differently.
`kernel.heartbeat` is only-in-snemu because of **that ordering**, not because
snemu invented telemetry.

This *looked* like the class the oracle's `canonical()` normalization was built
to absorb — but a follow-up probe (below) showed it is **not** a benign clock
artifact. It's a real snemu scheduler-fidelity gap.

## FOLLOW-UP (2026-07-05): it's a real scheduling starvation, not clock units

Ran the "how many heartbeats before the guard task runs" probe. It overturned the
"benign timing" lean above. Findings, in order of certainty:

- **Cadence:** the kernel programs the timer at `timebase / TICKS_PER_HEARTBEAT`
  = 10 MHz / 20 = one timer IRQ per **500k** snemu instructions; a heartbeat every
  20 ticks = **10M instructions**. So ~15 heartbeats ≈ 150M instructions.
- **Kernel tasks are never preempted.** `kernel/src/trap/mod.rs::handle_timer`
  calls `maybe_preempt(frame.sstatus & SSTATUS_SPP == 0)` — it only deschedules
  tasks trapped from **U-mode**. Kernel tasks (`SPP==1`) keep the cooperative
  "exclusive until I yield" invariant. `stack_guard_smoke` is a kernel task, so it
  runs **only** when the cooperative round-robin reaches it via `yield_now`.
- **The guard task is genuinely starved in snemu.** Clean run (no kernel
  instrumentation), 200M steps: **no guard fault, no `kernel stack overflow` Log**
  — the store never executes. Meanwhile `main` (`obs/heartbeat.rs::run`) yields on
  *every* loop iteration, into its per-hart runqueue.
- **It's hypersensitive to boot timing.** Merely adding one `println!` in the
  kernel shifted the guard task to fault at ~150M steps (task id 5, slot 3,
  `0xffffffc04000f000`). A one-line change flipping *whether a task ever runs* is
  the signature of a fragile scheduling divergence, not a clock-scale artifact.

**Why "starved" and not "just late":** the scheduler pick is priority-aged
(`kernel-core::sched::pick_next` → `max_by_key(aged_priority, Reverse(enqueued_tick))`).
The running task is held **off** the runqueue (`prepare_switch`, can't re-pick
itself). All boot tasks are `Priority::Normal`, so the tiebreak is
`Reverse(enqueued_tick)` = **longest waiter wins**. The guard task, spawned early
and never run, has the smallest `enqueued_tick` → it should be picked within 1–2
switches, with or without aging. It isn't. The only way that holds: **the guard
task is not a candidate in the runqueue that actually gets serviced.**

### The open root cause (next instrumented run)

`spawn` → `spawn_on(current_hartid(), …)` → `runqueues[hart].push_back(...)`.
So the guard task lands in whichever runqueue `current_hartid()` names *at spawn
time in snemu*. Two live suspects:

- **(a) wrong `current_hartid()` at spawn** — snemu's `tp`/per-hart wiring returns
  a hart id whose runqueue isn't the one `main` yields into, so `main`/`idle` on
  hart 0 never see the guard task as a candidate.
- **(b) it lands on hart 1's queue, and hart 1 never reaches it** — e.g. hart 1's
  tasks don't yield into that queue, or hart 1 isn't running the yielding loop we
  assume in the stack-guard workload.

Decisive cheap probe: log (once) the `hart` index the guard task is pushed to in
`spawn_on_with_arg`, and which `runqueues[me]` each hart's `prepare_switch` reads
— confirm the queue the guard sits in vs. the queues being serviced. **Accept
that adding the log shifts timing; we only need the hart index, not the cadence.**

This likely affects more than the three guard workloads: any cooperatively-
scheduled kernel task that depends on prompt round-robin service could interleave
differently under snemu, which the oracle's structural prefix-match would quietly
absorb as "cross-hart wobble."

## Secondary question — should the oracle special-case halting workloads?

Independent of the scheduler bug, the `kernel.heartbeat`-only-in-snemu verdict is
still a rough edge for **deliberately-halting** workloads. Options:
   - Treat `only-snemu` names that are *also in QEMU's vocabulary on a
     non-halting workload* (like `kernel.heartbeat`, which every normal workload
     emits) as benign — a name isn't "invented" if the kernel demonstrably emits
     it elsewhere.
   - Or: for workloads whose defining behavior is a crash, compare only the
     boot-prefix up to the crash, not the post-crash vocabulary.
   - Or: give these three workloads an explicit allowlist entry
     (`{workload → benign only-snemu names}`) so the oracle goes 43/43 honestly
     rather than papering over it.

## Bottom line

Two hypotheses died this session:
1. Post 2's **MMU-walk** story — wrong; the walk is faithful and the fault fires
   (page-fault-as-trap works, guard recognized, overflow reported).
2. This note's own first-draft **"benign clock-units"** story — also wrong; the
   guard task is *genuinely starved* by snemu's cooperative kernel-task
   scheduling (no fault in 200M clean steps).

The real bug is a **snemu scheduler-fidelity gap**: a hart-0-spawned cooperative
kernel task that QEMU runs within 0.2s never gets serviced under snemu. The
precise root is (a) vs (b) above — the guard task landing in a runqueue that the
servicing hart never reads — and needs one instrumented run logging the spawn
hart index. Fixing it should take the three guard workloads to PASS honestly and
may tighten other cooperative-task interleavings the oracle currently absorbs.

**Debug edits used and reverted:** in `snemu/src/cpu.rs` — a `translate_or_trap`
KSTACK-window `eprintln!`, a `dbg_timer_traps` field + timer-trap counter, and a
guard-fault `eprintln!`; in `kernel/src/sched/mod.rs` — a `println!` in
`touch_current_stack_guard`. All removed (`git status` clean except this note).
