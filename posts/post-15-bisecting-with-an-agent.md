# Post 15 — Bisecting a multi-hart race with an agent

> A day chasing a flaky kernel from a 60%-per-run baseline down to 8%, with the agent driving the mechanics and me catching the methodology. The fix is one byte of UART output; the real story is about who was thinking at each step.

## the starting point

Post 14 ended with a wart it didn't fix: ~1-2% per-scenario flake under `-smp 2 -accel tcg,thread=multi`. The kernel always reaches `I am alive — entering heartbeat`, then sometimes QEMU just exits a hundred milliseconds later. No panic, no kernel UART past that line. Silence and a closed socket.

Goal for the day: figure out what's going on. Bring the suite under control.

Where I ended up:

| state | per-scenario rate | per-run rate |
|---|---|---|
| baseline (just-after-post-14) | 6.7% | 60% (3/5 runs failed) |
| with the fix | 0.44% | 8% (4/50 runs failed) |

A ~15× suppression for what turned out to be one byte of UART output in `trap_handler`. The race isn't *fixed* — it's *masked* — but masking 90% of it is enough to make the suite a useful CI gate again with `--repeat 3`.

The interesting question is how I got there.

## two bugs, one bisection

The starting hypothesis — that the flake was introduced in v0.6 SMP work — was half right. There were two distinct failures hiding behind the same `heartbeat-cadence` scenario:

- **Bug A** — a boot-time panic at `kernel/src/percpu.rs: hartid out of range`. Introduced in `062e745` ("steps 4 and 5"), fixed in `35b171d` (the `LOGICAL_TO_MHARTID` translation work post 14 calls out). Already resolved on `main`.
- **Bug B** — a post-boot silent QEMU exit. Still open. Bisected to `c229605` "Clippy fixes" — but the clippy fixes turned out to be *codegen layout perturbation*. Per-file revert sweep proved no single hunk was responsible. The race lives somewhere in `387f793` (per-hart runqueue) or earlier; `c229605`'s aggregate diff just shifted binary layout enough to widen the race window.

That distinction — *introducing* a bug vs. *unmasking* a pre-existing race via codegen — is a class of bisection result worth recognising. When you land on a commit whose diff looks implausibly benign for the symptom (clippy fixes, `Default` impls, whitespace), the bisection has probably found a codegen edge, not a logic edge. The fix isn't in that commit's changes.

## the interventions that mattered

The agent drove the mechanics throughout: corridor identification, midpoint selection, building, running, parsing aggregate failure counts, sketching next experiments. Frictionless. What it kept getting wrong — and what I kept catching — was the strategic and statistical layer.

### "i dont thnk 30 is enough if rate is so low"

This came up again and again. The agent kept reaching for `--repeat 30` because that's what the early bisection used. But that signal was always *against an obviously-flaky baseline*. Once I was inside the suppressed regime — partial fixes meaning rate was 1-2% instead of 5-9% — `30` was nowhere near enough.

At true rate 2%, `P(0 in 30) ≈ 55%`. Cleanly "I didn't see a failure" tells you almost nothing when rate is low. The agent kept settling for "30/30, looks clean!" and I kept pushing back. By the end I'd internalised the rule: **once you cross into <3% rate territory, `--repeat 100` is the floor and `--repeat 200` is what you actually need for confidence.**

This is now memory. The pattern that triggered the correction was the agent's failure mode of "30 clean → done." The pattern it kept missing: factor in *what you're trying to distinguish from*.

### "it could very well be the thing we eagerly enabled"

The moment that taught me the most.

The agent had just measured L1 (hart 1 boots + idles, no `spawn_on`) at "1/100 flaky." A nice clean negative result that *almost* fit the suspect map. But while L1 was still running, the agent had already wired L2 (re-add `spawn_on`) so it could kick it off the second L1 finished.

I pushed back: *"oof, it could very well be the thing we eagerly enabled."*

`cargo xtask itest --repeat 100` was issuing 100 separate `cargo build` invocations, each checking source mtime and rebuilding if needed. The mid-test edit meant runs ~88-100 of L1 were actually running the L2 binary. The "1/100 flake" was contaminated.

Two things came out of this:

- **The harness improvement.** `itest::run()` now builds the kernel *once* up-front; `Harness::spawn_with_features` only triggers a build for non-default features. The build race can't bite again.
- **A discipline the agent now has memory of.** Never edit source while a `--repeat N` run is in flight.

But the thing worth flagging is what the agent *did* with the correction. It didn't just acknowledge and re-run. It checked the kernel ELF mtime against the source mtime, found the binary hadn't actually rebuilt for some reason I still don't fully understand, then *designed the harness fix* so the contamination class went away permanently. That's the agent at its best: turn a methodology error into a permanent guardrail.

### "is L2c actually a superset of L2b or not?"

Halfway through the L2 sub-bisection, I caught myself confused about the topology. I'd been treating L2a / L2b / L2c as a linear sequence — "L2c clean, so try L2b next, which is closer to L2a." But they aren't a sequence. They're three disjoint subsets of HEAD:

| | spawn_on | IPI | task runs on hart 1 |
|---|---|---|---|
| L1 | ✗ | ✗ | ✗ |
| **L2c** | ✗ | **✓** | ✗ |
| **L2b** | **✓** | ✗ | maybe |
| **L2a** | **✓** | **✓** | ✓ (no-op body) |

L2a is the *superset* of L2b ∪ L2c. The interesting prediction was that the race might emerge only when you *combine* `spawn_on` and IPI — neither alone is enough. Which is exactly what eventually came out.

I'd been about to run L2b assuming it was the obvious next step. The agent had a hedged "probably clean, let me think" answer that pointed at the actual structure. The missing piece was *me* asking the structural question. The agent had the data; I needed to call the framing.

### "is this issue caused by SMP, or by concurrency in general?"

The clarifying question I want to internalise. The agent was sketching suspect lists. I asked: are we sure the bug needs *parallel* execution, or could it be exposed by *interleaved* execution too?

The answer was load-bearing. `thread=single` clean over 100 runs, `thread=multi` 3-9% flaky. The bug requires two host threads in parallel. That immediately rules out a huge class of bugs — missing locks, IRQ-vs-main races, reentrant sections. Whatever it is, it's a cross-vCPU memory-model issue.

That one question converted a fuzzy "something SMP" suspect list into a narrowed "non-atomic shared state under genuine parallel execution" suspect list. The agent had been talking about the right things; I'd been treating the answer as obvious.

### "i really don't think the suite was this robust before"

Late in the day the agent declared the trap-return tag a 15× per-scenario improvement, with "8% per-run" as the baseline (quoting post 14). I pushed back: that didn't match what I *remembered*. The suite used to fail most runs.

I checked. Baseline at `d40e7cf` (just before the deflake work), suite x5: **3/5 runs failed.** 60% per-run, not 8%. Post 14's reported 8% was either after some partial mitigation I hadn't tracked, or just a different snapshot. The agent had been calibrating against the wrong yardstick the whole conversation — so its "15× improvement" math was right, but its absolute baseline framing was wrong.

This is a class of error the agent should be more guarded about: **trusting a remembered number from a doc instead of re-measuring**. Especially when the conclusion changes meaningfully depending on which number is true.

## what the agent carried

The parts that worked end-to-end without intervention:

- **bisection mechanics.** Identifying corridors, picking midpoints, running, reverting, re-running. The agent never lost track of where it was, even across parallel hypotheses.
- **sketching experiments.** L1, L2a, L2c, the fence sweep, the SeqCst sweep, the per-file revert sweep, the codegen-diff via `llvm-nm --print-size`, the UART tag instrumentation, the tag-region bisection. Each came from the agent thinking "what would distinguish hypothesis X from Y?" and proposing a concrete experiment.
- **catching its own corridor-direction error.** At one point I had it test `de8d799` as a midpoint between `8ad9f3a` (GOOD) and `062e745` (BAD). Turns out `de8d799` was *after* `062e745`, not between them — so testing it gave no bisection signal. The agent caught this from the chronological git log without prompting.
- **documentation as it ran.** `plans/deflake-bisection.md` kept growing in real time with each result. Catching the corridor-direction error was *only possible* because the agent had been writing down the corridor structure as it went and could check what it had previously concluded.
- **most of the code reading.** Walking through `spawn_on`, `yield_now`, the asm `switch`, hart 1's `secondary_main` loop, `trap_handler`. Identifying that the `task.context.get()` raw pointer is sound because Box pointers are stable. Spotting that `TICK_PENDING` / `TIMER_INTERVAL_TICKS` / `LAST_IRQ_DURATION` are global statics that should be per-CPU under SMP (deferred follow-up).

## the asymmetry that's worth a memory

One process insight worth highlighting: **flaky variants are 5-10× cheaper to confirm than clean ones.**

To establish "this configuration is clean," you need enough samples that `P(seeing 0 failures | true rate r)` is small. At 1% true rate that's 200+ runs. To establish "this configuration is flaky," one failure within `1/r` runs is usually enough.

I pointed this out explicitly during L2 narrowing. The implication is that you should *prefer* tests where you predict flakiness — they finish fast — and reserve the expensive clean-confirmation runs for the moments you actually need them. I had the agent change L2 narrowing order based on this. Its instinct had been "test the most likely-clean version first," which is precisely backwards if your goal is to *minimise wall-clock time to next decision*.

Now a memory. The agent had the relevant statistics in hand; what it lacked was the framing of "optimise for what tells you direction fastest."

## what we learned about the bug

Beyond the methodology lessons, some concrete things:

- The bug needs hart 1 to *actually run a Rust task* spawned by hart 0. Hart 1 booting and idling alone is fine (L1: 0/200). Hart 1 receiving an IPI alone is fine (L2c: 0/100). The combination of `spawn_on(1, ...)` + IPI delivery + hart 1's first asm-switch into the new task is what triggers it.
- Atomic ordering is *not* the cause class. I swept every `Ordering::Relaxed` / `Release` / `Acquire` / `AcqRel` in `kernel/` and `kernel-core/` (92 sites) to `Ordering::SeqCst`. Rate unchanged. So the bug lives somewhere `core::sync::atomic` can't reach — `static mut` accessed through raw pointers, the asm `switch`, the page tables, or something at the QEMU emulation boundary.
- A `fence(SeqCst)` at the critical spot gives *partial* suppression (~1% rate). A UART MMIO write at the same spot gives *full* suppression. The difference: an MMIO write under multi-thread TCG acquires QEMU's big lock, serialising across vCPU host threads. A fence is local. Whatever the race is, it requires cross-host-thread serialisation that only an MMIO trap provides.
- The critical spot is in `trap_handler`, specifically *at the very end before sret*. Removing the UART write at `trap return` re-opens the race. Removing it anywhere else doesn't.

The working theory: **after the IPI handler returns and hart 1 resumes via `sret`, the first memory access on hart 1's resumed path needs cross-vCPU visibility that the kernel's atomic instructions aren't providing under multi-thread TCG.** Quite possibly a QEMU emulation gap rather than a kernel bug. Real-hardware test deferred.

## working with an agent on something like this

The pattern I keep landing on: **the agent is great at the mechanical layer and the local reasoning layer; my job is the discipline layer.**

- **mechanical layer (agent-strong).** "Run --repeat 30, parse aggregate, propose next experiment, edit code, build, observe result, update plan." If I'd typed all of this myself, wall-clock would have been 3-5× what it was.
- **local reasoning layer (agent-mostly-strong).** "spawn_on does ABC, hart 1's idle loop does XYZ, the asm switch saves/loads these registers, so the suspect set narrows to these candidates." The agent did this fluently. The only thing it kept getting wrong was *interpreting partial signals* — reading 30/30 as "fully clean" when the right question was "could be 0-3% true rate."
- **discipline layer (human-required).** "Are we sure 30 is enough?", "could the thing we just changed contaminate the in-flight test?", "is the framing of L2b vs L2c right?", "what was the actual baseline?". This is where Bayesian thinking and methodological skepticism live, and it's the layer I was glad I was driving.

The good news: each time I caught a discipline-layer error, the agent absorbed it into a memory. Future debugging sessions start with these guardrails in place. The collaboration isn't static — it gets sharper each time I intervene.

## what's shipping

A single `tag()` call at the end of `trap_handler`, dressed up as a permanent single-byte UART write — no formatted writeln, no log noise. All other instrumentation reverted. The harness "build once" change stays — that's a correctness improvement independent of any kernel race. `-smp 2 -accel tcg,thread=multi` stays as the default. CI gets `--repeat 3` and a comfortable 99.95% pass probability.

The plan doc captures deferred work: hunting the residual 8%, designing a less hacky fix shape (no-op MMIO read vs. SBI ecall vs. targeted Acquire), lifting timer-IRQ statics to PerCpu, and the QEMU TCG investigation. Each tractable; none urgent.

## what's next

Post 16 is supposed to be the SMP-payoff post: workload consumer moved to hart 1, `Mutex<VecDeque>` chokepoint shows its cost under genuine cross-hart contention. That arc is still on. I needed the suite to be a usable yardstick first, and now it is.

Then post 17 retires the chokepoint with `heapless::spsc::Queue`. Same workload, three configurations, three observability stories. The deflake work was the unblock for the comparison to be measurable at all.
