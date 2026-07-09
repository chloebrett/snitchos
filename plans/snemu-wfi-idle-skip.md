# snemu wfi idle-skip (fast-forward)

## Outcome (measured) — the premise was wrong

**Shipped, correct, and it gives ~0 speedup on this suite.** The hypothesis below
— that the compute tail is dominated by *idle* emulation — turned out false. The
kernel's heartbeat loop busy-yields through the scheduler (`heartbeat.rs:159`),
and the itest workloads have always-ready tasks (task_a/task_b burning LCG, the
OOM allocation loops), so the instructions between heartbeats are **real work,
not wfi spinning**. Verified two ways:

- Full audit with idle-skip on vs. off (the `--no-idle-skip` flag): total instret
  and per-scenario cost are **byte-identical**; fidelity stays 106/108.
- Even the idle-by-design `init` scenarios are identical on/off (43M/41M/34M),
  because they reach their assertion during boot before the system ever settles
  into a steady idle loop. Only 3 of 108 scenarios use `init` at all.

So idle-skip is kept as a **fidelity improvement**, not a speed win: snemu now
models `wfi` like real hardware and QEMU (halt-until-interrupt) instead of a nop
that spins, with **zero regressions**. It's the right foundation for a future
idle-waiting workload (an interactive shell blocked on keystrokes), which this
suite simply doesn't contain. It ships behind `set_idle_skip` (default on;
`snemu-itest --no-idle-skip` for the A/B), mirroring the decode-cache flag so the
pure-interpreter path stays provably the oracle.

The actual speed lever for the compute-bound tail is the post-04 JIT, not idle
handling. The audit's big win was the multi-core host fan-out (355s → 66s, 5.4×).

## Why (original hypothesis — see Outcome above for how it held up)

The snemu-itest slowest-by-instret table (added alongside the multi-core audit
fan-out) shows the compute tail is dominated by **idle emulation**, not work:

| Minstret | scenario |
|---:|---|
| 1331M | frame-allocator-oom |
| 478M | heap-oom |
| 467M | workload-cooperative-baseline |
| 422M | spawn-reclaims-memory / -names |
| 400M | smp-tlb-shootdown-visible |
| 356M | sched-yield-round-trips |

These are **heartbeat-cadence-gated**: the metric that satisfies the assertion is
emitted once per heartbeat, and between heartbeats the kernel sits in the idle
task's `loop { wfi; yield_now() }`. Today snemu's `wfi` is a plain nop
(`cpu.rs:880`, "no interrupts to wait for in the interpreter"), so the machine
grinds through the entire ~220M-instruction inter-heartbeat gap one instruction
at a time until the instruction-clock (`time`) reaches `stimecmp` and the timer
fires. The bulk of the audit's instructions are spent emulating *idle*.

Real hardware — and QEMU — **halt** on `wfi` until an interrupt is pending; they
do not spin the idle loop millions of times. So idle-skip makes snemu *more*
faithful to QEMU, not less, while collapsing the tail. No test changes: this
preserves post-5's "don't rewrite the tests" thesis.

## Design

Model `wfi` as **blocking until an interrupt is pending**, and let the machine
**fast-forward the clock** to the earliest wake when every hart is idle.

- **`HartState::Idle`** (new, alongside `Running`/`Stopped`). A hart enters `Idle`
  when it executes `wfi` with no interrupt currently pending; it leaves `Idle`
  (→ `Running`) when `pending_interrupt()` becomes `Some` (its timer reached
  `stimecmp`, or an IPI raised `sip.SSIP`).
- **`wfi` execution**: advance PC past `wfi` (unchanged), then `if
  pending_interrupt().is_none() { state = Idle }`. If an interrupt is already
  pending, `wfi` stays a nop (the pending interrupt is taken next step).
- **`Hart::wake_deadline() -> Option<u64>`**: `stimecmp` iff the timer is
  *armable* — `sie.STIE` set and the `sstatus.SIE`/privilege gate met (the same
  gate as `timer_interrupt_pending`, minus the `cycle < stimecmp` check). `None`
  if no timer can wake it (then only an IPI can — impossible if all harts idle).
- **`Machine::step`**:
  1. If no hart is `Running` (all `Idle`/`Stopped`), fast-forward:
     `time = max(time, min wake_deadline over Idle harts)`. This makes the
     earliest timer pending. If no `Idle` hart has a deadline, do nothing (the
     step budget bounds the run; a real kernel never idles all harts with no
     timer armed).
  2. For each hart: `Stopped` → skip. `Idle` → `set_cycle(time)`; if it now wants
     a wake, `wake()` and step it (delivers the trap, `time += 1`); else skip
     (retire nothing, no clock tick). `Running` → step as today.

Cross-hart correctness: while hart 0 does real work, `time` advances per
instruction, and an `Idle` hart 1 is checked every round against the shared
clock — so hart 1's timer fires mid-round-robin without needing the all-idle
jump. The jump only covers the *all-idle* case, where nothing else moves the
clock.

`Cpu` (single-hart wrapper): must apply the same jump for its lone hart so the
~94 cpu tests and single-hart boots keep working — an `Idle` hart with a
deadline advances its own clock to it.

## The `time` = `instret` semantic shift (call it out)

`time` is both the `rdtime`/`stimecmp` clock **and** `Machine::instret()`.
Fast-forwarding is correct for the *clock* (idle real-time passes), but it means
`instret()` no longer counts only retired instructions — it counts guest *time
units*, idle included. Consequences:

- **rdtime / heartbeat timestamps**: still monotonic, cadence unchanged (gaps
  were already ~220M; now the gap is one jump instead of 220M steps). Scenarios
  assert monotonicity, not magnitude (post-5 rewrote `heartbeat-cadence`). ✓
- **`snemu bench` MIPS** (`instret / wall_clock`): meaning shifts from
  "instructions/host-sec" to "guest-time-units/host-sec". Determinism (same
  workload → same `instret`) is preserved (the jump is deterministic). Accept and
  document; if bench needs true retired-count later, add a separate `retired`
  counter (the multi-hart plan's anticipated "Machine-level cycle counter").
- **audit `steps_taken`**: counts `Machine::step` *rounds* (host-loop), not
  `time` — so it correctly drops. This is the metric the win shows up in.

## Increments (each TDD, green throughout) — all ✅ SHIPPED

1. **`HartState::Idle` + `wfi` blocks.** ✅ A hart that runs `wfi` with no timer
   armed enters `Idle`; `is_running()` false; `wfi_is_a_nop_that_advances` still
   holds. Gated on the `idle_skip` flag (default on).
2. **`Machine` fast-forward.** ✅ `earliest_wake_deadline` + the retired/skip
   loop. Tests: an all-idle machine jumps `time` to the earliest armed deadline
   and wakes exactly that hart; a running hart advances the clock and wakes an
   idle peer *without* a jump.
3. **`Cpu` wrapper parity.** ✅ Single-hart `Cpu::step` applies the jump; 135 cpu
   tests green.
4. **Flag + validation.** ✅ `set_idle_skip` on `Hart`/`Cpu`/`Machine` (default
   on) + `snemu-itest --no-idle-skip`; parity test proves off = bare-nop `wfi`.
   Full `snemu-itest` stayed 106/108 on **and** off, byte-identical — the
   validation that idle-skip changes only speed (of which there was none here;
   see Outcome).

## Non-goals

Modelling `wfi` power states, `mie`/M-mode timers (snemu is S-mode + Sstc only),
or external/PLIC interrupts as wake sources (none modelled). Timer + IPI wakes
only.
