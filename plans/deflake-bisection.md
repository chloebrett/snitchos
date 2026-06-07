# Deflake bisection: heartbeat-cadence post-boot wedge

Branch: `deflake`.

## Hypothesis

Post-14 surfaced a residual ~1–2% per-scenario flake rate. Failure mode: kernel
reaches `I am alive — entering heartbeat` then the scenario times out waiting
for the second heartbeat. Looks like a virtio-console or heartbeat-loop wedge.

Suspect: introduced somewhere in the v0.6 SMP work.

## Method

Bisect over commits that touched `kernel/`, `kernel-core/`, `protocol/`,
`collector/` only. xtask stays at HEAD so we keep:

- `/tmp/snitch-itest-*.log` per-scenario QEMU capture
- last-80-lines-on-failure dump
- `pkill qemu-system-riscv64` at suite start
- `--repeat N` aggregate flake report
- per-test wallclock budget surfacing

Workspace `Cargo.toml` stays at HEAD (the only delta vs `2e409f2` is
`exclude = ["learning"]`, which is harmless to older kernels).

### Per-candidate overlay procedure

```
git checkout deflake
git checkout <C> -- kernel kernel-core protocol collector
# patch xtask to compile against older protocol — see below
cargo xtask build
cargo xtask itest heartbeat-cadence --repeat 50
```

### xtask patches required for the overlay

HEAD's xtask references protocol items (`hart_id` field on `SpanStart` /
`ContextSwitch`, `HartRegister` variant) added during v0.6. Patches:

- `xtask/src/itest/harness.rs`: drop `hart_id` from `SpanStart` /
  `ContextSwitch` display arms; remove the `HartRegister` arm.
- `xtask/src/itest/scenarios.rs::smp_secondary_hart_boots`: replace the
  `OwnedFrame::HartRegister` matcher with `|_, _| false` (the scenario can't
  meaningfully run pre-SMP; we only need it to compile so heartbeat-cadence
  can run).

These patches are local-only on the bisection branch — never committed.

## Pinned scenario

`heartbeat-cadence` — introduced `a70f420` (v0.1), last meaningfully changed
`bde9fe3` (error-bound tweak). Exists unchanged across the entire corridor.
Directly stresses the heartbeat loop + virtio path — the subsystem post-14
fingered as the wedge locus.

Regimen: `cargo xtask itest heartbeat-cadence --repeat 50`. Classify:

- **GOOD** = 0 failures
- **BAD** = ≥2 failures
- **AMBIGUOUS** (1 failure) → re-run once

50 runs gives strong signal: at the observed HEAD rate (6%), P(0 failures in
50 | true rate 6%) ≈ 4.5%, so a clean run at a candidate commit is reliable
evidence that commit is GOOD.

## Endpoints (confirmed)

| Commit | Description | Flake rate |
|---|---|---|
| `2e409f2` | post 12 — end of v0.5, pre-SMP | **0/50** ✓ GOOD |
| `main` (efcbbf9) | post 14 — end of v0.6 step 10 | **3/50** ✗ BAD (runs 10, 42, 49) |

## Corridor

`git log --oneline 2e409f2..main -- kernel kernel-core protocol collector`:

```
efcbbf9 More lint fixes. No current clippy warnings.
800cca5 Clippy fixes.
4034d25 expect a dead code snippet
c229605 Clippy fixes
387f793 per-hart runqueue and idle
ce206f1 step 9.3
35b171d multi hart step 9 part 1
fe36ace 2nd hart metrics
db88062 Secondary hart boot scenario; debugged with gdb
0c4d4f2 ipi, sbi
de8d799 ordering documentation
062e745 steps 4 and 5
8ad9f3a update protocol for multi hart
8987556 Add new metrics to dashboard
cc7d764 cooperative histogram workload
3085e5d histogram logic
cb1ab9f lcg workload
```

17 commits → ~4–5 binary search steps → ~20 min test wallclock.

Suspect clusters (oldest → newest):

- `cb1ab9f` → `cc7d764`: pre-SMP workload (LCG + histogram + cooperative).
  Note: these *predate* the SMP cluster — if bisection lands here, the SMP
  hypothesis is wrong.
- `8ad9f3a`: protocol bump for multi-hart (wire format).
- `062e745`: percpu plumbing + weak-memory audit (steps 4 & 5).
- `0c4d4f2`: IPI / SBI primitives.
- `db88062`: secondary hart boot scenario.
- `35b171d` → `ce206f1`: SBI HSM bring-up (step 9).
- `fe36ace`: 2nd hart metrics.
- `387f793`: per-hart runqueue + idle.
- `4034d25` → `efcbbf9`: lint cleanup tail.

## Progress log

| Step | Commit | Result | Notes |
|---|---|---|---|
| 0 (endpoint) | `2e409f2` | 0/50 GOOD | clean baseline |
| 0 (endpoint) | `efcbbf9` (HEAD) | 3/50 BAD | runs 10, 42, 49 failed; UART log shows "I am alive — entering heartbeat" then timeout (post-boot wedge) |
| 1 | `062e745` (midpoint) | 3/50 BAD | runs 10, 23, 39 failed; **different failure mode**: boot-time `panicked at kernel/src/percpu.rs:71:5: hartid out of range` — kernel never reaches "I am alive" |
| 2 | `8987556` | 0/50 GOOD | clean — new GOOD endpoint |
| 3 | `8ad9f3a` | 0/50 GOOD | clean — new GOOD endpoint; corridor narrowed to 2 commits |
| 4 | `de8d799` | (killed at run 33, ≥1 fail) | **redundant step — de8d799 is _after_ `062e745` in commit order, not between it and `8ad9f3a`**. `8ad9f3a..062e745` contains only `062e745`. So Bug A introducing commit is `062e745` itself. |

## Bug A localized: introduced at `062e745` ("steps 4 and 5")

The step-5 percpu plumbing commit. The asm/static layout for `PER_HART_DATA`
and the `percpu::init` bounds check both live in this commit. The
`hartid out of range` panic fires when OpenSBI hart-roulette hands the boot
to mhartid=1 and the bounds check rejects it.

Post-14 ties this to the `LOGICAL_TO_MHARTID` translation introduced in
step 9 (`35b171d` / `ce206f1`) — that's where Bug A was fixed.

## Bug B (heartbeat wedge) — bisection corridor

The HEAD-side flake (post-boot wedge — kernel prints "I am alive" then no
second heartbeat) is a different bug. It must have been introduced between
`062e745` and `efcbbf9`. But within that range Bug A is also alive (until
fixed in step 9), so we'll see two failure modes overlapping until we get
past the Bug A fix commit.

Strategy for Bug B:
- Use `062e745..efcbbf9` (15 commits) as the corridor.
- Classify per failure mode: percpu panic = Bug A (treat as `skip`-like
  for Bug B purposes); post-boot wedge = Bug B (treat as BAD).
- The bisection question becomes: "is the first commit where Bug B fires
  earlier or later than commit X?"
- Once Bug A's fix commit is past, runs should be clean except for Bug B.

Better practical approach: **fix Bug A first**, then re-run HEAD to see if
Bug B even still exists. Bug A's fix is presumably in the commit history
already (`35b171d`/`ce206f1`); cherry-picking it onto `062e745` to confirm
isolation might be faster than chasing two bugs simultaneously.

## Bug B bisection progress

| Step | Commit | Result | Notes |
|---|---|---|---|
| B-0 | `35b171d` | 0/50 GOOD | Bug A fixed here, Bug B not present — new GOOD endpoint |
| B-1 | `387f793` | 0/50 GOOD | structural suspect (per-hart runqueue + idle) is clean |
| B-2 | `4034d25` | 5/50 BAD | same UART trace as HEAD: "I am alive — entering heartbeat" then disconnect ~100ms later. Corridor narrowed to 1 candidate (`c229605`). |
| B-3 | `c229605` | 1/50 BAD | introducing commit confirmed. |

## Bug B localized: `c229605` "Clippy fixes"

Rate is 1/50 here vs 5/50 at the next commit (`4034d25`) — either statistical
noise, or a secondary issue piles on at 4034d25. Either way, `c229605` is
where Bug B enters the tree.

### The diff

All textbook-benign clippy fixes:

- `kernel-core/src/heap_smoke.rs` — `Default` impl, `map_or(true, ...)` →
  `is_none_or(...)`, `n % k == 0` → `n.is_multiple_of(k)`
- `kernel-core/src/intern.rs` — `Default` impl, nested `if let Some(e) { if e.x { ... } }`
  → `if let Some(e) = entry && e.x == ... { return ... }` (let-chain)
- `kernel-core/src/mmu.rs` — `Default` impl, test-only lifetime elision
- `kernel-core/src/preinit.rs` — `Default` impl
- `kernel-core/src/workload.rs` — single `#[allow(clippy::should_implement_trait)]`
- `kernel/src/percpu.rs` — comment punctuation
- `kernel/src/sched.rs` — unused import removal
- `collector/*` — host-only, irrelevant to kernel runtime

None of these should change runtime behavior. But one of them did.

### Hypotheses to investigate (in priority order)

1. **`intern.rs` let-chain rewrite.** `intern` is on the boot + heartbeat
   hot path (every `register_counter_owned`, every span name lookup). A
   subtle codegen difference between nested `if let` and let-chain could
   surface here. The rewrite is macro-level lowering; check the actual
   indentation/scope — the diff shows only `return` inside the block, no
   visible closing brace where the previous nesting would have had one.
   If the let-chain accidentally moved code out of the loop body, lookups
   could behave wrong.
2. **`is_none_or` / `is_multiple_of`.** Theoretically equivalent to the
   old forms, but worth confirming on the kernel's exact stdlib version.
   This is in heap_smoke, which runs at heartbeat — plausible vector.
3. **`Default` impls.** Almost certainly inert, but listing for completeness.

### Next step

Inspect `intern.rs` at `c229605` carefully. If the let-chain rewrite has a
subtle scope error, that's the bug. If it's clean, move to hypothesis 2.

## Hunk-level investigation

Per-file revert sweep — at the c229605 overlay, revert one file at a time
to `387f793`'s version and re-run x50.

| Sub-step | File reverted | Result | Notes |
|---|---|---|---|
| H-1 | `kernel-core/src/intern.rs` | BAD: 1 fail / 28 runs | let-chain rewrite NOT the cause |
| H-2 | `kernel-core/src/heap_smoke.rs` | BAD: 1 fail / 28 runs | `is_none_or` / `is_multiple_of` swaps NOT the cause |
| H-3 | `kernel-core/src/mmu.rs` | BAD: 1 fail / 41 runs | `Default` impl + test-only lifetime elision NOT the cause |
| H-4 | `kernel-core/src/preinit.rs` | BAD: 1 fail / 97 runs (50 clean, then 1 fail at run 47/50 of confirmation pass) | `Default` impl NOT the cause; first 50 was statistical luck — at ~2% true rate, P(0 in 50) ≈ 36% |

## Updated hypothesis: c229605 unmasks, not introduces

Both highest-likelihood files ruled out, same Bug B signature each time.

The c229605 diff is genuinely all benign clippy cleanup — no file in it
contains a semantic change. Yet bisection consistently points here.

Working theory: **c229605's minor codegen ripples (function layout in
`.text`, monomorphization order, inlining decisions) nudge timing enough to
open a race window that pre-existed in the kernel**. The "bug" lives in the
original code; c229605 is the trigger, not the cause.

Evidence:
- Failure rate is 1–10%, characteristic of a timing race.
- Failure mode: kernel boots fully, virtio wedges within ~100ms — classic
  shape for a race between virtio TX queue setup, the first heartbeat,
  and timer IRQ enabling.
- The clippy fixes that *should* be 100% inert sometimes still alter
  codegen via function ordering inside the ELF.

Implication: even if we find the offending hunk via revert sweep, fixing
that hunk won't fix the underlying race. The bisection tells us **where
the timing window opened**, not **what bug exists**.

### Sweep result: codegen-unmasks-race hypothesis confirmed

All four files that *could* plausibly affect runtime (intern, heap_smoke,
mmu, preinit) were reverted individually. Bug B persisted at the same
~1–3% rate in every case. The remaining three files (`workload.rs`
`#[allow]` attribute, `kernel/src/percpu.rs` comment punctuation,
`kernel/src/sched.rs` unused-import removal) cannot meaningfully affect
runtime, so testing them is unnecessary.

**Conclusion: Bug B is a pre-existing race condition in the kernel code at
`387f793` (or earlier). c229605's *aggregate* codegen footprint nudges the
binary layout enough to open the race's timing window. No single hunk in
c229605 is responsible — reverting any one of them leaves the race intact.**

## The actual race

The kernel UART reaches `I am alive — entering heartbeat`, then virtio-console
disconnects within ~100 ms in ~2% of runs. Suspects on the virtio-console
init / first-tx ordering, in order of likelihood:

1. **virtio TX queue race against the first heartbeat span.** First
   heartbeat emits frames via `virtio_console::send`; if device-ready
   handshake (`VIRTIO_STATUS_DRIVER_OK` write, queue notify) isn't
   sequenced before the first send under all codegen layouts, occasional
   wedge.
2. **Timer IRQ enabling vs static initializer order.** `TIMER_INTERVAL_TICKS`
   is set during boot and read by the IRQ handler. If the IRQ enable happens
   before the interval is published with the right memory ordering — possible
   under reordered codegen.
3. **`Mutex<Inner>` first-acquire ordering.** virtio-console's mutex on the
   TX path; if first-acquire happens during a window where the printed
   "I am alive" UART path is still holding its own lock and codegen
   reorders the release, a deadlock.

Next step (separate investigation, not bisection):

- Confirm codegen theory empirically with `nm --size-sort` or
  `riscv64-elf-objdump -d` diff between kernels built at 387f793 and
  c229605. If function ordering / sizes have shifted meaningfully, theory
  confirmed.
- Read `kernel/src/main.rs` boot path from "I am alive" through first
  `kernel.heartbeat` SpanStart. Look for sequencing assumptions between
  virtio init, timer init, and the first frame emission.
- Add stronger ordering primitives (compiler/memory fences, Release/Acquire
  where Relaxed is currently used) on the suspect publication patterns.
  The `kernel::percpu` "memory ordering discipline" doc — added in
  `de8d799` — is the right home for any new invariants surfaced here.

## Process takeaways

- `--repeat 50` is borderline for distinguishing a clean run from a 2-3%
  flake rate; budget for a confirmation pass when a step looks clean at
  the bisection edge. Saved as feedback memory.
- Bisection localizes *where the symptom manifests*, not always *where the
  bug lives*. When the introducing commit's diff is implausibly benign, the
  bisection has found a codegen edge, not a logic edge. Recognize this
  shape: the per-file revert sweep is what proves it.
- Documenting the bisection log as it ran (this file) was invaluable for
  catching my own corridor-direction error at sub-step 4. Keep doing this.

## Bug B root cause: cross-hart memory ordering under multi-thread TCG

### Corrected understanding of the failure

I initially read the UART log as "kernel hangs right after 'I am alive —
entering heartbeat'." That was wrong. The harness's per-failure frame
dump shows the kernel runs well into the workload before dying:

```
Metric { "snitchos.heap_smoke.candidate" value=202 t=13177300 }   # ~1.3 s
SpanEnd { id=SpanId(9) t=13177630 }
ContextSwitch { from=0 to=1 reason=Yield t=13178340 hart=0 }
ContextSwitch { from=1 to=2 reason=Yield t=20703800 hart=0 }
SpanEnd { id=SpanId(5) t=21251040 }
ContextSwitch { from=2 to=3 reason=Yield t=21255000 hart=0 }
ContextSwitch { from=6 to=7 reason=Yield t=21255090 hart=1 }
SpanStart { "task_b.tick" id=SpanId(10) parent=SpanId(0) t=21256700 task=3 hart=1 }
```

Both harts are running, tasks are cooperating, heap smoke is producing
metrics. Then the connection dies. The scenario reports `no second
heartbeat within 20s of the first`. So:

1. Kernel boots cleanly.
2. First heartbeat fires.
3. Kernel keeps running into the workload — several seconds of frames.
4. At some point, the kernel silently exits / faults to M-mode →
   OpenSBI reset → QEMU exits → harness sees socket EOF.

UART output stops at "I am alive" because that was the last `println!`
on the normal boot path; after that the kernel only emits via virtio.
Absence of UART output post-failure is not informative.

### Counterfactual: thread=single TCG

Tested HEAD with `-accel tcg,thread=single` (replacing the default
`thread=multi`) plus the virtio_console::transmit fence. Result:
**100 runs, 0 failures (50/50 + 50/50 confirmation)**.

At HEAD's prior rate of 3-9%, P(0 in 100) ≈ 5×10⁻⁵ to 4×10⁻⁴. The bug
is suppressed by single-thread TCG with extremely high confidence.

### Why this isolates the cause

- `thread=single` multiplexes both vCPUs on one host thread. The kernel
  still *sees* two harts, timer IRQs still fire, tasks still context-switch
  — but at the host level only one instruction executes at a time. Memory
  ops are effectively sequentially consistent.
- `thread=multi` gives each vCPU its own host thread. Two physical CPUs.
  Memory ops may be reordered by the host CPU unless barriers prevent it.

If the bug were a missing lock, a reentrant section, an IRQ-vs-main race
on the same hart, etc., single-thread interleaving would still expose it.
It does not. The bug *requires* parallel execution → it must be a
**memory-ordering issue on cross-hart shared state**.

### What's almost certainly the bug

Post 14's weak-memory audit pass (`062e745` "steps 4 and 5") deliberately
left most kernel atomics as `Relaxed` with the rationale "single hart
today; revisit when SMP arrives." Once hart 1 is genuinely running on its
own host thread, those Relaxed sites that span hart 0 ↔ hart 1 become
genuine cross-hart synchronization points and need Release/Acquire pairs.

Specific suspects to audit (highest priority first):

1. **`SECONDARY_READY` spin-wait** in `kmain` before `unmap_identity`.
   Hart 0 spin-waits for `SECONDARY_READY = true` to know hart 1 has
   trampolined past the identity gigapage before tearing it down. If
   hart 0 sees `SECONDARY_READY = true` (Relaxed) but hasn't seen hart 1's
   page-table-related writes, unmap_identity tears down a mapping hart 1
   is still using → fault on hart 1 → triple-fault → OpenSBI reset.
   Fits the symptom shape exactly (delayed silent reboot after both harts
   are running).
2. **`CURRENT_TASK` per-cpu loads on cross-hart wake.** `spawn_on(hart, ...)`
   sets up the target hart's runqueue then fires an IPI wakeup. If the
   IPI handler reads runqueue state without Acquire ordering against the
   `spawn_on` writer's Release, hart 1 could pop a half-initialized task.
3. **The TLB shootdown `shootdown_va` / `shootdown_ack` pair**. Documented
   as needing Release/Acquire in the percpu.rs docstring. Verify the
   implementation matches the doc — easy to drop one of the orderings
   during refactoring.
4. **Intern table cross-hart access.** If hart 1 emits a frame with a
   name only hart 0 has registered so far, the intern table mutex
   serializes — but the underlying entries are read after lock release.
   The mutex's lock/unlock should provide Acquire/Release, but verify
   `kernel::sync::Mutex` actually emits the right ordering.

### Things ruled out

- **`virtio_console::transmit` missing memory barrier between descriptor
  write and notify.** Hypothesized first because the doc comment explicitly
  flagged it. Fix applied (`fence(Release)` before the notify). Ran HEAD
  + fence at thread=multi for 36 runs, got 3 failures (same rate). The
  fence is correct on its own merits but does not fix Bug B. Keeping
  the change.

### Decision: revert thread=single

`thread=single` would mask the bug and re-introduce timer fairness
problems (the whole reason `thread=multi` was adopted in `318a2e5`). The
real fix is to audit cross-hart atomic orderings.

## Next session

1. Revert qemu.rs to `thread=multi`.
2. Keep the `fence(Release)` in virtio_console::transmit (correct
   regardless of Bug B).
3. Audit `SECONDARY_READY` ordering as the first suspect — `kmain` and
   `secondary_main`. Confirm the spin-wait uses `Acquire` and hart 1's
   publish uses `Release`.
4. Re-run heartbeat-cadence --repeat 100 after each fix to gauge progress.

## SeqCst sweep — atomic ordering ruled out

Replaced every `Ordering::Relaxed`/`Release`/`Acquire`/`AcqRel` in
`kernel/src` and `kernel-core/src` (92 sites total) with `Ordering::SeqCst`.
Re-ran heartbeat-cadence at `-smp 2, thread=multi`: 3 failures in 56 runs
≈ 5.4%, statistically identical to the un-modified baseline.

**Conclusion: the bug is not in any of the kernel's Rust atomic ordering
sites.** Post 14's anticipation that "every Relaxed becomes a question
mark when hart 1 actually runs" turned out to be wrong / incomplete —
the weak-memory audit's classifications were correct as far as the
atomics go.

What this DOES rule in: the race is hiding in something the SeqCst sweep
cannot reach — a non-atomic shared-state pattern.

Candidates:
- `static mut` accessed via raw pointers (TX_QUEUE, TX_STAGING, page
  tables, virtqueue rings)
- The asm `switch` context manipulation
- `sfence.vma` (asm — not a Rust atomic)
- Hart 1's hardware MMU page-table walks against pages hart 0 mutates
- Per-task `SpanCursor` stack (raw array in `Box<Task>`)
- SBI / OpenSBI interaction (cross-hart IPI is mediated by M-mode)
- QEMU multi-thread TCG behavior at one of those boundaries

## Hart 1 disable experiment — bug isolated to hart 1 code

Patched `kmain` to skip the secondary-hart bringup entirely:

- Skip `sbi::hart_start(secondary_mhartid, ...)`
- Skip the `SECONDARY_READY` Acquire spin-wait
- Skip `spawn_on(1, "hart_1_probe", ...)`

Hart 1 stays parked in OpenSBI. QEMU still emulates two vCPU threads at
`-accel tcg,thread=multi`, but only hart 0 executes Rust kernel code.

Result: **100/100 clean** at `cargo xtask itest heartbeat-cadence --repeat 100`.

### Cumulative empirical map

| Config | hart 1 active? | Result |
|---|---|---|
| Baseline (HEAD) | yes | 3-9% fail |
| `tcg,thread=single` | yes | 0/100 |
| HEAD + SeqCst sweep | yes | ~6% fail |
| `kmain` skip hart 1 | **no** | **0/100** |

The race lives in **hart-1-active code paths**: secondary bringup asm,
`prepare_for_secondary`, `secondary_main`, the `SECONDARY_READY` publish,
`spawn_on(1, ...)`, hart 1's IPI handler, hart 1's idle/scheduler loop,
or the probe task itself.

## Wind-back plan

Add hart 1 bringup back in layers and test each.

| Layer | Add back | Hypothesis |
|---|---|---|
| L1 | `sbi::hart_start` + `SECONDARY_READY` wait. Hart 1 boots and idles forever; no spawn_on. | Bug is in the bringup mechanics (asm, tp, trampoline, SECONDARY_READY publish). |
| L2 | Add `spawn_on(1, "hart_1_probe", ...)`. Hart 1 actually runs a Rust task. | Bug is in cross-hart spawn / IPI / runqueue access. |
| L3 | (When step 11 lands) move workload consumer to hart 1. | Bug specific to cross-hart producer/consumer queue. |

Each step ≈ 7 min (--repeat 100). 2-3 narrowing steps land us on the
guilty subsystem; from there it's read-the-code and fix.

## Workaround note (not adopted)

A `--features no-hart-1` build config would suppress the flake while we
investigate. Not pursued — investigation should resolve, not paper over,
the underlying race. But documented here in case CI needs reliability
before the fix lands.

## L2 sub-narrowing — disjoint experiments

After L1 came back 0/200 (hart 1 boots + idles, no cross-hart work), the
next narrowing tested L2 sub-cases. These are DISJOINT, not nested:

| Test | spawn_on(1, …) | IPI fired | Task runs on hart 1 | Result |
|---|---|---|---|---|
| L1 | ✗ | ✗ | ✗ | 0/200 |
| **L2c** | ✗ | ✓ (hart 0 → hart 1) | ✗ | **0/100** |
| **L2a** | ✓ | ✓ (via spawn_on) | ✓ (no-op body, no tracing) | **1/30 BAD** |
| L2 / HEAD | ✓ | ✓ | ✓ (probe with tracing) | ~3-9% |

### Test recipes

- **L2c**: replaced `spawn_on(1, "hart_1_probe", probe_entry)` with
  `ipi::send(1, ipi::IPI_WAKEUP)`. Hart 1's IPI handler runs but finds
  empty runqueue.
- **L2a**: added `extern "C" fn noop_entry() -> ! { loop { sched::yield_now(); } }`
  and used `spawn_on(1, "hart_1_noop", noop_entry)`. No frame emissions
  from hart 1.

### Conclusions from L2c + L2a

- **The IPI dispatch itself is innocent** (L2c clean). Hart 1's IPI
  handler reading `ipi_pending`, dispatching WAKEUP, returning to idle —
  none of that is racy.
- **The probe's frame emissions on hart 1 are innocent** (L2a flakes even
  with a no-op task body). The bug is NOT in cross-hart tracing /
  virtio TX / intern table access from hart 1.
- **The race is in the spawn handoff itself**: spawn_on writing to
  runqueue[1] + IPI delivery + hart 1's yield_now finding the new task
  + the asm `switch` into the new task on hart 1.

### L2a' (post-pickup-idle) — also flaky

Replaced `noop_entry`'s `loop { yield_now() }` with `loop { wfi }` — the
picked-up task does NO scheduler interaction, just sits in wfi forever.

- spawn_on(1, "hart_1_noop", noop_entry-wfi-only) x30 → 1 failure at run 26
  (same fast-disconnect 0.1s signature)

**Implication: the race is in the spawn handoff + first pickup itself**,
not in any post-pickup scheduler activity. The bug fires before noop_entry
even gets to execute its body in a meaningful way.

The race lives in one of:
1. `spawn_on` from hart 0: Task::new_bare (with 2 × register_counter_owned
   = intern lock + virtio TX × 2) → SCHEDULER lock → push tasks + runqueue →
   unlock → emit_thread_register → ipi::send.
2. Hart 1's IPI handler firing during spawn_on's tail.
3. Hart 1's first yield_now: SCHEDULER lock → pop_front → find ctx → unlock.
4. The first asm `switch(current_ctx, next_ctx)` on hart 1.

### Code read — nothing obvious

Read every step of (1)-(4):

- `spawn_on` is a normal mutex-protected push, then virtio TX, then IPI.
  Mutex provides AcqRel; happens-before to hart 1's yield_now is sound.
- `task.context.get()` returns a stable heap address (`Box<Task>` doesn't
  move on `Vec` reallocation), so the raw pointer passed to asm `switch`
  is correct.
- `TaskContext` defaults to all-zero; first switch saves into hart_1_main_ctx
  before loading noop_ctx, so the all-zero default never gets executed.
- Asm `switch` (`kernel/src/sched.S`) is a straightforward callee-saved
  save/restore + `ret`. Sound by inspection.

Whatever the bug is, it's not visible in the kernel's Rust source under
ordinary code review.

### Symptom shape supports silent-reset-to-OpenSBI

Failure UART ends at `I am alive — entering heartbeat` with no kernel
panic message. The Rust panic handler prints "Kernel panic: …" before
halting, so we'd see that for any path through `trap_handler`'s
`panic!()` arm. Absence implies:

- A double-fault: a trap fires during trap entry asm (e.g., stack write
  during `addi sp, sp, -N` + register-save sequence) → M-mode → OpenSBI
  reset → QEMU exits.
- OR a trap that doesn't get delegated to S-mode and goes straight to
  M-mode (less likely — virt machine delegates exceptions standardly).

Either way: QEMU exits ~100ms after kernel boot. Fits the data.

### Spawn amplification didn't work

Hypothesis: if the race fires per-spawn, calling `spawn_on(1, ...)` in
a tight 20× loop would compound to a near-100% per-run failure rate,
letting downstream narrowing tests use x10 instead of x100.

Reality: 20× spawn at thread=multi, no instrumentation → **0/10 runs failed**.
Amplification did not compound the rate.

Likely explanation: the race is in the **first** spawn/pickup only.
After the first task lands on hart 1, hart 1's IPI handler has run once,
its scheduler state is warm, runqueue is settled — subsequent spawns hit
a primed, less vulnerable hart 1. Multiplying spawns doesn't multiply the
race window because the window is in the *first* handoff.

Implication for downstream tests: cannot speed up by amplification. Must
use x100+ iterations per narrowing test to distinguish "rate ≈ 0" from
"rate ≈ 1-3%". At 1-2% true rate, P(0 in 30) ≈ 50-74% (too high a chance
of false-negative); P(0 in 100) ≈ 4-37%; P(0 in 200) ≈ 0.2-13%. x100
is the minimum credible test length once we're in the rate-suppressed
regime.

### UART tag instrumentation — race fully suppressed

Added raw UART writes (via `console::emergency_uart_base()`, no lock) at
9 choke points in the spawn/pickup path:

- Hart 0 in `spawn_on`: enter, after-push, after-emit_thread_register, after-IPI (4 tags)
- Hart 1 in `secondary_main` idle loop: yield-enter, yield-return (2 tags)
- Hart 1 in `noop_entry`: hit (1 tag)
- Hart 1 in `trap_handler`: enter, return (2 tags)

Result: **200/200 clean** at thread=multi. Race fully suppressed.

### Fence sweep — partial only

Replaced the UART `tag(_s)` helper body with `fence(SeqCst)`. Same 9 sites,
but instead of MMIO writes, just CPU pipeline fences.

Result: **2/200 (~1%)** — partial suppression. Down from ~5% baseline,
but not eliminated.

**Conclusion**: the bug has both an ordering and a timing/cross-thread-sync
component. Fences reduce rate (ordering covers part of the window). Only
MMIO + QEMU BQL acquire (which serializes across vCPU host threads) closes
it completely.

### Tag location bisection

Started removing UART tag regions to find which one carries the
suppression. Cleanest reading is comparing rates against the two
endpoints:

- All 9 tags as UART: **0/200**
- All 9 tags as fence: **2/200 (~1%)**
- Baseline (no tags): **3/100 (~3-5%)**

| State | Tags kept (UART) | Result |
|---|---|---|
| All H0 + H1 | 9 sites | 0/200 |
| H0 tags removed, H1 kept | 5 sites (loop ×2, noop, trap ×2) | 0/130 (30+100) |
| H1 loop + noop removed, trap ×2 kept | 2 sites (trap enter, trap return) | 0/100 |
| Only `trap enter` | 1 site | 1/100 (~1%) — **partial suppression** |
| Only `trap return` | 1 site | _in progress_ |

So far: `trap return` is the candidate for full suppression. `trap enter`
gives the same ~1% rate as fence-everywhere, suggesting `trap enter` is
roughly fence-equivalent (CPU-side ordering only). `trap return` may be
the lone critical MMIO trap.

### Why might `trap return` be critical?

If true, this points at: **a write performed after `trap_handler` returns
but before sret on hart 1 is not visible to hart 0 in time**, OR **a load
performed on hart 1 right after sret needs cross-vCPU sync** that only the
MMIO BQL acquire provides.

The UART write at `trap return` happens INSIDE `trap_handler` (just before
its Rust frame is popped), so before the actual `sret` instruction. It's
the last thing the trap handler does before returning. That UART write
forces hart 1's vCPU to take BQL just before the trap returns — serializing
against hart 0's concurrent activity.

Without that BQL acquire, hart 1 returns from the trap and resumes
execution. If the resumed code reads state hart 0 was setting up, hart 1
might see a stale value.

In the noop-entry-wfi-forever variant, the resumed code on hart 1 after
the IPI handler returns is hart_1_main's idle loop in `secondary_main`:
the `yield_now()` call (or the `wfi` resume after a timer trap, etc.).
yield_now then reads the SCHEDULER mutex / runqueue state set up by hart 0.

## Separate finding: timer-IRQ statics are global, should be per-CPU

While reading hart 1's pickup path, noticed `kernel/src/trap.rs` keeps
three timer-related atomics as global statics, not `PerCpu<T>`:

- `TIMER_INTERVAL_TICKS` — interval read by both harts' IRQ handlers.
  Both harts call `init_timer(interval_ticks)`. Same value end up, so
  benign, but architecturally wrong.
- `TICK_PENDING` — flipped by either hart's timer IRQ, polled by hart 0's
  heartbeat loop. Hart 1's timer firing causes hart 0 to do heartbeat
  work on hart 1's schedule. Telemetry-perturbing, not crash-causing.
- `LAST_IRQ_DURATION` — both harts' timer IRQs overwrite it; hart 0's
  heartbeat reads it for the duration histogram. Hart 1's overwrites
  corrupt hart 0's reading. Telemetry corruption, not crash.

The docstring at trap.rs:19-30 explicitly notes these are correct under
the assumption that "trap return synchronises this hart's handler with
this hart's main thread" — true single-hart, no longer true under SMP
in the sense that each hart now has its own thread to coordinate with.

Fix: lift each to `PerCpu<AtomicX>`. Not Bug B (these are correctness
issues for telemetry only), but should land alongside the Bug B fix.
Keep this issue paired with the deflake plan so it doesn't get lost.

## Harness improvement landed during this session

`cargo xtask itest` previously called `qemu::build_kernel` from inside
`Harness::spawn_with_features`, which ran on every scenario invocation —
i.e. once per --repeat iteration. This caused mid-run source edits to
race with the in-flight test binary: if I edited a kernel source file
between iterations, the next iteration's `cargo build` would rebuild
and swap the kernel ELF under the running QEMU, contaminating the rest
of the --repeat sweep.

Fix: build kernel once up-front in `itest::run()`, and skip the
in-Harness build when features is empty. Feature-specific scenarios
(currently just `oom-leak`) still rebuild per call.

Corridor narrowed to 4 commits, all surface-level:

```
efcbbf9 More lint fixes.        <- BAD
800cca5 Clippy fixes.
4034d25 expect a dead code snippet
c229605 Clippy fixes
387f793 per-hart runqueue + idle <- GOOD
```

**Striking finding**: Bug B was introduced by what should be cosmetic
commits. Most likely a clippy autofix with semantic effect (post-14
already documents `deref_addrof` autofix breaking the kernel's required
`&mut *(&raw mut STATIC)` idiom — that exact hazard, possibly recurring).

Next midpoint: `4034d25` ("expect a dead code snippet").

## Two distinct failure modes

Step 1 surfaced that the corridor likely contains **two interleaved bugs**:

- **Bug A — boot-time percpu panic** at `062e745`. `percpu.rs:71` asserts hartid
  in range; OpenSBI hart-roulette (described in post 14) hands boot to mhartid=1
  with a `MAX_HARTS=?` bounds check that rejects it. Post 14 calls out the
  `LOGICAL_TO_MHARTID` translation fix landing in step 9 (`35b171d` / `ce206f1`)
  — that's almost certainly where this gets fixed.
- **Bug B — post-boot heartbeat wedge** at HEAD. Kernel boots fine, prints
  "I am alive — entering heartbeat", then second heartbeat never arrives.

The pinned scenario (`heartbeat-cadence`) treats both as failures. The
bisection will localize the **earlier-introduced** bug first. Plan: fix that,
rebuild HEAD, re-run; if HEAD still flakes, bisect again for Bug B.

## Bisection mechanic going forward

`062e745` BAD → new corridor `2e409f2..062e745` (7 commits):

```
de8d799 ordering documentation
062e745 steps 4 and 5            <- BAD endpoint
8ad9f3a update protocol for multi hart
8987556 Add new metrics to dashboard
cc7d764 cooperative histogram workload
3085e5d histogram logic
cb1ab9f lcg workload              <- GOOD-adjacent
```

Next midpoint: `8987556`. log2(7) ≈ 3 more steps.

## Tradeoff to watch

`heartbeat-cadence` is a boot/heartbeat-path scenario. If the flake is
specifically a multi-hart race (IPI / shootdown handshake), the pinned
scenario might be insensitive to it. If the corridor closes on a commit
whose changes look unrelated to heartbeat/virtio, switch the pinned scenario
to `smp-spawn-on-hart-1-runs` and re-bisect over the narrower (post-SMP)
sub-corridor.
