# Post 16 — Falsifying my own fix

> The post-15 fence was supposed to be load-bearing. A day of targeted storm scenarios said otherwise: at `--repeat 50`, fix-on and fix-off rates are statistically indistinguishable. Removed the fence. Suite-wide rate unchanged. Somewhere between post 15 and now we accidentally fixed the actual bug; the fence had been carrying free weight ever since.

## what this post was supposed to be

Post 15 ended saying post 16 would be the SMP-payoff: workload consumer migrated to hart 1, `Mutex<VecDeque>` chokepoint lit up under real cross-hart contention, dashboard graphs telling the story.

That arc is still on. But before I could trust the suite as a yardstick for the chokepoint comparison, I wanted to actually find the residual race post 15 was masking — the one the `crate::tag("trap return")` MMIO write was keeping suppressed at ~8% per-scenario rather than fixing.

I did not find it. I found that it isn't there anymore.

## the plan

Five targeted storm scenarios. Each one isolates a kernel surface that _one specific hypothesis_ says the bug lives on. Run with the fix off at `--repeat 50`. The first storm that flakes hard names the surface.

In order of design cost:

| Storm                     | Surface                                                                     | Hypothesis                                                                                     |
| ------------------------- | --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `deflake-ipi-pong`        | hart 0 → hart 1 `IPI_WAKEUP` in a tight loop; nothing else                  | the residual is on the post-`sret` resume window — the deflake-doc's lead suspect              |
| `deflake-shootdown-storm` | hart 0 → hart 1 `mmu::shootdown(va)` in a tight loop                        | the residual is on the IPI payload-read path (`shootdown_va` Acquire-after-`ipi_pending`-swap) |
| `deflake-spawn-storm`     | hart 0 calls `spawn_on(1, body)` 200×, each task `exit_now`s                | the residual is on fresh-`switch` post-fresh-`wfi` (stale `ra`/`sp` read in the asm switch)    |
| `deflake-mutex-storm`     | both harts hammer a shared `kernel::sync::Mutex<()>`                        | the residual is in `spin::Mutex` Acquire/Release under multi-thread TCG                        |
| `deflake-virtio-storm`    | hart 0 emits virtio frames in a tight loop; hart 1 does Relaxed `fetch_add` | the residual is in the virtio TX path (`TX_STAGING` + descriptor ring + MMIO notify)           |

Five hypotheses, five storms.

## the side quest: task-exit

`deflake-spawn-storm` needed work before it could actually test what I wanted to test. The v0.5 cooperative scheduler has no task termination — bodies are `fn() -> !`. So the original storm body had to be `loop { yield_now() }` after acking, which kept hart 1's runqueue populated forever. After the first spawn, hart 1 was busy round-robining, not in `wfi`. Each subsequent IPI just set the pending bit; hart 1 trapped mid-cooperative-yield, not post-`sret`-from-`wfi`. Effective trials per boot: ≈ 1.

So v0.5.x minimal task-exit:

- New asm `switch_into(*to)` in `sched.S` — load-only sibling of `switch`. Loads callee-saved + `sp` + `ra` from `*to` and `ret`s. The exiting task's registers aren't saved; it's never coming back.
- `sched::exit_now() -> !` — locks scheduler, flips `TaskState::Exited`, pops next from the runqueue, drops lock, calls `switch_into`. Per-task metric snapshots filter out `Exited`. The exited `Box<Task>` and `Box<Stack>` leak (no reaping yet — bounded leak per boot is fine).
- Storm body becomes one bump + `exit_now`. Hart 1 returns to `wfi` between iterations.

Half a day of work; ~150 lines including the asm, the state-machine plumbing, and a `sched-task-exits-cleanly` integration scenario that proves a task can die without taking the kernel down. The minimal-scope plan in `plans/v0.5.x-task-exit-minimal.md` is the doc.

The point of building this _now_: post 15's working theory said the residual was on the asm `switch` reading a fresh-ctx that hart 0 just published. The only way to test that at scale is hart 1 reaching the switch from a fresh-`wfi` 200 times per boot. Without task-exit, the storm would have been ~1 effective trial per boot regardless of how I structured the body.

## the results

Fix off, `--repeat 50` for each:

| Storm                                  | Trials/boot            | Per-boot rate |
| -------------------------------------- | ---------------------- | ------------- |
| `deflake-ipi-pong`                     | 10,000 IPIs            | 0/100         |
| `deflake-shootdown-storm`              | 5,000 shootdowns       | 0/50          |
| `deflake-spawn-storm` (post task-exit) | 200 fresh-switches     | 1/100         |
| `deflake-mutex-storm`                  | ~200,000 acquires/hart | 2/70          |
| `deflake-virtio-storm`                 | 5,000 frame emits      | 0/50          |

None of these is even close to the suite-wide ~5% the post-15 doc treats as "the residual." The mutex-storm — by far the highest raw cross-hart mutex contention rate in the suite, with 1M+ atomic operations per run on a single shared CAS — sits at the bottom of the suite's mutex-pressure cluster.

By the third or fourth clean storm, the correct move was to stop building storms and ask whether the suite itself even still flakes at the rate I assumed. It took my collaborator pushing back to actually do it.

## the punchline

Re-ran `kernel-heap-metrics` — a default flaky scenario — fix-off at `--repeat 50`:

```
=== baseline comparison ===
kernel-heap-metrics:
  current  1/50  (2.0%, 95% CI [0.4%, 10.5%])
  baseline 2/40  (5.0%, 95% CI [1.4%, 16.5%]) at 0a56a95
  verdict  consistent (p=0.43)
```

Same for the other three default flaky scenarios:

| Scenario                        | Fix off (50) | Fix on (40) | p    |
| ------------------------------- | ------------ | ----------- | ---- |
| `kernel-heap-metrics`           | 2.0%         | 5.0%        | 0.43 |
| `sched-spans-carry-task-id`     | 0%           | 5.0%        | 0.11 |
| `sched-span-survives-yield`     | 10.0%        | 2.5%        | 0.16 |
| `workload-cooperative-baseline` | 2.0%         | 2.5%        | 0.87 |

All consistent. Aggregate fix-off 7/200 = 3.5% vs fix-on 6/160 = 3.75%. The MMIO fence at trap-return — the headline of post 15 — had **stopped doing anything detectable**.

Somewhere between post 15 shipping and this measurement, a change landed that fixed the actual Bug B. Candidate suspects, none of them on my "deflake" radar at the time:

- v0.5.x **task-exit** (this session) — `switch_into` changes the asm-switch surface
- the **`TICK_PENDING` / `LAST_IRQ_DURATION` per-CPU lift** (early this session) — those were globals shared across harts; under multi-thread TCG that's exactly the shape of bug the post-15 fence was masking
- the **`main.rs` split** + scheduler refactors — codegen layout shifts, the same class of perturbation that originally _unmasked_ Bug B at the bisection corridor

Most likely: the per-CPU lift. It was nominally architectural cleanup; it was probably the actual fix.

Removed the fence. Re-verified the four flaky default scenarios at fix-removed `--repeat 50` — all still consistent with the fix-on baseline. `crate::panic::tag` and its only caller are gone. `trap_handler` is one MMIO write lighter on every trap exit.

## what each storm taught (negative results count)

A clean result at high N is still a measurement. Each storm rules out a surface:

- **`deflake-ipi-pong`** — 100,000 trials, zero flake. The bare hart-1 resume-from-`sret` path is _not_ the surface. Post 15's working theory was wrong about location.
- **`deflake-shootdown-storm`** — 50,000 trials, zero flake. The IPI payload-publication chain works as designed under multi-thread TCG. The Acquire-after-`fetch_or`-Release pattern in `ipi.rs` is fine.
- **`deflake-spawn-storm`** (post task-exit) — 4,000 fresh `switch`-into-fresh-ctx trials at fix-off, per-boot rate ≲ 5.4% (upper CI). If H1 were the dominant bug, the storm should be the _flakiest_ scenario by orders of magnitude. It's tied for _lowest_ non-zero rate.
- **`deflake-mutex-storm`** — highest raw mutex contention in the suite, lowest rate in the mutex-pressure cluster. Whatever shape the bug was, "the `spin::Mutex` CAS Acquire/Release pair" doesn't fit.
- **`deflake-virtio-storm`** — 250,000 virtio frame emissions concurrent with cross-hart atomic activity. Zero flake. The TX path is not a stale-read surface either.

Five hypotheses ruled out. The strongest-fitting interpretation is that there is no longer a coherent residual bug — what's left is uncorrelated jitter from cooperative-v0.5 scheduling under multi-thread TCG.

## what I caught (the discipline-layer beats again)

Same shape as post 15: agent driving mechanics, me driving methodology. Two interventions worth flagging.

### "`--repeat 20` × 2 isn't a falsification"

The agent tried to declare H1/H2 falsified after `--repeat 20` × 2 came back 20/20 each. I pointed out the 95% CI on 0/20 is [0%, 16.8%]. The suite-wide rate is 5%. "I didn't see a fail in 20 tries" doesn't rule out 5%.

The fix wasn't a one-off correction. The agent now reaches for `--repeat 50` by default when the rate could be < 5%, and `--repeat 100` when the comparison is load-bearing. That's a memory now.

### "stop building the next storm and look at it"

Three storms in a row came back clean. The agent's instinct was "design the fourth storm." Mine was "look at the suite — does the bug it's masking even still exist?"

This is the discipline-layer move that post 15 also flagged in a different form: **when several targeted experiments all come back negative, question the premise, not the design.** Three minutes of `kernel-heap-metrics --repeat 50` saved a day of building more storms.

The agent absorbed it on the spot — re-ran the comparison, saw p=0.43, proposed removing the fence. From "design the next storm" to "the fence is dead" inside a 10-minute exchange. That's the collaboration working.

## what shipped

- `crate::panic::tag` and its single caller in `trap_handler`: **removed.** Suite-wide rate unchanged.
- v0.5.x minimal task-exit: `switch_into.S`, `sched::exit_now`, `TaskState::Exited` plumbing through `task_count` and `task_snapshots`. Documented in `plans/v0.5.x-task-exit-minimal.md`.
- Five permanent storm scenarios — each is a useful per-subsystem regression test, and each falsified a hypothesis that would have wasted future debugging time.
- A small refactor: `heartbeat.rs` lost 90 lines to two macros, `define_metrics!` and `emit!`. The struct + register impl that used to be two parallel lists is now one declarative block.
- `plans/residual-race-investigation.md` with three appendices: the hypothesis tree, the storm results, and the "fence was dormant" conclusion. The whole investigation, in writing, for future me.

## what's _actually_ next

Same arc post 15 promised:

- step 11 — `spawn_on(1, "workload_consumer", ...)`. Producer stays on hart 0, consumer migrates to hart 1, `Mutex<VecDeque>` is now under genuine cross-hart contention. Watch lock-wait rate climb; watch queue depth shape change.
- step 12 — swap `Mutex<VecDeque>` for `heapless::spsc::Queue`. Watch lock-wait fall off a cliff.

The suite is finally a usable yardstick: ~3–5% per-scenario uncorrelated jitter, no longer a coherent bug masking the signal. Whatever cost the cross-hart mutex shows up at, we'll see it on the wire.

A footnote for anyone who chased a fence as load-bearing for several sessions and quietly carried it as part of the kernel's identity: it is _much harder_ to falsify your own fix than to falsify someone else's hypothesis. The fence had a story attached. The storms had no story; they were just measurements. The measurements won.
