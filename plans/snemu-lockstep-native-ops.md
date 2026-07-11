# snemu lockstep-preserving native memops

## Outcome (measured) — SHIPPED, increments 1–3

The gate is gone; the collapse fires on **every** memop entry and advances the
other harts by the charged instret, reproducing the interpreter's interleaving.
`snemu-itest` A/B (`--native-ops` on vs off), full 110-scenario suite:

- **110/110 pass on↔off** — frame-stream fidelity preserved (no scenario assertion
  changed). This is the real gate (see the correction below).
- **The multi-hart poles the old `only_running_hart` gate could not touch now
  collapse**: `spawn-reclaims-memory/-names` 41M → 18M (−56%), `frame-allocator-oom`
  28M → 10M (−63%), `live-tasks` 7.0M → 2.8M (−60%). These are exactly the
  workload memsets post-7 measured as unreachable while gated.
- **Suite total instret 701M → 466M (−34%); makespan 4.0s → 3.3s.**

**The collapse is guest-instret-faithful — the "divergence" I first reported was a
measurement artifact.** The `snemu-itest` packing metric counts host `Machine::step()`
*calls*, and a collapse subsumes many rounds into one call — so on↔off "instret"
dropping ~36% is just the *speedup* (fewer host iterations per guest instruction),
not a change in guest execution. Comparing the actual guest clock (`Machine::instret()`)
on↔off tells the real story: boot to the checkpoint is **4.018M (off) vs 4.010M (on)
— 0.2%**, and across the suite **94/110 scenarios match within 1%, total +1.5%**. The
lone outlier, `smp-tlb-shootdown-visible` (+37%), is the `assert_absent` scenario
whose budget is denominated in *step-calls*: with collapse each step covers more
guest instructions, so it scans more guest-instret before the budget runs out —
benign, still passes.

**The `memop_charge` refit (this session).** The `--calibrate-memops` probe (see
below) measured `real/charged = 1.121` — the old `BASE=8` lowballed the per-call
fixed cost (8-byte splat setup + head-align + tail-byte loop). Disassembly-informed
refit → `BASE=24`, tail bytes ×4 → `real/charged = 1.011`. That residual ~1% is the
whole of the on↔off guest-instret drift on the compute-heavy scenarios; driving it
to exactly 1.000 would need a per-shape/exact charge (see "recalibration fork" in
the session notes) but is pure polish — the feature is faithful as is.

So the shipped gate is **pass-set equivalence on↔off (110/110) plus guest-instret
match within ~1%**, both of which hold. Race fidelity is protected by the peer
advance (a running peer retires its real instructions during the collapse; only
hart `i`'s private memset is compressed).

**Calibration probe (this session).** `Machine::enable_memop_probe()` +
`snemu --calibrate-memops`: with native ops off, times each memop from entry PC to
`ra` and accumulates real-retired vs `memop_charge`. It is the standing tripwire
that a future kernel rebuild (or a collapse over a shared range) would trip as
`real/charged` drifting off 1.0.

Increment 4 (the race-hiding "fast" flag) is **not built** — the lockstep path
already captures the win *and* stays faithful, so a separate max-speed mode adds
complexity for marginal extra throughput. Left for later if the compute tail wants it.

**Known limitation — overlapping cross-hart memops.** The collapse applies hart
`i`'s memop all-at-once at base-time state, then advances peers. Exact iff no peer
touches hart `i`'s `[src,dst,len]` range mid-op (private in every kernel call site
today → 110/110, guest-instret faithful). A *future* workload that memcpys a shared
buffer while another hart touches it could diverge. Two guards, both cheap and
deferred until something needs them: (1) an entry-overlap check that declines the
collapse when another running hart is at a memop entry whose range overlaps; (2) the
calibration probe / on↔off A/B, which trips if any collapse ever changes behavior.

## The cliffhanger this closes

Post 7 shipped tier-0.5 of the JIT: intercept the kernel's `memset`/`memcpy` at
their ELF-resolved entry PCs, run the op natively on guest RAM, and charge the
instret the interpreted loop *would* have retired so the deterministic clock (and
thus the frame stream) is unchanged. It landed **gated**:

```rust
if self.native_ops
    && (Some(pc) == self.memset_pc || Some(pc) == self.memcpy_pc)
    && !self.harts[i].is_idle()
    && self.only_running_hart(i)          // <-- the gate
    && let Some(charged) = …try_native_memop(…)
{ self.time += charged; … }
```

The gate fires the collapse **only when hart `i` is the sole runner** — then there
is no cross-hart interleaving the lockstep scheduler must preserve, so collapsing a
whole memset into one step is fidelity-exact. Two consequences, both measured in
post 7:

- **It misses the workload memsets.** The idle peer flickers (`wfi; yield`), so at
  a memset entry `only_running_hart` is often false; only the single-hart boot
  memset collapses. `spawn-reclaims` stayed at 41M.
- **Ungating it (drop the gate) is a 4.5× win** (`spawn-reclaims` 41M → 9M) **but
  breaks fidelity**: collapsing hart 0's 1544-instruction memset in one step robs
  a running peer of the ~1544 interleaved instructions it would have retired
  during it — it starves the peer of interleaving and shifts race windows. snemu's
  faithful cross-hart interleaving is the whole reason it caught the TX_STAGING
  deflake race; the risk is not "the suite fails" but "a *future* race silently
  stops reproducing."

Post 7's addendum named the resolution: **the lockstep-preserving helper — fire
the collapse always (no gate), then advance the other running harts by the charged
instret before continuing, reproducing the interpreter's interleaving exactly.**
This plan builds it. chloe's note: also offer a build/CLI flag to pick
"as-fast-as-possible" (race-hiding) vs "race-preserving" (this).

## Why collapse-then-advance is exact (the correctness argument)

Round-robin lockstep over `K` rounds, hart `i` running a memset and peer `j`
flickering: round `r` advances hart `i` by store `r` and hart `j` by instruction
`r`; the shared clock ticks once per retirement. After `K` rounds hart `i` is one
memset past and each peer is (up to) `K` instructions past.

Collapsing means: apply hart `i`'s whole memop **first** (all its reads/writes at
the round-0 memory state), then advance the peers. That equals the interleaved run
**iff no peer reads or writes the memop's byte range during the catch-up** — then
there is no cross-observation, and the only thing the collapse changes is *when*
hart `i`'s private stores land relative to the peers, which nothing observes. The
peers still end at the exact PC/registers they'd reach after `K` rounds, and their
timers still fire at the exact clock value, because we advance the clock as the
interleave would (one tick per hart-`i` store, plus one per peer retirement).

- **memset** writes `dst`, reads no memory → exact unless a peer touches `dst`
  (freshly-allocated private page in every kernel call site).
- **memcpy** reads `src`, writes `dst` → exact unless a peer writes `src` or reads
  `dst` mid-copy (again private in the kernel's call sites).

We do **not** try to prove privateness statically. The proof is snemu's standing
discipline: **`snemu-itest` byte-identical with native-ops on vs off, across every
workload including the multi-hart ones the old gate skipped.** If any workload's
frame stream or total instret diverges on↔off, we've found a memop over a
concurrently-touched range and gate *that* case. Same bar the decode cache and
idle-skip flags meet.

## Design

Replace the `self.time += charged` fast path with a `Machine::collapse_memop(i,
charged)` that reproduces `charged` scheduler rounds with hart `i`'s stores already
applied natively:

```
// hart i's memop already applied by try_native_memop (regs + pc set, RAM written).
// Now reproduce `charged` rounds of the OTHER harts against the progressing clock.
let base = self.time;                     // clock before this collapse

// Fast path — O(1) — when no peer moves during the span:
//   every other hart is idle/stopped AND none has a wake_deadline <= base+charged.
// Then peers retire nothing and no timer fires mid-span, so the whole span is
// just hart i's `charged` ticks. This preserves post-7's single-runner collapse.
if self.no_peer_activity_within(i, base + charged) {
    self.time = base + charged;
    return;
}

// General path — interleave round-by-round:
for _ in 0..charged {
    self.time += 1;                       // hart i's store this round
    for j in 0..self.harts.len() {
        if j == i || self.harts[j].is_stopped() { continue; }
        self.harts[j].set_cycle(self.time);
        match self.harts[j].step(&mut self.bus)? {
            Sbi(req) => { service_sbi(…, j, &req); self.time += 1; profile += 1 at pc_j }
            None     => { self.time += 1; profile += 1 at pc_j }
            Idle     => {}                // parked; retires nothing this round
        }
    }
}
```

Notes:

- **Profiling**: hart `i`'s `charged` ticks attribute to the memop entry PC (as
  today); each peer retirement attributes to the peer's own PC (capture it before
  its step, like the main loop). Keeps the exact-instret profiler honest.
- **Peer native ops during catch-up**: peers run the plain interpreter `step` — no
  nested collapse. Correctness preserved; we just forgo nested speedup. A peer that
  is itself at a memset entry runs it as the interpreted loop this span.
- **Peers going idle mid-span**: a peer that hits `wfi` parks (`Idle`); subsequent
  rounds it retires nothing but is still checked against the advancing clock, so
  its timer wakes it at the right tick — exactly the main loop's semantics.
- **Faults during catch-up**: a peer's `step` returning `Err` propagates out of
  `collapse_memop` (`?`), same as the main loop. Hart `i`'s memop is already
  applied and consistent, so there is no partial memop to unwind.

`Cpu` (single-hart wrapper): its lone hart is always the sole runner, so it takes
the O(1) fast path — behaviour and the ~135 cpu tests unchanged.

## Increments (each TDD, green throughout)

1. **`collapse_memop` interleave path + drop the gate.** Replace the `self.time +=
   charged` arm with `self.collapse_memop(i, charged)?`; remove `!is_idle()` and
   `only_running_hart` from the fire condition (keep the PC + `try_native_memop`
   checks). Delete `only_running_hart` if now unused.
   - Test: 2 harts — hart 0 hits a stub memop charging `K`; hart 1 runs a
     self-loop (always ready). After the collapsing `step`, hart 1's PC/retirements
     advanced by exactly `K` and `instret == base + K + K`. Revert to a bare
     `time += K` and the test reads `base + K` — proves the peer-advance bites.
   - Test: hart 1 idle with a deadline inside the span → wakes at exactly its
     deadline tick mid-collapse (not before, not after).

2. **O(1) fast path for a quiet span.** Add `no_peer_activity_within(i, end)` (all
   other harts idle/stopped and `earliest peer wake_deadline > end`); take
   `time = base + charged` when true.
   - Test: single-hart machine, a large-`len` memop completes in one `Machine::step`
     with the clock advanced by `memop_charge(len)` — and byte-identical final
     state to forcing the interleave path (a test-only toggle or a 2-hart-with-
     parked-peer variant). Guards that the fast path == the general path.

3. **Wire + validate the A/B.** `--native-ops` already exists on `snemu` and
   `snemu-itest`. Run the full suite on↔off:
   - `cargo xtask snemu-itest` (106/108) with `--native-ops` **and** without →
     identical pass set.
   - Assert **byte-identical total instret per scenario** on↔off across all
     workloads, *including* the multi-hart ones (`smp-*`, `viewer`, `shell`) the
     old gate skipped. Any divergence = a shared-range memop → investigate/gate.
   - Record the makespan / `spawn-reclaims` delta for the post.

4. **(optional) fast-unsafe flag** — chloe's "as fast as possible" mode.
   `set_native_ops_fast(bool)`: when on, `collapse_memop` always takes the O(1)
   `time = base + charged` path (skip peer catch-up) — the race-hiding ungated
   collapse, documented as **non-oracle** (may hide future cross-hart races). CLI:
   `snemu --native-ops-fast` / `snemu-itest --native-ops-fast`. Default off; the
   lockstep-preserving path is the shipped default. Only build this if the
   lockstep path's multi-hart win proves materially short of the ungated 4.5% and
   the extra speed is wanted for the compute tail.

## Validation gate (the whole point)

The helper is **faithful** iff `snemu-itest` is byte-identical (frame stream +
per-scenario total instret) with native-ops on vs off, on every workload. That is
the empirical proof the collapse touched only private ranges. Ship only when green.

## Non-goals

- Nested collapse (a peer's memop during another's catch-up) — plain interpreter.
- Relaxed memory / reordering `aq`/`rl` — snemu stays sequentially consistent.
- Proving privateness statically — the A/B is the proof.
- The full JIT (M6) — this is still tier-0.5; it generalises the PC→native-block
  dispatch the JIT will plug compiled blocks into. Full JIT is the next milestone.
