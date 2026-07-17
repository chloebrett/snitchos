# Development Guidelines for Claude

> **About this file (v4.0.0):** Lean version optimized for context efficiency. Core principles here; detailed patterns loaded on-demand via skills.
>
> **Architecture:**
> - **CLAUDE.md** (this file): Core philosophy + quick reference (~120 lines, always loaded)
> - **Skills**: Detailed patterns loaded on-demand (tdd, testing, mutation-testing, test-design-reviewer, functional, refactoring, expectations, planning, cli-design, finding-seams, characterisation-tests, storyboard, teach-me, diagrams, find-skills, find-gaps, hexagonal-architecture, domain-driven-design, twelve-factor, api-design)
> - **Agents**: Specialized subprocesses for verification and analysis
>
> **Previous versions:**
> - v3.0.0: TypeScript / Vitest stack

## Core Philosophy

**TEST-DRIVEN DEVELOPMENT IS NON-NEGOTIABLE.** Every single line of production code must be written in response to a failing test. No exceptions. This is not a suggestion or a preference - it is the fundamental practice that enables all other principles in this document.

I follow Test-Driven Development (TDD) with a strong emphasis on behavior-driven testing and functional programming principles. All work should be done in small, incremental changes that maintain a working state throughout development.

## Quick Reference

**Key Principles:**

- Write tests first (TDD)
- Test behavior, not implementation
- No `unsafe` without documented invariants
- No `.unwrap()` without justification
- Immutable data by default (no `mut` unless necessary)
- Small, pure functions
- `Result<T, E>` and `Option<T>` over panics

**Preferred Tools:**

- **Language**: Rust (stable)
- **Testing**: `cargo test` / `cargo-nextest` + `insta` for snapshots
- **Mutation testing**: `cargo-mutants`
- **Linting**: `cargo clippy` with strict config
- **Coverage**: `cargo llvm-cov`

## Rust Quality Gates

The Rust compiler catches most type and safety violations automatically. These are the remaining rules that require discipline:

**No `.unwrap()` or `.expect()` without justification**
- If it can't fail by contract, document why: `// SAFETY: vec is non-empty, checked above`
- Prefer `?` operator, pattern matching, or returning `Result`/`Option`

**No `unsafe` without documented invariants**
- Every `unsafe` block must have a `// SAFETY:` comment explaining why it upholds Rust's safety invariants

**No gratuitous `.clone()`**
- Reaching for `.clone()` to satisfy the borrow checker often signals a design problem — rethink ownership first

**No `#[allow(clippy::...)]` without explanation**
- If suppressing a lint, add a comment explaining why the lint doesn't apply here

**Never silently discard a `Result`**
- `let _ = fallible_fn();` is a smell — handle the error or explicitly document why it's safe to ignore

**Clippy config** — projects should have a `clippy.toml` or deny attributes:
```rust
#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)] // example of justified suppression
```

## Testing Principles

**Core principle**: Test behavior, not implementation. Full coverage through business behavior.

**Quick reference:**
- Write tests first (TDD non-negotiable)
- Test through public API exclusively
- Use factory functions for test data (no `static mut`, no shared mutable state across tests)
- Tests must document expected business behavior
- No 1:1 mapping between test files and implementation files

For detailed testing patterns and examples, load the `testing` skill.
For verifying test effectiveness through mutation analysis, load the `mutation-testing` skill.

## Code Style

**Core principle**: Functional programming with immutable data. Self-documenting code.

**Quick reference:**
- No data mutation — prefer owned/immutable values, avoid `mut` bindings
- Pure functions wherever possible
- No nested if/else — use early returns (`?`, `return`, `guard` patterns)
- No comments — code should be self-documenting
- Config structs or builder pattern over long positional parameter lists
- Iterator chains (`.map()`, `.filter()`, `.fold()`) over loops

For detailed patterns and examples, load the `functional` skill.

## Development Workflow

**Core principle**: RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR in small, known-good increments. TDD is the fundamental practice.

**Quick reference:**
- RED: Write failing test first (NO production code without failing test)
- GREEN: Write MINIMUM code to pass test
- MUTATE: Run `cargo mutants` to verify test effectiveness, produce a report
- KILL MUTANTS: Address surviving mutants (ask human when value is ambiguous)
- REFACTOR: Assess improvement opportunities (only refactor if adds value)
- Each increment leaves codebase in working state
- **All work is done directly on `main`.** No feature branches. Commits land on main.
- **The user handles all commits.** Never run `git commit` — present the work and stop.

For detailed TDD workflow, load the `tdd` skill.
For refactoring methodology, load the `refactoring` skill.
For significant work, load the `planning` skill. Plans live in `plans/` directory.
**Override of the planning skill's "delete when complete" step**: on completion, move
the plan to `plans/legacy/` (`git mv`) instead of deleting it — keep the historical
record. `plans/legacy/` already holds many prior completed plans; follow that
precedent.
For CI failure diagnosis, load the `ci-debugging` skill.
For hexagonal architecture projects, load the `hexagonal-architecture` skill.
For Domain-Driven Design projects, load the `domain-driven-design` skill.
For 12-factor service projects, load the `twelve-factor` skill.
For CLI tool design (stream separation, format flags, exit codes, composability), load the `cli-design` skill.
For making untestable code testable, load the `finding-seams` skill.
For documenting existing behavior before changes, load the `characterisation-tests` skill.
For multi-surface design audits before code, load the `storyboard` skill.
For structured learning of any topic (interactive tutoring, courses, quizzes), use `/teach-me [topic]`.
For discovering and installing agent skills from the open ecosystem (`npx skills`), load the `find-skills` skill.
For adversarial review of plans, acceptance criteria, or design mocks, load the `find-gaps` skill.
For relentless plan or design interrogation before implementation, load the `grill-me` skill.

**Project onboarding:** Run `/setup` in any new project to detect its tech stack and generate project-level CLAUDE.md, hooks, commands, and PR review agent in one shot.

**Project-level hooks:** Projects should add a PostToolUse hook in `.claude/settings.json` to run `cargo clippy` after Write/Edit on `.rs` files. Use `/setup` to generate this automatically.

## Output Guardrails

- **Write to files, not chat** — When asked to produce a plan, document, or artifact, always persist it to a file. You may also present it inline for approval, but the file is the source of truth.
- **Plan-only mode** — When asked for a plan, design, or document only, produce ONLY that artifact. Do not write production code, test code, or make any implementation changes unless explicitly asked.
- **Incremental output** — When exploring a codebase, produce a first draft of output within 3-4 tool calls. Refine iteratively rather than front-loading all exploration before producing anything.
- **Atomic Bash, no narration pipelines** — Issue one logical command per Bash call. Do NOT chain steps with `;`/`&&`, and do NOT append `echo`/`$?` to narrate exit codes — the tool result already carries stdout/stderr and exit status. Multi-statement scripts and shell expansions (`$?`, `$(…)`) defeat the permission allowlist and force a manual prompt; atomic calls clear it silently. If steps are independent, emit them as parallel Bash calls in one message instead of one chained command. Pipes into a single filter (`cmd | grep …`) are fine; chaining whole commands is not.

## Working with Claude

**Core principle**: Think deeply, follow TDD strictly, capture learnings while context is fresh.

**Quick reference:**
- ALWAYS FOLLOW TDD - no production code without failing test
- Assess refactoring after every green (but only if adds value)
- Update CLAUDE.md when introducing meaningful changes
- Ask "What do I wish I'd known at the start?" after significant changes
- Document gotchas, patterns, decisions, edge cases while context is fresh

For detailed TDD workflow, load the `tdd` skill.
For refactoring methodology, load the `refactoring` skill.
For detailed guidance on expectations and documentation, load the `expectations` skill.

## Resources and References

- [The Rust Programming Language](https://doc.rust-lang.org/book/)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Rust Design Patterns](https://rust-unofficial.github.io/patterns/)
- [cargo-nextest](https://nexte.st/)
- [insta snapshot testing](https://insta.rs/)

## Project-Specific: SnitchOS

SnitchOS is a RISC-V microkernel whose first-class concern is observability. Boot-time spans, metrics, and events go out a dedicated virtio-console as postcard-encoded `Frame`s; a host-side `collector` decodes them into OTLP traces (Tempo) and Prometheus metrics (Grafana).

### Workspace layout

```
kernel/       no_std, no_main, riscv64gc-unknown-none-elf only.
              Asm boot stub, trap handler, virtio-console driver,
              ns16550a UART, panic handler, the `mmu::enable` /
              trampoline / `unmap_identity` flow, and the static
              `frame::FRAME_BITS` storage + `Mutex<Allocator>` wrapper,
              and the `#[global_allocator]` `heap::HEAP` backed by
              `linked_list_allocator` over a 4 MiB linear-map region.
              Statics live here.
kernel-core/  no_std, host-buildable. Pure data + bookkeeping with
              no asm / MMIO / CSRs: intern table, span registry,
              pre-init buffer, scause decoding, Clock + FrameSink
              traits, Sv39 page-table primitives + `KERNEL_OFFSET` /
              `LINEAR_OFFSET` / `va_to_pa` / `pa_to_kernel_va`, and
              the `frame::Bitmap`. All host-tested via `cargo test`.
protocol/     no_std by default. The `Frame` enum + postcard
              encoding. `features = ["std"]` opts in to
              `protocol::stream` (decoder + `OwnedFrame`).
collector/    Host-side decoder. Reads virtio-console socket,
              decodes frames, exports OTLP + serves Prometheus.
xtask/        Build / run / test orchestration. See subcommands
              in README.
stack/        docker-compose for Tempo + Prometheus + Grafana.
plans/        Per-milestone and per-refactor implementation plans.
docs/         Architecture and design.
posts/        Devlog notes per milestone.
```

### Running

```
cargo xtask boot              # build kernel + run in QEMU; no bootarg now boots `init` (the userspace root, v0.13)
cargo xtask boot --workload demo  # the former default: kernel scheduler demo (task_a/task_b + producer/consumer)
cargo xtask boot --workload smp   # boot a runtime-selected workload (implies itest-workloads); see "Runtime workloads"
cargo xtask collect           # build + run collector (OTLP + Prometheus)
cargo xtask reader            # collector in text-only mode (no docker stack)
cargo xtask stack {up,down,logs}  # docker-compose the Tempo/Prometheus/Grafana stack
cargo xtask itest [scenario]  # kernel integration tests in QEMU (host unit tests are `cargo xtask test`)
cargo xtask build             # just build the kernel ELF
cargo xtask clippy [-- args]  # clippy the WHOLE workspace correctly (see note below)
cargo xtask diagram <target>  # generate a diagram (deps|itest-matrix|caps|trace|switches) into docs/generated/; --check gates the static ones
cargo xtask diagram png       # render the hand-drawn mermaid docs to local PNGs (needs mmdc, Node >=18)
```

**Diagrams:** the `diagram` crate + `cargo xtask diagram` draw the workspace's
diagrams. Static targets (`deps`, `itest-matrix`) are `--check`-gated (drift
fails `cargo xtask test`); telemetry targets (`caps`, `trace`, `switches`) fold
`OwnedFrame`s from a snemu boot. Committed `.md` renders on GitHub. Design +
what's-deferred: [docs/diagrams-design.md](../docs/diagrams-design.md).

**Linting:** use `cargo xtask clippy`, not `cargo clippy --workspace`. The kernel
only builds for `riscv64gc-unknown-none-elf`; a plain workspace clippy compiles it
for the host, where it can't link (duplicate `panic_impl`, unknown `a7` registers).
`xtask clippy` lints host crates for the host and the kernel for riscv in one go.
Forward args to both, e.g. `cargo xtask clippy -- --fix --allow-dirty`. **Caveat:**
do NOT blanket `--fix` the kernel — clippy's `deref_addrof` autofix rewrites the
required `&mut *(&raw mut STATIC)` idiom into a forbidden direct `&mut STATIC`.
Those sites carry a justified `#[allow(clippy::deref_addrof, reason = ...)]`.
(Stable can't auto-target a single package; `forced-target` needs nightly.)

### Tests, by layer

| Layer | Command | What it covers |
|---|---|---|
| Unit (kernel-core) | `cargo test -p kernel-core` | Intern table, span registry, pre-init buffer, scause decoding, frame sink capture |
| Unit (protocol)    | `cargo test -p protocol --features std` | Frame roundtrips + stream decoder |
| Unit (collector)   | `cargo test -p collector` | Span state machine, prom/otlp encoding |
| Integration        | `cargo xtask test` | Boots the kernel in QEMU, asserts on the decoded wire frame sequence |

The kernel binary itself has no `#[test]`s — it's `no_std`/`no_main` and won't build for the host target. All testable logic lives in `kernel-core`; everything that touches asm / CSRs / MMIO stays in `kernel/` and is covered (transitively) by the QEMU integration tests.

### Integration test scenarios

`cargo xtask itest` spawns one QEMU per scenario and reads decoded `Frame`s off the virtio-console socket. The whole suite builds **one** `itest-workloads` kernel up front and selects per-scenario via the `workload=` bootarg (see "Runtime workloads" below). Scenarios in `xtask/src/itest/scenarios.rs`:

- **`boot-reaches-heartbeat`** — Hello → `kernel.boot` SpanStart → `Dropped(0)` after pre-init flush → first `kernel.heartbeat` SpanStart.
- **`heartbeat-cadence`** — two consecutive heartbeats with monotonic timestamps.
- **`pre-init-order`** — first `StringRegister` is `kernel.boot`, every subsequent SpanStart resolves through an earlier StringRegister.
- **`kernel-runs-at-higher-half`** — kernel's own `auipc`-read PC is ≥ `KERNEL_OFFSET` post-trampoline.
- **`frame-allocator-metrics`** — `snitchos.frames.allocated_total` ≥ 1 within 10 s.
- **`frame-allocator-oom`** — selects `workload=frame-oom`; asserts `alloc_failed_total > 0` within 15 s and the kernel keeps heartbeating after.
- **`kernel-heap-metrics`** — `snitchos.heap.alloc_total ≥ 1` and `snitchos.heap.bytes_used` observed within 10 s; heartbeat survives after.
- **`heap-oom`** — selects `workload=heap-oom`; leaks 4096 × 4 KiB blocks per heartbeat (16 MiB/tick) via `Vec::try_reserve_exact` + `mem::forget`. Watermark grow adds 1 MiB/tick; net pressure ~15 MiB/tick exhausts ~120 MiB usable RAM in ~8 heartbeats. Asserts `heap.grow_total > 0` (P2 grow engaged), then `heap.alloc_failed_total > 0` (clean OOM), then the kernel keeps heartbeating.
- **`sched-context-switch-smoke`** — asserts `snitchos.sched.smoke_marker_hits == 1` within 10 s. Boot-time round-trip of the asm `switch` primitive into a hand-rigged marker function and back.
- **`sched-spawn-registers-thread`** — asserts `ThreadRegister` frames appear for `main`, `idle`, `task_a`, `task_b`.
- **`sched-yield-round-trips`** — asserts both `task_a.loops > 0` and `task_b.loops > 0` plus `sched.context_switches_total > 0`. Proves cooperative round-robin reaches all demo tasks.
- **`sched-spans-carry-task-id`** — asserts each demo task's `task_x.tick` SpanStart carries `task_id` matching that task's ThreadRegister id.
- **`sched-context-switches-on-wire`** — asserts a `ContextSwitch{Yield}` frame with both endpoints being known task ids appears on the wire.
- **`sched-span-survives-yield`** — task_a yields mid-span; scenario asserts the SpanStart→ContextSwitch(leave)→ContextSwitch(return)→SpanEnd sequence with matching span id and `parent==SpanId(0)`. Structural proof that per-task `SpanCursor` wiring works.
- **`workload-cooperative-baseline`** — default build (single-hart producer/consumer); asserts `samples_consumed ≥ 200` then `histogram_sum ≥ consumed`.
- **`smp-producer-consumer-correctness`** — `workload=smp` (producer hart 0, consumer hart 1); asserts `samples_consumed ≥ 1000` then `histogram_sum ≥ consumed` across the hart boundary (the cross-hart Release/Acquire oracle).
- **`smp-secondary-hart-boots`**, **`smp-spawn-on-hart-1-runs`**, **`ipi-self-wakeup`** — SMP bring-up + IPI smokes.
- **`spawn-storm` / `ipi-pong` / `shootdown-storm` / `mutex-storm` / `virtio-storm`** — cross-hart stress/regression workloads (formerly the `deflake-*` features), selected via `workload=<name>`. Kept as guards after the bug they characterised (a dropped `MutexGuard` in `virtio_console::send`) was fixed.
- **`kernel-panic-emits-frame`** — `workload=panic-now` (a kernel task that just `panic!()`s). Asserts a `Log("kernel panic …")` reaches the wire: the panic handler emits telemetry, not just the emergency UART. The emit is panic-safe — no alloc, no intern (reuses `Frame::Log`), non-blocking bounded-retry `try_send_panic` — see [plans/legacy/panic-emits-telemetry.md](../plans/legacy/panic-emits-telemetry.md). `snemu-diff` conditions its benign-`kernel.heartbeat` pass on this frame (proof snemu reached the crash, not a fail-to-halt). `panic-now` also began as the minimal repro that isolated the guard-page family's snemu-vs-QEMU divergence to crash-vs-heartbeat *timing*, not MMU/scheduling.

Add a scenario: implement a `Result<(), String>` function in `scenarios.rs`, register it in `xtask/src/itest.rs::SCENARIOS`, run `cargo xtask itest <name>`. The harness handles QEMU lifecycle and socket cleanup.

### Runtime workloads

Non-default boot behaviours (the SMP cross-hart workload, the OOM leaks, the stress storms) are **selected at runtime**, not compiled in per-build. One `itest-workloads` kernel binary holds the whole registry; the `workload=<name>` kernel bootarg (QEMU `-append`) picks one. With no bootarg the kernel boots `init` (v0.13; the former kernel scheduler demo is `workload=demo`), so the registry is **purely additive** — production builds leave `itest-workloads` off and compile none of it. Full design + rationale: [docs/runtime-workload-selection-design.md](../docs/runtime-workload-selection-design.md).

- **From a scenario:** `Harness::spawn_with_workload(label, "smp")` — boots the shared `itest-workloads` build with `-append workload=smp`. `Harness::spawn(label)` boots the same build with no bootarg (now `init`, the userspace root). Neither rebuilds; the suite builds once.
- **Live (measurement / demo):** `cargo xtask boot --workload smp` then `cargo xtask reader` (or `collect` → Grafana). No rebuild to switch workloads.
- **Adding a workload:** add a `WorkloadKind` variant + parse arm in `kernel-core::bootargs` (host-tested), then dispatch on `boot_workload::selected()` in `kmain` (spawn layout) and/or `heartbeat` (per-tick behaviour); storm bodies live in `kernel::storms` (itest-workloads only).
- **Genuinely compile-time variants** (rare — something that must change codegen, not just boot behaviour) would reintroduce a build hook; none exist today.

Skips cleanly (exit 0) if `qemu-system-riscv64` isn't on `PATH`.

### When changing the wire format

`protocol::Frame` is the contract between kernel and host. If you:

- **Add a variant**: update `kernel-core::sink::FrameSink` consumers if relevant, then add a matching arm in `OwnedFrame::from_borrowed` (`protocol/src/stream.rs`). Tests will fail to compile until you do.
- **Change a field**: re-run the integration tests; they assert on the post-decode shape.
- **Reorder existing variants**: don't. Postcard's enum encoding is positional — reordering silently breaks the wire format for all old captures.

### Two telemetry channels, don't confuse them

- **NS16550A UART** (`println!`) — human-readable boot log, no protocol, no decoder. Use for ad-hoc debugging.
- **virtio-console** (telemetry frames) — structured `Frame`s, decoded by `collector` and the integration tests. Use for anything we want to observe or assert on.

### Scheduler (v0.5)

Cooperative round-robin over a single CPU. The kernel runs out of one of its tasks at any moment; there is no dedicated scheduler thread. The scheduler is a *library*: `kernel_core::sched::Runqueue` + `kernel::sched::yield_now()`. `yield_now` runs on the calling task's stack — saves its callee-saved registers into its `TaskContext`, picks the next ready task, loads its registers, `ret`s into it.

Four tasks at boot:

- **task 0 = "main"** — IS `kmain`. `register_bare_task("main")` declares the running boot context as task 0; from then on, every `yield_now` from main saves into / restores from task 0's `TaskContext`. The boot stack from `entry.S` is task 0's stack.
- **idle** — `loop { wfi; yield_now(); }`. Owns the `wfi` for the whole kernel; nobody else sleeps.
- **task_a, task_b** — demo workers. Each opens a `task_x.tick` span, burns LCG iterations, yields, closes the span. task_a holds its span open across a yield to exercise per-task `SpanCursor` correctness.

Key static state:

- `SCHEDULER: kernel::sync::Mutex<Scheduler>` — task table (`Vec<Box<Task>>`) and `Runqueue`. `Box<Task>` guarantees stable heap addresses so raw `*mut TaskContext` and `*const SpanCursor` pointers stay valid past the mutex drop.
- `CURRENT_TASK: AtomicU32` — id of the running task on this hart. Read by `current_task_id()` for `SpanStart.task_id`.
- `CURRENT_TASK_ENTRY_TICK: AtomicU64` — timestamp when the running task last became Running. `yield_now` computes `now - entry` and adds to the outgoing task's `cpu_time_ticks`.
- `CURRENT_SPAN_CURSOR: AtomicPtr<SpanCursor>` — pointer to the running task's span cursor. Updated on every switch. `tracing::span_start` reads it for the parent; `Span` guards remember which cursor they opened on so close lands on the right one even after surviving a yield.
- `CONTEXT_SWITCHES: AtomicU64` and `LAST_YIELD_OVERHEAD_TICKS: AtomicU64` — telemetry.

API: `spawn(name, entry: extern "C" fn() -> !)` allocates a 16 KiB `Box<Stack>`, rigs `TaskContext { ra: entry, sp: stack.top() }`, registers + queues. `yield_now()` voluntarily switches. `current_task_id()` for telemetry. `task_snapshots()` for per-task metric drain. No `unspawn` / `exit` / `join` in v0.5 (tasks are `-> !`).

Lock discipline:

- **Never hold a `kernel::sync::Mutex` across `yield_now()`.** Cooperative; not enforced; a debug-only "held locks at yield" assert is a v0.5.x candidate.
- The scheduler mutex is dropped before the asm `switch` runs — Tasks live in `Box<Task>` so the raw context pointers remain valid past the drop.
- `virtio_console::send` stages the frame bytes through a static `TX_STAGING` buffer. **Required because `mmu::va_to_pa()` only handles `KERNEL_OFFSET`-range VAs; heap-allocated task stacks have VAs in `HEAP_VA_BASE+` which `va_to_pa` passes through unchanged**, so without staging the virtio descriptor would carry a heap VA where the device expects a PA — silently DMA-ing wrong physical memory. Caught during v0.5 step 7 debug.
- **`let x = *MUTEX.lock();` releases the lock at the `;`.** `lock()` returns a temporary `MutexGuard`; deref-and-copy doesn't borrow it, so no lifetime extension — the guard drops immediately and any shared state touched afterward is unprotected. Bind the guard: `let g = MUTEX.lock(); let x = *g; …`. This exact bug in `virtio_console::send` was the ~2% cross-hart wedge (two harts racing `TX_STAGING` + the virtqueue ring during hart-1 task registration); the failure-signature classifier + capture corpus pinned it. See [plans/legacy/tx-staging-cross-hart-race.md](../plans/legacy/tx-staging-cross-hart-race.md).

Per-task observability:

- Each `Task` carries `cpu_time_ticks` and `runs` atomics + pre-registered `cpu_time_metric` and `runs_metric` StringIds.
- Names are built once at spawn via `format!` and leaked into `'static` via `tracing::register_counter_owned(String)` — bounded leak (one pair per task).
- Heartbeat walks `sched::task_snapshots()` per tick and emits one `snitchos.task.<name>.cpu_time_ticks` + `snitchos.task.<name>.runs_total` per task.

The post angle (v0.5): "following a trace across a context switch." Tempo's trace view shows `task_a.tick` opened, then ContextSwitch frames, then SpanEnd — all attributable via the `thread.name` OTLP attribute populated from `ThreadRegister`.

### Capabilities & userspace — SHIPPED through v0.13

User mode, capability-mediated syscalls, IPC, a RAMfs over IPC, and a userspace
`init` root all exist today. **v0.8 preemption, v0.9 IPC, v0.10 RAMfs, v0.11
Spawn-with-caps, v0.12 Exit/Wait/reap + the Notification primitive, and v0.13 the
`init` bootstrap are all shipped.** The default boot (no `workload=` bootarg) now
boots **`init`** (the userspace delegation-graph root); the former kernel scheduler
demo (`task_a`/`task_b` + producer/consumer + cross-hart probe) is `workload=demo`.

- **Caps** (`kernel-core/src/user/cap.rs`, host-tested): `Capability { object, rights }` named by an opaque `Handle` (slot+generation `u32`), validated against the calling process's `CapTable`. `Object` = `TelemetrySink | SpanSink | Endpoint{id,badge} | Reply{caller} | Notification{id}` (no separate `File` object — FS files are *badged* `Endpoint` caps the FS server mints). `generation` is the revocation hook (dead-weight at 0).
- **Cap-id spine (v0.13)**: every *holding* (a `CapTable` slot) carries a stable global `cap_id` (`Slot.cap_id`, set via `insert_with_id`/`bootstrap_with_ids`, read via `cap_id_of`), minted kernel-side (`next_cap_id`). A transfer records the **source holding's** id as the child's `parent_cap_id` — so `CapEvent::Transferred` frames reconstruct the derivation tree. Wire `cap_id` == stored id at every grant/transfer site (`run_with_caps`, `run_ipc`, `handle_mint_badged`, `NotifyCreate`, `EndpointCreate`). Only genuinely-root grants (and the not-yet-linked reply-cap mint) keep `parent_cap_id: 0`.
- **Syscalls** (`abi::Syscall` 0–25, dispatch `kernel/src/syscall/mod.rs`): cap-mediated — `SpanOpen`/`Send`/`Receive`/`Call`/`Reply`/`ReplyRecv`/`MintBadged`/`Signal`/`WaitNotify`/`EmitMetric`/`RegisterMetric`. Ambient — `Exit`/`Yield`/`SpanClose`/`MapAnon`/`DebugWrite`/`ConsoleRead`/`ConsoleWrite`/`ClockNow`/`Spawn`/`Wait`/`WaitAny`/`NotifyCreate`/`EndpointCreate`. Refusals snitch (`SyscallRefused` frame + counter), never silent.
- **Startup-cap ABI**: a spawned child is born with bootstrap telemetry@handle 0, span@1, then parent-**delegated** caps at handles `2..` (`delegated_handle(i) = 2 + i`). `Spawn`'s `a1`=`[u32;N]` handle array delegates from the caller's table (copy semantics, all-or-nothing). **An endpoint lands at handle 2 in *both* launch paths** — `run_ipc` (after the two bootstrap caps) and an init-`Spawn` delegating it (delegated[0]) — so IPC programs read their endpoint via `delegated_handle(0)`, not the legacy `a2` startup slot.
- **`init` (v0.13, `user/hello/src/bin/init.rs`)**: the first userspace process, holding only telemetry+span. It `EndpointCreate`s its own IPC endpoint (kernel knows no IPC topology), `Spawn`s the FS server delegating `RECV|MINT` + a client with a *minted* bare `SEND`, and supervises via `WaitAny` (reap any child). Children come from the `SPAWNABLE` registry (`kernel/src/trap/user.rs`); kernel-launched workloads use `LAYOUTS` + `run`/`run_ipc`. Caveat: copy-semantics delegation means init over-holds `RECV` on the endpoint it gave the server (revocation deferred).
- **Userspace** (`user/`): `runtime` (crt0, syscall bindings, `talc` heap), `std`, `macros` (`#[entry]`), `hello` (bins incl. `init`/`spawnee`/`supervisor`/`spinner`/`ep_maker`), `fs` (`fs::serve` + `fs-client`). IPC + RAMfs designs: `docs/ipc-design.md`, `docs/filesystem-design.md`.

### Memory layout, post v0.4 step 4

The kernel image is linked at higher-half VAs (`0xffffffff80200000+`) and runs at higher-half PC after the trampoline in `kmain`. Identity mappings are torn down by `mmu::unmap_identity` later in boot. There are three logical address spaces in play, and which one to use depends on the consumer:

- **Higher-half kernel VAs** (`KERNEL_OFFSET + pa`) — for kernel image, statics, stack. PC-relative addressing (under `code-model=medium`) resolves here naturally for `&static`.
- **Linear-map VAs** (`LINEAR_OFFSET + pa`, via `pa_to_kernel_va`) — for any allocated frame the kernel wants to dereference. One 1 GiB Sv39 huge-page leaf in `BOOT_PT_ROOT[322]` covers all of physical RAM up to 1 GiB. The page-table walker (`KernelPtMem::read_entry`/`write_entry`) reads/writes intermediate tables through this lens too.
- **Heap VAs** (`HEAP_VA_BASE = 0xffffffc0_00000000+`, root PTE 256) — a dedicated 1 GiB window owned by the kernel heap. `heap::init` calls `mmu::map` 1024 times to install per-page leaves backing the first 4 MiB; `heap::extend` (triggered by the heartbeat watermark policy) grows incrementally up to the 1 GiB ceiling. Heap VAs are contiguous by construction; PA frames are scattered.
- **Physical addresses** — for anything handed to a device (virtio DMA buffer pointers, queue ring addresses written to `REG_QUEUE_DESC_LOW/HIGH`). Devices have no MMU.

Gotchas worth re-reading `plans/v0.4-memory-findings.md` before disturbing:

- Anything that passes a kernel address to a device must go through `mmu::va_to_pa`. There are four such sites in `virtio_console.rs`; grep before adding a fifth.
- Anything that needs the *physical* address of a kernel symbol (e.g., reserving the kernel image in the frame allocator's bitmap) must do `va_to_pa((&raw const __sym) as usize)` because post-trampoline that operand is a higher-half VA.
- `fmt::Arguments` (every formatted `println!`) embeds absolute fn-pointer values to type-specific formatters. Those resolve only after the higher-half mapping is live, so **no formatted `println!` before `mmu::enable`**.
- **The higher-half trampoline is not a barrier for PC-relative address materialization.** Under `code-model=medium`, `&static` is computed via `auipc` and resolves to *whichever half PC is in*. The optimizer may hoist a `&static`-into-register computation *before* the trampoline `jr` (where PC is still physical) and carry it in a callee-saved reg across the jump, yielding a **truncated/physical** address in a higher-half world. This bit `tp` in `percpu::init` (release-only; `mv tp, &PER_HART_DATA[hartid]` hoisted → `tp = 0x0000…8032xxxx`, masked for ages by `current_hartid`'s range check, exposed when the exception-stack asm `ld …,24(tp)` read it raw). Fix pattern: materialize the address with a **side-effecting `asm!("lla …")`** (ordered after the trampoline's `asm!`, so it can't hoist). Debug hides this; only release codegen hoists. See `notes/release-build-exposes-timer-death-and-uart-corruption.md`.
- **Release itests build the kernel at opt-3 but pin the embedded userspace to opt-1** (`kernel/build.rs`, nested build `--config profile.release.opt-level=1`). There is a latent opt≥2 UB *class* in the userspace crates (talc OOM-loop → hang; confirmed in `snitchos-user`, at least one more crate); the itest speedup is kernel-dominated so opt-1 userspace costs ~nothing and sidesteps it. Root-causing the userspace UB (so the pin can go) is an open follow-up. Repro: `cargo xtask snemu-itest --release`.
- Anything that walks the DTB pre-MMU under higher-half link crashes silently in a way we never isolated. MMIO regions in `kmain` are hardcoded for QEMU `virt`; the DTB-driven `collect_mmio_regions` exists but is parked behind `#[expect(dead_code)]`.
- The frame allocator's `Bitmap::frames_free` is the maintained source of truth for the free count. Don't compute `count_free` by popcount-scanning the bits — it's O(words) and the OOM workload showed it stalls heartbeats. The maintained counter is also what makes `alloc` O(1) when the pool is empty.
- The kernel heap starts at 4 MiB and grows on demand via the heartbeat-driven watermark policy (`kernel_core::heap::watermark_grow_decision`). It caps at 1 GiB (the full root-PTE slot). Each grow allocates frames + installs PTEs via `mmu::map`; the policy fires when `free < capacity * 25%`. Watermark constants live in `kernel::heap::WATERMARK`; tune there.
- All kernel-internal locking goes through `kernel::sync::{Mutex, MutexGuard, Once}` — thin wrappers over `spin` types with no-op acquisition/release hooks today. The wrappers exist so preempt-disable (v0.5.x) and SMP IRQ-disable (v0.7+) land in one file. A workspace `disallowed_types` clippy lint blocks raw `spin::Mutex` / `spin::Once` outside `kernel::sync`. One flavour (always-IRQ-safe-when-implemented); two-flavour split (Linux-style `lock` vs `lock_irqsave`) is documented as a deferred follow-up if a hot path proves it needs it.
- Per-CPU storage goes through `kernel::percpu::PerCpu<T>` + `current_hartid()`. As of v0.6 `MAX_HARTS = 2` and `current_hartid()` reads the running hart's logical id through `tp` (set up in `percpu::init` to point at this hart's `PER_HART_DATA` slot); it falls back to 0 only if `tp` is outside that static (pre-init). Call sites stayed stable across the single→multi-hart change.
- `kernel::mmu::map(va, pa, perms)` is the runtime page-table-mutation primitive; `kernel::mmu::remap(va, new_pa, perms)` overwrites an existing leaf. Walk logic lives in `kernel_core::mmu::{map,remap}` (pure, host-tested via a `PtMem` mock; no `unsafe` in kernel-core), kernel-side adds `KernelPtMem` over `frame::alloc_zeroed` + `pa_to_kernel_va`. `map` does a local `sfence.vma vaddr`; `remap` follows the PTE write with a cross-hart `kernel::mmu::shootdown(va)` (TLB-shootdown IPI + per-hart ack barrier), since overwriting a cached translation must invalidate it on every hart. The kernel heap (`heap::init` + `heap::extend`) was the first consumer; v0.7 userspace page tables use the same primitive.
- Never emit a telemetry frame from inside `GlobalAlloc::alloc`/`dealloc`. The virtio TX path takes a `Mutex` that, on first use of a string, registers through the intern table which may itself allocate. Re-entry deadlock. Same pattern as the frame allocator and the IRQ handler: bump an atomic in the alloc path, drain in the heartbeat loop.

### Architecture references

- [docs/observability-design.md](../docs/observability-design.md) — wire format, span semantics.
- [docs/trap-and-interrupt-model.md](../docs/trap-and-interrupt-model.md) — RISC-V trap handling.
- [docs/roadmap-and-milestones.md](../docs/roadmap-and-milestones.md) — current milestone, what's done, what's next.
- [plans/legacy/v0.4-memory-concepts.md](../plans/legacy/v0.4-memory-concepts.md) — Sv39, higher-half, frame allocator concepts.
- [plans/legacy/v0.4-memory-step-3-frame-allocator-concepts.md](../plans/legacy/v0.4-memory-step-3-frame-allocator-concepts.md) — the linear-map design call.
- [plans/legacy/v0.4-memory-step-4-kernel-heap.md](../plans/legacy/v0.4-memory-step-4-kernel-heap.md) — heap region strategy, allocator choice, deferred-emission constraint.
- [plans/legacy/v0.4-memory-step-5-page-table-mutation.md](../plans/legacy/v0.4-memory-step-5-page-table-mutation.md) — `map(va, pa, perms)` API; P1 (primitive) + P2 (growable heap) split.
- [plans/legacy/v0.5-pre-smp-sync-prefactor.md](../plans/legacy/v0.5-pre-smp-sync-prefactor.md) — `kernel::sync` chokepoint + `kernel::percpu` stub. Lands before v0.5 threading so lock discipline is in one place.
- [plans/legacy/v0.5-threading.md](../plans/legacy/v0.5-threading.md) — cooperative round-robin scheduler, per-task span stack, `ThreadRegister` + `ContextSwitch` wire frames.
- [posts/post-12-the-kernel-takes-turns.md](../posts/post-12-the-kernel-takes-turns.md) — v0.5 devlog.
- [plans/v0.4-memory-findings.md](../plans/v0.4-memory-findings.md) — what we learned building higher-half + frame allocator; read **before** touching the boot order or any address-translation site.
- [plans/scaling-corners.md](../plans/scaling-corners.md) — known corners that v0.1 sidesteps (SMP, lock-during-IRQ, etc.).

## Summary

The key is to write clean, testable, functional code that evolves through small, safe increments. Every change should be driven by a test that describes the desired behavior, and the implementation should be the simplest thing that makes that test pass. When in doubt, favor simplicity and readability over cleverness.
