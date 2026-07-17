# SnitchOS

The operating system that snitches on itself 🐀

SnitchOS is an experimental operating system built on the logical extrapolation of two ideas. In practice, this gives it some curious - and at times novel - properties.

## Idea 1: 💋 Elegance > compatibility: design from first principles.

- 🐀 **Everything the kernel and userspace does gets "snitched"**: traced, logged and visible in Grafana. Distributed-systems-tier observability on a toy microkernel - super helpful for debugging, and a level of monitoring no other OS can give you.
- 🔐 **Strictly no Linux-style ambient authority** - capabilities as a core concept and threaded through every part of the OS. The confused deputy problem is structurally impossible; the price is no POSIX compatibility.
- 📁 The **filesystem lives in userspace**, and owns its own capabilities. The kernel owns the generic capability mechanism, but has no idea what a file is.
- ⌨️ Forget Bash: the **shell is designed from the ground up** with capabilities and typed data in mind. No untyped Linux byte-pipes: `|>` gives fast in-process spawns mediated by the shell; `~>` spawns cross-process with kernel-mediated authority - basically the actor model at OS-level.

## Idea 2: 🧬 Don't be afraid to reinvent a better wheel.

- 🔋 **Snemu**: I got sick of QEMU taking 1 second to boot for each of all 100+ integration tests - so I built my own RISC-V emulator. Snemu runs the whole suite in <5 seconds, has its own JIT, is fully deterministic, and supports a unique tree-based snapshot system. It's also the path to running the OS inside a web browser with all the same support as the real one.
- 🪡 **Stitch**: Bash is an unspecified mess that assumes the ambient-authority untyped-data world of Linux - let's not go there. I built Stitch instead: a language first and foremost, taking inspiration from Rust, Kotlin, Haskell and some others (e.g. Gleam for `use <-` syntax). Then a shell on top of it, with `|>` for in-process pipes and `~>` for cross-process: typed data, capabilities mediated by the kernel.
- 🫨 **Stim**: a Vim clone built in Stitch, with full support for capabilities. Read-only mode is kernel-enforced; editing a file grants only the rights to that file. Every mode switch is a span; every command is traced.

<!-- TODO: add visual examples for each of the above! Animated is best. -->

Here's an example of a Grafana dashboard showing the heap pressure:

![SnitchOS in Grafana 2](posts/tracing-30min-2.png)

## What's been built

- Boot in QEMU, boot fully traced to Grafana.
- The usual kernel scaffolding: Interrupts / trap handling, Sv39 paging, clock, priority-based scheduler.
- Multi-hart / SMP (cooperative inside the kernel, preemptive in user-space).
- Full suite of integration tests, heavily optimized, with its own emulator.
- Capabilities, and IPC with capabilities as a first class concept.
- RAMfs filesystem that lives in userspace and hands out capabilities.
- Console input / output.
- Process lifecycle: spawn, wait, reap.
- Notification system.

## What's next

TODO

## Detailed status log

**v0.1 "Hello, traced world"** — _complete_. Kernel boots on RISC-V in QEMU, emits a structured boot-phase span tree over a dedicated virtio-console channel, host-side collector decodes and prints.

**v0.2 "Grafana arrives"** — _complete_. Tempo + Prometheus + Grafana stack via docker-compose; collector exports OTLP traces + serves Prometheus `/metrics`; provisioned dashboard shows live kernel telemetry.

**v0.3 "Interrupts & clock"** — _complete_. Full S-mode trap handling (entry/exit asm + Rust dispatcher); SSTC-based timer interrupts; heartbeat is timer-driven (`wfi` between ticks) instead of busy-spin. First histogram metric (`snitchos.irq.timer.duration_ticks`) end-to-end through the collector's bucket accumulation into Grafana.

**v0.3.1 "Making the kernel testable"** — _complete_. Carved out `kernel-core` (host-buildable `no_std` library) holding the intern table, span registry, pre-init buffer, scause decoding, and the `FrameSink`/`Clock` traits. 29 host unit tests over the data logic. New `xtask test` harness boots the kernel in QEMU, decodes the virtio-console telemetry stream, asserts on the `Frame` sequence — 3 scenarios passing in ~5s wallclock. See [posts/post-8-making-the-kernel-testable.md](posts/post-8-making-the-kernel-testable.md).

**v0.4 "Memory"** — _complete_. Five steps:

- **Steps 1–3.** Sv39 paging on, dual-mapped boot table, kernel relinked at higher-half VAs with PC trampoline + identity unmap, `va_to_pa` at every device-DMA site so virtio still works. 1 GiB Sv39 huge-page leaf installs a linear map of all physical RAM at `0xffffffd0_00000000+` so any allocated frame has a kernel-reachable VA via `pa_to_kernel_va`. Bitmap-based frame allocator in `kernel_core::frame` with O(1) free-count + short-circuit; kernel-side `frame::{alloc, alloc_zeroed, free, stats}` API; DTB-driven init reserving SBI / kernel image / DTB regions. Five `snitchos.frames.*` metrics drive five Grafana panels including an OOM curve under the `oom-leak` feature.
- **Step 4 (kernel heap).** `#[global_allocator]` backed by `linked_list_allocator` (locally forked for `Heap::free_block_stats` so fragmentation is observable). Initial 4 MiB heap; `Box` / `Vec` / `String` / `BTreeMap` work inside the kernel. Telemetry: `snitchos.heap.{alloc_total, dealloc_total, alloc_failed_total, bytes_capacity, bytes_used, bytes_free, grow_total, grow_failed_total, free_blocks, largest_free_block_bytes}`.
- **Step 5 (runtime page-table mutation + growable heap).** `kernel::mmu::map(va, pa, perms)` walks Sv39 from `BOOT_PT_ROOT` to the leaf, allocating intermediate tables via the frame allocator on demand, `sfence.vma`'ing on success. Walk logic in `kernel_core::mmu::map` is pure and host-tested via a `PtMem` mock (11 tests). P2 migrates the heap to a dedicated 1 GiB VA window at `HEAP_VA_BASE = 0xffffffc0_00000000`; `heap::extend` grows under a heartbeat-driven watermark policy (extracted to `kernel_core::heap::watermark_grow_decision`, 6 host tests).

See [posts/post-9-moving-the-kernel-without-breaking-it.md](posts/post-9-moving-the-kernel-without-breaking-it.md), [posts/post-10-frame-by-frame.md](posts/post-10-frame-by-frame.md), and [posts/post-11-boxes.md](posts/post-11-boxes.md).

**v0.5 "Threading & round-robin scheduler"** — _complete_. Cooperative round-robin scheduler over a single CPU; four kernel threads at boot (main, idle, task_a, task_b). `Task` struct + `Scheduler` + asm context switch in `kernel::sched`; pure-data shape (Runqueue, TaskState) in `kernel_core::sched`. New wire frames: `ThreadRegister` and `ContextSwitch{reason}`; `SpanStart` gains `task_id`. Per-task `SpanCursor` swapped on context switch so spans can survive yields without parent-chain corruption. Per-task `cpu_time_ticks` + `runs_total` metrics on the wire; Grafana thread-timeline + active-threads + switches/sec + yield-overhead percentiles. SMP-prep pre-factor before threading: `kernel::sync::{Mutex, Once}` chokepoint + `kernel::percpu::PerCpu<T>` stub + clippy `disallowed_types` lint blocks raw `spin::Mutex` outside `kernel::sync`. See [posts/post-12-the-kernel-takes-turns.md](posts/post-12-the-kernel-takes-turns.md).

**v0.6 "Cooperative SMP"** — _complete_. Three-post arc landing at a producer/consumer workload migrated across two harts.

- **Step 1 (cooperative single-hart baseline)** — _complete_. Producer/consumer histogram workload: `kernel::sync::Mutex<VecDeque<u64>>` queue, `[AtomicU64; 64]` histogram, pure-logic `Lcg` / `bin_of` / `bin_sample` in `kernel_core::workload` (8 host tests). Five metrics drive the new "Workload" Grafana section. See [posts/post-13-producer-consumer-baseline.md](posts/post-13-producer-consumer-baseline.md).
- **Steps 3–10 (SMP infrastructure)** — _complete_. Hart 1 boots. Wire format adds `hart_id: u8` to `SpanStart` + `ContextSwitch` and a new `HartRegister { id, mhartid, role }` variant (`PROTOCOL_VERSION` 1→2). `tp` register convention + `PerHartData[MAX_HARTS]` with cacheline-aligned slots; `CURRENT_TASK` / `CURRENT_TASK_ENTRY_TICK` / `CURRENT_SPAN_CURSOR` lifted into `PerCpu<T>`. IPI primitive over SBI `sPI` extension; `IpiMessage` bitflags (`Wakeup`, `TlbShootdown`); receive-side trap-dispatcher; first `Release`/`Acquire` pair the kernel uses. SBI HSM bring-up (`sbi::hart_start`); `_secondary_start` asm sets up SP + SATP + trampoline; `secondary_main` enrolls as `hart_1_main` and runs an idle yield/wfi loop. TLB shootdown slots + receive-side handler + initiator (`mmu::shootdown(va)`); per-(hart) `shootdown_va` / `shootdown_ack`; second cross-hart handshake. Per-hart runqueue + `spawn_on(hart, name, entry)` + cross-hart `IPI_WAKEUP`. Weak-memory audit documented every kernel atomic. SMP visibility on the system dashboard (`Harts online`, `Boot hart mhartid`, `Secondary hart wfi rate`). Six bugs caught by the integration suite along the way (linker section collision, mhartid-vs-logical-id translation, `CURRENT_TASK` seeding, intern-table overflow, asm stack-pointer dereference, virtio TX staging requirement). See [posts/post-14-hart-1-wakes-up.md](posts/post-14-hart-1-wakes-up.md).
- **Step 11 (workload consumer migrates to hart 1)** — _complete_. `workload=smp` runs the producer on hart 0 and the consumer on hart 1 over the shared `Mutex<VecDeque>` queue; the cross-hart correctness oracle (`histogram_sum >= samples_consumed` across the boundary) is hardened with `Release`/`Acquire` guards. The chokepoint earns its keep.
- **Step 12 (`Mutex<VecDeque>` → `heapless::spsc::Queue`)** — _complete_. The lock-free SPSC queue retires the chokepoint; the counter-intuitive result ("lock-free made it slower" at low contention) is [post 19](posts/post-19-lock-free-made-it-slower.md).
- **Steps 13–14 (integration suite + closeout)** — _complete_. SMP scenarios: `smp-producer-consumer-correctness`, `smp-spans-carry-hart-id`, `smp-tlb-shootdown-visible` (added `mmu::remap` + a counterfactual-verified stale-TLB oracle), plus the ping-pong alternation oracle (which surfaced a real lost-wakeup, documented in `plans/scaling-corners.md`). Collector cashes the wire's `hart_id` into a `host.cpu_id` OTLP span attribute so Tempo can slice traces by CPU. See [post 21](posts/post-21-make-it-fail-first.md).

**v0.7 "Userspace & capabilities"** — _complete_. Two steps. **v0.7a** drops the first userspace process to U-mode (the `user/` crates: a `runtime` with crt0 + syscall bindings + a `talc` heap, a `std` facade, an `#[entry]` macro, demo programs) behind one deliberately-_ambient_ syscall — built the "Unix way" so the rewrite could feel the pain. **v0.7b** replaces ambient authority with **capabilities**: `kernel_core::cap` defines a `Capability { object, rights }` named by an opaque `Handle` and validated against the _calling_ process's own `CapTable` — no ambient authority, every invocation checked. Per-process Sv39 page tables + the `U`-bit isolation firewall; `copy_from_user` validates a user range under a transient `SUM`; every refusal snitches a `SyscallRefused` frame. See [docs/capability-system-design.md](docs/capability-system-design.md).

**v0.8 "Preemption & priorities"** — _complete_. Timer-driven preemption of **userspace** (the `SPP == User` gate; kernel code stays cooperative), layered on the v0.5 cooperative switch — the preempted task's full `TrapFrame` parks on its kernel stack while the scheduler swaps only the callee-saved registers. `ContextSwitch{Preempt}` + `snitchos.sched.preemptions_total`. **v0.8b** adds static priorities (Low/Normal/High) with **aging** anti-starvation: `pick_next` takes the highest _effective_ priority, ties broken by longest wait — "ordered, but fair."

**v0.9 "IPC over capabilities"** — _complete_. Synchronous **endpoints** (a pure rendezvous state machine in `kernel-core`): `send`/`receive` of an inline `[u64; 4]` message, blocking until a peer arrives. **v0.9b** `call`/`reply` RPC over a one-shot, kernel-minted **reply capability** (possession is authority; consumed on use). **v0.9c** **badged endpoints**: a `MINT`-holder derives badged `SEND` caps (`MintBadged`) and the kernel delivers the _unforgeable_ badge to the receiver, so one endpoint demuxes many clients by capability — plus cap-transfer-in-reply. Trace context crosses the boundary (the sender's span parents the receiver's); new `Message` + `CapEvent` wire frames. See [docs/ipc-design.md](docs/ipc-design.md).

**v0.10 "RAMfs"** — _complete_ (cap-agnostic core + protocol; front-end wired). The deliverable is the **`Filesystem` trait** (`fs-core` — inode-addressed, host-testable, imports no cap/IPC types) with a flat in-memory impl (`ramfs`) behind it, and `fs-proto` (the `Badge` packing `(inode, file_rights)`, opcodes, wire `Request`/`Response`). A **File cap is a badged endpoint cap**: the FS owns `MINT` and attenuates on `lookup`. Bulk bytes cross via a kernel **cross-address-space copy** (`CopyFromCaller`/`CopyToCaller` — message-passing, not shared memory). The `user/fs` front-end (`fs-server`/`fs-client`) does the badge→inode demux above the cap-agnostic trait. See [docs/filesystem-design.md](docs/filesystem-design.md).

**v0.11 "Console input & spawn"** — _complete_. **Tier-0 polled console input**: a host-tested `ConsoleRing`, drained from the UART in the timer handler (hart 0) into a `ConsoleRead` syscall and a userspace `console_read` — a typed key round-trips host → kernel → userspace (`ConsoleWrite` mirrors it). **Spawn-with-caps**: a `Spawn` syscall that creates a userspace process holding **exactly** the capabilities the parent delegates (`spawn(program, caps)`), with `cap::delegate` the host-tested, all-or-nothing core. The road to an **explicit-authority shell** where every grant is an observable `CapEvent`. See [plans/legacy/spawn-shell-and-console.md](plans/legacy/spawn-shell-and-console.md).

Working:

- no_std kernel; handwritten boot stub + linker script; ns16550a UART driver
- DTB parse (memory, UART, timebase)
- virtio-console driver: discovery + modern-spec handshake + virtqueue + TX
- S-mode trap handler: register save/restore asm, Rust dispatcher with typed `scause` decoding, `stvec` install at boot
- SSTC timer: arm via `stimecmp` CSR; per-source + global interrupt enable; deferred-work pattern (IRQ stays tiny, main thread does heartbeat)
- `Clock` trait + `SstcClock` impl (abstraction surface for future SBI / non-RISC-V impls)
- `protocol` crate: postcard-encoded `Frame` enum (`Hello`, `SpanStart/End`, `Event`, `Metric`, `MetricRegister`, `StringRegister`, `Dropped`) with `MetricKind` (`Counter`/`Gauge`/`Histogram`), hosted TDD
- `tracing` module: timestamps from the `time` CSR, string intern table with metric-type registration, RAII-guarded spans via the `span!` macro, pre-init buffering with a `Dropped { count }` checkpoint after flush
- `kernel-core` library (host-buildable `no_std`): intern table, span registry, pre-init buffer, scause decoder, `FrameSink` + `Clock` traits — 29 host unit tests cover the data logic
- kernel-side metric helpers: `register_counter` / `register_gauge` / `register_histogram` / `emit_metric`
- `kernel.boot` opens at boot with `console_init` + `telemetry_init` sub-spans; `kernel.heartbeat` span + metric set emitted once per timer tick
- `collector` (host-side): decodes the wire stream, reassembles spans, exports OTLP/HTTP to Tempo, serves Prometheus text on `/metrics` with full counter/gauge/histogram bucketing
- docker-compose stack: Tempo + Prometheus + Grafana, all auto-provisioned (datasources + dashboard with timer-IRQ percentile panel)
- `xtask` orchestration: `cargo xtask boot` (kernel) / `cargo xtask collect` (collector) / `cargo xtask stack {up,down,logs}` / `cargo xtask test` (kernel integration scenarios in QEMU)
- Sv39 page tables, higher-half kernel, identity unmap, linear map at `0xffffffd0_00000000` so all physical RAM is reachable via `pa_to_kernel_va`
- physical frame allocator (4 KiB pages, bitmap-tracked) with `frame::{alloc, alloc_contiguous, alloc_zeroed, free, stats}`; DTB-driven init; per-frame metrics on the wire and in Grafana
- kernel heap: `#[global_allocator]` backed by `linked_list_allocator` over a dedicated 1 GiB VA window at `HEAP_VA_BASE` (root PTE 256). `heap::init` installs the first 4 MiB by calling `mmu::map` per page over scattered frames; `heap::extend` grows under a heartbeat-driven watermark policy (extracted to `kernel_core::heap::watermark_grow_decision`, host-tested). `Box` / `Vec` / `String` / `BTreeMap` work; seven heap metrics on the wire including grow counters
- runtime page-table mutation: `kernel::mmu::map(va, pa, perms)` walks Sv39 from `BOOT_PT_ROOT` to the leaf, allocating intermediate tables via the frame allocator on demand, `sfence.vma`'ing on success. Walk logic is `kernel_core::mmu::map`, pure and host-tested via a `PtMem` mock (11 tests). Heap grow is the first runtime consumer; v0.7 per-process page tables will be the second. SMP adds `kernel::mmu::remap`, which fires a cross-hart TLB shootdown after overwriting a leaf
- SMP-shaped sync primitives: every kernel lock goes through `kernel::sync::{Mutex, Once}`, a single chokepoint with no-op preempt/IRQ hooks today. `kernel::percpu::PerCpu<T>` + `current_hartid()` stub the per-CPU access pattern. Workspace `disallowed_types` clippy lint blocks raw `spin::Mutex` outside `kernel::sync`. Sets v0.5 threading up so preempt-disable + SMP IRQ-disable land in one file
- cooperative round-robin kernel-thread scheduler: `Task` struct + `Scheduler` + asm context switch (`kernel::sched::switch`); 4 threads at boot (main, idle, task_a, task_b); `spawn(name, entry)` + `yield_now()` API; cumulative `context_switches_total`, per-task `cpu_time_ticks` + `runs_total` metrics; per-task `SpanCursor` swapped on context switch so spans can survive yields. Wire format additions: `ThreadRegister`, `ContextSwitch{reason}`, `task_id` on `SpanStart`. Collector populates OTLP `thread.id`, `thread.name`, and (v0.6) `host.cpu_id` attributes per span — Tempo trace view shows scheduler decisions inline and traces can be sliced by the hart they ran on.

The **explicit-authority shell has landed** — everything it depended on (`Exit`/`Wait`, `init`, the shell itself) shipped across v0.12 and v0.13. `workload=shell` runs `view <path>`: the shell looks the file up on the FS with READ-only rights, spawns a separate `viewer` holding **only** that attenuated cap, and revokes it when the viewer exits. The whole delegate → use → revoke cycle is `CapEvent`s in the trace, pinned by the `viewer-reads-delegated-file` and `shell-view-command-revokes-cap` itests.

See [posts/](posts/) for the per-milestone devlog.

## Quick start

Three terminals:

```
# Once per session: bring up the observability stack.
cargo xtask stack up
# (Grafana → http://localhost:3000 — anonymous admin)

# Terminal A — kernel + QEMU. Blocks at the telemetry chardev until
# the collector connects in terminal B.
cargo xtask boot

# Terminal B — collector. Decodes frames, posts OTLP to Tempo,
# serves Prometheus /metrics on :9091.
cargo xtask collect
```

Then open Grafana → Dashboards → SnitchOS → SnitchOS Overview.

Quit QEMU with `Ctrl-A x`. `cargo xtask stack down` shuts the stack.

For ad-hoc debug without the stack:

```
cargo xtask reader              # text-only frame dump, no docker
cargo xtask reader -- --pretty  # multi-line debug format
```

## Subcommands

```
cargo xtask build              # build the kernel ELF
cargo xtask boot                 # build kernel + run in QEMU
cargo xtask snemu boot         # build full kernel + run under the snemu emulator (meta-loop driver)
cargo xtask snemu boot --max-steps 3000000  # cap the run at N instruction steps
cargo xtask snemu boot --frames  # also decode + dump the telemetry frames snemu captures off the virtio-console
cargo xtask snemu boot --workload demo  # boot a runtime workload under snemu (injects the DTB bootarg)
cargo xtask snemu diff          # differential oracle: diff snemu vs QEMU telemetry on the default boot
cargo xtask snemu diff --workload demo  # ...on a specific workload (both emulators run it)
cargo xtask snemu diff --all    # sweep every workload; print an agree/disagree summary table
cargo xtask collect            # build + run collector (OTLP + Prometheus)
cargo xtask collect -- --text  # also print decoded frames to stdout
cargo xtask reader             # collector in text-only mode (no docker needed)
cargo xtask stack up           # docker-compose up the stack
cargo xtask stack down         # docker-compose down
cargo xtask stack logs         # tail container logs
cargo xtask test               # all host-side checks: unit tests + loom model-checks + diagram drift + doc links
cargo xtask links              # just the doc-link check (instant; run it after any `git mv` of a doc)
cargo xtask itest              # run kernel integration tests in QEMU (integration only)
cargo xtask test && cargo xtask itest   # the gate: host checks, then integration
cargo xtask itest <scenario>   # run one scenario by name
cargo xtask itest --tag userspace  # run all scenarios carrying a tag
cargo xtask itest --shared     # group scenarios by workload; one kernel boot per group (faster)
cargo xtask itest --repeat N   # run the suite N times back-to-back; aggregate flake report
cargo xtask itest-show         # print a failed capture's frame transcript (.itest-runs/); --scenario/--tail/--grep
cargo xtask baseline show      # inspect the flake baseline (also: promote/discard/recover/adopt/prune/export/push)
cargo xtask mutants            # mutation-test the host crates (cargo-mutants)
cargo xtask clippy             # clippy the WHOLE workspace correctly (kernel for riscv, host crates for host)
cargo xtask measure <workload> # benchmark a runtime workload (timed sampling)
cargo xtask audit <crate>      # crate-audit evidence (per-pub-symbol callers, dead-code candidates)
cargo xtask debug              # build kernel + run QEMU paused with GDB stub on :1234
cargo xtask loc                # lines of code by crate + production/test split
cargo xtask --help
```

> **snemu runs optimized by default.** xtask runs the snemu interpreter
> in-process, and `[profile.dev.package.snemu] opt-level = 3` (root `Cargo.toml`)
> keeps just that crate optimized even under the default dev build — so
> `snemu boot` / `snemu diff` / `snemu fork` are ~20x faster without recompiling
> the rest of xtask in release. snemu is deterministic (instruction-count clock),
> so this changes only speed, never the guest's execution or telemetry. To debug
> snemu itself, set that `opt-level = 0` temporarily.

## Tests

Two layers, two commands.

**`cargo xtask test`** runs every host-side check in ~1 second:

- `kernel-core` — intern table, span registry, pre-init buffer, scause decoding, MMU walk logic, frame bitmap, heap watermark policy, scheduler runqueue, workload pure logic (LCG / bin_of / bin_sample).
- `protocol --features std` — wire-format roundtrip tests + OwnedFrame conversion.
- `collector` — span state machine, OTLP/Prometheus encoding, histogram bucketing.
- the **loom model-check** tests — a separate `--cfg loom` compilation, so a plain `cargo test` compiles them to nothing.
- the **generated-diagram drift check** — `docs/generated/` are contract artifacts; a stale one fails here.

**`cargo xtask itest`** runs kernel integration scenarios in QEMU —
**integration only**. It does not run the host-side checks first; compose
the gate explicitly with `cargo xtask test && cargo xtask itest`. Each scenario spawns its own
QEMU and asserts on the decoded wire stream. Requires
`qemu-system-riscv64` on `PATH` (skips cleanly if missing). Stale
QEMU processes from prior `cargo xtask boot` or debug sessions are
killed at suite start by default (`--keep-existing-qemus` to disable).

The suite builds **one** `itest-workloads` kernel and selects per-scenario
via the `workload=<name>` bootarg (no per-scenario rebuilds; see
[docs/runtime-workload-selection-design.md](docs/runtime-workload-selection-design.md)).
~71 scenarios across these families:

- **Boot + telemetry**: `boot-reaches-heartbeat`, `heartbeat-cadence`, `pre-init-order`, `kernel-runs-at-higher-half`.
- **Frame allocator / heap**: `frame-allocator-metrics`, `frame-allocator-oom`, `kernel-heap-metrics`, `heap-oom`, `heap-grows-on-demand`.
- **Scheduler (v0.5)**: context-switch smoke, spawn/register, yield round-trips, spans carry task id, span survives yield, clean exit.
- **SMP + workload (v0.6)**: cooperative baseline, cross-hart producer/consumer correctness (`histogram_sum >= samples_consumed` across the boundary), secondary-hart boot, IPI wakeup, TLB shootdown visibility, ping-pong cadence.
- **Userspace + capabilities (v0.7)**: runs to U-mode, the `U`-bit fault firewall, bad-user-pointer refusal, span-quota refusal, two cooperative workers.
- **Preemption + priorities (v0.8)**: runaway-user-task preempted, preemption telemetry, `syscall-hog-still-preempted`, `priorities-ordered-but-fair`.
- **IPC (v0.9)**: message + trace cross the endpoint, prompt wakeup, RPC round-trips/nesting/`reply_recv`, badge mint+refuse, badge handout + per-client demux.
- **Filesystem (v0.10)**: connect-mints-root, stat/create/write-read, lookup rights-gate, remove, readdir, cross-process span nesting.
- **Console + spawn (v0.11)**: `console-echo-round-trips` (injected UART byte echoes back), `spawn-delegates-to-child` (parent delegates a cap; child uses it).
- **Stress storms**: `spawn-storm`, `ipi-pong`, `shootdown-storm`, `mutex-storm`, `virtio-storm` — cross-hart regression guards.

Tags for `--tag`: `boot`, `frame`, `heap`, `oom`, `sched`, `smp`, `ipi`, `workload`, `userspace`, `ipc`, `stress` (set per-row in the `catalog!` table in `xtask/src/itest.rs`).

Useful flags:

- `--repeat N` — run the whole suite N times back-to-back, then print an aggregate flake table listing scenarios that failed at least once.
- `--tag <tag>` — run every scenario carrying `<tag>` (union). Repeatable / comma-separated: `--tag frame --tag heap` or `--tag frame,heap` runs scenarios tagged either — same comma-means-also convention as the positional scenario list. An unknown tag errors with the known set; can't be combined with a named scenario. Tags are set per-row in the `catalog!` table in `xtask/src/itest.rs` (`boot`, `frame`, `heap`, `oom`, `sched`, `smp`, `ipi`, `workload`, `stress`, `userspace`).
- `--shared` — shared-boot mode: group scenarios by their `workload` and run each group against a _single_ kernel boot instead of one boot per scenario (the ~19 default-demo and the userspace scenarios each boot QEMU once). Each scenario reads the same recorded frame stream through its own cursor. Off by default — the flake gate (`--repeat 10`) and baselines want the per-scenario isolation of separate boots. Composes with `--tag`/`--skip`. Cuts total QEMU boots (CPU time ~40% on a full run); see [plans/legacy/itest-shared-boot-mode.md](plans/legacy/itest-shared-boot-mode.md).
- `--keep-existing-qemus` — don't `pkill` stale QEMUs at start (rare; useful if you want a concurrent debug QEMU).

On each test line, the runner prints `(max wait Xs of Ys budget)` so
over-sized budgets are visible at a glance. On failure, the last 80
lines of the scenario's QEMU log (kernel UART + QEMU stderr) are
dumped inline — captures panic messages without anyone re-running
under a debugger.

Runtime is dominated by per-scenario QEMU boots (the suite builds
**one** `itest-workloads` kernel up front and selects workloads at
runtime, so there are no per-scenario rebuilds). `--shared` groups
scenarios by workload to cut total boots substantially.

## Reading

- [docs/README.md](docs/README.md) — design overview (the three pillars: observability, capabilities, microkernel).
- [docs/capability-system-design.md](docs/capability-system-design.md) — the v0.7b capability model (handles, CapTable, rights, no ambient authority).
- [docs/ipc-design.md](docs/ipc-design.md) — v0.9 endpoints, reply caps, badges.
- [docs/filesystem-design.md](docs/filesystem-design.md) — v0.10 RAMfs: the `Filesystem` trait, File caps as badged endpoints, option-D copy.
- [plans/legacy/spawn-shell-and-console.md](plans/legacy/spawn-shell-and-console.md) — the v0.11 explicit-authority shell: component inventory + critical path.
- [docs/v0.1-hello-traced-world.md](docs/v0.1-hello-traced-world.md) — v0.1 milestone plan.
- [plans/legacy/v0.2-grafana.md](plans/legacy/v0.2-grafana.md) — v0.2 implementation plan.
- [plans/legacy/virtio-console.md](plans/legacy/virtio-console.md) — virtio-console implementation plan.
- [plans/legacy/v0.3-interrupts.md](plans/legacy/v0.3-interrupts.md) — v0.3 implementation plan.
- [plans/legacy/kernel-core-carveout.md](plans/legacy/kernel-core-carveout.md) — the host-testability extraction plan + as-built notes.
- [plans/legacy/kernel-integration-tests.md](plans/legacy/kernel-integration-tests.md) — the QEMU-driven scenario harness.
- [plans/legacy/v0.4-memory-concepts.md](plans/legacy/v0.4-memory-concepts.md) — Sv39, higher-half, frame allocator concepts before code.
- [plans/legacy/v0.4-memory-step-1-satp-on.md](plans/legacy/v0.4-memory-step-1-satp-on.md) — Sv39 identity boot table + first `csrw satp`.
- [plans/legacy/v0.4-memory-step-3-frame-allocator-concepts.md](plans/legacy/v0.4-memory-step-3-frame-allocator-concepts.md) — bitmap vs linked-list vs buddy; the linear-map design call.
- [plans/legacy/v0.4-memory-step-3-frame-allocator.md](plans/legacy/v0.4-memory-step-3-frame-allocator.md) — frame allocator implementation plan.
- [plans/legacy/v0.4-memory-step-4-kernel-heap.md](plans/legacy/v0.4-memory-step-4-kernel-heap.md) — kernel heap implementation plan.
- [plans/v0.4-memory-findings.md](plans/v0.4-memory-findings.md) — what we learned (and what we worked around) building higher-half.
- [plans/legacy/v0.5-pre-smp-sync-prefactor.md](plans/legacy/v0.5-pre-smp-sync-prefactor.md) — `kernel::sync` chokepoint + `PerCpu<T>` stub. The SMP-shaped pre-factor that landed before v0.5 threading.
- [plans/legacy/v0.5-threading.md](plans/legacy/v0.5-threading.md) — cooperative round-robin scheduler, per-task span stack, `ThreadRegister` + `ContextSwitch` wire frames.
- [plans/legacy/v0.6-smp-cooperative.md](plans/legacy/v0.6-smp-cooperative.md) — the SMP-cooperative milestone: producer/consumer workload migrated across two harts in three posts.
- [plans/scaling-corners.md](plans/scaling-corners.md) — known corners for SMP / interrupts.
- [posts/](posts/) — devlog notes as we go.

## Workspace layout

```
kernel/         no_std RISC-V S-mode kernel; entry.S, linker.ld, drivers, scheduler
kernel-core/    host-buildable no_std lib: pure data + bookkeeping, unit-tested
                (intern table, MMU/frame/heap logic, sched, cap, ipc, console ring)
protocol/       postcard-encoded telemetry Frame enum (no_std); std-gated stream decoder
abi/            kernel↔userspace syscall ABI (numbers, rights bits) — shared, no_std
collector/      host-side: decode frames; export OTLP; serve /metrics
fs-core/        the `Filesystem` trait + types (cap-agnostic, host-tested)
ramfs/          flat in-memory `Filesystem` impl
fs-proto/       FS IPC wire protocol: Badge packing, opcodes, Request/Response
user/           userspace: runtime (crt0/syscalls/heap), std facade, macros,
                and programs (hello, fs, shell-to-be)
itest-harness/  the integration-test runner: scenarios, captures, flake baseline
xtask/          orchestration commands (this file's "Quick start")
stack/          docker-compose: Tempo + Prometheus + Grafana + provisioning
stitch/         "Stitch" — a managed language for SnitchOS (side project)
docs/           project design + milestone plans
plans/          in-progress implementation plans
posts/          devlog notes
learning/       standalone "toy" crates for understanding concepts (separate workspace)
```

## QEMU controls

- `Ctrl-A x` — quit QEMU.
- `Ctrl-A c` — toggle to QEMU's monitor (debug shell). Same combo again to return.
- `Ctrl-A h` — help.

## Useful one-offs

Dump the QEMU `virt` machine's device tree (binary → readable):

```
qemu-system-riscv64 -machine virt -machine dumpdtb=virt.dtb
brew install dtc           # one-time
dtc -I dtb -O dts virt.dtb -o virt.dts
```

Inspect the kernel ELF's section layout:

```
cargo objdump -p kernel --target riscv64gc-unknown-none-elf -- -h
```

(needs `rustup component add llvm-tools-preview` and `cargo install cargo-binutils`)

Check what Prometheus is scraping:

```
curl -s http://localhost:9091/metrics
```
