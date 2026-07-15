# snemu-itest performance: remaining options

Status: options analysis, 2026-07-15. Follows [posts/snemu-09-the-fast-part-wasnt-the-native-part.md](../posts/snemu-09-the-fast-part-wasnt-the-native-part.md),
which established the current state: the block-JIT frontend (Backend A, `--speedup hi`)
is the ~3× lever at ~3.1s for the full suite; Backend B native codegen measured
*slower* (~3.6s); the harness side is maxed (snapshot tree, 99% worker utilisation,
per-scenario clone measured at 0.19s). This doc catalogues what's left, priced by
what we now know about where guest instructions actually go.

## The reframing fact

**The waiting that's left in snemu isn't timer waiting — it's spin waiting.**

Idle-skip already makes heartbeat-bound time nearly free: when every hart is parked
on `wfi`, `Machine::step_round` (`snemu/src/machine.rs:228`) jumps the shared clock
straight to the earliest `stimecmp` deadline (`snemu/src/cpu.rs:1455`,
`wake_deadline`), so "wait 8 heartbeats for OOM" costs almost nothing in emulated
instructions.

What idle-skip *cannot* touch is any hart that busy-polls, and the kernel is full of
those:

- TLB-shootdown ack wait — `kernel/src/mem/mmu.rs:688` (initiator spins on
  `shootdown_ack` with `Acquire`; the ack is a plain store, not an interrupt, so
  `wfi` is not an option there as-written).
- IPI-pong / reader / setup-sync / ping-pong loops —
  `kernel/src/workloads/storms.rs:332, 610, 623, 705`. Several fence via a **UART
  MMIO read** (`fence_via_uart_lsr()`) *inside the loop body* — a bus round-trip per
  iteration.
- **The cooperative scheduler itself.** With `task_a`/`task_b` always ready, the
  runqueue never empties, the idle task's `wfi` never runs on a busy hart, and the
  demo workload is one giant unskippable yield-loop.

This matches the profiler repeatedly naming "scheduler and spin-wait loops" as the
pole. Every option below is priced against that picture.

---

## Measurement result (2026-07-15) — the pole is telemetry, not spinning

Option 1 turned out to be already-built: `cargo xtask snemu-profile` does exact
per-PC instret counting rolled up to kernel function names (`xtask/src/snemu_profile.rs`,
`snemu/src/symbols.rs`). Ran it 400M post-boot instructions on the two workloads that
should discriminate the hypothesis — cross-hart `smp` and single-hart `demo`.

**Neither is spin-wait bound.** The dominant cost in both is the **per-heartbeat
telemetry emission path** — iterator-heavy walks over kernel data structures every
tick — plus a debug-build tax (the itests run the unoptimized kernel for fidelity).

`demo` (single-hart), top of 800M retired:
- 16.5% `Range::spec_next`, 12.2% `Step::forward_unchecked`, 5.9%
  `unchecked_add::precondition_check` — debug-build iterator/UB-check machinery.
- 10.3% `kernel::sched::demo_tasks::burn_lcg` — the demo's **deliberate** busy-loop.
- ~14% iterator adapters (`enumerate`/`map`/`flatten`/`chain`/`fuse`::next) + intern
  table (`InternTable::lookup_or_insert`, `as_str`) — the heartbeat per-task metric walk.
- **No spin-wait function anywhere in the top 20.**

`smp` (cross-hart), top of 800M retired:
- 11.3% `slice::Iter::position` — a linear scan (largest single line).
- ~15% atomics + lock (`atomic_load` 7.0%, `Atomic<bool>::load` 4.3%, `Mutex::lock`
  3.8%) — the closest thing to cross-hart polling, but second-order, not the pole.
- ~10% BTree navigation (`btree::search`/`navigate`/`into_kv`) — intern-table iteration
  for per-tick telemetry.
- 3.4% `kernel_core::stack::high_water_bytes` — the per-tick stack high-water scan.
- ~10% the same iterator-adapter family as `demo`.

**Consequences for the options below:**
- **Option 2 (spin-wait elision) and Option 3.1 (wfi-convert spins) are demoted.**
  The profiled poles are not spin-bound; at most ~15% of `smp` is cross-hart atomics,
  and `demo` has none. `smp-tlb-shootdown-visible` (a 100%-spin negative-oracle
  scenario) remains a genuine spin case, but it's already budget-capped at 60M and is
  not where the bulk instret lives.
- **The real lever is doing the per-heartbeat telemetry walk fewer times** — i.e.
  reaching each budget-bound scenario's threshold in fewer heartbeats (Option 3.3,
  now promoted to first), and/or making the per-tick walk itself cheaper (the
  `slice::position` linear scan and intern-table BTree iteration are hot in a path
  that fires every tick — a kernel-perf win that also helps real hardware).
- **`burn_lcg` (10% of `demo`) is deliberate busy-work** — the demo's iteration count
  is a direct knob on `workload-cooperative-baseline`'s cost.

## Option 1 — Per-scenario instret breakdown (measure first)

**What:** a report classifying each scenario's guest instret into
**spin-wait / IPI-ack-barrier / scheduler-idle-churn / real work**, by bucketing PCs
against known symbol ranges. The guest-side equivalent of the `--speedup` ladder
table: "scenario X spends 84% of its instret in `yield_now` + idle churn."

**How:** the pieces exist and need joining.

- `snemu_audit::record_shared_stream` (`xtask/src/itest/snemu_audit.rs:47`) already
  tags every decoded frame with the instret it arrived at, per scenario.
- The profiler (`xtask/src/itest/snemu_profile.rs:55`) already folds a PC→instret
  histogram into named per-function buckets.

Compose them: per-scenario PC histogram → behaviour buckets → one table.

**Cost / risk:** ~a day of xtask-only work. No emulator or kernel changes, no oracle
risk.

**Why first:** the table decides everything else. Options 2–4 have payoffs that range
from 15% to 3× depending on the spin-wait/real-work split, and snemu-09's lesson
(three plausible optimisations in a row measured flat or negative) says don't build
before pricing.

## Option 2 — Spin-wait elision in snemu (likely the big lever)

**What:** idle-skip's sibling. Recognize a block that is a **pure poll loop** — a
load (typically `Acquire`), a compare, a backward branch, no stores, no MMIO, no
AMOs, no other side effects — and instead of re-executing it, park the hart in a new
`HartState::Polling { addr }` until either (a) another hart stores to the watched
address, or (b) an interrupt becomes pending. Essentially modelling ARM WFE/SEV
without the guest asking for it.

**How:** the block JIT's reified `Vec<Op>` IR makes poll loops recognizable at
compile time in `block.rs` lowering. The machine dispatcher already round-robins
harts and mediates all stores, so wake-on-write is implementable at the
`Machine::step_round` level, symmetrical with how all-harts-idle fast-forwards the
clock today.

**Classifier must be conservative:** any load from a non-RAM region, any store, any
AMO in the body → not elidable. Note the storms loops fence via UART MMIO inside the
body, so they may not qualify as-written (see option 3 — that's fixable guest-side,
or acceptable: the storms are intentionally expensive memory-ordering repros).

**Oracle cost — the real decision:** elision **forfeits instret byte-identity**. The
parked hart retires fewer poll iterations than the faithful run, so the A/B oracle
that has protected every snemu optimisation so far can't be "byte-identical instret"
for this one. Fallback oracle is **verdict-level**: identical frame *sequence* and
identical scenario verdicts, elision on↔off. Weaker but still strong — the frame
stream is causally downstream of everything except how many times a loop polled.

**Scope limit:** this does not help single-hart scheduler churn — yield-loops are
not pure polls (they context-switch, emit spans, do work). It targets the cross-hart
storm/SMP scenarios specifically. Whether that's 20% or 60% of suite instret is
exactly what option 1 answers.

## Option 3 — Guest-side cooperation (cheapest wins, zero emulator risk)

Three sub-options, all kernel/xtask-side, all of which also improve QEMU runs and
real hardware:

1. **Make spin waits wfi-eligible where an interrupt genuinely ends them.** Audit
   each loop in `storms.rs` / `mmu.rs` for "what actually wakes me"; any wait
   terminated by an IPI or timer can spin-then-`wfi` and becomes idle-skippable for
   free. (The shootdown ack wait does *not* qualify as-written — the ack is a plain
   store. Converting it would mean adding an IPI-back-ack, a design change to weigh
   separately.)
2. **Get the MMIO fence out of poll bodies** — or poll N times per fence. Each
   `fence_via_uart_lsr()` is a bus round-trip per iteration in snemu. If the fence is
   load-bearing for the memory-ordering repro the storm characterises, keep it and
   accept the cost knowingly.
3. **Tunable heartbeat period via bootargs.** `TICKS_PER_HEARTBEAT` is a hardcoded
   const (`kernel/src/trap/mod.rs:71`); `bootargs::param_usize`
   (`kernel-core/src/workloads/bootargs.rs:270`) already exists, so `hb_div=<n>` is a
   small, host-testable change. **Caveat:** per the reframing fact, this only pays
   for scenarios where busy tasks pin the clock to real instret *while* the scenario
   waits for the Nth heartbeat (e.g. the OOM leaks, which leak per heartbeat while
   the workload churns). Pure-idle heartbeat waits are already skipped. Option 1
   prices this before touching the timer path.

## Option 4 — Block chaining (emulator-core endgame; do last)

**What:** at a block's exit, enter the successor block directly instead of returning
to the dispatcher. The hook point is concrete: every block exit already resolves its
successor PC before returning (`snemu/src/block.rs:451` sets PC, returns to the
dispatch at `snemu/src/cpu.rs:892`).

**The delicate part:** chaining skips the per-block `pending_interrupt()` check
(`snemu/src/cpu.rs:881`). Today's contract is "interrupt delivery at most one block
late"; chaining makes it "at most one *chain* late," so a chain budget is required —
check interrupts every N blocks, or on every backward edge — to keep timer delivery
bounded and deterministic.

**Two sequencing insights:**

- **Prototype chaining in Backend A first.** Nothing about chaining requires native
  code — an interpreted block can tail-dispatch into the next cached block. That
  isolates "what does chaining buy" from "what does native codegen buy," in the
  spirit of the speedup ladder. Only if A-with-chaining wins does B's native version
  (a patched jump, truly zero-cost) have a reason to exist — and it's the thing that
  would finally let B beat A.
- **Payoff is anti-correlated with options 2–3.** Chaining shines on hot loops;
  options 2–3 delete hot loops. Re-measure after them.

## Option 5 — Memory-op codegen for Backend B (deprioritized)

Still the nominal "next lever" on the B roadmap, and the prerequisite for the
genuinely hot (load/store-bearing) blocks to run native at all. But the ladder data
already bounds it: native memops as an interpreter feature were part of a ~20% tier
(`med`), and B must first claw back its per-block `extern "C"` call overhead before
showing any gain. Worth doing only if B is being kept alive as the chaining
substrate (chaining into a block that immediately falls back to A is pointless). As
an itest wall-clock play, the weakest option here.

---

## Recommended order

1. **Instret breakdown** (option 1) — xtask-only, ~a day; produces the table that
   prices everything else.
2. **Guest-side fixes the table points at** (option 3) — cheap, oracle-safe,
   benefits QEMU too.
3. **Spin-wait elision** (option 2) if the table shows cross-hart polling dominates —
   the one big emulator feature with a clear mechanism and a defensible
   (verdict-level) oracle.
4. **Chaining, prototyped in Backend A** (option 4), only if real-work hot loops
   remain the pole after 2–3. Backend B memops (option 5) only in service of native
   chaining.

## Key file reference

| Thing | Where |
|---|---|
| wfi idle-skip (hart state, deadline calc) | `snemu/src/cpu.rs:28, 1375, 1455` |
| All-idle clock fast-forward | `snemu/src/machine.rs:228` |
| Per-block interrupt check / block dispatch | `snemu/src/cpu.rs:881, 892` |
| Block exit paths (chaining hook) | `snemu/src/block.rs:395-527` |
| Heartbeat period const | `kernel/src/trap/mod.rs:71` |
| Bootarg param parsing | `kernel-core/src/workloads/bootargs.rs:270` |
| Shootdown ack spin | `kernel/src/mem/mmu.rs:688` |
| Storm spin loops (UART-fenced) | `kernel/src/workloads/storms.rs:332, 610, 623, 705` |
| Frame↔instret tagging | `xtask/src/itest/snemu_audit.rs:47` |
| PC→bucket profiler | `xtask/src/itest/snemu_profile.rs:55` |
