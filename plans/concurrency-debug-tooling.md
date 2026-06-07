# Concurrency-debug tooling

What the v0.6 deflake session would have benefited from, expressed as concrete tool changes — plus a draft skill for the next round.

## tool improvements

In order of expected hours-saved per implementation hour. The harness already does the build-once thing; everything below is additive.

### inline per-scenario flake rates

`cargo xtask itest --repeat N` currently prints a final aggregate. Improve so the per-run line *and* the final summary include rate-so-far:

```
=== run 17/50 ===
test heartbeat-cadence ... ok (max wait 1.3s of 45s budget)  [1/17 ≈ 5.9% so far]
```

And the final table sorts by rate descending. Lets you eyeball "this scenario is the noisy one" without grepping.

**Cost**: ~50 LOC in `itest.rs`. **Saves**: every "which scenario failed how often" grep across the session.

### statistical confidence framing on aggregate

When the aggregate prints, also print the Bayesian interpretation:

```
heartbeat-cadence: 0/100 runs failed
  P(0 in 100 | true rate 1%) = 37%   ← cleanly inconclusive
  P(0 in 100 | true rate 3%) = 5%    ← inconsistent with 3%
  rule-of-thumb upper bound on rate (95% CI): 3.0%
```

If we'd had this we'd have never declared the 30-run results clean. The harness has all the numbers; it just needs to do the arithmetic.

**Cost**: ~30 LOC. **Saves**: every "is 30 enough?" round-trip with the agent.

### versioned per-scenario baseline file

Builds on the confidence framing above. Instead of computing "P(0 | rate=r)" for hand-picked r, store a real baseline rate per (scenario, commit) in a versioned file and compare against it.

**Shape**: `.itest-baseline.json` in repo root, per scenario:

```json
{
  "scenarios": {
    "heartbeat-cadence": {
      "current": {
        "commit": "d40e7cf",
        "runs": 200,
        "failures": 12,
        "rate": 0.06,
        "ci_95": [0.034, 0.103],
        "recorded_at": "2026-06-08"
      },
      "history": [
        {"commit": "efcbbf9", "runs": 100, "failures": 8, "recorded_at": "2026-06-07"},
        {"commit": "d40e7cf", "runs": 200, "failures": 12, "recorded_at": "2026-06-08"}
      ]
    }
  }
}
```

`current` is what comparisons run against; `history` is the append-only log. `current` only changes when you explicitly run `--update-baseline`. No auto-promotion — that would hide regressions.

**CLI surface**:

- `cargo xtask itest --repeat N` — runs as normal AND prints comparison vs `current` baseline:
  ```
  heartbeat-cadence:   3/50  (6.0%, 95% CI [1.7%, 16.2%])
    vs baseline:       12/200 (6.0%, 95% CI [3.4%, 10.3%])
    verdict:           consistent (p=0.99, Fisher's exact)
  ```
- `cargo xtask itest --update-baseline --repeat N` — runs, appends to `history`, sets `current` to the new entry.
- `cargo xtask itest --baseline-show` — just prints the baselines.
- `cargo xtask itest --baseline-set-from <commit>` — finds the most recent history entry for that commit, sets it as `current`.

**Indexing**:

- Index by `(commit, features)` — `heap-oom` feature kernel may have a different baseline. Same for `deflake-spawn-storm`.
- Store both `commit` and `build_hash` (sha of the kernel ELF). `commit` is the comparison key; `build_hash` is a sanity check that warns on mismatch (caught codegen perturbation from uncommitted edits).
- If `current.commit` is N commits behind HEAD, print a warning. Don't auto-update.

**Why this is better than freestanding `P(0 | rate=r)` framing**: the question becomes "did the rate change?" — which has a well-defined answer (Fisher's exact / two-proportion z) once you have two samples. Far less hand-waving than picking `r` by hand. The "8% vs 60% baseline" confusion of this session becomes impossible: the actual measured baseline lives in the repo.

**Counterfactual baseline as a knock-on**: `current.thread_multi` and `current.thread_single` stored side-by-side gives you "does the bug need parallelism" as a single comparison instead of a one-off experiment.

**Cost**: ~200 LOC including the JSON I/O, Wilson-score CI, Fisher's exact. **Saves**: the entire "what was the baseline rate again?" recurring confusion, plus a permanent regression-detection capability.

### --fail-fast=K

`cargo xtask itest --repeat 100 --fail-fast=3` stops after 3 failures and prints what it has. When you're trying to *confirm* flakiness (cheap by construction), this saves you the long tail of the run.

**Cost**: trivial. **Saves**: ~5 min per failed-confirmation test, of which we ran ~10.

### counterfactual matrix runner

`cargo xtask itest --counterfactual <scenario>` runs the scenario under each of:

- baseline (`-smp 2 -accel tcg,thread=multi`)
- `-smp 2 -accel tcg,thread=single`
- `-smp 1 -accel tcg,thread=multi` (skips if kernel doesn't boot)

…and prints a 3-row rate table at the end:

```
              baseline       thread=single    smp=1
heartbeat-cadence  6.0% (3/50)   0.0% (0/50)     panic at boot
```

The thread=single counterfactual is the single biggest "rule out missing-locks" tool we used. Making it one command means it gets used *early* in the next investigation instead of late.

**Cost**: ~100 LOC. **Saves**: hours of "should we try this counterfactual?" friction.

### tag-counter + dump-on-fail

The session's `mark(c: u8)` writes a single byte to UART for each boot phase. Extend this:

- Replace single-byte UART writes with a `[AtomicU64; N_MARKS]` table indexed by call site, where each `mark!()` does `MARKS[i].fetch_add(1, Relaxed)`.
- `mark!()` is a macro that captures `file!()` + `line!()` and pre-registers a slot at build time (`MARK_SITES: &[&str]`).
- On scenario failure, print the table: which marks were hit how many times.

So instead of "the kernel reached H but not I," you see exactly how far the kernel got *across multiple runs* and which paths were hot.

**Cost**: ~150 LOC for the macro + table + dump. **Saves**: a tag-region bisection in any future investigation. The whole UART-tag bisection we did would have been a single `mark!()` audit.

### gdb-on-fail

`cargo xtask itest <scenario> --gdb-on-fail` runs the scenario; on failure, re-runs with `-s -S` and prints the gdb attach command (`gdb -ex 'target remote :1234' kernel.elf`). The harness stays paused so I can attach and inspect.

This is the one thing that would have *directly* told us where hart 1 was when QEMU exited. We hypothesized "double-fault to M-mode" without confirming. GDB would have shown us the PC, the sp, the saved trap state.

**Cost**: medium — needs harness state-machine work. **Saves**: the entire "we think it's a silent reset but can't prove it" dead end.

### source-tree lock during --repeat

Block `cargo build` from picking up source changes mid-run. Two options:

1. **Lockfile-based**: at start of `itest::run`, touch `target/.itest-running`; on file-watcher / cargo invocation, check that file and error.
2. **Tree-snapshot-based**: at start, copy `kernel/src` to `target/kernel-src-snapshot/` and build from there.

(1) is simpler; (2) is more bulletproof. Either kills the contamination class permanently — better than the current "build once" mitigation, which I bypassed in this session by editing source mid-run.

**Cost**: ~50 LOC. **Saves**: the next contamination incident, which will happen.

### repro recorder

`cargo xtask itest <scenario> --record` captures everything that could vary run-to-run: QEMU rng seed, kernel build hash, timing of socket-accept, cargo target dir state. `--replay <token>` re-runs deterministically (modulo TCG nondeterminism).

This isn't possible for the multi-thread-TCG race itself (it's inherently nondeterministic) but is gold for ruling out "did this commit change a thing that shifted timing?" questions.

**Cost**: medium-high. **Saves**: probably less than the others; defer until needed.

### in-kernel race-rate harness

The `deflake-spawn-storm` feature you've already added is a great pattern. Generalize it:

- A test mode where the kernel runs the suspect operation N times in one boot and reports the per-trial flake rate as a single metric.
- The harness reads that metric and reports a rate-with-confidence.
- One kernel boot ≈ N trials, so suite x5 is 5N samples instead of 5.

For a 1% race, this lets you confirm a fix candidate in one boot instead of 200. The asymmetry between "confirming flakiness" and "confirming cleanness" collapses — both fast.

**Cost**: scenario-specific. **Saves**: enormous for residual-race hunting.

## skill draft: concurrency-debug

Distilled from the session. Goal: when an agent picks this up next time, it should apply these reflexively, not after being prompted.

### before bisecting, run the cheap counterfactual ladder

Each cheap counterfactual eliminates a class of suspect. Run them first; bisect only when they don't help.

| Counterfactual | If clean, rules out |
|---|---|
| `thread=single` TCG | missing-locks, IRQ-vs-main races, reentrant sections — the bug requires *parallel* execution |
| `-smp 1` | cross-hart code paths |
| Disable workload tasks | producer/consumer races |
| Disable secondary hart's spawn | hart-1-active code |
| All atomics → SeqCst | Rust atomic ordering |
| One static mut at a time → Mutex | specific static-mut hazards |

If any of these makes the bug vanish, you've narrowed the suspect class by an order of magnitude in 10-30 minutes. **Do this before commit bisection**, not after.

### recognize codegen edges

When commit bisection lands on an *implausibly benign* commit (clippy fixes, `Default` impls, comment-only edits, whitespace), the bisection has found a **codegen edge**, not a logic edge. The commit didn't introduce the bug; it perturbed binary layout enough to widen the race window. The bug lives upstream.

- Per-file revert sweep at the introducing commit confirms (each revert leaves rate unchanged).
- Switch from bisection to subsetting; the commit is no longer informative.

### statistical discipline

For low-rate flakes (<3%):

- 30 runs of "no failures" is essentially noise. `P(0 in 30 | rate=2%) ≈ 55%`.
- 100 runs is the floor for "this looks clean."
- 200 runs is what you need to ship a fix on.
- For "confirming flakiness," however, one failure within `~2/r` runs is enough. **Order narrowing tests so the predicted-flaky variant runs first.**

If the harness prints confidence intervals (see tooling above), the agent doesn't need to derive this each time.

### subsetting via feature flags

Instead of editing the production kernel and reverting, add `#[cfg(feature = "deflake-X")]` regions for hypotheses you want to test. Each cfg corresponds to one hypothesis. Lets you:

- Run the hypothesis in isolation (`--features deflake-X`)
- Combine hypotheses (`--features "deflake-X deflake-Y"`)
- Ship a clean kernel without the experimental code

The `deflake-spawn-storm` feature is the canonical example: a scenario built specifically to drive the suspect operation at high rate, *without* polluting the production boot path.

### never edit source while --repeat is running

Cargo rebuilds per-iteration unless told otherwise. Mid-run edits contaminate the rest of the run. Mitigations:

- Build-once harness (already done).
- Source-tree lock (proposed above).
- As a discipline: if you want to "get ahead," write the next experiment to a scratch file, not to the actual source.

### re-measure key reference numbers

Don't trust "baseline rate was X%" from a doc. Re-derive at the current commit. The conclusion of an investigation can hinge on whether the baseline was 8% or 60% — that's a 7× difference, and the wrong one will produce confidently-wrong claims.

### MMIO traps as cross-thread sync diagnostic

If a `fence(SeqCst)` doesn't suppress a race but an MMIO write at the same spot does, the race needs **cross-host-thread serialization** that local fences don't provide. Under QEMU multi-thread TCG, MMIO traps acquire the BQL — that's the cross-vCPU sync. This points at:

- A QEMU emulation gap (the kernel may be correct on real hardware)
- A `static mut` accessed via raw pointers (atomics + fences don't cover non-atomic accesses)
- Memory model assumptions the kernel makes that QEMU TCG doesn't honor

A pragmatic fix is then an MMIO read at the critical point (no-op register, no side effects). A real fix needs identifying *which* specific load on the receiver hart sees stale data.

### document as you run

Write what you tested, what the result was, and what you concluded. This is what lets you catch your own errors mid-investigation — corridor direction, repeated suspects, conflicting conclusions. Without persistent state, the next experiment overwrites the memory of the last.

For SnitchOS specifically: `plans/<investigation>.md` grows incrementally with each result. Every numerical result gets a row; every hypothesis change gets a paragraph; every dead end gets a "what we ruled out" line.

### the fix-vs-mask distinction

A masking workaround that reduces rate by 15× is a valid ship if:

- The alternative is days of investigation with uncertain outcome
- The mask doesn't change the production code shape (a one-byte UART write is fine; a `thread=single` rollback is not)
- The deferred work is captured in a plan doc with concrete next steps

**Document the mask as a mask**, not as a fix. Otherwise the next person to see the code won't know the underlying race is still there.

### symptom shapes for SnitchOS-specific failures

- Silent QEMU exit ≈ OpenSBI reset, usually triggered by an M-mode trap on the kernel side. Suspect: double-fault during trap entry (bad sp, bad stvec).
- "Kernel reached `I am alive` then nothing" is *not* a wedge at `I am alive` — it's the last UART output, and the kernel runs further before dying. Check the wire frames (virtio-console captured) to see actual progress.
- `QEMU disconnected` (vs `timeout`) in the harness logs means QEMU exited, not that the kernel hung. Different bug class.

## priority for the next session

If I were sequencing the tool improvements:

1. **Inline per-scenario rates + statistical confidence framing** (the cheapest changes; biggest decision-quality win).
2. **`--fail-fast=K`** (trivial, immediately useful).
3. **Counterfactual matrix runner** (changes the *opening move* of every future investigation).
4. **Tag-counter + dump-on-fail** (kills the UART-tag bisection style entirely).
5. **GDB-on-fail** (only if a residual investigation actually needs PC-level inspection).
6. **Source-tree lock** (paranoia; ship when convenient).
7. **Repro recorder** (defer until you have a specific reason to need it).

Skill draft can land alongside (1)-(3); the rest are nice-to-have.
