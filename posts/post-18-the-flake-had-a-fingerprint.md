# Post 18 — The flake had a fingerprint

> Post 16 declared the residual cross-hart flake "uncorrelated jitter" and closed the investigation. It was wrong — there was a real, single, one-line bug hiding in it. What finally caught it wasn't a cleverer bisection; it was a *failure classifier*: bucket every flake by signature, capture the frame transcript at the moment of death, and let the pattern speak. The pattern spoke immediately — ten wedges, all clustered at hart-1 registration, all with the same two telemetry strings smashed together on the wire. That fingerprint pointed straight at `virtio_console::send`, where a `MutexGuard` was being dropped one line too early. `let base = *handle.lock();` releases the lock at the semicolon; the staging buffer and the virtqueue were being written unlocked. One line to fix. 12/500 → 0/500, p=0.0005.

## what this post was supposed to be

Same thing post 16 and 17 promised: step 11, the SMP payoff — workload consumer on hart 1, the `Mutex<VecDeque>` lit up under real cross-hart contention. That arc is *still* on. But I couldn't trust the suite as a yardstick while it flaked at ~5% with no explanation, and post 16's "it's just jitter" conclusion had been quietly bothering me. So before the payoff, one more pass at the residual.

This time it broke open. And the lesson isn't about the kernel — it's about what you can see.

## the residual post 16 gave up on

Recap of where post 16 left it: the cross-hart wedge that the `tag("trap return")` MMIO fence had been masking didn't reproduce in any of five targeted storm scenarios. Fix-on and fix-off rates were statistically indistinguishable. I removed the fence, the suite rate didn't move, and I wrote: *"there is no longer a coherent residual bug — what's left is uncorrelated jitter from cooperative-v0.5 scheduling under multi-thread TCG."*

That was a reasonable read of the evidence I had. The problem was the evidence itself. Every failure looked identical: `QEMU disconnected`, the harness sees a fast-exit, the UART log shows "I am alive — entering heartbeat" and then nothing. Twenty-five scenarios, one undifferentiated 5% rate, no way to tell a real kernel wedge from a slow-host timeout from a Ctrl-C. When every failure is the same opaque shrug, "it's noise" is the only conclusion the data supports.

The fix wasn't to look harder at the kernel. It was to stop throwing away information about the failures.

## the idea: categorize the failures

The move — and credit where it's due, the agent proposed this one — was to stop treating "5% flaky" as a number and start treating each failure as a *thing with a cause*. Bucket every failure:

- **wedge** — the socket disconnected; the kernel died. A genuine kernel residual.
- **stalled** — timed out, QEMU alive, but the kernel went quiet well before the deadline.
- **budget_exhausted** — timed out, alive, frames flowing right up to the deadline. Slow, not broken.
- **harness** — infra: spawn failure, an external SIGINT.
- **unknown** — not enough evidence to say. (Honest non-answers beat forced guesses.)

The classifier is a pure function over the evidence the harness captures at the moment of failure. And here's the thing I hadn't appreciated until I read the actual logs: **the evidence isn't in the log file.** Telemetry leaves over virtio; the UART log carries none of it. The kernel doesn't even UART-print per heartbeat. So every failure's `.log` is the same silence — useless. The load-bearing signal is whether the frame *socket* disconnected (kernel died) or merely *timed out* (kernel alive), which `wait_for` knows at the instant of failure and was throwing away.

So alongside the classifier, a **capture**: on every failure, persist a `fail-<scenario>-<n>.capture.json` next to the log — the wait outcome, frames seen, per-hart last-timestamp, a frame-type histogram, and a transcript of the last frames on the wire. The kernel snitches on itself right up to the moment it dies; the harness should keep the black-box recording. The whole project's pitch is observability, and the suite had been deleting the one observable that mattered exactly when it mattered most.

## the payoff

Ran a flaky scenario 500 times. The summary came back:

```
Failure signatures (12 total):
  wedge: 10
  budget_exhausted: 2
```

Ten wedges. The thing post 16 said didn't exist as a coherent bug. And `budget_exhausted` — the genuinely-just-slow ones — cleanly separated out. The classifier had partitioned the "jitter" into a real bug and real noise.

Then the captures, and this is where it got good. The ten wedge sidecars weren't scattered. Eight of ten disconnected at the *same point*: frame ~142, during hart-1 task registration. And the frame payloads were visibly corrupted:

```
ThreadRegister { id=8 name="hart_\u{8}\u{8}\u{b}har" }
StringRegister { StringId(62) = "snitchos.tas\u{6}>(snitchos.task.hart_1_main" }
MetricRegister { "?" kind=Counter }
```

Two telemetry strings smashed into one — `snitchos.tas`, truncated, a control byte, then the *other* string, `snitchos.task.hart_1_main`. Not a decode bug; the decoder faithfully decoded garbage the kernel had *emitted*. Same string, same seam, across independent failures. That determinism is the tell: a structural race on a fixed buffer, not random corruption. And it only happened during hart-1 registration because that's the one window where both harts emit telemetry at the same time — hart 1 registering its tasks while hart 0 heartbeats.

The old tooling saw "5% flaky." The new tooling saw "≈2% wedge, clustered at hart-1 registration, two strings interleaved in a shared buffer, suspect = the emission path." That's a bug you can go fix.

## the bug: a guard dropped one line too early

`virtio_console::send` stages each frame into a single static `TX_STAGING[256]` buffer, meant to be serialized by the `CONSOLE` mutex. The lock was taken like this:

```rust
let base = *handle.lock();
```

`handle.lock()` returns a temporary `MutexGuard`. `*guard` copies the `usize` base out — and because that's a deref-and-copy, not a borrow, the guard isn't held by anything. It drops at the semicolon. The lock is released *before* the staging copy and the `transmit` that follows. The `SAFETY` comment underneath asserted "single writer to TX_STAGING for the duration of the lock + transmit" — the exact invariant the code had just failed to keep.

Two harts in `send` at once therefore raced two shared resources:

1. **`TX_STAGING`** — both `copy_nonoverlapping` into the same 256 bytes → interleaved frame text. That's the corruption you can read on the wire. Harmless, cosmetic — it just happens to be human-readable, which is what made it such a clean fingerprint.

2. **The virtqueue** — `transmit` writes a single shared descriptor (slot 0, reused every send), bumps `avail.idx`, and busy-waits on `used.idx`. Two harts overwrite the one descriptor mid-DMA (torn address / length) and race the ring index. QEMU validates descriptors against guest RAM; a bogus address or an inconsistent ring trips its sanity checks, it marks the device broken and tears down the virtio-console, and the host socket the harness reads gets EOF. *That's* the wedge.

One cause, two symptoms: garbled text (visible, harmless) and a dead device (invisible, fatal).

The fix is to bind the guard so it lives across the whole stage-and-transmit:

```rust
let guard = handle.lock();
let base = *guard;
// ... stage into TX_STAGING, transmit ...
drop(guard);
```

One line of substance. It's a classic Rust footgun — `*mutex.lock()` reads like "lock, use the value" but means "lock, snapshot the value, *unlock*." Clippy catches a sibling (`let _ = mutex.lock()`) but not the deref-copy form. It's now a gotcha in CLAUDE.md so I don't write it again.

## the result

The integration suite is the test here — cross-hart races aren't host-unit-testable. The classifier made the before/after measurable instead of eyeballed:

```
sched-span-survives-yield:
  current  0/500  (0.0%, 95% CI [0.0%, 0.8%])
  baseline 12/500 (2.4%, 95% CI [1.4%, 4.1%])
  verdict  better (p=0.0005)
```

Twelve wedges to zero. And because `send` is the shared emission path, the fix cleaned up the whole cluster: a full-suite `--repeat 10` came back with **zero wedges across 250 scenario-runs**, every previously-wedge-prone scenario clean. The one remaining failure was a `budget_exhausted` — alive, just slow. Exactly the residual the classifier said would remain.

## "it's jitter" was half right

Here's the satisfying part. Post 16 concluded the residual was uncorrelated jitter. That turned out to be *half* true, and the classifier is what untangled the halves:

- There **was** a coherent bug — the `send` wedge — wrongly lumped into "jitter" because the old tooling couldn't tell a disconnect from a slow timeout.
- There **is** an uncorrelated timing tail — the `budget_exhausted` failures — which genuinely is jitter.

"It's jitter" only becomes *true* after you remove the real bug it was hiding. You can't get there by staring at a blended rate. You get there by splitting it.

## what I caught (the discipline-layer beats again)

Same shape as 16 and 17 — except this time the agent had the central idea (categorize the failures), and my job was mostly to keep it honest when it got ahead of itself. Two beats worth flagging.

### "verify the premise before you build on it"

With the wedge fixed, the residual was the timing tail, and the plan had a workstream for it: *guest-time budgets*. The idea was that a budget measured in the kernel's own clock would be immune to host-CPU starvation — a busy laptop slows wall-clock but not guest progress, so a guest-time deadline lets a healthy-but-starved kernel pass.

The agent built the machinery and was one `wait_for` rewrite from shipping it. I asked it to check the premise first. It pulled two `budget_exhausted` captures and compared the kernel's guest timestamp to the wall duration:

| failure | wall | guest `t` | ratio |
|---|---|---|---|
| 369 | 32.2s | 31.5s | 0.98 |
| 496 | 33.2s | 32.5s | 0.98 |

Guest time and wall time are the *same clock* — 98% identical. Without `-icount`, QEMU's virtual clock just tracks host wall-clock; my own post-17 notes literally said so ("the wall-clock cost of 'wait for 5 heartbeats' is gated by mtime advancing"). A guest-time budget would have behaved identically to the wall budget it replaced. The whole workstream depends on `-icount` (a *different* workstream) to mean anything. We parked it and deleted the speculative code.

That's the post-17 "surely QEMU can run faster than 10MHz" beat, recurring: a confident, quantitative premise that needed one empirical check before it cost a day. The agent built it fast and well — and would have "verified" it by re-running, seen unchanged rates, and not understood why.

### "stop building the next storm and look at it" — still the meta-lesson

The thing that cracked this open was the same instinct post 16 already named: when several targeted experiments come back inconclusive, question the premise, not the design. Post 16 applied it and *still* drew the wrong conclusion, because the premise it questioned ("is the fence load-bearing?") wasn't the load-bearing one. The actual premise to question was "is a blended flake rate even the right unit of measurement?" It wasn't. The unit was wrong. Categorize, and the bug falls out.

## a footnote on the tooling

One honest note, because Chloe-from-the-future will want it: this bug had survived a multi-session investigation, and it was caught not long after I switched the agent from Opus 4.7 (medium effort) to 4.8 (high effort). The categorize-the-failures idea came from the 4.8 side. I don't want to over-claim a model changelog — the *hero* of this story is the tool, the failure classifier, not the model that suggested it. But the suggestion mattered, and it came right after the switch, so it goes in the record as a footnote.

## what shipped

- **The fix:** `virtio_console::send` holds the `CONSOLE` guard across stage + transmit. One line, plus a corrected `SAFETY` comment and a CLAUDE.md gotcha. `plans/tx-staging-cross-hart-race.md` has the full writeup; `plans/tx-staging-race-evidence/` preserves three corrupted captures (`.itest-runs/` is gitignored, so the evidence is copied somewhere permanent).
- **The failure classifier** — pure, host-tested, buckets every failure into wedge / stalled / budget_exhausted / hart_stalled / harness / unknown. Stamps a `signature` on each NDJSON row.
- **The capture sidecar** — per-failure `FailureCapture` (outcome, frame stats, per-hart timestamps, histogram, frame transcript), with a `--capture summary|tail|full` knob. Records all failures by default; turn it up to investigate.
- **`hart_stalled`** — a per-hart bucket added after a `sched-yield-round-trips` failure showed one hart's clock frozen ~43 guest-seconds behind the other while the kernel otherwise looked alive. The global rate couldn't see it; `last_t_per_hart` could.
- **2-D Grafana** — per-scenario × per-bucket signature counts flow through the baseline, the prom/OTLP exporters, and a dashboard table. "5% flaky" became "which scenario fails how."
- **`--skip <scenario>`**, and `metadata.toml` now records `jobs` / `cpu_jobs` / the full invocation, so a failure's run dir tells you the parallelism it ran under.

## what's actually next

- The `sched-yield-round-trips` `hart_stalled` is a *real, open* bug — hart 1 freezes at ~1.2s, almost certainly the v0.6 `workload_consumer`'s cross-hart lock discipline. The tooling now labels it; next session fixes it.
- Guest-time budgets are blocked on an `-icount` spike (does deterministic, instruction-counted time even work with `-smp 2`, and is it fast enough?). If yes, the workstream comes back to life; if no, the timing tail is a `--jobs` measurement artifact and the answer is "don't oversubscribe the baseline run."
- And still, finally, the actual SMP payoff: step 11, consumer to hart 1, the `Mutex<VecDeque>` under genuine contention. The suite is a trustworthy yardstick now — no coherent bug masking the signal — so when the lock-wait shows up, I'll believe it.

A footnote for anyone sitting on a flaky suite they've written off as noise: the rate is lying to you, or rather, it's hiding the truth by averaging over it. Don't ask "how flaky is it." Ask "*how* does each one fail," capture the evidence at the moment of death, and let the failures sort themselves into kinds. Mine sorted into "a dropped lock guard" and "a busy laptop." Only one of those was a bug, and the average had been telling me there were none.
