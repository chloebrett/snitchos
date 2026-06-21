# Plan: Userspace-defined metrics (debt #2)

**Work lands on:** `main` (no feature branches — see CLAUDE.md). User handles commits.
**Status:** Step 1 SHIPPED (`kernel_core::metric::MetricTable` — host-tested, mutants all caught). Next: Step 2 (`RegisterMetric` + `EmitMetric` syscalls).

## Goal

Let a userspace component **name and emit its own metrics** via a syscall, in its
**own per-process namespace** — the kernel copies the name from user memory,
interns it for an *id + wire frame only*, and lets the process emit to handles it
registered. The kernel never decides what a userspace metric is called, and one
process can neither read nor pollute another's (or the kernel's) metrics.

## Re-scoping the debt

The register's #2 framed this maximally ("rip the intern table out, kernel knows
nothing of names"). Investigation narrows it:

- **Names already flow to the host and are never interpreted by the kernel** —
  `register_*` emits `StringRegister`/`MetricRegister`; the collector builds its
  own name table + `metric_kinds`. The kernel assigns ids and dedups; it doesn't
  read names.
- **The kernel naming its *own* metrics is legitimate** (frame/heap/sched
  observability). Not the debt.
- The real inversion: **userspace can't name a metric at all** — the FS denial
  gauge had to be kernel-registered and the FS sink re-aimed (`run_ipc_counter`).

So this plan adds **userspace-named metrics**; it does **not** relocate the kernel
intern table or change how the kernel names its own metrics.

## Security: why per-process, no global dedup

The naive design (reuse the global, content-deduped intern table for userspace
names) is **insecure** — the table is shared across all processes *and* the
kernel's own metrics:

- **Poisoning (serious).** Content-dedup returns a *shared* `StringId`. A process
  registers `"snitchos.frames.allocated_total"` → gets the *kernel's* id → emits
  into the kernel's stream. Or registers `"snitchos.fs.denied"` → pollutes the FS
  server's gauge. A poisonable snitch defeats the project's whole premise. (Worse
  if emit takes a raw `StringId`: a process could emit to any id without even
  registering.)
- **Disclosure (minor).** Returned-id ordering / quota-charge lets a process
  probe which names exist system-wide — ambient authority a cap OS shouldn't grant.

**Fix — scope names per authority:**
- **No cross-process dedup.** Each process's registration gets its *own* `StringId`
  (two processes' `"foo"` are distinct metrics — correct; different emitters).
- **Emit authorized by registration.** Emit takes a **per-process metric handle**;
  the kernel only resolves handles the process itself registered. No emitting to
  ids you didn't create; no colliding with the kernel or another process.

The kernel still uses the global intern table to *allocate an id + emit the wire
frames* (the collector needs the name once), but **`lookup_by_content` is not
used for cross-process dedup** — that shared-id path is the vulnerability.

> The existing **span-name** path (`span_open_bounded`) has the same global-dedup
> disclosure/poisoning issue — logged as a sibling follow-on, not fixed here.

## Design decisions (LOCKED)

- **Q1 = A.** `TelemetrySink` becomes "authority to register + emit named metrics"
  (no fixed counter); bootstrap pre-registers `snitchos.user.telemetry_total` and
  hands back its handle so `telemetry().emit(v)` keeps working.
- **Q2 = B (security-revised).** `RegisterMetric` returns a **per-process metric
  handle** (index into the process's metric table); `EmitMetric` takes it. The
  kernel maps handle → `StringId` per process. Distinct `StringId` per emitter
  (no cross-process dedup).
- **Q3 = new syscalls** `RegisterMetric` + `EmitMetric` (gated by the sink cap);
  `Invoke` stays for the legacy fixed sink during migration.
- **Q4 = quota.** Per-process metric table is bounded (`MAX_METRIC_NAMES`),
  mirroring `MAX_SPAN_NAMES`. The table's capacity *is* the quota.

## What's already in place (reuse)

| Piece | Where | Reuse |
|---|---|---|
| Fault-safe user-string copy | `copy_from_user` (#6) + `user_range_readable` | name copy |
| Id allocation + wire frames | `InternTable::register_metric(&'static, kind, sink)` → `StringRegister` + `MetricRegister` | allocate id + emit (per registration, **no** `lookup_by_content`) |
| Metric kinds on the wire | `protocol::MetricKind`; collector `metric_kinds` | no change |
| Emit path | `tracing::emit_metric(StringId, i64)` | reuse |
| Per-process handle table pattern | `kernel_core::user::cap::CapTable` (handle → object) | model the metric table on it |

## Prefactor decision (researched — additive-first, unify-last)

Considered prefactoring the existing telemetry path before building this. Finding:
`SpanSink` is **already** the clean authority-only shape (`invoke_span` just
validates the EMIT right; the name arrives per-call). `TelemetrySink { counter }`
is the odd one out — name fused into the cap, `invoke_telemetry` (11 lines)
returns it. So the "prefactor" is "make `TelemetrySink` look like `SpanSink`."

**Decision: do NOT prefactor first.** The new mechanism (per-process `MetricTable`
+ `RegisterMetric`/`EmitMetric`) is **genuinely additive** — separate handle
table, separate syscalls, reuses `copy_from_user` (#6). It validates the
authority-only target *on its own*. Prefactoring first means rewriting the
working emit path (`Invoke` → fused counter → `telemetry_total`, which every
program's markers depend on) toward an *unvalidated* target, plus an awkward
"`Invoke` = emit to default handle 0" to stay behavior-preserving. Risk + contortion
for no benefit.

**So: keep the order below (Step 1 = `MetricTable`, additive), and add a FINAL
unify step** — once Steps 2–3 prove the SpanSink-style path end-to-end, retire the
legacy `Invoke`/fused-counter path, drop `{ counter }` from `TelemetrySink`, and
migrate `telemetry_total` onto the register path. That cleanup is the prefactor —
done *last*, against a proven target, netted by existing itests. (The one real
existing-debt item, the `run_ipc_counter` hack, is already removed in Step 3 when
the FS server self-registers — coupled to the feature, can't go earlier.)

Principle: **refactor toward a validated target, not a hoped-for one.**

### Step 5 (was 4) — unify + retire legacy (the deferred prefactor)

**Acceptance criteria:** legacy `Invoke`/fused-counter path retired; `TelemetrySink`
becomes authority-only (`{ counter }` dropped, mirroring `SpanSink`);
`telemetry_total` migrated onto the register path; existing marker-emitting
scenarios stay green. **Done last**, after the new path is proven in Steps 2–3.

## Steps

Each step is RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR; present acceptance criteria
and confirm before coding each. Pure logic host-tested (`kernel-core`); bare-metal
(syscall, runtime, FS server) pinned by itest.

### Step 1 — per-process `MetricTable` (the security boundary; host-tested) ✅ SHIPPED

> Landed as `kernel_core::metric` (`MetricTable` + `MetricHandle`), re-exported at
> `kernel_core::metric`. `MAX_METRIC_NAMES = 16` (mirrors `Process::MAX_SPAN_NAMES`).
> `register(StringId) -> Option<MetricHandle>` (append-only, `None` at the cap);
> `resolve(MetricHandle) -> Option<StringId>` (index validated against this table).
> 6 host tests, clippy clean, `cargo mutants --file …/metric.rs` → all caught.


**Acceptance criteria:** a `kernel_core` `MetricTable` (modelled on `CapTable`):
`register(StringId) -> Option<MetricHandle>` appends if under `MAX_METRIC_NAMES`
(else `None`); `resolve(MetricHandle) -> Option<StringId>` returns the id only for
a handle this table issued (out-of-range / never-issued → `None`). This is what
stops a process emitting to a metric it didn't register.
**RED:** host tests — register up to the cap returns distinct handles; one past
the cap returns `None`; `resolve` of an issued handle returns its id; `resolve` of
an out-of-range handle returns `None`.
**GREEN:** the table.
**MUTATE:** `cargo mutants` (the `<`/`==` capacity boundary, the bounds check).
> **PR boundary** — pure data structure, independently mergeable.

### Step 2 — `RegisterMetric` + `EmitMetric` syscalls + runtime bindings

**Acceptance criteria:** `abi::Syscall` gains the two syscalls (gated by the sink
cap). `RegisterMetric` copies the name (`copy_from_user`), interns it (fresh id +
frames), `MetricTable::register` → handle in `a0`. `EmitMetric` resolves the
handle against the *caller's* table → `emit_metric(id, value)`; an unregistered
handle is **refused** (`SyscallRefused`), not silently emitted. An itest proves a
probe program registers `"snitchos.probe.custom"` (gauge), emits 42 (wire shows
`MetricRegister` + `Metric{42}` under that name), and that emitting an
unregistered handle is refused.
**RED:** itest `userspace-custom-metric` (register + emit + the refusal path).
**GREEN:** the syscalls + `snitchos_user::{register_metric, …}`.
> **PR boundary** — the syscall surface + runtime API + the per-process table wired
> onto `Process`.

### Step 3 — First consumer: FS denial gauge self-registers (removes the `run_ipc_counter` hack)

**Acceptance criteria:** the FS server registers `"snitchos.fs.denied"` (gauge)
itself and emits via the new path; the kernel-side `FS_DENIED_METRIC` registration
**and** the `run_ipc_counter` special-casing are removed — the FS server gets a
plain bootstrap sink like every other IPC process. The existing
`fs-lookup-rights-gate` itest still asserts the structured denial under
`snitchos.fs.denied`.
**RED:** `fs-lookup-rights-gate` stays green after the migration; `--repeat 10`.
**GREEN:** FS server change + delete the kernel special-casing.
> **PR boundary** — the migration; pays down the FS-era debt.

### Step 4 (optional) — runtime ergonomics + telemetry_total migration

**Acceptance criteria:** a clean `snitchos_user` API (`register_counter(name) ->
Metric`, `Metric::emit(value)`); decide whether bootstrap `telemetry_total` stays
pre-registered (for existing markers) or becomes runtime-registered. Additive.
> **PR boundary** — polish; only if the API proves clunky in Steps 2–3.

## Out of scope / follow-ons

- **Collector emitter dimension** — with no cross-process dedup, two processes
  naming a metric the same become two `StringId`s both labelled that name. A
  `task_id`/process label keeps them distinct in Prometheus. *Deferred*: no
  collision today (kernel per-task metrics already embed the task in the name; the
  FS gauge has a single emitter). Add when a real two-process-same-name case lands.
- **Span-name path** has the same global-dedup disclosure/poisoning issue — its
  own follow-on plan (apply per-process scoping there too).
- **Relocating the kernel intern table / a content-hash id scheme** — the maximal
  "kernel knows nothing of names." Names already transport; the table is a fine
  id-allocator. Deferred unless a concrete need appears.

## Pre-PR Quality Gate

1. `cargo mutants` on touched host crates (`kernel-core` — `MetricTable`).
2. `cargo xtask itest --repeat 10` on touched scenarios (metric/FS).
3. `cargo xtask clippy` (whole workspace).
4. Wire stability: no renumbered `Frame` / `MetricKind` / `Syscall` discriminants
   (append-only).

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
