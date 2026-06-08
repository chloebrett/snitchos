# Post 21 — Make it fail first

- the v0.6 SMP milestone shipped its headline arc but left the back half of its integration suite unwritten — the scenarios that guard the SMP machinery against silent regression. "finish the test list" sounds like chores. two of the three turned into the most useful thing a test can be: one I had to deliberately break the kernel to trust, and one that failed the instant I wrote it and handed me a real scheduler bug. post 19's lesson was "run it again." this one: a green test proves nothing until you've watched it go red.

## the list, and which ones bit

- six SMP scenarios planned; three existed. remaining: TLB-shootdown-visible, spans-carry-hart-id, ipi-wakes-idle-hart, ping-pong-cadence (the last two collapsed once I understood the kernel — see below).
- expected a rubber-stamp session. got two findings instead. the scenarios you write to "finish the list" are exactly the ones you're tempted to wave through.

## the TLB test that was allowed to be a lie

- the kernel already had `shootdown-storm`: boot two harts, hammer `mmu::shootdown` in a loop, assert the IPI counters climb. passes. has passed for weeks.
- it proves almost nothing. it shoots down a *fixed, always-mapped* address — its own doc admits "sfence with any VA is harmless on a fresh-mapping kernel." a shootdown that sfenced the *wrong* address would pass it. it tests the plumbing (an IPI went out), not the purpose (a stale translation got invalidated on the other hart).
- the real test: hart 0 points a VA at frame A, hart 1 reads through it (caching that translation), hart 0 repoints to frame B + shoots down, hart 1 reads again and **must** see B, never stale A.

- **there was no way to remap.** `mmu::map` does a *local* sfence only and refuses to overwrite — a fresh mapping can't be cached stale anywhere. a remap is the opposite. so `shootdown` existed but nothing real called it; only the storm did, directly. added `mmu::remap(va, new_pa, perms)`: pure host-tested page-table walk in `kernel-core` + a kernel wrapper that overwrites the leaf and *then* fires the cross-hart `shootdown`. first genuine mmu path that broadcasts one — closes a gap the milestone left open.
- **the obvious version of this test is vacuous.** if hart 1 has no cached entry, its page-fault walker just reads the new PTE and gets B *whether or not the shootdown ran*. a fresh-map test passes on a totally broken shootdown. teeth require hart 1 to hold the **old** translation at remap time, so a miss leaves it reading stale. under QEMU TCG the soft-MMU TLB only flushes on an intercepted `sfence.vma`, so a missed shootdown genuinely leaves the stale entry to be caught — but I had no proof my test hit that window rather than passing for free.

## make it fail first

- before trusting the green, I made it red on purpose: changed `mmu::remap` to do a local sfence only — exactly what someone who forgot the broadcast would write — and reran.

```
[smp-tlb-shootdown-visible] FAILED
  hart 1 observed a STALE TLB translation after a remap
  (tlb_stale_reads > 0) — mmu::remap's shootdown did not
  invalidate the other hart's cached entry.
```

- failed on the exact oracle, not on a crash or timeout. reverted the sabotage; watched it pass. now the green means something, because I've seen its red and confirmed I can produce it.
- **moral: a coherency test you've never seen fail is a green light wired to nothing.** the counterfactual is the test of the test.

## ping-pong: the IPI that only fired once a second

- supposed to be the easy one. two tasks, one per hart, passing a turn flag. both counters reaching K proves K strict cross-hart alternations.
- first cut leaned on IPIs on purpose: take your turn, flip the flag, `IPI_WAKEUP` the partner, yield to idle (which `wfi`s). partner gets poked awake, takes its turn, pokes back.
- ran it. passed. **timed out at 30s having done 29 turns.** 29 turns in 29 seconds isn't a wakeup rate — it's the 1 Hz *heartbeat timer*. the IPIs weren't waking anything; each handoff waited for the next timer tick.

| version | rate | what actually wakes the hart |
|---|---|---|
| IPI-wake-from-`wfi` (first cut) | ~1 turn/sec | the 1 Hz **timer**, not the IPI |
| busy-spin (final) | passes in ~1.1s | nothing sleeps |

- **textbook lost-wakeup, and a real limitation — not a flake.** idle is `loop { wfi; yield }`. the waiter does: check flag, not my turn, yield → idle → `wfi`. the IPI can land *in the gap between the check and the `wfi`*: the handler clears the pending `SSIP` (the `Wakeup` arm is a no-op — its whole job is to break the `wfi`) and returns. `wfi` then runs with nothing pending and sleeps until the *next* interrupt: the timer, a second later. the flag was flipped and visible; idle just never re-checks it.
- **why the other IPI tests dodge it:** `spawn-storm` / `ipi-pong` wake an idle hart too, but their signal is a *runqueue entry* the idle loop re-checks after every `wfi`. a spurious early wake just loops once more and finds the work. a bare memory flag has no such re-check.
- race-free condition-wait needs interrupts *disabled* across check-and-`wfi` (RISC-V `wfi` wakes on pending interrupts even when masked — that's the property the idiom relies on). that's preemption-era machinery → v0.9, not cooperative SMP.
- rewrote ping-pong to busy-spin (it's an *alternation* oracle; the one-directional IPI-wake path is already covered) and wrote the corner into `scaling-corners.md`.
- **moral: an assumption that's free on one hart — "you never sleep waiting for yourself" — becomes a bug the moment a second hart has to wake the first.** that's the entire reason v0.6 does SMP early: surface these while the audit surface is still small. the test surfaced one.

## the two quiet wins

- **`smp-spans-carry-hart-id`** — assert a span opened on hart 0 carries `hart_id=0` on the wire and one on hart 1 carries `hart_id=1`. only work: teach hart 1's idle-probe to actually open a span so there's one to check. passed, stayed passed. not every test has to teach you something; some just hold a line.
- **`smp-producer-consumer-correctness`: 17.1s → 2.1s.** by far the slowest scenario. almost shrugged it off as "QEMU is slow." it wasn't — same lesson as post 19 in a new hat: it ran at `burst=1`, which is cadence-bound (~64 samples/s), so reaching 1000 samples took ~16s of harts politely sleeping between batches. `burst=256` overlaps the batches: hits the threshold in under a second *and* runs the correctness oracle under real contention instead of near-serial 1 Hz blips. the slow test wasn't slow; it was barely running.
- also taught `cargo xtask itest` to take a comma-separated list, and hoisted a thrice-copied `fence_via_uart_lsr` helper into one place. housekeeping.

## what i learned

- **a green you haven't earned is a lie you haven't caught.** running it again (post 19) catches the *flaky* test. it does nothing for the test that passes every time *because it would also pass on a broken kernel*. the only fix for that is to break the kernel and watch the test notice.
- **the counterfactual is part of the test.** for any coherency/ordering assertion, the question isn't "does it pass" — it's "have I seen it fail for the right reason." budget the five minutes to sabotage and revert.
- **take an honest failure as a gift.** a green ping-pong would have taught me nothing; a 1-Hz ping-pong handed me a scheduler bug. the failure *was* the payload.
- **lost-wakeups hide behind "it works, just slowly."** 29 turns in 29 seconds looked like a pass with a tight budget. the rate was the tell. if a thing that should be IPI-fast is running at timer cadence, the wakeup is broken, not slow.
- **tests aren't downstream of the design — sometimes they force you to wire it up.** the shootdown had been "done" for weeks. it wasn't connected to a real mmu path until a test needed it to be.

## what's next

- **collector `host.cpu_id`** — the wire now carries `hart_id` end-to-end and the spans-carry-hart-id test proves it, but the collector still drops it on the floor. populate the OTLP `host.cpu_id` attribute + per-hart metric labels so Tempo/Grafana can finally slice traces by CPU. this is the v0.6 promise that isn't cashed yet.
- **v0.6 closeout** — CLAUDE.md SMP section, README "Working" bullets, the `scaling-corners.md` items v0.6 actually resolved.
- **the lost-wakeup is a v0.9 IOU.** when preemption lands and `local_irq_save`/`restore` exist, race-free IPI condition-wait becomes possible — and ping-pong can go back to sleeping between turns instead of busy-spinning. noted in the corners doc so future-me doesn't rediscover it the hard way.
