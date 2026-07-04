# snemu 2 — let QEMU tell you when you're wrong

- post 1 ended with snemu booting SnitchOS through its own Sv39 MMU and running at higher-half. since then it grew up: the A extension, the `time` CSR, a virtio-console device model, an sstc timer, an SBI firmware shim, and a second hart — enough that it boots the *real* kernel, on two harts, through **every one of the 43 runtime workloads**. but a home-grown emulator has a nagging question hanging over it: *how do I know it's right?* this post is about building the answer — a **differential oracle** that runs the same kernel under snemu and QEMU and diffs what comes out — and the thing I didn't expect: the oracle's best moments weren't when it said "you're faithful." they were when it said "you're wrong."

## the idea: a second opinion, for free

- SnitchOS already emits structured telemetry — postcard-encoded `Frame`s over a virtio-console, the thing the integration suite asserts on. so snemu and QEMU, running the same kernel, each produce a *stream of frames*. if snemu is faithful, those streams should agree. that's a whole validation suite I don't have to write: **boot the identical binary under both, diff the telemetry.** the integration tests already say what a correct kernel looks like; the oracle asks whether snemu *is* one.
- the ergonomics fell out nicely because snemu is a Rust library. QEMU is a subprocess you spawn and talk to over a socket; snemu you just... call. `cargo xtask snemu-diff` boots the kernel in-process, steps it, reads the bytes off the emulated virtio-console, and decodes them with the exact same `protocol` crate the collector uses. no toolchain, no socket, no new test data.

## the design tension: they will never be byte-identical

- the naive version — decode both, compare — fails instantly, and *why* it fails is the whole design. snemu and QEMU disagree on things that are **correct** to disagree on:
  - **timestamps.** snemu's clock is deterministic (an instruction counter — more on that below); QEMU's is real cycles. every `t` field differs.
  - **metric values.** heartbeat counts, bytes-used — these drift with wall-clock. comparing them is comparing noise.
  - **ordering under concurrency.** two harts registering task metrics race; snemu's round-robin and QEMU's true-parallel `thread=multi` interleave them differently, so the same string gets a different id.
- so the diff has to be **structural**. `canonical()` zeroes the volatile fields (timestamps everywhere, metric values) so the deterministic clock compares equal to real cycles. then two comparisons: the **boot-prefix frame sequence** (identical up to the first cross-hart ordering wobble), and — the robust one — the **registered-name vocabulary**: the *set* of telemetry names each side emits, order-free and run-length-free. the verdict rule is asymmetric on purpose: a name snemu emits that QEMU never did is a real divergence (snemu invented telemetry → **fail**); names only QEMU emits are just behavior snemu didn't reach in its budget (**fine**).
- run it across all 43 workloads and you get a table. **40 PASS, 3 FAIL.** snemu reproduces QEMU's telemetry — scheduler, SMP, OOM, storms, IPC, caps, filesystem, userspace, the lot — byte-faithfully on everything but a tight cluster of three. that's a good day. but the 40 greens aren't the interesting part.

## the oracle caught me twice

- here's what I actually learned building this: **a differential oracle is a machine for catching your own confident-but-wrong beliefs.** twice this session I was sure of something, and the oracle — or the discipline of validating against it — proved me wrong before I could ship the mistake.

- **the snapshot that "worked."** snemu has no hidden state (no JIT cache, no host threads), so snapshotting a running machine is `#[derive(Clone)]` — a deep copy of registers, RAM, devices. that unlocks a lovely optimization: boot the kernel's common prefix *once*, then fork per-workload by cloning the snapshot and patching the `workload=` bootarg into its RAM. I built it, ran it, and every workload dutifully produced telemetry. it "worked." I was ready to call it done — the frame counts were all the same, but I told myself the fork budget was just too small to reach the divergence.
  - then I did the boring thing and *checked*: vary the budget, confirm the workloads actually diverge. they didn't. **41 of 43 forks produced byte-identical output** — silently wedged at the same stuck state. the one that ran correctly was the workload the base image happened to boot with. the RAM patch was writing a *different-sized* DTB over the booted region; the guest re-parsed a differently-shaped blob and hung. my "it works, budget's just small" was a comfortable story over a real bug. (the fix was mundane — pad the bootarg to a fixed-width field so every DTB is the same size, layout-preserving — but I'd never have looked without a test that could say *no*.)

- **the fix that fixed the wrong thing.** three workloads failed: the stack-guard family, each diverging on a single name — `kernel.heartbeat`, emitted by snemu but not QEMU. obvious, I thought: these workloads overflow the stack onto an unmapped guard page; QEMU *traps* the fault and reports it, but snemu's page fault *halts the machine* instead of delivering a guest trap — so snemu never diverges onto the fault path, it just keeps heartbeating. so I implemented it: page faults become S-mode traps (scause 12/13/15, `stval` = the faulting VA), exactly like the ecall and timer traps. clean, unit-tested. I re-ran the oracle on `stack-guard`, expecting green.
  - **still FAIL.** same divergence. and the clue was there all along in the STOP column: those runs hit the *step limit* — snemu had never produced a fault at all. trap-vs-halt was moot; there was nothing to deliver. the real bug is one layer down: **snemu's page-table walk allows the guard-page access that QEMU rejects.** an MMU-fidelity issue, not a trap-delivery one. my whole diagnosis was a plausible story that the oracle refused to co-sign. (the change still shipped — it's *correct*, and it fixed a *different* bug, `init` halting on a genuine userspace fault — but it didn't earn the green I wanted.)

- both times the pattern was the same: I had a mechanism, a plausible cause, and a strong urge to believe it. the oracle's job was to make believing cheap to falsify. that's worth more than the 40 passes.

## the performance twist

- I'd internalized "snemu boots fast" as gospel — the whole pitch in post 1 was milliseconds vs QEMU's second of firmware. so I added timing (time-to-first-span, time-to-a-100-frame milestone) expecting to watch snemu lap QEMU. it lost. **snemu took 1.9s to first telemetry; QEMU took 0.11s.** seventeen times slower.
- the instrumentation had just corrected another belief. snemu's *startup* is near-zero (no process spawn, no OpenSBI, no DTB-gen) — that part was true. but the emulator I was timing was a **debug build**, and a debug interpreter is ~20× slower per instruction than release. the ~2M instructions of boot-before-first-frame dominated everything.
- `cargo run --release` — snemu's an in-process lib, so xtask's profile *is* snemu's — and first-span dropped to **0.08s**. now snemu is at *parity* with QEMU, even a hair ahead: its zero-startup advantage exactly offsets its interpreter being marginally slower than QEMU's JIT. "snemu is fast" was true — about startup, in release. two qualifiers I'd been quietly dropping.

## the clock is the point

- one detail underneath all of this: snemu's `rdtime` doesn't model the DTB's 10 MHz timebase. it returns the **instruction count**. the guest's clock and the guest's timer are driven by *instructions retired*, not wall-time. this is why snemu is deterministic — and it's the lever the whole exercise is really about.
- because the QEMU integration suite (97 scenarios, ~54s) isn't bound by boot or by the host's speed. it's bound by **real time**: scenarios wait for the heartbeat to tick, for samples to accumulate — and QEMU's timer fires at true 10 MHz, so "wait for the 5th heartbeat" costs five real seconds no matter how fast your machine is. snemu's instruction-clock produces those same five heartbeats in wall-*fractions*. it removes a floor QEMU literally cannot beat. (QEMU can decouple too, via `-icount` — but that forces single-threaded TCG, which serializes the harts and blinds it to exactly the cross-hart races the suite's `thread=multi` exists to catch. same trade-off snemu's round-robin already makes. there's no free lunch, only a choice of which lunch.)
- so the endgame comes into focus: **a test suite that runs on snemu** — boot once, snapshot-fork per workload, no real-time floor, no process spawn, no flakes — validated as a faithful substitute by the very oracle we built. the pieces are all here now. that's the next post.

## what I learned

- **build the thing that can tell you no.** the oracle's value wasn't the 40 greens; it was the two times it contradicted a fix I believed in. a validation tool you only consult when you expect to pass isn't a validation tool.
- **"it works" is a hypothesis, not a result** — especially when the evidence is *absence* (no crash, no diff). the fork "worked" because nothing screamed. the check that mattered was one designed to make it *fail* if it were broken.
- **a plausible cause is not a confirmed one.** page-fault-as-trap was a beautiful, correct, completely-wrong-for-this-bug fix. the difference between "explains the symptom" and "is the cause" is one re-run of the oracle.
- **profile before you brag.** I nearly wrote "snemu boots in milliseconds" into a post while it was taking two seconds. the debug/release gap was a 20× tax hiding in plain sight.

## what's next

- the guard-page thread the oracle handed me, gift-wrapped: *why doesn't snemu's `translate` fault where QEMU's does?* the suspects are narrow — the guard PTE's encoding, or the kernel's `remap`+shootdown that installs the guard not landing in snemu's walk. three FAILs, one root cause, a clear place to dig.
- and then the payoff this was all for: **the snemu-backed integration suite.** boot once, fork per workload, run the existing scenario assertions against snemu's frames instead of QEMU's — deterministic, floor-free, flake-free, and trustworthy precisely because a differential oracle spent this whole post proving snemu tells the truth. mostly. and telling me when I didn't.
