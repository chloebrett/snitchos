# Residual cross-hart race investigation

Follow-up (a) from `plans/deflake-bisection.md`. The trap-return `tag()`
masked the bulk of the race; ~8% per-run still flakes with the same
QEMU-disconnect fast-exit signature. This document plans the
investigation — no code yet.

## What we know

- Single failure mode: kernel wedges, QEMU's virtio-console client
  disconnects, integration harness sees fast-exit.
- No single bad scenario — different runs trip on different scenarios.
- Vanishes entirely under `tcg,thread=single` (memory of run from the
  TCG single-thread counterfactual).
- The current "fix" is `crate::tag("trap return")` at the end of every
  `trap_handler` invocation (`kernel/src/trap.rs:133`). The mechanism is
  the MMIO write itself — QEMU's MMIO path acquires the Big QEMU Lock,
  which serialises against other vCPUs and incidentally provides a
  cross-hart memory fence that the kernel's `Release/Acquire` atomics
  don't (under multi-thread TCG).
- 8% residual means roughly 1 in 12 runs. With `--repeat` we can
  generate enough signal in ~5 minutes per experiment.

## The hart 1 pickup path

Hart 1 spends most of its time in `idle_entry`'s `wfi`. The race fires
when hart 0 enqueues new work and IPIs hart 1. The path on hart 1:

1. **Trap entry.** Hardware sets `sepc`, traps to `trap_entry` (asm),
   saves GPRs into `TrapFrame`, calls `trap_handler`.
2. **`handle_pending`** (`ipi.rs:84`). Clear `SSIP`. Then
   `swap(0, Acquire)` on `ipi_pending` — paired with the sender's
   `fetch_or(_, Release)` (`ipi.rs:97`).
3. **`tag("trap return")`** (`trap.rs:133`). Currently masks the bug.
4. **`sret`.** Hardware restores `sepc`, jumps back into `idle_entry`'s
   loop body — specifically the instruction after `wfi`.
5. **`sched::yield_now()`** (`sched.rs:429`). Takes `SCHEDULER.lock()`
   (spin Mutex), pops next id from this hart's runqueue.
6. **Walk `sched.tasks`** to find the next `*mut TaskContext`. Pointer
   is to a `Box<Task>` allocation on the heap, so stable.
7. **Drop the scheduler lock.** Mutex release.
8. **`switch(current, next)`** (`sched.S:32`). Save callee-saved into
   `*current`; load callee-saved from `*next`; `ret`.
9. **First instruction of new task** — `task_a`, `task_b`, or whoever
   was just spawned.

The sender side on hart 0 (e.g., `spawn_on(1, task_a)`,
`sched.rs:388`):

1. Heap-allocate the new stack (`Stack::new_zeroed` — writes 16 KiB).
2. Heap-allocate the `Task`, including its `TaskContext`.
3. Write `ctx.ra = entry as u64; ctx.sp = sp_top`. **This is the
   cross-hart payload.**
4. `SCHEDULER.lock()` (spin Mutex).
5. `tasks.push(task); runqueues[1].push_back(id)`.
6. Drop lock.
7. `ipi::send(1, IPI_WAKEUP)` →
   `fetch_or(IPI_WAKEUP, Release)` then SBI `send_ipi`.

## Hypotheses, in declining order of suspicion

### H1: `*next_ctx` dereference in `switch` sees stale `ra`/`sp`

The asm at `sched.S:48-49`:

```
ld ra, 0(a1)
ld sp, 8(a1)
```

These reads happen after the scheduler lock has been dropped. The
happens-before chain *should* be:

- Hart 0: write ctx.ra/sp → Mutex release → IPI Release.
- Hart 1: IPI Acquire → Mutex acquire → read ctx.ra/sp.

Either edge alone should suffice. If both fail under multi-thread TCG,
the asm loads see zero or stale prior values, the `ret` jumps to
0x0 or the wrong PC, and the kernel faults silently. Strongest
candidate.

### H2: New task's stack reads see uninitialised data

Hart 0 zeros the stack in `Stack::new_zeroed`. If those writes haven't
propagated when the new task's first instruction touches the stack
(prologue, first call, first stack-relative load), it reads garbage.
Same MMU-side faulting plausibility as H1. Less likely if H1 is
already covered by the same fence, but worth distinguishing because
the stack writes precede the ctx writes in program order — a partial
fence might cover one and not the other.

### H3: Runqueue payload itself

`Runqueue::pop_front` returns a `TaskId`. The receive side observed
the queued id before grabbing the context pointer. If the
`push_back(id)` write hasn't propagated, `pop_front` returns `None`,
hart 1 returns from `yield_now` without doing anything, the IPI is
effectively lost, and we just keep idling — no crash. So H3 explains
"work goes missing" but not "kernel wedges." Probably not the bug we
have.

### H4: SpanCursor pointer race

`CURRENT_SPAN_CURSOR.this_cpu().store(next_cursor, Relaxed)` happens
before the switch. `tracing::span_start` reads it on the new task's
side. Same shape as H1 but for telemetry, not control flow — would
corrupt spans, not crash.

### H5: Compiler reordered something across the lock drop

`spin::Mutex`'s `Drop` impl performs a `Release` store. The compiler
must keep `ctx.ra/sp` writes before the drop. We trust LLVM here, but
worth eyeballing the disassembly for `spawn_on` and `yield_now` once.

## The MMIO Heisenberg problem

Any probe that touches MMIO acquires the BQL and may close the race
we're trying to observe. The trap-return tag is exactly this. So:

- **`tag()` is unusable for fine-grained instrumentation.** Every call
  is a fence.
- **Plain memory writes are observable.** Per-hart `AtomicU64`
  progress counters bumped at each step give us "how far did hart 1
  get" without an MMIO fence. They're polled by hart 0's heartbeat,
  which emits them as metrics. A flake shows up as a stalled counter
  on the wire.
- **Counters must be `Relaxed` and `PerCpu`.** Anything else
  introduces ordering of its own.

## Experiment plan

### E1: Characterise the residual signature without the fix

Remove `tag("trap return")`. Run `cargo xtask itest --repeat 20`.
Catalogue per-scenario flake rates and where the QEMU log cuts off.
Need a baseline to test interventions against.

**Predicted outcome:** flake rate climbs from ~8% to ~80%+, kernel
dies at different points each run with no obvious pattern.

**Falsifies:** "the fix is doing nothing." If removal doesn't change
the rate, our model of what `tag()` is doing is wrong.

### E2: Add per-hart progress counters, re-add fix

Insert atomic bumps at points 2, 5, 6, 8 (just before asm), and at
the entry of each task body. Heartbeat emits the latest values.
Reintroduce `tag("trap return")` so the suite is stable.

Predicted outcome: under stable suite, counters all advance
monotonically; a successful steady-state run looks normal.

This step establishes the instrumentation harness without claiming
anything about the bug.

### E3: Remove fix, observe stalled counter

Remove `tag("trap return")`. Run `--repeat 50`. On flake runs, inspect
the last-emitted counter values. The hart-1 step whose counter is one
less than the others is where it stalled.

**Predicted outcome:** stalls cluster at step 8 (post-switch) or task
entry — H1 / H2 territory. If they cluster at step 5 or 6, H3 is
back in play and the model needs reworking.

### E4: Targeted fence

Replace `tag("trap return")` with the cheapest cross-hart fence we
can find that explains the win:

- `fence iorw, iorw` — RVWMO fence over all loads/stores incl. I/O.
- Read of a no-op MMIO reg (UART LSR — cheap, no side effects).
- SBI ecall probe (extension 0).

Try each, `--repeat 50`. Whichever clears the flake without log noise
is the production fix.

**Predicted outcome:** `fence iorw, iorw` alone does *not* close it
(consistent with the broad fence sweep mentioned in the deflake doc).
Single MMIO read does close it. SBI probe inconclusive.

If `fence` alone closes it under instrumented build but not vanilla,
that's evidence the bug is sensitive to instruction sequence, not
just memory ordering — file a QEMU bug.

### E5: Real fix attempt

If E4 identifies a specific load that's the stale-reader (say, the
`ld ra, 0(a1)` in `switch`), try a fence *inside* `switch.S` right
before the loads. If that closes the race, the fix is local and
correct (no MMIO needed); we file the QEMU multi-thread TCG behaviour
as a documented "weaker than RVWMO" finding rather than a kernel bug.

## What "done" looks like

One of:

- We find a kernel-side ordering site where adding a single fence
  (without an MMIO write) closes the race. Land the fence, remove
  `tag("trap return")`, document.
- We construct a minimal QEMU repro outside SnitchOS — small kernel,
  one IPI, no kernel infrastructure — and the same flake reproduces.
  File upstream. Keep the `tag` fix as long as we run multi-thread
  TCG.
- We can't reproduce in a minimal harness but the residual remains
  under SnitchOS. Document, defer, move on.

## Tooling we'd need

- Per-hart progress counters: `PerCpu<AtomicU64>` for the pickup
  sequence; bumped at each step. Emit in heartbeat as
  `snitchos.deflake.hart1_step_<n>`.
- Heartbeat needs to emit values from *both* harts. Today only hart 0
  emits. Either: have hart 1's idle loop bump a "last seen step"
  counter and hart 0 reads it (one-way; works because counters only
  ever increase), or: extend the heartbeat to walk per-CPU data.
  Probably the former — simpler.
- Harness mode that runs `--repeat N` and aggregates per-scenario
  per-step distribution. Today `--repeat` reports per-scenario flake
  counts; we'd need an extra column for "last step value seen before
  flake." Defer until E3 — until then, manual log inspection.

## Risks / pitfalls

- **Probe effect.** Even atomic bumps could affect timing enough to
  hide the race. Mitigation: counters are `Relaxed`, single instruction
  each. If they suppress the race, that's itself a finding.
- **Stale counters at the moment of crash.** If the crash takes the
  kernel down before the heartbeat reads the counter, we see the
  last *successfully emitted* value, not the value at crash. Manage
  with: heartbeat cadence frequent enough; counters bumped before
  the suspected stale read so we see "got to step N but not N+1".
- **Single-stepping QEMU.** `-d in_asm,int` would capture every
  instruction but slows everything 100×. Last-resort, only after E3
  narrows the window.

---

## Appendix A: High-rate repro — `deflake-spawn-storm` scenario

The 8% suite-wide rate is one trial per scenario. Iterating on (a) at
that rate is slow. Goal: a single scenario that flakes ≥80% per run
without the fix so each experiment converges in `--repeat 5`.

### Per-trial mechanics

A "trial" is *hart 1 in `wfi` → IPI arrives → trap → `yield_now` →
`switch` into the freshly-spawned context*. Per-trial probability `p`
times N independent trials gives roughly `1 - (1-p)^N` per-run flake
rate. If `p ≈ 0.01`, N=200 ⇒ 87%; N=500 ⇒ 99%.

Constraints that maximise `p` per spawn:

- Hart 1 must be in `wfi` when the IPI fires — otherwise the IPI
  coalesces into the pending bit and doesn't produce a fresh trap.
  Therefore hart 0 must serialise: spawn, wait for ack, spawn again.
- No BQL fence between hart 0's `ctx.ra/sp` writes and the IPI send.
  Today `spawn_on` calls `tracing::emit_thread_register` between
  them (virtio MMIO write — BQL acquired). Gate that out under the
  feature flag so the storm's spawns are fence-free on the sender
  side.

### Scenario shape

Kernel build: `--features deflake-spawn-storm` (default off).

Under the feature:

- `kmain` skips `task_a`, `task_b`, workload spawns. The kernel boots
  with just `main` + `idle` on hart 0 and `idle` on hart 1.
- `spawn_on` skips `emit_thread_register`. The downstream ack chain
  doesn't need ThreadRegister, and removing it removes the per-spawn
  BQL fence.
- After one heartbeat settles, hart 0 runs:
  ```rust
  const N: usize = 200;
  for i in 0..N {
      sched::spawn_on(1, deflake_body);
      while {
          fence_via_uart_lsr();
          ACK_COUNTER.load(Acquire) < (i as u64 + 1)
      } { core::hint::spin_loop(); }
  }
  tracing::emit_metric(spawn_storm_acks, N as i64);
  ```
- `deflake_body`: stack-local touch (H2 probe) →
  `ACK_COUNTER.fetch_add(1, Release)` → `loop { yield_now() }`. Tasks
  can't exit; the body sits on the runqueue cycling through yields.
  200 tasks × 16 KiB stacks = 3.2 MiB heap. Fits comfortably.

### Mitigation: MMIO-fenced poll on hart 0

The naive `while ACK_COUNTER.load(Acquire) <= i` spin on hart 0 is
itself a cross-hart load. If multi-thread TCG drops hart 0's Acquire,
hart 0 never sees hart 1's bump, the test times out, and we can't
tell apart "kernel wedged on hart 1's pickup" (the bug we want) from
"hart 0 can't observe updates" (a different, unwanted, possibly real
problem).

Mitigation: precede each load with a UART LSR read. MMIO read acquires
the BQL → cross-hart fence → guaranteed-fresh memory view. Hart 0 is
the observer, not the hunted hart — adding fences to it is free. The
race we want still lives on hart 1's untouched path.

Helper:

```rust
/// BQL fence via single MMIO read of UART LSR (line status, no
/// side effects). Used by the spawn-storm scenario on hart 0 to
/// guarantee its cross-hart load of the ack counter is not stale,
/// without touching hart 1's pickup path.
fn fence_via_uart_lsr() {
    let lsr = console::emergency_uart_base() + 5;
    unsafe { core::ptr::read_volatile(lsr as *const u8) };
}
```

### Sanity ladder

Before trusting "fix off + scenario fails" as evidence of the bug,
walk this ladder:

| Configuration                                     | Expectation          | Meaning                              |
|---------------------------------------------------|----------------------|--------------------------------------|
| Fix on, MMIO-fenced poll                          | 100% pass            | Harness is sound                     |
| Fix on, plain Acquire poll                        | 100% pass            | Hart 0 has no symmetric bug          |
| Fix off, MMIO-fenced poll                         | ≥80% flake           | The bug                              |
| Fix off, plain Acquire poll                       | Don't run            | Confounded — two possible failures   |

Row 2 might fail. If it does, that's an interesting independent
finding (hart 0 also flakes on cross-hart loads) and the storm
scenario isn't a clean signal for (a) until we fence hart 0 anyway.
Either way, row 1 must pass for the experiment to be meaningful.

### Why this might not repro

- The race might be specific to the post-`sret` `yield_now` reached
  through `idle_entry`'s `wfi`, not via the trap path generally. The
  scenario covers exactly that path, so it should fire.
- Heap-allocated stacks reuse virtual addresses occasionally. If
  per-trial races depend on cold-cache state on hart 1, repeated
  iterations may warm the cache and lower `p`. The 200-spawn budget
  assumes per-trial rate is reasonably constant; if it falls off,
  we need a stale-cache-friendly variant (different VA each time,
  or explicit `sfence.vma` on the receiver).
- ThreadRegister isn't the only BQL fence in `spawn_on`. Audit
  carefully: the heap `Box::new` calls don't touch MMIO; the
  `SCHEDULER.lock()`/drop is a normal atomic CAS+store; `ipi::send`
  ends with the SBI `send_ipi` MMIO write but that's *after* the
  Release on `ipi_pending`, so the race window has closed for that
  particular fence to help. Looks clean — but verify by stepping
  through the disassembly once we have a build.

### What I will NOT add to this scenario

- Per-step progress counters. Those are E2 territory; this scenario
  is *signal generation* (does the bug reproduce reliably?), not
  *diagnosis* (where exactly does it die?).
- `tag()` calls outside the trap return. Adding MMIO touches inside
  the storm would corrupt the signal we're trying to generate.

---

## Appendix B: First-pass results

### `deflake-spawn-storm` did not reproduce the residual

Three configurations × 20 runs each:

| Fix | UART LSR fence (hart 0) | Pass rate |
|-----|-------------------------|-----------|
| on  | on                      | 30/30     |
| off | on                      | 20/20     |
| off | off                     | 19/20     |

The single failure with both off is consistent with hart 0's
*observer*-side load-acquire flake, not the hunted hart 1 pickup
race: hart 0 was spinning on `ACK_COUNTER.load(Acquire)` without an
MMIO fence and never saw hart 1's `Release` bump. Without the fence
we cannot distinguish that from a real hart 1 wedge, so the 5% is
not load-bearing evidence of the doc's hypothesis.

**Root cause of the low signal:** spawned tasks `loop { yield_now() }`,
so once any deflake task lands on hart 1's runqueue, hart 1
round-robins between it and `hart_1_main` *forever*. Hart 1 only
enters `wfi` once — before the first spawn. Spawns 2–200 arrive
while hart 1 is busy cooperative-switching, so the IPI just sets the
pending bit and hart 1 traps mid-loop rather than `wfi → IPI → sret`.
**Effective trial count per boot ≈ 1, not 200.**

This is a v0.5 cooperative-scheduling constraint: tasks cannot exit,
so the runqueue only grows. Fixing it properly requires task-exit
support (v0.5.x or later).

### `deflake-ipi-pong` falsified the post-sret hypothesis

Hart 0 sends 10 000 paced `IPI_WAKEUP`s to hart 1, no spawning. Each
iteration is one `hart 1 in wfi → IPI → trap → swap-Acquire → sret →
resume` cycle — directly the window the deflake doc suspected.

| Fix | Pass rate | Trials |
|-----|-----------|--------|
| on  | 10/10     | 100 000 |
| off | 10/10     | 100 000 |

**The deflake doc's primary hypothesis is falsified at 100 000 trials.**
The race does not live on hart 1's post-sret memory-ordering window.

### Updated hypothesis ranking

- ~~H1: stale `*next_ctx` dereference in `switch`~~ — still possible,
  but specifically *after sret from IPI trap* is ruled out. The
  spawn-storm could not test the "fresh `switch` after fresh `wfi`"
  case at scale (constraint above).
- ~~H2: new task stack reads see uninitialised data~~ — same caveat.
- **H6 (new): boot-time race, not steady-state.** Maybe the residual
  fires during secondary hart bring-up or the
  `unmap_identity → first heartbeat` window, not during normal
  scheduling. The suite-wide 8% would then map to "8% of boots are
  broken from the start," not "8% of running kernels eventually
  trip." Worth testing by adding a `--repeat`-friendly
  boot-and-immediately-exit scenario.
- **H7 (new): cross-hart heap allocator state.** The frame allocator
  and `linked_list_allocator` both serialise through a `Mutex`, but
  the allocations themselves return raw pointers that the *caller*
  dereferences. If the mutex `Drop`/`Release` doesn't actually fence
  on multi-thread TCG, the caller (potentially on a different hart)
  could read stale bytes.
- **H8 (new): hart 0 load-side, not hart 1.** The MMIO fence we kept
  through `tag("trap return")` was on hart 1's trap exit. But what
  if hart 0 is the hart with the stale read? The fence on hart 1
  flushes BQL globally, fixing both directions. Removing the fix
  could fail by hart-0-side stale loads (e.g. virtio descriptor
  reads after hart 1 wrote them, allocator metadata, etc.).

### What `deflake-ipi-pong` is worth keeping for

- Permanent regression test: 100 k IPI roundtrips clean per run is a
  strong guarantee that the basic SMP IPI mechanism is sound.
- Cheap (~2 s) — adds negligible suite cost.
- Falsifies a class of future hypotheses about the IPI path
  cheaply if we revisit.

### What `deflake-spawn-storm` is worth keeping for

- Smoke test for the cross-hart spawn path itself — proves `spawn_on`,
  `emit_thread_register` (gated), enqueue, IPI, and pickup all wire
  through end-to-end.
- Heap stress as a side effect: 200 × 16 KiB stack allocations + their
  `Box<Task>` allocations.
- If we get task-exit later, the same scenario becomes a real H1/H2
  probe at scale.

### `deflake-shootdown-storm` falsified the payload-read hypothesis

Hart 0 calls `mmu::shootdown(KERNEL_OFFSET)` 5 000 times paced at
~200 µs. Each iteration: write `shootdown_va` → IPI → hart 1 Acquire-
swaps `ipi_pending`, reads `shootdown_va`, sfences, Release-bumps
`shootdown_ack` → hart 0 Acquire-spins on `shootdown_ack`.

| Fix | Pass rate | Trials |
|-----|-----------|--------|
| on  | 10/10     | 50 000 |
| off | 10/10     | 50 000 |

**The Release/Acquire chain on `shootdown_va` is also not where the
residual lives.** Notable because this scenario has a built-in
confounder: hart 0's spin-wait inside `mmu::shootdown` is itself an
Acquire load that, if multi-thread TCG dropped it, would wedge hart 0
before hart 1 could be blamed. It does not wedge — so neither
direction of the cross-hart Acquire is broken at this scale.

### Combined falsification summary

| Code path | Trials (fix off) | Pass rate |
|-----------|------------------|-----------|
| Hart 1 post-sret resume (ipi-pong)        | 100 000 | 100% |
| Hart 1 IPI payload read (shootdown-storm) |  50 000 | 100% |
| Hart 1 fresh-`switch` post-IPI (spawn-storm) | ~10 effective | 100% w/ fence, 95% w/o |

Two of the original three hypothesis classes are out at high trial
counts. Spawn-storm's design ceiling means we can't test the third
(fresh `switch` post-fresh-`wfi`) at scale without task-exit.

### Hypothesis ranking post-experiments

- ~~H1: stale `*next_ctx` dereference in `switch`~~ — possibly still
  live for the *fresh-context-after-fresh-wfi* case only. Spawn-storm
  could not test at scale.
- ~~H2: new task stack reads~~ — same caveat.
- ~~H3: runqueue id race~~ — already ruled out at planning time.
- ~~H4: SpanCursor pointer race~~ — same.
- ~~H5: compiler reorder across lock drop~~ — implausible.
- ~~H_post-sret (the deflake doc's lead)~~ — falsified at 100 k trials.
- ~~H_payload (shootdown IPI carries data via Release/Acquire)~~ —
  falsified at 50 k trials.
- **H6: boot-time race.** The residual fires during secondary
  bring-up, MMU teardown, or the first-heartbeat window — not during
  steady-state scheduling. Suite-wide 8% maps to "8% of boots come
  up broken." Cheap to test: boot-and-immediately-exit scenario
  under `--repeat 50` with fix off.
- **H7: cross-hart heap allocator state.** Allocator metadata mutates
  under Mutex on one hart and is dereferenced on another. If the
  mutex Release isn't honoured by multi-thread TCG, the dereferencing
  hart can see stale chunk-list pointers, free-list bytes, etc.
- **H8: hart 0 load-side, not hart 1.** The `tag("trap return")`
  fence runs on hart 1's trap exit but flushes BQL globally, so it
  fences hart 0 too. Removing it could fail by hart-0-side stale
  loads (virtio descriptor reads after hart 1 wrote them, allocator
  metadata, anything in `kmain`'s heartbeat that reads a counter
  hart 1 might have bumped).
- **H9: the original 8% residual is at least two distinct bugs.**
  Discovered while refactoring kmain: an unrelated metric-registration
  ordering bug caused `workload-cooperative-baseline` to fail at ~40%
  with a clearly distinct signature — `max wait 41.3s of 45s budget`,
  kernel alive and emitting, just throughput-starved. This is
  **not** the `QEMU disconnected: fast-exit` signature of the
  cross-hart race. The fix was non-controversial (registration was
  interleaving 70 virtio sends with workload-task time slices).
  Implication: when the original 8% was measured, some unknown
  subset may have been *budget-exhaustion* failures (kernel alive,
  just slow), not the cross-hart wedge. Re-baselining needs a
  per-failure signature check before counting it as the race.
- **H10: `tag("trap return")` may be load-bearing for *timing*, not
  *memory ordering*.** The UART write costs a few microseconds per
  trap exit. If a subset of the residual is timing-pressure
  (slow-enough scenario just barely meets its budget), this delay
  could be doing the load-bearing work — not the cross-hart memory
  fence we attributed it to. Falsifiable: replace `tag()` with a
  fixed `rdtime`-spin of equivalent duration (no MMIO, no memory
  side effect). If the race stays suppressed, the fix is timing,
  not fence. If it returns, the fix is the fence. This isolates the
  two effects we've been conflating.

### Next experiments worth trying

- **Boot-bisect.** A scenario that boots and asserts only that the
  kernel reaches the first heartbeat. `--repeat 50` under fix off.
  A non-zero failure rate supports H6 cleanly.
- **Hart 0 load-side audit (H8).** Walk `kmain` post-secondary-
  bringup and the heartbeat path; identify every load that depends
  on something hart 1 wrote. Each one is an Acquire-load that
  multi-thread TCG might drop. Output: a list of candidate stale-
  read sites with proposed local fences. Cheaper than instrumenting.
- **Heap stress storm (H7).** Tight loop of cross-hart spawns where
  the spawned task body does a tight `Vec::with_capacity` / drop
  loop. Forces alternating-hart allocator metadata mutation. Looks
  for stale chunk-list reads.
- **Build task-exit.** Big undertaking, but unlocks a real test of
  the fresh-`switch`-after-fresh-`wfi` window the spawn-storm
  couldn't reach. Defer unless the cheaper experiments above don't
  narrow further.
- **Fence vs delay isolation (H10).** Replace `tag("trap return")`
  with `rdtime`-spin of ~5 µs (matching the UART write's wall
  duration). Run default suite `--repeat 20`. If the suite-wide
  rate stays at the fixed value, the fix was timing-pressure. If
  it climbs to the fix-off baseline, the fix was the MMIO fence.
  This is the cheapest experiment that splits H10.
- **Signature-classify the original 8%.** Re-run the default suite
  `--repeat 50` with fix off. Per failure, classify as
  *disconnect-fast-exit* (kernel wedge → cross-hart race candidate)
  or *budget-exhausted* (kernel alive → timing/throughput
  candidate). Output: two separate rates. Lets us see if the 8%
  is one phenomenon or several (H9).
