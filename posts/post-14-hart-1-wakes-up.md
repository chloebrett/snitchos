# Post 14 — Hart 1 wakes up

> v0.6 steps 3–10: a second CPU joins the system. Wire format, percpu plumbing, IPIs, SBI hart bring-up, TLB shootdown slots, per-hart runqueues. Six bugs caught by the integration suite along the way. The producer/consumer workload still runs on hart 0 — that migration is the next post — but everything below it is now genuinely multi-CPU.

## what we had

Post 13 dropped a producer/consumer histogram workload on a single hart, cooperatively scheduled. The chokepoint `Mutex<VecDeque>` exists but does nothing because there's no contention. The whole point was to set up the comparison: post 13 is the baseline, post 15 (after this one) is "I added a second hart and the chokepoint lit up."

But "add a second hart" turns out to be a lot of small, load-bearing pieces. None of them is heroic; together they're the entire SMP substrate. This post is about laying that substrate.

## the layers

In dependency order:

1. **Wire format additions.** `hart_id: u8` on `SpanStart` and `ContextSwitch`, new `HartRegister { id, mhartid, role }` variant, `PROTOCOL_VERSION` bumped 1→2. Done now while no external consumer of the format exists — postcard encodes fields positionally, so adding a field to an existing variant is a wire break.
2. **`tp` register convention + `PerHartData`.** RISC-V reserves `tp` for per-hart pointers. `PerHartData` is a `#[repr(C, align(64))]` struct with `hart_id`, `ipi_pending`, `shootdown_va`, `shootdown_ack`. One slot per hart in `PER_HART_DATA[MAX_HARTS]`. `percpu::init(hartid)` writes `tp = &PER_HART_DATA[hartid]`. `current_hartid()` reads `tp` and dereferences the slot. Cacheline alignment matters: under SMP, false sharing would have hart 0's writes invalidate hart 1's cache of an unrelated field.
3. **Per-CPU lift of scheduler globals.** `CURRENT_TASK`, `CURRENT_TASK_ENTRY_TICK`, `CURRENT_SPAN_CURSOR` go from plain atomics to `PerCpu<AtomicX>`. Call sites become `X.this_cpu().load(...)`. Single-hart observable behaviour unchanged; the surface is now correct for SMP.
4. **Weak-memory audit pass.** Documentation only. Every atomic site gets a one-line classification: counter, per-CPU, same-CPU IRQ handoff, init-once. Conclusion: zero existing atomics need their ordering changed today. The audit's value is the home doc in `kernel::percpu`'s docstring, where the three cross-hart `Release`/`Acquire` patterns that *will* arrive (IPI pending bits, shootdown ack, cross-hart wake) get spelled out so future me knows the contract.
5. **IPI primitive.** SBI shim (`sbi::send_ipi` via EID `0x735049 "sPI"`), `IpiMessage` bitflag constants (`IPI_WAKEUP`, `IPI_TLB_SHOOTDOWN`), `ipi::send(target, msg)` and `ipi::handle_pending()`. Sender does `fetch_or(msg, Release)` on `target.ipi_pending`; trap handler does `swap(0, Acquire)`. First place in the kernel that genuinely needs cross-hart Release/Acquire. Single-hart smoke: hart 0 sends itself a Wakeup IPI before init; the trap handler fires and bumps a counter.
6. **SBI HSM bring-up.** `sbi::hart_start(hartid, entry_pa, opaque)` via EID `0x48534D "HSM"`. `_secondary_start` (asm) loads SATP from a static, trampolines to higher-half, calls `secondary_main`. `secondary_main` sets `tp`, emits a `HartRegister { id: 1, role: Worker }` frame, sets `SECONDARY_READY = true`. Hart 0 spin-waits on `SECONDARY_READY` (Acquire) before calling `unmap_identity` — otherwise we'd tear down the identity gigapage that hart 1 is mid-trampoline through.
7. **TLB shootdown slots + handler + initiator.** `shootdown_va` and `shootdown_ack` per `PerHartData`. `mmu::shootdown(va)` does local `sfence.vma`, then for each other online hart: write `shootdown_va`, snapshot ack, fire `IPI_TLB_SHOOTDOWN` (Release), spin-wait ack to advance (Acquire). The receive-side handler runs `sfence.vma vaddr` and bumps ack (Release). The first cross-hart handshake the kernel performs.
8. **Per-hart runqueue + per-hart idle, plus `spawn_on`.** `Scheduler.runqueues: [Runqueue; MAX_HARTS]`. `yield_now()` pops from the *calling hart's* runqueue. `spawn(name, entry)` pushes to the current hart; `spawn_on(hart, name, entry)` pushes to a target and sends `IPI_WAKEUP` if the target is a different hart. `secondary_main` enrolls itself as `hart_1_main` and runs a yield-then-wfi loop. A small probe task gets `spawn_on`'d to hart 1 from `kmain` as a smoke test.

That's the whole v0.6 substrate. The producer/consumer workload sits untouched on hart 0 for the moment. Step 11 (next post) moves the consumer to hart 1 via `spawn_on(1, ...)` — that's where the chokepoint lit up.

## the bugs

A milestone with eight non-trivial pieces was always going to have war stories. We got six. The integration suite caught every one.

### the linker order bug

`entry.S` and `secondary.S` both used `.section .text.boot`. The linker placed *one* of them first at the entry point PA `0x80200000` — and the choice was **non-deterministic** between feature builds.

- Default kernel: `_start` at `0x80200000` ✓
- `--features heap-oom` kernel: `_secondary_start` at `0x80200000` ✗

In the broken build, OpenSBI jumped the boot hart into `_secondary_start`. That asm reads an uninitialized SATP slot, writes garbage into the CSR, then tries to trampoline. Kernel dies before printing a byte.

GDB from QEMU's `-s -S` debug stub took us from "no idea" to "single-line fix" in under five minutes. The whole investigation: `info address _start` → `info address _secondary_start` → see addresses swapped between builds. The fix: give `entry.S` a dedicated `.section .text.entry` section and put it first in the linker script.

Lesson: under Rust 2024 edition + cargo features, the symbol layout of feature-flagged builds can shift in surprising ways. Section names that aren't unique are a quiet correctness hazard.

### the asm stack-pointer bug

`_secondary_start` had `la sp, SECONDARY_STACK_TOP` — load the address of the `SECONDARY_STACK_TOP` static into `sp`. But `SECONDARY_STACK_TOP` is itself a `static u64` containing the stack-top *value* (which hart 0 writes before `sbi_hart_start`). The asm should have been `la t0, SECONDARY_STACK_TOP; ld sp, 0(t0)`.

Hart 1 ran on a "stack" that pointed at the 8-byte u64 itself. Pushes corrupted adjacent statics. It happened to work for the smoke test where hart 1 only does atomics and `wfi` — no real stack writes. The moment hart 1 needed to actually use the stack (yielding, calling `register_bare_task`), things got weird.

Found by trace inspection — the smp scenario passed, but downstream "spawn task on hart 1" tests failed structurally.

### the mhartid translation

`ipi::send(target_hart: usize, msg)` does `sbi::send_ipi(1u64 << target_hart, 0)`. But `target_hart` is the *logical* hart id (dense, `0..MAX_HARTS`), and SBI expects the *physical* `mhartid`. These match when OpenSBI picks `mhartid 0` as boot hart. Under `-smp 2`, OpenSBI sometimes picks `mhartid 1` as boot — at which point `logical 1` is `physical 0`, and `ipi::send(1, ...)` would target the running boot hart instead of the secondary.

Fix: `LOGICAL_TO_MHARTID: [AtomicU64; MAX_HARTS]` populated in `kmain` from the boot hart's mhartid. `ipi::send` translates before calling SBI.

The boot-hart-roulette caused fully 40% of suite runs to fail in seemingly-random ways before this was found. Standalone runs that happened to get mhartid=0 worked. Suite runs sometimes got mhartid=1, sometimes didn't.

### the CURRENT_TASK seeding

`register_bare_task("main", Running)` on hart 0 worked because main got `id = 0` and `CURRENT_TASK[0]` was already `AtomicU32::new(0)`. Pure coincidence. Hart 1's `register_bare_task("hart_1_main", ...)` gets a non-zero id; `CURRENT_TASK[1]` stays at 0. `current_task_id()` then returned task 0's id on hart 1 — every span hart 1 emitted was misattributed to main.

Fix: `register_bare_task` now explicitly seeds `CURRENT_TASK.this_cpu()` with the new id.

The fix is one line. Finding it required noticing that hart 1's emitted spans had `task_id = 0` instead of `task_id = 7`.

### the intern table overflow

`MAX_INTERNED = 64` was sized for v0.1. By v0.6 step 10 we have:

- ~15 boot/system strings (`kernel.boot`, `kernel.heartbeat`, init phases, ...)
- 4 v0.5 tasks × 3 strings each (name + 2 per-task metric names) = 12
- ~10 workload + SMP metric names
- 2 new tasks (`hart_1_main`, `hart_1_probe`) × 3 strings = 6
- Plus the metric names referenced via `tracing::register_counter_owned`

Total: ~50–60 strings. Right at the edge of the static array. The 64th `register_counter_owned` panicked silently — UART output went to QEMU stdout that the harness was discarding.

This was the most expensive bug to find because the failure mode was "QEMU exits cleanly without surfacing any of the kernel's last words." Took adding a per-scenario QEMU log capture + dumping on test failure before the panic message became visible.

Fix: `MAX_INTERNED = 128`.

### the integration suite as bug-finder

Six bugs, every one caught by `cargo xtask itest` failing in some configuration. Three of them are the kind of thing static analysis would never catch: the linker order one is build-system semantics, the mhartid one is platform-handoff semantics, and the intern-table one was only visible because the suite forced the kernel through the path that exhausted it.

The corollary: when the suite was *mostly green*, individual scenarios that flaked turned out to be load-bearing signal. Every "let me re-run that, it usually passes" was a deferred bug report.

## the harness ate the rest of the budget

A full third of this milestone's wallclock went to debugging the test harness itself.

Symptoms: the suite would pass 100% standalone (each scenario `--repeat 10` is green), but a sequential `--repeat 5` would flake 40% of the time, with different scenarios failing different runs. Multi-thread TCG fixed a class of timer-starvation flakes. Bigger budgets absorbed another class. There was a persistent residual.

The deepest red herring was "QEMU processes are accumulating across scenarios." My monitoring shell script was `ps aux | grep -c qemu-system-riscv64`, which **counted itself + the parent shell's argv** as matches. The "constant 3-4 QEMUs" turned out to be 0-1 QEMUs + 3 false positives. `pgrep qemu-system-riscv` (matches by command name, no argv search) gave the real count.

Things that came out of this debugging:

- `cargo xtask test` now runs all *host* unit tests (`kernel-core`, `protocol --features std`, `collector`) — fast, matches Cargo's normal mental model.
- `cargo xtask itest` runs the *integration* scenarios. Defaults to running unit tests first (skip with `--skip-unit-tests`).
- `cargo xtask itest --repeat N` runs the full suite N times back-to-back and emits an aggregate flake report ("scenarios that failed at least once").
- `cargo xtask itest --keep-existing-qemus` is the opt-out for the new default of `pkill -9 qemu-system-riscv64` at suite start — a stale QEMU from an interrupted `cargo xtask boot` would otherwise compete for host CPU.
- Per-scenario QEMU log files at `/tmp/snitch-itest-<label>-<pid>.log` capturing stdout (kernel UART under `-nographic`) and stderr (QEMU's own messages). Dumped inline on test failure: last 80 lines, prefixed with `|`.
- Per-test wallclock timing in the output line: `test name ... ok (max wait 1.6s of 30s budget)`. Surfaces over-sized budgets without anyone digging through logs.
- `Harness::Drop` now SIGKILLs QEMU and polls with `try_wait`, with a 5-second deadline and a hard panic if the corpse doesn't appear. Defensive: if a future bug ever causes QEMU to refuse SIGKILL, we want to know loudly rather than silently leak it into the next scenario.

Residual flake rate after all this: 1–2% per scenario, surfacing as ~8% suite-level under `--repeat 5`. Every visible failure now has the kernel's UART output captured. The kernel reaches `I am alive — entering heartbeat` in every failing case — so the flakes are *post-boot*, kernel-internal, and look like virtio-console wedges or heartbeat-loop hangs. That's a separate investigation; for this milestone's purposes, the suite is reliable enough to land step 11 against.

## what works as of now

```
cargo xtask test                    # ~1 second, all unit tests across the workspace
cargo xtask itest                   # ~50 seconds, full integration suite
cargo xtask itest --repeat 5        # ~5 minutes, flake hunting
cargo xtask itest smp-spawn-on-hart-1-runs  # one scenario only
```

The integration scenarios that exercise the new SMP plumbing:

- **`smp-secondary-hart-boots`** — `HartRegister { id: 1 }` arrives within 5 s. Proves SBI HSM + secondary entry asm + `tp` setup + trampoline.
- **`ipi-self-wakeup`** — Hart 0 sends itself an IPI early in boot; the trap handler bumps `snitchos.ipi.received_total`. Proves SSIE + trap routing + bit dispatch.
- **`smp-spawn-on-hart-1-runs`** — `kmain` calls `spawn_on(1, "hart_1_probe", probe_entry)`. The probe increments a counter on hart 1's runqueue. Proves per-hart runqueue + IPI wakeup + hart 1's yield_now + task execution end-to-end.
- **`smp-frames-carry-hart-1`** — Asserts at least one `ContextSwitch` frame carries `hart_id: 1`. Closes the wire-format loop end-to-end: kernel reads `current_hartid()` via `tp` → field encoded by postcard → field decoded by collector.

Dashboard side: three new panels under "System" in Grafana — `Harts online` (stat, today reads "2"), `Boot hart mhartid` (stat, reveals the OpenSBI roulette in real time), and `Secondary hart wfi rate` (timeseries; today near zero because hart 1 mostly idles, will jump in step 11).

## what's next

Step 11: `spawn_on(1, "workload_consumer", ...)`. The consumer task moves to hart 1; the producer stays on hart 0. The queue between them is the same `kernel::sync::Mutex<VecDeque>` from post 13 — now under genuine cross-hart contention.

Three things we'll watch:

1. **Lock-wait rate.** Single-hart it was zero. Cross-hart with `Mutex<VecDeque>` it will be visibly non-zero — the cacheline ping-pong cost made tangible.
2. **Throughput.** Sometimes more than 1×, sometimes (counter-intuitively) less than 1×. Lock contention can make adding a second hart *slower*. If that happens, it's the post.
3. **Queue depth.** Single-hart oscillates between 0 and one batch. Cross-hart should settle into a steadier shape because producer and consumer are now genuinely concurrent.

Step 12 replaces the `Mutex<VecDeque>` with `heapless::spsc::Queue`. The chokepoint's value gets demonstrated by contrast — the lock-wait graph falls off a cliff.

That's the v0.6 trilogy. Post 13 was the baseline. Post 14 (this one) was the infrastructure. Post 15 is the cost. Post 16 is the win.

## side work: a `learning/` sandbox

Parallel to the SMP plumbing, a deliberate detour: instead of writing more kernel code I half-understood, I built a place to *understand the primitives the kernel rests on* — from scratch, in isolation, test-first. It lives in `learning/`, a **separate cargo workspace** excluded from the root so it never drags `no_std`/riscv constraints into host-buildable toys (and never gets built by `cargo xtask`).

Three moving parts:

- **`concept-map.md`** — ten conceptual areas (privilege model, boot/linking, Sv39 MMU, frame allocator, heap, traps, concurrency, scheduling, DMA, observability) broken into ~60 sub-topics, each tied to a real file. I self-rated each 0–5 via a quiz. The honest result: strong on "what is it for" and concurrency *theory*, weak on *applied mechanics* — DMA scored a flat **0**, context-switch register saving **0**, per-CPU and heap-growth **1**. The gaps clustered exactly where I'd been pattern-matching instead of knowing.
- **`lesson-plan.md`** — eight lessons ordered by dependency, not by weakness. The MMU address-space model comes early because DMA, heap growth, and the trampoline are all corollaries of it.
- **`toy-*/` crates** — standalone, host-tested miniatures of one kernel concept each, shipped with a full test suite and the core algorithm left as a `todo!()` exercise. Each maps line-for-line to the real thing:
  - **`toy-allocator`** — three strategies. A free-list with splitting + coalescing (the `linked_list_allocator` heap model), a one-bit-per-frame bitmap (`kernel-core/src/frame.rs`), and — because it's worth knowing what the *other* design looks like — a power-of-two **buddy** allocator (what Linux uses for physical pages; the O(1) XOR-buddy coalescing trick, in deliberate contrast to SnitchOS's bitmap choice).
  - **`toy-pagetable`** — the Sv39 walk: VA split, `translate` (the *read* walk the kernel never writes — it leans on hardware), and `map_4kib` (the write walk). The thing that finally clicked: **a huge page is just stopping the walk early, so the index bits you didn't consume fold into the page offset.**
  - **`toy-virtqueue`** — the descriptor ring, modelling physical memory as a `Vec<u8>` the fake device indexes *directly*. That direct index is the whole lesson: a device has no MMU, so the descriptor must carry a **physical** address. It even reconstructs the real `TX_STAGING` hazard as a stretch goal.

Building these the hard way paid off immediately. The free-list alone surfaced two genuine bugs — a `usize` underflow when freeing at the front of a multi-block list, and a merge-both **double-count** that the unit test literally named `freeing_a_hole_coalesces_both_neighbours` *failed to exercise* (its geometry routed through the tail branch instead). That second one is the lesson: **green ≠ correct.** The fix was to graduate from hand-picked cases to a `proptest` property test (`free + live == capacity`, "no two free blocks adjacent"), which found the bug class from a random sequence and *shrank* it to a four-op reproducer — `Alloc(1), Alloc(1), Free(6), Free(0)` — that I checked in as a regression seed. The same conservation invariant is what the kernel's `heap.bytes_used + bytes_free == capacity` gauge asserts at runtime; the toy is the unit-test version of the live health check.

(There's a noted follow-up out of this: `kernel-core/src/frame.rs` only ever asserts `count_free()` against literals, never against a popcount of the bits — so a drift of the maintained `frames_free` counter would go unnoticed. A `proptest` for the real frame allocator is queued.)

## clippy across a split-target workspace

The mixed workspace has a sharp edge I hadn't hit until I tried to lint everything: **`cargo clippy --workspace` fails.** The kernel only builds for `riscv64gc-unknown-none-elf`, and a plain workspace clippy compiles it for the *host*, where it can't link — duplicate `panic_impl` lang item, unknown `a7`/`a6` registers, the works.

The clean fix would be Cargo's `forced-target`, which pins a single package to one target. But that's `per-package-target`, **nightly-only** — and worse, putting it in `Cargo.toml` makes the manifest fail to load on stable *entirely*, committing the whole project (and CI, and every contributor) to nightly. For a `--target` papercut, on a kernel that's deliberately stable (riscv64 is a tier-2 target; nothing here needs nightly), that trade is lopsided. So instead: a **`cargo xtask clippy`** subcommand that lints host crates for the host and the kernel for riscv in one go, with args forwarded to both. Documented in CLAUDE.md so nobody reaches for the broken `--workspace` form again.

Then the cleanup itself — ~45 lints across the workspace — turned into its own small war story, fitting for this post:

- **`cargo clippy --fix` broke the kernel.** Its `deref_addrof` autofix rewrote `&mut *(&raw mut STATIC)` — the *required* edition-2024 idiom for taking a reference to a mutable static — into a forbidden direct `&mut STATIC`, turning `frame.rs` and `mmu.rs` into ~15 compile errors. Reverted via `git checkout` (both files were clean), and the sites now carry a justified `#[allow(clippy::deref_addrof, reason = …)]`. Lesson, rhyming with the linker-order one above: never blanket-`--fix` code that does raw-pointer-to-static work.
- **`vec_box` wanted to delete a load-bearing `Box`.** It flagged `Scheduler.tasks: Vec<Box<Task>>` as redundant indirection — but the `Box` is exactly what gives each task a *stable heap address* so the raw `*mut TaskContext` pointers survive the mutex drop (post 12's whole correctness argument). Clippy's "fix" would have been a real bug. `#[allow(clippy::vec_box, reason = …)]`.
- **The dead-code warnings were this milestone's own scaffolding.** `mmu::shootdown`, `percpu::this_cpu_mut`, `Task.state`, `Scheduler::tasks()` — the SMP pieces wired up in *this* post but not yet *called* — got `#[expect(dead_code, reason = …)]`, which self-resolves the moment a caller arrives.

Net: `cargo xtask clippy` is zero-warning across the whole workspace, and the kernel's intentional false-positives are documented rather than suppressed blind.

---

*[TBD: screenshots — the new system-dashboard panels showing 2 harts online; Tempo trace view of hart_1_probe ticking; the lone red flake from the suite's last `--repeat 5` showing kernel UART output dumped under FAILED]*
