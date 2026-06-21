# Plan: Userspace-defined metrics (debt #2)

**Work lands on:** `main` (no feature branches — see CLAUDE.md). User handles commits.
**Status:** Design locked (Q1=A, Q2=A, Q3=new syscalls, Q4=quota). Ready for Step 1.

## Goal

Let a userspace component **name and emit its own metrics** via a syscall,
instead of the kernel pre-registering every metric name. The kernel copies the
name from user memory, interns it, and transports an opaque `MetricRegister` +
`Metric` frame — it never decides *what* a userspace metric is called.

## Re-scoping the debt (important)

The register's #2 says "push the observability vocabulary out of the kernel /
~60 names hardcoded / intern table in kernel memory / kernel knows nothing of
names." Investigation narrows that:

- **Names already flow to the host and are never interpreted by the kernel.**
  `register_*` interns a name → emits a `StringRegister`/`MetricRegister` frame;
  the collector builds its own name table + `metric_kinds` from those frames.
  The kernel assigns ids and dedups; it doesn't read names.
- **The kernel naming its *own* metrics is legitimate** — frame/heap/sched
  telemetry is the kernel's own observability. That's not the debt.
- The actual layering inversion is: **userspace can't name a metric at all.**
  The FS denial gauge (`snitchos.fs.denied`) had to be kernel-registered and the
  FS server's bootstrap sink re-aimed at it (the `run_ipc_counter` hack).

**So this plan does NOT** rip out the kernel intern table or change how the
kernel names its own metrics (churn for ~no gain — names already transport). It
adds the missing capability: **userspace registers its own named metrics**,
reusing the existing span-name interning path + the fault-safe `copy_from_user`
(debt #6). The maximal "intern table → collector" idea is explicitly deferred /
questioned — capture in a follow-on only if a real need appears.

## What's already in place (reuse)

| Piece | Where | Reuse |
|---|---|---|
| Transient-name intern (content-keyed, leak-on-miss, quota) | `tracing::span_open_bounded` + `InternTable::lookup_by_content` | mirror for metrics |
| Per-process name quota | `Process::span_names_registered` / `MAX_SPAN_NAMES` | analogous metric-name quota |
| Fault-safe user-string copy | `copy_from_user` (debt #6) + `user_range_readable` | name copy |
| Metric kinds on the wire | `protocol::MetricKind` + `MetricRegister` frame; collector `metric_kinds` | no change |
| Emit path | `tracing::emit_metric(StringId, i64)` | reuse |
| Sink authority | `Object::TelemetrySink { counter: StringId }` + `invoke_telemetry` | **generalise — see Q1** |

## Design decisions (LOCKED)

- **Q1 = A.** `TelemetrySink` becomes "authority to register + emit named
  metrics" (no fixed counter); bootstrap pre-registers `snitchos.user.telemetry_total`
  and hands back its id so `telemetry().emit(v)` keeps working.
- **Q2 = A.** `RegisterMetric` returns the interned `StringId`; `EmitMetric` takes it.
- **Q3 = new syscalls** `RegisterMetric` + `EmitMetric` (gated by the sink cap);
  `Invoke` stays for the legacy fixed sink during migration.
- **Q4 = quota.** Per-process `MAX_METRIC_NAMES` + `Process::metric_names_registered`,
  mirroring `MAX_SPAN_NAMES`.

<details><summary>Original options (for the record)</summary>

**Q1 — Authority / sink model.** Today `TelemetrySink { counter: StringId }` is
"emit to this *one* pre-named counter" (`Invoke` → `emit_metric(counter, a1)`).
For userspace-named metrics the sink should be "authority to register + emit
metrics, names chosen by the holder." Options:
- **(A) Generalise `TelemetrySink`** to carry no fixed counter; `RegisterMetric`
  (gated by the sink cap) interns a name+kind → returns an id; `EmitMetric`
  (gated by the sink cap) emits `(id, value)`. The bootstrap pre-registers
  `snitchos.user.telemetry_total` and hands back its id so existing
  `telemetry().emit(v)` still works. *Cleanest end-state; changes the emit ABI.*
- **(B) Keep the fixed-name sink** (`Invoke` unchanged for `telemetry_total`) and
  add a *separate* register+emit path for components that need named metrics.
  *Less ABI churn, two mechanisms.*
- *Lean: A — the sink becomes a real telemetry authority; backward-compat via a
  pre-registered bootstrap id.*

**Q2 — Emit handle.** What does `RegisterMetric` return and `EmitMetric` take?
- **(A) the `StringId`** directly (already the wire key; simplest).
- **(B) a per-process metric handle** (slot index; hides kernel ids).
- *Lean: A — StringIds already cross the wire; no need to invent a second id.*

**Q3 — New syscalls vs generalise `Invoke`.** Add `RegisterMetric` +
`EmitMetric`, or fold emit into `Invoke`? *Lean: two new ambient-ish syscalls
gated by the sink cap (`Invoke` stays for the legacy fixed sink during
migration).*

**Q4 — Quota.** Per-process metric-name quota mirroring `MAX_SPAN_NAMES`
(unbounded registration is an intern-table DoS — same risk as span names).
*Lean: yes, a `MAX_METRIC_NAMES` quota + `Process::metric_names_registered`.*

</details>

## Steps

Each step is RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR; present acceptance criteria
and confirm before coding each. Pure logic host-tested (`kernel-core` / intern
table); bare-metal (syscall, runtime, FS server) pinned by itest.

### Step 1 — `register_metric_bounded` (kernel-side dynamic metric registration)

**Acceptance criteria:** a function that, given a transient `&str` name, a
`MetricKind`, a per-process quota counter, and a max, interns the name
content-keyed (dedup), refuses past quota, and on a fresh name emits a
`MetricRegister` and returns its `StringId` — the metric analogue of
`span_open_bounded`. The intern/quota/dedup logic that can be host-tested lives
where the intern table does; the frame emit is the kernel side.
**RED:** host tests on the intern table's content-keyed metric registration +
quota refusal (dedup returns the same id; past quota returns `None`).
**GREEN:** the function.
**MUTATE:** the intern/quota logic.
> **PR boundary** — pure-ish registration helper, independently mergeable.

### Step 2 — `RegisterMetric` + `EmitMetric` syscalls (per Q1–Q3)

**Acceptance criteria:** `abi::Syscall` gains the agreed syscalls; the handlers
validate the sink cap, copy the name via `copy_from_user`, call
`register_metric_bounded`, and emit; refusals snitch (`SyscallRefused`). An itest
proves a userspace program registers a custom-named metric and emits a value the
decoded wire shows under that name.
**RED:** itest — a probe program registers `"snitchos.probe.custom"` (gauge),
emits 42, scenario asserts a `MetricRegister` + `Metric{value=42}` under that name.
**GREEN:** the syscalls + runtime bindings (`snitchos_user::register_*`).
> **PR boundary** — the syscall surface + runtime API.

### Step 3 — First consumer: FS denial gauge self-registers (removes the `run_ipc_counter` hack)

**Acceptance criteria:** the FS server registers `"snitchos.fs.denied"` (gauge)
itself at startup and emits via the new path; the kernel-side `FS_DENIED_METRIC`
registration **and** the `run_ipc_counter` special-casing are removed — the FS
server gets a plain bootstrap sink like every other IPC process. The existing
`fs-lookup-rights-gate` itest still asserts the structured denial under
`snitchos.fs.denied`.
**RED:** `fs-lookup-rights-gate` (already exists) stays green after the FS server
moves to self-registration; a `--repeat 10` confirms no flake.
**GREEN:** FS server change + delete the kernel special-casing.
> **PR boundary** — the migration; proves the mechanism end-to-end and pays down
> the FS-era debt.

### Step 4 (optional) — runtime ergonomics + telemetry_total migration

**Acceptance criteria:** a clean `snitchos_user` API (`register_counter(name) ->
Metric`, `Metric::emit(value)`); decide whether the bootstrap
`snitchos.user.telemetry_total` becomes runtime-registered or stays
pre-registered for the existing markers. Additive; no behaviour change to
existing scenarios.
> **PR boundary** — polish; do only if the API proves clunky in Steps 2–3.

## Explicitly out of scope

- Ripping out / relocating the kernel intern table (names already transport;
  the table is a reasonable dedup mechanism).
- Changing how the kernel names its *own* metrics.
- A content-hash id scheme (would only matter if the intern table moved).

These are the maximal "kernel knows nothing of names" framing — deferred unless a
concrete need appears, captured then in its own plan.

## Pre-PR Quality Gate

1. `cargo mutants` on touched host crates (intern table / `kernel-core`).
2. `cargo xtask itest --repeat 10` on touched scenarios (the metric/FS ones).
3. `cargo xtask clippy` (whole workspace).
4. Wire stability: no renumbered `Frame` / `MetricKind` / `Syscall` discriminants
   (append-only).

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
