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
cargo xtask up                # build kernel + run in QEMU (telemetry chardev waits for a client)
cargo xtask collect           # build + run collector (OTLP + Prometheus)
cargo xtask reader            # collector in text-only mode (no docker stack)
cargo xtask stack {up,down,logs}  # docker-compose the Tempo/Prometheus/Grafana stack
cargo xtask test [scenario]   # kernel integration tests in QEMU
cargo xtask build             # just build the kernel ELF
```

### Tests, by layer

| Layer | Command | What it covers |
|---|---|---|
| Unit (kernel-core) | `cargo test -p kernel-core` | Intern table, span registry, pre-init buffer, scause decoding, frame sink capture |
| Unit (protocol)    | `cargo test -p protocol --features std` | Frame roundtrips + stream decoder |
| Unit (collector)   | `cargo test -p collector` | Span state machine, prom/otlp encoding |
| Integration        | `cargo xtask test` | Boots the kernel in QEMU, asserts on the decoded wire frame sequence |

The kernel binary itself has no `#[test]`s — it's `no_std`/`no_main` and won't build for the host target. All testable logic lives in `kernel-core`; everything that touches asm / CSRs / MMIO stays in `kernel/` and is covered (transitively) by the QEMU integration tests.

### Integration test scenarios

`cargo xtask test` spawns one QEMU per scenario and reads decoded `Frame`s off the virtio-console socket. Scenarios in `xtask/src/itest/scenarios.rs`:

- **`boot-reaches-heartbeat`** — Hello → `kernel.boot` SpanStart → `Dropped(0)` after pre-init flush → first `kernel.heartbeat` SpanStart.
- **`heartbeat-cadence`** — two consecutive heartbeats with monotonic timestamps.
- **`pre-init-order`** — first `StringRegister` is `kernel.boot`, every subsequent SpanStart resolves through an earlier StringRegister.
- **`kernel-runs-at-higher-half`** — kernel's own `auipc`-read PC is ≥ `KERNEL_OFFSET` post-trampoline.
- **`frame-allocator-metrics`** — `snitchos.frames.allocated_total` ≥ 1 within 10 s.
- **`frame-allocator-oom`** — built with `--features oom-leak`; asserts `alloc_failed_total > 0` within 15 s and the kernel keeps heartbeating after.
- **`kernel-heap-metrics`** — `snitchos.heap.alloc_total ≥ 1` and `snitchos.heap.bytes_used` observed within 10 s; heartbeat survives after.

Add a scenario: implement a `Result<(), String>` function in `scenarios.rs`, register it in `xtask/src/itest.rs::SCENARIOS`, run `cargo xtask test <name>`. The harness handles QEMU lifecycle and socket cleanup. Use `Harness::spawn_with_features(label, &["feature"])` if the scenario needs a non-default kernel variant (currently only `oom-leak` does).

Skips cleanly (exit 0) if `qemu-system-riscv64` isn't on `PATH`.

### When changing the wire format

`protocol::Frame` is the contract between kernel and host. If you:

- **Add a variant**: update `kernel-core::sink::FrameSink` consumers if relevant, then add a matching arm in `OwnedFrame::from_borrowed` (`protocol/src/stream.rs`). Tests will fail to compile until you do.
- **Change a field**: re-run the integration tests; they assert on the post-decode shape.
- **Reorder existing variants**: don't. Postcard's enum encoding is positional — reordering silently breaks the wire format for all old captures.

### Two telemetry channels, don't confuse them

- **NS16550A UART** (`println!`) — human-readable boot log, no protocol, no decoder. Use for ad-hoc debugging.
- **virtio-console** (telemetry frames) — structured `Frame`s, decoded by `collector` and the integration tests. Use for anything we want to observe or assert on.

### Memory layout, post v0.4 step 4

The kernel image is linked at higher-half VAs (`0xffffffff80200000+`) and runs at higher-half PC after the trampoline in `kmain`. Identity mappings are torn down by `mmu::unmap_identity` later in boot. There are three logical address spaces in play, and which one to use depends on the consumer:

- **Higher-half kernel VAs** (`KERNEL_OFFSET + pa`) — for kernel image, statics, stack. PC-relative addressing (under `code-model=medium`) resolves here naturally for `&static`.
- **Linear-map VAs** (`LINEAR_OFFSET + pa`, via `pa_to_kernel_va`) — for any allocated frame the kernel wants to dereference. One 1 GiB Sv39 huge-page leaf in `BOOT_PT_ROOT[322]` covers all of physical RAM up to 1 GiB. The kernel heap also lives here: `heap::init` grabs 1024 contiguous frames from `frame::alloc_contiguous` and hands their linear-map VA to `linked_list_allocator`. The 4 MiB region is sub-allocated by `#[global_allocator]`; `Box`/`Vec`/`String` and the rest of `alloc::` work after `heap::init` runs.
- **Physical addresses** — for anything handed to a device (virtio DMA buffer pointers, queue ring addresses written to `REG_QUEUE_DESC_LOW/HIGH`). Devices have no MMU.

Gotchas worth re-reading `plans/v0.4-memory-findings.md` before disturbing:

- Anything that passes a kernel address to a device must go through `mmu::va_to_pa`. There are four such sites in `virtio_console.rs`; grep before adding a fifth.
- Anything that needs the *physical* address of a kernel symbol (e.g., reserving the kernel image in the frame allocator's bitmap) must do `va_to_pa((&raw const __sym) as usize)` because post-trampoline that operand is a higher-half VA.
- `fmt::Arguments` (every formatted `println!`) embeds absolute fn-pointer values to type-specific formatters. Those resolve only after the higher-half mapping is live, so **no formatted `println!` before `mmu::enable`**.
- Anything that walks the DTB pre-MMU under higher-half link crashes silently in a way we never isolated. MMIO regions in `kmain` are hardcoded for QEMU `virt`; the DTB-driven `collect_mmio_regions` exists but is parked behind `#[expect(dead_code)]`.
- The frame allocator's `Bitmap::frames_free` is the maintained source of truth for the free count. Don't compute `count_free` by popcount-scanning the bits — it's O(words) and the OOM workload showed it stalls heartbeats. The maintained counter is also what makes `alloc` O(1) when the pool is empty.
- The kernel heap region is fixed at 4 MiB (1024 contiguous frames) for v0.4. To grow it without finding a new contiguous PA run, we need a dedicated heap VA range with per-frame PTE install — strategy (b) in `plans/v0.4-memory-step-4-kernel-heap.md`. Until then, `HEAP_FRAMES` is the only knob.
- Never emit a telemetry frame from inside `GlobalAlloc::alloc`/`dealloc`. The virtio TX path takes a `Mutex` that, on first use of a string, registers through the intern table which may itself allocate. Re-entry deadlock. Same pattern as the frame allocator and the IRQ handler: bump an atomic in the alloc path, drain in the heartbeat loop.

### Architecture references

- [docs/observability-design.md](../docs/observability-design.md) — wire format, span semantics.
- [docs/trap-and-interrupt-model.md](../docs/trap-and-interrupt-model.md) — RISC-V trap handling.
- [docs/roadmap-and-milestones.md](../docs/roadmap-and-milestones.md) — current milestone, what's done, what's next.
- [plans/v0.4-memory-concepts.md](../plans/v0.4-memory-concepts.md) — Sv39, higher-half, frame allocator concepts.
- [plans/v0.4-memory-step-3-frame-allocator-concepts.md](../plans/v0.4-memory-step-3-frame-allocator-concepts.md) — the linear-map design call.
- [plans/v0.4-memory-step-4-kernel-heap.md](../plans/v0.4-memory-step-4-kernel-heap.md) — heap region strategy, allocator choice, deferred-emission constraint.
- [plans/v0.4-memory-findings.md](../plans/v0.4-memory-findings.md) — what we learned building higher-half + frame allocator; read **before** touching the boot order or any address-translation site.
- [plans/scaling-corners.md](../plans/scaling-corners.md) — known corners that v0.1 sidesteps (SMP, lock-during-IRQ, etc.).

## Summary

The key is to write clean, testable, functional code that evolves through small, safe increments. Every change should be driven by a test that describes the desired behavior, and the implementation should be the simplest thing that makes that test pass. When in doubt, favor simplicity and readability over cleverness.
