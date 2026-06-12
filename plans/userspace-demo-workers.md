# Userspace demo workers + userspace tracing (v0.7 follow-on)

**Work lands on:** `main` (no feature branches — see CLAUDE.md)
**Status:** Active — prerequisite for `plans/v0.8-preemption.md`.

Two coupled deliverables:

1. **Userspace tracing** — give U-mode programs a way to emit spans, with
   a *string-in* API (interning is the kernel's problem, never the
   program's). This un-regresses the demo workers (today's kernel-mode
   `task_a`/`task_b` emit rich `task_x.tick` spans), supercharges the
   v0.8 preemption demo ("watch a userspace span get preempted
   mid-flight"), and is a **hard dependency of v0.9 IPC** ("spans cross
   the process boundary"). Built now, it pays off three times.
2. **Move the demo workers into userspace** — `task_a`/`task_b` become
   userspace processes that emit those spans, sharing one hart
   cooperatively. They were kernel-mode threads as a pre-userspace
   stopgap (v0.5).

Together they finish the v0.7 story and produce the fixtures v0.8 needs.

## The crux: two U-mode tasks must share one hart

Today there is exactly **one** user process. `kmain` does
`sched::spawn_on(1, "user_main", user::user_main_entry)`; the kernel-side
entry runs `user::run` → `Process::bootstrap` → load ELF → `enter()`,
which switches `satp` and `sret`s into U-mode. From there the only way
back into the kernel is a **trap** (ecall / fault / timer).

A userspace task therefore **cannot call `yield_now()`** — that's a
kernel function on kernel stacks. With one process this never mattered.
Two userspace workers on one hart must share the CPU, and there are only
two mechanisms: a **`yield` syscall**, or **preemption** (v0.8). Without
one, two workers **starve each other** — a broken intermediate state the
"known-good increments" rule forbids.

## Load-bearing decision — LOCKED

**Decision (confirmed by user): add a cooperative `yield` syscall —
cooperative userspace first, preemption (v0.8) second.** Cooperative
workers become the *peer* that makes progress under v0.8; v0.8's
`user-hog` (a worker that omits the `yield`) demonstrates preemption
catching the non-cooperative case. Mirrors the kernel's own
cooperative→preemptive arc, and `yield` is the first relative of v0.9
IPC's blocking syscalls.

## Userspace tracing design

**API contract (LOCKED): userspace passes a string; the kernel interns
or reuses.** The program says "open a span named `worker_a.tick`" with
the *bytes*; interning is invisible to it. No `StringId`s cross the
boundary — that was the anti-pattern of the first sketch.

- **Why on-demand interning is safe here.** The re-entry deadlock that
  forced pre-registration of the existing counters is a property of
  *re-entrant* emit sites (the `GlobalAlloc` path, IRQ drains) that may
  already hold the intern/TX `Mutex`. **Verified** (`heap.rs:88-112`):
  `alloc`/`dealloc` only bump `ALLOC_COUNT`/`DEALLOC_COUNT` atomics — no
  synchronous emit — so the chain `emit`→`intern`→`alloc`→`emit` is
  broken at the alloc step. A synchronous U-mode `ecall` holds no kernel
  locks, so `intern()` (which may allocate + emit a `StringRegister`) is
  safe from the syscall handler. Pre-registration stays only for
  `faults_total` / `cap_denied` / alloc-drain counters.
- **Lock discipline (required).** The handler must hold **no `Mutex`
  across the intern/emit** — `copy_from_user` the name, drop any caps
  lock, *then* `intern()` + emit. `handle_invoke` already models this
  (trap.rs:228-233: resolve under `caps.lock()`, copy out, drop, then
  `emit_metric`). `handle_span_open` follows the same shape.
- **`copy_from_user` — the new primitive.** The first syscall to carry a
  *buffer*, not a scalar. During the trap `satp` is still the user
  process's table (the kernel high-half is mapped into it), so the user
  string is **directly readable** — no walk needed. The kernel only
  **range-validates** the `(ptr, len)`: the whole span must lie in the
  user VA window (below `KERNEL_OFFSET`), with no wraparound, bounded
  length. A crafted pointer into the kernel high-half is rejected.
  *Fault-graceful copy (a bad-but-in-range pointer that isn't mapped)
  is a deferred refinement — v0.8-era range-validates and trusts the
  mapping; a follow-on adds a recoverable copy.* This primitive is also
  what v0.9 IPC message buffers need.
- **Attribution is free.** `tracing::span_start` already reads the
  per-task `CURRENT_SPAN_CURSOR` for the parent and `current_task_id()`
  for `task_id`. A userspace span reuses that path verbatim — the
  userspace side is just "open(name)/close(id)," the kernel does the real
  emission, correctly parented and attributed.
- **Authority via capability.** Span emission is gated by a `SpanSink`
  capability granted at startup (alongside the existing `TelemetrySink`)
  — consistent with the v0.7b "authority is a capability, not ambient"
  thesis. Distinct from `TelemetrySink` because emitting spans and
  bumping a counter are different rights.
- **Return a span-id token; RAII guard in the runtime.** `span_open`
  returns an opaque `span_id`; `span_close(span_id)` ends it. The
  `snitchos-user` runtime wraps this in a `Span` guard whose `Drop`
  closes — mirroring the kernel's `Span` guard, so the program writes
  `let _s = tracer.span("worker_a.tick");`.
  - The guard holds `Option<SpanId>`: `SpanOpen` can be **refused** (quota),
    so `Drop` skips the close when the open never succeeded — otherwise it'd
    `SpanClose` a bogus id.
- **`drop()` is preemption-safe — by construction, not luck.** A userspace
  span held open across a `yield`/preemption closes correctly because (1) the
  span cursor is **per-task and restored on every context switch** (the
  existing `sched-span-survives-yield` machinery), (2) `SpanClose` is handled
  in the *closing task's own context*, so the current cursor *is* the
  opener's, (3) Rust's LIFO drop order matches the cursor stack, and (4) the
  **non-preemptible-kernel** v0.8 choice means the handler can't be preempted
  mid-cursor-update. Userspace can't store a kernel cursor pointer (as the
  kernel guard does defensively) — it doesn't need to: same task ⇒ same cursor.
- **`handle_span_close` validates the id against the cursor top** and refuses
  a mismatch (nonzero `a0`). A buggy/forged out-of-order close then can't
  silently desync even its *own* cursor; blast radius is that task's cursor
  regardless (can't touch another task or process). `mem::forget(span)`
  remains a self-inflicted, *observable* leak (an unterminated span on the
  wire) — same known weakness as the kernel guard, not worth defending now.

## Intern table & the userspace-name DoS (DONE: growable table)

Letting userspace name strings turns the intern table into a
resource-exhaustion surface. Two facts shaped the fix:

- **Interning runs pre-allocator** — `span!("kernel.boot")` interns long
  before `heap::init()` (`kmain`). So the table can't be a plain `Vec`.
- **The table dedups `register_or_lookup` by *pointer*** — fine for
  `'static` literals, but a userspace name repeated each tick would
  re-register and (formerly) overflow the fixed 128-entry array → panic.

**DONE — growable hybrid table** (`kernel-core::intern`): a fixed inline
region (`INLINE_CAP = 64`) for the pre-allocator boot prefix, spilling
into a heap-backed `Vec` once the allocator is up. Removes the arbitrary
cap and the panic; ids stay dense across the boundary. Invariant:
pre-heap strings (~10–20) must stay under `INLINE_CAP` (documented in the
struct). `lookup_by_content` (added earlier) gives content-dedup so a
repeated userspace name resolves instead of re-registering. O(n) lookup
is fine because `n` is bounded by the quota below (reasoning recorded at
the const).

**Per-process span-name quota (lands in `handle_span_open`, 3b-1).** With
the table growable, the *only* DoS bound is a quota on distinct names a
process introduces — which also bounds the permanent `Box::leak` of each
new name.

- State: `Process { span_names_registered: AtomicU32 }`,
  `MAX_SPAN_NAMES_PER_PROCESS` (≈16).
- Enforcement (under the intern lock already held for the lookup):
  `lookup_by_content(name)` → `Some` ⇒ reuse, free (repeats/shared names
  cost nothing); `None` ⇒ if `span_names_registered >= cap` **refuse**
  (nonzero `a0`, no panic), else leak+register and `+= 1`.
- Refusal is itself observable: snitch `snitchos.user.span_refused_total`.
- Counts only *names this process introduced* (a name another process
  already registered resolves free), so the sum across processes bounds
  total userspace names.
- TOCTOU: a process is single-threaded today; and the check+increment sit
  under the intern lock, so it's precise for free.

## Scope

- **Userspace spans: IN** (the point of this plan).
- **A standalone "log line" frame: out** — the string-in/intern API is
  realized through span *names*; a separate log primitive is additive
  later if wanted.
- **Worker telemetry:** rich `worker_x.tick` spans (via the new
  `SpanSink`) **plus** a per-worker progress counter (existing
  `TelemetrySink` pattern) so v0.8 has a simple liveness signal too.

## Goal

`task_a`/`task_b` run as **userspace processes** sharing one hart,
cooperating via a `yield` syscall, each emitting a `worker_x.tick` span
(name interned on demand by the kernel) and a progress counter — with no
kernel-mode `task_a`/`task_b` threads remaining.

## Decisions locked in

| decision | choice |
|---|---|
| Cooperation mechanism | `yield` syscall |
| Tracing API | string-in (`open(name: &str)`), kernel interns-or-reuses; no `StringId` crosses the boundary |
| User-buffer transfer | `copy_from_user`: range-validate `(ptr,len)` in the user VA window; direct read (satp is the user's during trap); fault-graceful copy deferred |
| Span attribution | reuse `CURRENT_SPAN_CURSOR` + `current_task_id()` — kernel-side, free |
| Authority | distinct `SpanSink` capability granted at startup |
| Intern table | **DONE** — growable hybrid: fixed `INLINE_CAP=64` inline (pre-allocator boot prefix) + heap `Vec` overflow; no fixed cap, no panic; O(n) lookup bounded by the quota |
| Userspace-name DoS bound | per-process span-name quota in `handle_span_open` — `MAX_SPAN_NAMES_PER_PROCESS≈16`, refuse-not-panic, `span_refused_total` snitched; repeats/shared names free |
| Span handle | `span_open -> span_id` token; runtime `Span` RAII guard closes on drop |
| Syscall ABI | append `Yield`, `SpanOpen`, `SpanClose` to `snitchos_abi::Syscall`; never renumber |
| Worker telemetry | `worker_x.tick` span + per-worker progress counter |
| Placement | both workers co-located on one hart (the point is sharing) |
| Wire format | none new — reuses `StringRegister` + `SpanStart`/`SpanEnd` + counter frames |

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production
code without a failing test. PR boundaries marked.

### Step 1: `Yield` syscall returns control to the scheduler

**Acceptance criteria**: a U-mode `ecall` with the `Yield` number causes
the kernel to `yield_now()` on the caller's behalf and later `sret` back
to the instruction after the `ecall`, registers intact. *Confirm before
coding.*
**RED**: (1a) `abi` host test `from_usize(2) == Some(Yield)`; (1b) itest
`user-yield-round-trips` — `hello` calls `yield_now()` before `exit()`;
assert a `ContextSwitch{Yield}` with the user task's id as `from`, and
`snitchos.user.exits_total == 1` (proves control returned past the
yield). The existing `userspace_emits_telemetry` still passes.
**GREEN**: append `Yield` to `abi`; `snitchos_user::yield_now()` wrapping
`ecall a7=Yield`; one `yield_now()` before `exit()` in `hello`;
`Some(Syscall::Yield) => crate::sched::yield_now(),` in
`handle_user_ecall` (existing `sepc += 4` handles resume).
**MUTATE**: `from_usize` arm + dispatch arm.
**KILL MUTANTS**: survivors (esp. dropped arm → unknown syscall).
**REFACTOR**: assess.
**Done when**: `--repeat 10` green, mutation reviewed, human approves.

> **PR boundary** — cooperation primitive, proven with one process.

### Step 2: `copy_from_user` range validation (pure, host-tested)

**Acceptance criteria**: a pure `kernel-core` function accepts a
`(ptr, len)` wholly inside the user VA window (below `KERNEL_OFFSET`,
non-wrapping, `len` within a cap) and rejects: zero-page, kernel-high-half
pointers, wraparound, over-long. No kernel wiring yet. *Confirm before
coding.*
**RED**: `kernel-core` unit tests for accept + each reject case.
**GREEN**: the bounds check.
**MUTATE**: each boundary (`<` vs `<=`, wrap check, len cap).
**KILL MUTANTS**: survivors.
**REFACTOR**: assess.
**Done when**: host + mutation green, human approves.

> **PR boundary** — pure validation, independently mergeable.

### Step 3: userspace tracing — split into sub-steps

Done so far (each its own commit):
- **3a-L1** ✅ cap-core `Object::SpanSink` + `invoke_span` + `Denied::WrongObject`.
- **3a-L2** ✅ `protocol::CapObject::SpanSink`; `CapTable::bootstrap` grants
  both caps; `run()` snitches both `CapEvent::Granted`s; itest
  `userspace-spansink-granted`.
- **3b-0** ✅ `InternTable::lookup_by_content` (content dedup) + growable
  hybrid table (inline + heap overflow); boot itest confirms pre-allocator
  path.

**3b-1 (the behavioral half) — split into green checkpoints:**

- **abi** ✅ `SpanOpen=3`/`SpanClose=4` (host-tested, 0 missed mutants).
- **CP1 — deliver + `SpanOpen` → `SpanStart`** ✅ `enter()` `a1` + 2-field
  `repr(C) Startup` + runtime `Tracer::open` + `handle_span_open` (cap
  check → SUM-guarded `copy_from_user` → on-demand intern via
  `register_or_lookup_owned` → `span_open_owned` emits `SpanStart`). itest
  `userspace-emits-span`, 0/10. *(Bug caught: `enter()` had the `a1` param
  but the asm wasn't wired — the struct-ABI footgun. Found via the capture.)*
- **#1 — syscall refusal observability** ✅ `Frame::SyscallRefused {
  syscall, reason, task_id, … }` + `RefusalReason` enum, emitted at every
  refusal site (`refuse()` helper). Denials are labelled wire events, not
  silent. itest `userspace-refusal-snitched`, 0/10. Collector passes it
  through (Prometheus `syscall_refused_total{reason}` export is a follow-on).
- **CP2 — `SpanClose` + RAII guard (NEXT).** `handle_span_close`:
  **validate the id against the cursor top, refuse a mismatch** (snitch
  `SyscallRefused`) → `span::close` → `SpanEnd`. Runtime `Span` guard
  holding `Option<SpanId>` (Drop skips close when open was refused).
  `hello` switches to `let _s = tracer.span(...)`. itest extends
  `userspace-emits-span` to assert the matching `SpanEnd`.
- **CP3 — per-process span-name quota.** In `handle_span_open`: on a
  *new* name, if `proc.span_names_registered >= MAX_SPAN_NAMES_PER_PROCESS`
  → `refuse(SpanOpen, RefusalReason::Quota)` (the reason already exists,
  thanks to #1) and do NOT register; else register + `+= 1`. Repeats/shared
  names cost nothing. itest: a program opening `>cap` distinct names is
  refused with `Quota`, the kernel keeps heartbeating.

**MUTATE** (per checkpoint): cap rights check, attribution
(`task_id`/parent), intern-or-reuse double-register, the
close-validates-cursor-top check, the quota boundary.

> **Follow-on itest (3b-2, after `Yield` + spans both exist):**
> `userspace-span-survives-yield` — a U-mode program opens a span, `yield`s
> mid-span, resumes, closes; assert `SpanStart → ContextSwitch(leave) →
> ContextSwitch(return) → SpanEnd` with matching span id. The userspace
> analog of `sched-span-survives-yield`; the direct proof `drop()` isn't
> confused by a context switch.

> **Deferred instrumentation (debuggability follow-ons, from the CP1
> delivery-bug retro):** (#2) a startup delivery self-check — the runtime
> echoes received handles, or `CapEvent::Granted` carries the local handle
> value — so an unwired-`a1`-style bug is self-evident at boot; (#4) a
> name-keyed capture summary (distinct span names / `user.*` metrics seen)
> so early-but-scrolled-off activity is queryable without a re-run.
> (#1 — reason-coded refusals — is **done**, above.)

> **PR boundary** (per checkpoint) — each of CP1/#1/CP2/CP3 is its own
> commit. Userspace tracing is the v0.9-enabling capability and the v0.8
> demo enricher.

### Step 4: A `worker` program — span + progress + yield, in a loop

**Acceptance criteria**: an embedded `worker` ELF loops { open
`worker.tick` span; bump progress counter; `yield`; close span }; a new
`workload=workers` running one worker shows its progress counter climb
and repeated `worker.tick` spans attributed to it. *Confirm before
coding.*
**RED**: itest asserting progress ≥ N and ≥2 `worker.tick` `SpanStart`s
for the worker within a few seconds.
**GREEN**: write `worker`; wire build + fixture + `include_bytes!`
(mirror `HELLO_ELF`); workload registry + `kernel-core::bootargs` arm.
**MUTATE**: bootargs parse arm.
**KILL MUTANTS / REFACTOR**: as usual.
**Done when**: counter + spans climb, `--repeat 10` green, human approves.

> **PR boundary** — the worker program exists and is observable.

### Step 5: Two workers share one hart, both observable

**Acceptance criteria**: `workload=workers` spawns **two** worker
processes on one hart; both progress counters climb and both emit
`worker.tick` spans (neither starves), with `context_switches_total > 0`
across the two userspace tasks. *Confirm before coding.*
**RED**: itest `two-userspace-workers-round-robin` — assert both
workers' progress > 0 and both emit spans within N seconds.
**GREEN**: spawn two `Process`es (distinct page tables + user stacks +
distinct counters/span names); co-locate on one hart; parameterise the
worker via `a0` (already threaded through `enter`) or two ELFs.
**MUTATE**: spawn-layout selection.
**KILL MUTANTS / REFACTOR**: as usual.
**Done when**: both observable, `--repeat 10` green, human approves.

> **PR boundary** — cooperative multi-userspace-task scheduling works.

### Step 6: Retire kernel-mode `task_a` / `task_b`

**Acceptance criteria**: the kernel `sched::spawn("task_a", …)`/`task_b`
calls and `demo_tasks::task_*_entry` are gone; the default demo's worker
tasks are the userspace ones; sched itest scenarios referencing
`task_a`/`task_b` are updated to the userspace workers. *Confirm before
coding — touches several scenarios.*
**RED**: update affected `scenarios.rs` to assert the userspace workers'
behaviour; they fail against the still-kernel workers.
**GREEN**: remove the kernel demo-task spawns + `demo_tasks`; point the
default workload at the userspace workers.
**MUTATE**: n/a (deletion) — rely on updated scenarios.
**KILL MUTANTS**: grep `task_a`/`task_b` for silently-lost coverage.
**REFACTOR**: remove now-dead span plumbing.
**Done when**: full `cargo xtask itest --repeat 10` green, human approves.

> **PR boundary** — kernel-mode demo workers removed; end state reached.
> v0.8 builds on `workload=user-hog` (a worker variant omitting `yield`).

## Pre-PR Quality Gate

1. Mutation testing — run `mutation-testing` skill.
2. Refactoring assessment — run `refactoring` skill.
3. `cargo xtask clippy` clean (host crates + kernel for riscv).
4. `cargo xtask itest --repeat 10` green for any step touching the
   scheduler / syscall / trap path (commit-gate rule).

## Open questions

- **One parameterised `worker` ELF vs two.** `enter` already passes
  `startup_a0`; a single ELF keyed on `a0` (worker id) is less bloat.
  Decide at Step 4.
- **Shared read-only text vs separate page tables** for the two workers.
  Shared text is the real-OS answer (one binary, two processes) and
  cheaper on frames; separate is simpler. Lean shared if `user::load`
  makes it easy.
- **`SpanSink` vs extending `TelemetrySink`.** Distinct cap is the
  cleaner thesis (different rights); fold-in is less plumbing. Leaning
  distinct — revisit at Step 3 if the grant path is heavier than
  expected.
- **Fault-graceful `copy_from_user`.** v0.8-era range-validates and
  trusts the mapping; a recoverable copy (handle an unmapped in-range
  pointer without panicking) is a follow-on, shared with v0.9 IPC.
- **Syscall numbers.** `Yield = 2`, `SpanOpen = 3`, `SpanClose = 4` —
  append-only; document beside `Invoke`/`Exit`.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
