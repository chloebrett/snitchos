# Post 18 — The flake had a fingerprint

> Post 16 called the residual cross-hart flake "uncorrelated jitter" and closed the case. It was wrong — a real, single, one-line bug was hiding in it. What caught it wasn't a cleverer bisection; it was a *failure classifier*: bucket every flake by signature, capture the frame transcript at death, let the pattern speak. It spoke instantly — ten wedges, all at hart-1 registration, all with the same two telemetry strings smashed together on the wire. The fingerprint pointed at `virtio_console::send`, where a `MutexGuard` dropped one line too early. 12/500 → 0/500, p=0.0005.

## what this post was supposed to be

- step 11 — the SMP payoff. Same thing posts 16 and 17 promised. Still on.
- but I couldn't trust the suite as a yardstick at ~5% flake with no explanation, and post 16's "it's just jitter" had been bothering me.
- one more pass at the residual. This time it broke open — and the lesson isn't about the kernel, it's about what you can *see*.

## the residual post 16 gave up on

- post 16's read: the cross-hart wedge didn't reproduce in five storm scenarios; fix-on and fix-off rates were statistically indistinguishable; removed the masking fence, rate didn't move. Conclusion: **"no coherent residual bug — uncorrelated jitter."**
- reasonable read of the evidence. The problem was the *evidence*.
- every failure looked identical: `QEMU disconnected`, fast-exit, UART log shows "I am alive" then silence. 25 scenarios, one undifferentiated 5% rate.
- **when every failure is the same opaque shrug, "it's noise" is the only conclusion the data supports.** The fix wasn't to look harder at the kernel — it was to stop throwing away information about the failures.

## the idea: categorize the failures

- (agent's suggestion, credit where due) stop treating "5% flaky" as a number; treat each failure as a *thing with a cause*. Buckets:
  - **wedge** — socket disconnected, kernel died. A genuine kernel residual.
  - **stalled** — timed out, alive, but went quiet well before the deadline.
  - **budget_exhausted** — timed out, alive, frames flowing to the deadline. Slow, not broken.
  - **harness** — infra: spawn failure, external SIGINT.
  - **unknown** — not enough evidence. Honest non-answers beat forced guesses.
- the classifier is a pure function over evidence captured at the moment of failure.
- **the evidence isn't in the log file.** Telemetry leaves over virtio; the UART log carries none of it; the kernel doesn't even UART-print per heartbeat. Every failure's `.log` is the same useless silence.
- the load-bearing signal is whether the *socket* disconnected (kernel died) vs merely *timed out* (alive) — which `wait_for` knew at the instant of failure and was throwing away.
- so: a **capture** alongside the classifier — `fail-<scenario>-<n>.capture.json` per failure, holding the wait outcome, frames-seen, per-hart last-timestamp, a frame-type histogram, and a transcript of the last frames on the wire.
- the irony: the whole project is observability, and the suite was deleting the one observable that mattered exactly when it mattered most.

## the payoff

- ran a flaky scenario 500×:
  ```
  Failure signatures (12 total):
    wedge: 10
    budget_exhausted: 2
  ```
- ten wedges — the thing post 16 said didn't exist. And the genuinely-slow ones cleanly separated out. The "jitter" was a real bug *plus* real noise.
- the captures clustered. 8 of 10 disconnected at the **same point**: frame ~142, during hart-1 task registration. And the payloads were visibly corrupted:
  ```
  ThreadRegister { id=8 name="hart_\u{8}\u{8}\u{b}har" }
  StringRegister { StringId(62) = "snitchos.tas\u{6}>(snitchos.task.hart_1_main" }
  MetricRegister { "?" kind=Counter }
  ```
- **two telemetry strings smashed into one** — `snitchos.tas`, truncated, a control byte, then the *other* string. Not a decode bug; the decoder faithfully decoded garbage the kernel *emitted*.
- same string, same seam, across independent failures. **Determinism = structural race on a fixed buffer, not random corruption.**
- only during hart-1 registration because that's the one window where both harts emit telemetry at once — hart 1 registering its tasks while hart 0 heartbeats.
- old tooling: "5% flaky." New tooling: "≈2% wedge, clustered at hart-1 registration, two strings interleaved in a shared buffer, suspect = the emission path." A bug you can go fix.

## the bug: a guard dropped one line too early

- `send` stages each frame into a single static `TX_STAGING[256]`, meant to be serialized by the `CONSOLE` mutex. The lock:
  ```rust
  let base = *handle.lock();   // BUG
  ```
- `handle.lock()` returns a temporary `MutexGuard`. `*guard` copies the `usize` out — deref-and-copy, **not** a borrow — so nothing holds the guard. **It drops at the semicolon.** The staging copy and `transmit` run *unlocked*.
- the `SAFETY` comment underneath asserted "single writer for the duration of the lock + transmit" — the exact invariant the code had just failed to keep.
- two harts in `send` at once race **two** shared resources:
  - **`TX_STAGING`** — both `copy_nonoverlapping` into the same 256 bytes → interleaved text. The corruption you can read on the wire. Harmless, cosmetic — just happens to be human-readable, which made it a clean fingerprint.
  - **the virtqueue** — `transmit` writes one shared descriptor (slot 0, reused), bumps `avail.idx`, busy-waits on `used.idx`. Two harts overwrite the descriptor mid-DMA (torn addr/len) and race the ring index. QEMU validates descriptors, sees a bogus one, marks the device broken, tears down the virtio-console → host socket EOF. **That's the wedge.**
- one cause, two symptoms: garbled text (visible, harmless) + dead device (invisible, fatal).
- the fix — bind the guard so it lives across stage + transmit:
  ```rust
  let guard = handle.lock();
  let base = *guard;
  // ... stage, transmit ...
  drop(guard);
  ```
- one line of substance. Classic Rust footgun: `*mutex.lock()` reads as "lock, use the value," means "lock, snapshot, *unlock*." Clippy catches the `let _ = mutex.lock()` sibling, not this. Now a CLAUDE.md gotcha.

## the result

- the integration suite is the test (cross-hart races aren't host-unit-testable). The classifier made before/after measurable instead of eyeballed:
  ```
  sched-span-survives-yield:
    current  0/500  (0.0%, CI [0.0%, 0.8%])
    baseline 12/500 (2.4%, CI [1.4%, 4.1%])
    verdict  better (p=0.0005)
  ```
- twelve wedges → zero.
- `send` is the *shared* emission path, so the fix cleaned up the whole cluster: full-suite `--repeat 10` → **zero wedges across 250 scenario-runs**, every previously-wedge-prone scenario clean.
- the one remaining failure was a `budget_exhausted` — alive, just slow. Exactly the residual the classifier predicted.

## "it's jitter" was half right

- post 16 said the residual was uncorrelated jitter. *Half* true, and the classifier untangled the halves:
  - there **was** a coherent bug — the `send` wedge — lumped into "jitter" because the old tooling couldn't tell a disconnect from a slow timeout.
  - there **is** an uncorrelated timing tail — the `budget_exhausted` failures — which genuinely is jitter.
- **"it's jitter" only becomes true after you remove the real bug it was hiding.** You can't get there staring at a blended rate. You get there by splitting it.

## what I caught (the discipline-layer beats again)

- same shape as 16/17 — except this time the agent had the central idea (categorize), and my job was keeping it honest when it got ahead.

### "verify the premise before you build on it"

- with the wedge fixed, the residual was the timing tail. The plan had a workstream for it: **guest-time budgets** — a budget in the kernel's own clock, immune to host-CPU starvation.
- agent built the machinery, was one `wait_for` rewrite from shipping. I asked it to check the premise first. It pulled two `budget_exhausted` captures and compared guest `t` to wall time:

  | failure | wall | guest `t` | ratio |
  |---|---|---|---|
  | 369 | 32.2s | 31.5s | 0.98 |
  | 496 | 33.2s | 32.5s | 0.98 |

- guest time and wall time are the **same clock** (98%). Without `-icount`, QEMU's virtual clock just tracks host wall-clock — my own post-17 notes said so ("the wall-clock cost ... is gated by mtime advancing").
- a guest-time budget would behave identically to the wall budget it replaced. The whole workstream depends on `-icount` (a *different* workstream) to mean anything. **Parked it, deleted the speculative code.**
- the post-17 "surely QEMU can run faster than 10MHz" beat, recurring: a confident quantitative premise that needed one empirical check before it cost a day.

### the meta-lesson, sharpened

- the crack came from the same instinct post 16 named: when targeted experiments come back inconclusive, question the premise, not the design.
- post 16 applied it and *still* drew the wrong conclusion — because the premise it questioned ("is the fence load-bearing?") wasn't the load-bearing one.
- the premise to question was **"is a blended flake rate even the right unit of measurement?"** It wasn't. Categorize, and the bug falls out.

## a footnote on the tooling

- this bug survived a multi-session investigation. It was caught not long after I switched the agent from Opus 4.7 (medium effort) to 4.8 (high effort); the categorize-the-failures idea came from the 4.8 side.
- not over-claiming a model changelog — the **hero is the tool**, the failure classifier, not the model that suggested it. But the suggestion mattered and came right after the switch, so it goes in the record as a footnote.

## what shipped

- **the fix** — `send` holds the `CONSOLE` guard across stage + transmit. One line + corrected `SAFETY` comment + CLAUDE.md gotcha. Writeup in `plans/tx-staging-cross-hart-race.md`; three corrupted captures preserved in `plans/tx-staging-race-evidence/` (`.itest-runs/` is gitignored).
- **the failure classifier** — pure, host-tested; buckets every failure (wedge / stalled / budget_exhausted / hart_stalled / harness / unknown); stamps a `signature` on each NDJSON row.
- **the capture sidecar** — per-failure `FailureCapture` (outcome, frame stats, per-hart timestamps, histogram, transcript), `--capture summary|tail|full` knob. Records all failures by default.
- **`hart_stalled`** — added after `sched-yield-round-trips` showed one hart's clock frozen ~43 guest-seconds behind the other while the kernel otherwise looked alive. The global rate couldn't see it; `last_t_per_hart` could.
- **2-D Grafana** — per-scenario × per-bucket counts through the baseline, prom/OTLP exporters, and a dashboard table. "5% flaky" became "which scenario fails how."
- **`--skip <scenario>`**, and `metadata.toml` now records `jobs` / `cpu_jobs` / the full invocation — a failure's run dir tells you the parallelism it ran under.

## what's next

- `sched-yield-round-trips`'s `hart_stalled` is a **real open bug** — hart 1 freezes at ~1.2s, almost certainly the v0.6 `workload_consumer`'s cross-hart lock discipline. Tooling labels it; next session fixes it.
- guest-time budgets blocked on an `-icount` spike (does deterministic time work with `-smp 2`, and is it fast enough?). If yes the workstream revives; if no the timing tail is a `--jobs` measurement artifact → don't oversubscribe the baseline run.
- finally, the actual SMP payoff: step 11, consumer to hart 1, `Mutex<VecDeque>` under genuine contention. The suite is a trustworthy yardstick now — no coherent bug masking the signal.

---

*Footnote for anyone sitting on a flaky suite they've written off as noise: the rate is lying to you — it hides the truth by averaging over it. Don't ask "how flaky." Ask "*how* does each one fail," capture the evidence at death, let the failures sort into kinds. Mine sorted into "a dropped lock guard" and "a busy laptop." Only one was a bug, and the average said there were none.*

*[TBD: screenshots — Grafana itest table broken down by signature; a wedge capture's corrupted transcript]*
