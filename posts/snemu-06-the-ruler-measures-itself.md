# snemu 6 — the ruler measures itself

- post 5 ended with a promise: snemu runs the real itest suite deterministically, and it's an in-process library with no shared state, so it "parallelizes trivially." fan it across cores, I said, and ~355s of single-threaded CPU becomes ~44s of wall-clock. this is that post. it starts there — and then spends the rest of itself being wrong about where the time goes, four times in a row, each time because the harness's own instrumentation told me so. the audit that measures the kernel ended up measuring, and optimizing, itself. we land at **25.9 seconds and 97% core utilization**, and find a real O(n²) allocator bug and a latent kernel UB on the way.

## the easy 5×, and the floor it hit

- the fan-out was as trivial as promised: an order-preserving work-queue over `thread::scope`, one scenario per worker, results slotted back into report order so the output stays deterministic regardless of worker count. `cargo xtask snemu-itest -j 10`. 355s → 66s. done.
- except 5.4×, not the 8× the core count promised. the wall-clock floored at the **single heaviest scenario**: a handful of scenarios (the OOM leaks, the Stitch tree-walker) carry billions of instructions each, and no amount of fan-out beats the slowest one running alone. that's the whole rest of the post: the floor kept being one scenario, and each time I killed it a different one took its place.

## a north-star that isn't wall-clock

- the first thing I built wasn't an optimization, it was a **ruler**: a per-scenario "slowest by guest-instructions" table. instret is deterministic — contention-free, reproducible — so unlike wall-clock it says exactly where the CPU goes, the same way every run. total instret became the north-star. every lever after this is either "reduce instret" or "run instret faster," and the table told me which scenario to point it at.
- (a cheap early win the table found: one Stitch scenario computed `primes(10)`; `primes(5)` exercises the identical load-parse-eval-emit path for a third the tree-walker cost. the magnitude added no coverage.)

## the negative result: idle-skip that skipped nothing

- the table said ~90% of the tail was a handful of scenarios doing ~220M instructions each between heartbeats. "that's idle," I thought — the kernel's idle task spins `wfi` waiting for the timer, and snemu's `wfi` was a nop that stepped one instruction at a time through the whole wait. so I taught snemu to *fast-forward*: when every hart is parked on `wfi`, jump the clock straight to the next timer deadline instead of grinding through the idle loop. modelled `wfi` as a real halt, added a `HartState::Idle`, the whole thing. six unit tests. it made snemu *more* faithful to QEMU, which halts on `wfi` too.
- it saved **zero instructions.** the flag toggled on vs off was byte-identical. because the ~220M between heartbeats isn't idle — it's *work*: the demo tasks burn LCG, the OOM loops allocate. the heartbeat loop busy-yields through the scheduler; nothing sleeps. I'd optimized a workload shape the suite doesn't contain. kept the flag (it's correct, and a future interactive shell *will* idle), reframed the post section from "the big win" to "the myth I busted," and moved on. the ruler is a bug-finder pointed at your hypotheses too.

## the ~2B nobody needed to spend

- next the ruler caught something structural: every scenario re-booted the kernel from scratch. ~25M instructions of boot, 108 times, over ~30 distinct workloads — ~2B of *redundant* boot. the machine is `Clone`, and post 2 built it that way on purpose. so: boot each workload **once** to the `entering heartbeat` checkpoint, snapshot it, and fork the snapshot per scenario. fidelity-exact — the clone carries the emitted-frame history in its virtio-TX buffer, so even boot-time-assertion scenarios see their frames. total instret dropped ~19%.

## the packing was terrible, and the data said why

- but wall-clock barely moved, and the new utilization report explained it: one worker at **100%**, five stranded at **~31%**, mean 51%. the fan-out unit was the *workload group* — because `Machine` held a `RefCell` (single-thread by design) and couldn't cross a thread boundary, so a group's scenarios ran serially on one worker. the 20-scenario demo group was one indivisible 55-second pole.
- the fix was to make the snapshot shareable: swap the UART's `RefCell` for a `Mutex` (which *is* `Sync`; the lock only fires on actual UART MMIO, off the hot path), so a booted snapshot can be forked across workers. now the unit is the *scenario*, and a workload's scenarios spread across all ten cores. min utilization 31% → 63%. the stranded workers were gone; one pole remained.

## the pole, and the number that wouldn't add up

- the packing artifact — a JSON the audit now drops per run — named the pole outright: `frame-allocator-oom`, running **46.7 seconds straight** on w0 while everyone else finished at ~32s and idled. one scenario. 774M instructions to leak 128 MiB of RAM.
- and here's where I got stuck, productively. **why 774M?** I measured: it's ~31K frame allocations (the whole pool). that's ~25,000 instructions *per allocation*. absurd for a bitmap allocator. I ruled it out piece by piece with direct experiments: zeroing the frames? measured — only 52M. the bitmap scan? it stops at the first free word. the lock? a `spin::Mutex`, a handful of instructions. I could account for maybe 95M of the 774M. the other ~680M was hiding in a bare `alloc()`.
- so I did what the whole project is built to do: **I asked the guest to show its work.** dumped the frame metrics over time — `in_use` climbing 10871 → 19063 → 27255 → 32768, OOM at the fourth heartbeat. and doubled the leak rate as a controlled experiment: instret didn't change. so it wasn't per-heartbeat cost and it wasn't the leak loop — it was a **fixed cost proportional to the number of frames allocated.**

## the O(n²) hiding in an unoptimized loop

- the answer was two things stacked. one: `Bitmap::alloc` scans from word 0 for the first free bit, and a sequential fill marches that frontier upward — so filling the pool is **O(n²)**, ~8 million word-iterations for 32K frames. two: the itest kernel is a **debug build**, so each iteration of that scan is *tens* of unoptimized instructions, not a few. ~8M × ~70 ≈ ~600M. plus zeroing, plus change: 774M. a linear-scan allocator hitting its worst case, in unoptimized code, that no "does it boot" test would ever surface — only the audit's own ruler, pointed at the kernel under exhaustion.

## two fixes, and making the fast one *assertable*

- **smaller RAM.** because the cost is quadratic in pool size, shrinking the frame-oom machine pays super-linearly: 48 MiB is 3/8 the frames, so ~(3/8)² ≈ 0.14×. this meant patching the DTB `/memory` node (in-place, both engines, so snemu and QEMU run the *identical* machine — the leak rate went pool-relative, `total/4` per tick, which is exactly the old `8192` at 128 MiB and gradual at any size). 774M → 131M. and it's a real coverage win independent of speed: an OS should work regardless of physical RAM, and now one scenario proves it on a genuinely small machine.
- **an O(1) allocator.** the root-cause fix: a `next_hint` cursor so `alloc` scans from the frontier, not word 0; `free` rewinds it when it returns a frame behind the cursor. O(n²) → O(n) to fill. frame-oom at 128 MiB: 774M → 98M. and it speeds *all* kernel allocation — spawn-reclaims dropped too.
- the part I'm happiest with: **we made the performance property a test.** a correctness optimization is easy to guard; a performance one usually isn't — the frames handed out are byte-identical whether you scan from 0 or from the cursor, so no behavioral test can tell. so I exposed the scan cost (a `scan_words` counter — also a legitimate observability signal) and asserted a full fill is O(n). then I *proved the test bites*: reverted `alloc` to scan-from-0 and confirmed that mutant fails **only** the linear-fill test while every behavioral test still passes. the O(1) claim rests on a killed mutant now, not on an audit measurement.

## the workload tax, one more time

- with frame-oom gone the pole became `cooperative-baseline` (438M). same lesson as the OOM workloads: it rides the `demo` layout's `task_a`/`task_b`, which burn LCG it doesn't need and eat half the scheduler's turns — starving both the producer/consumer it *does* need and the `main` task that emits its metric. so I carved out a `Cooperative` workload: just the pair, no demo tasks. **438M → 19M.** 23×, not the 2× I predicted, because the effects compound — the pair gets every turn *and* the heartbeats that gate the metric come rapid-fire.
- that took the makespan to **25.9s** and utilization to **97.2% mean / 95.8% min**. packing is *done* — you cannot pack tighter than 97%. from here the only levers are reducing instret further or running it faster.

## the rabbit hole I climbed back out of

- "running it faster" pointed straight at the debug build — if unoptimized code inflates instret across the *whole* suite, a release kernel should be a suite-wide win. so I tried it. and it broke everything: under snemu the kernel didn't boot; under QEMU it booted and ran userspace but **the timer IRQ stopped firing** and the UART emitted garbage. because it reproduces on QEMU, it's not a snemu gap — it's a **latent kernel UB that release codegen exposes.** classic signature: works in debug, dies under optimization, corruption-flavored. a missing `volatile` on a timer CSR or MMIO write is my bet. filed it, backed out. a real bug the audit found by trying to go faster — and a real speedup still waiting behind it.

## what I learned

- **the measurement is the discovery — and it's recursive.** post 5 said a fidelity audit is a bug-finder pointed at yourself. this post: the *instret table* found the boot tax, the *utilization report* found the packing bug, the *packing JSON* named the pole, the *frame metrics* cracked the O(n²), and the *release attempt* found a UB bug. the ruler I built to measure the kernel kept measuring the harness, the allocator, and itself. build the instrument first; the optimizations fall out of reading it.
- **negative results are results if you instrument them.** idle-skip saving zero wasn't a waste — it was a precise disproof of "the tail is idle," which redirected me to "the tail is real work," which is why boot-once and the O(1) allocator were the right next moves. the flag-toggle A/B that showed *byte-identical* instret is what made the negative result trustworthy.
- **when the arithmetic won't close, stop reasoning and ask the guest.** I burned real time trying to explain 774M from first principles and kept landing at ~95M. the frame metrics + the doubling experiment closed it in two measurements. the OS's whole thesis is observability; use it on the OS's own guts.
- **you can make a performance property assertable.** expose the cost, assert the bound, and prove the assertion kills the regression mutant. then your O(1) rests on a test, not a vibe.
- **going faster finds bugs.** the release attempt didn't give a speedup — it gave a latent UB bug that debug had been hiding. that's arguably worth more.

## what's next

- packing is maxed, so the remaining wins are instret (the new tail — `smp-tlb-shootdown`, `sched-yield`, the Stitch tree-walkers, `spawn-reclaims`) and MIPS (post 4's JIT tiers). but the biggest single lever is now that **timer-death UB**: fix it, and a release kernel unlocks a suite-wide instret cut on top of everything here. the ruler found the bug; next post, we hunt it.
