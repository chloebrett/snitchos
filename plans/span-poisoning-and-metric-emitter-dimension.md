# Plan: Span-name per-process scoping + metric emitter dimension

**Work lands on:** `main` (no feature branches — see CLAUDE.md). User handles commits.
**Status:** Parts A & B SHIPPED — plan complete. Both observability follow-ons closed; this file can be deleted once committed.

## Goal

Close the two observability follow-ons the userspace-defined-metrics plan parked
(both rooted in the same principle — *no cross-process name dedup, so emitter
identity must be explicit*):

- **Part A — span-name poisoning + disclosure.** The userspace `SpanOpen` path
  content-dedups span names against the **global** intern table, so a process can
  (a) emit a span under *another process's or the kernel's* name (e.g. open
  `"kernel.heartbeat"` and get the kernel's `StringId`), and (b) probe which names
  exist system-wide / free-ride the quota (a name another process already
  registered costs this process nothing). Same class of bug `telemetry_total` had,
  fixed the same way: per-process scoping.
- **Part B — collector emitter dimension.** With no cross-process dedup, two
  processes that register a metric with the **same name** get two `StringId`s that
  resolve to the same string. The collector keys Prometheus output by `name_id` and
  emits one `# TYPE`/`# HELP` per `name_id`, so two same-named metrics become
  **duplicate metric families — invalid Prometheus exposition**. Distinguish them
  with an emitter label sourced from the registering task.

## Background — the exact mechanisms

**Part A.** `kernel/src/obs/tracing.rs::span_open_bounded` (the body of the
`SpanOpen` syscall) does, under the intern lock:
```rust
if let Some(id) = table.lookup_by_content(name) { id }          // <-- cross-process/kernel dedup
else if registered.load() >= max { return None }                // per-process quota (count only)
else { let leaked = Box::leak(...); table.register_or_lookup(leaked, sink) }
```
`lookup_by_content` scans the **global** `INTERN_TABLE`, so a name the kernel or any
other process already registered resolves to *their* id (poisoning) and costs no
quota (disclosure/free-ride). The per-process state today is just a bare counter,
`Process.span_names_registered: AtomicU32`. `SpanStart` already carries `task_id`,
so the collector attributes spans correctly regardless — Part A is **kernel-only**
(no wire or collector change). `lookup_by_content` is used *only* here (kernel spans
go through pointer-keyed `register_or_lookup`; metrics through `register_metric`), so
Part A removes its last caller — delete it (and its tests).

**Part B.** `Frame::Metric { name_id, value, t, hart_id }` (no `task_id`);
`Frame::MetricRegister { name_id, kind }` (no emitter). Collector
(`collector/src/state.rs`, `collector/src/prom.rs`): `metric_values:
HashMap<(name_id, hart_id), i64>`; Prometheus export `group_by_name` groups by
`name_id` and writes `# TYPE`/`# HELP` + `name{hart="N"} value` per name_id. Two
name_ids → same string → two families with the same name → invalid. Spans avoid this
because `SpanStart.task_id` + `ThreadRegister{id,name}` give the collector a
`thread.name` attribute (`state.rs` resolves `thread_names[task_id]` at `SpanEnd`,
`otlp.rs` attaches it). The metric path has no such resolution step — Part B adds one.

## Design decisions (LOCKED)

- **A: per-process span-name table, no cross-process dedup.** Each process interns
  its span names into its *own* bounded table; a novel name always leaks a fresh
  global `StringId` (so its `"kernel.heartbeat"` is a distinct id from the kernel's)
  and always costs one quota slot. Repeats *within the process* resolve to its own
  id (no re-leak). Mirrors `MetricTable` but content-keyed.
- **B: stamp the emitter on `MetricRegister`, not `Metric`.** A `name_id` is
  per-process by construction (only its registering process emits to it), so the
  emitter is fixed at registration — no need to bloat the per-sample hot path.
  Append `task_id: u32` to `MetricRegister` (postcard-positional append at the END,
  exactly like `ThreadRegister.priority`); `NO_EMITTER = u32::MAX` marks kernel-global
  metrics (registered via the `&'static` `register_counter/gauge/histogram` path),
  which stay **label-free** for back-compat. Userspace metrics (the `RegisterMetric`
  syscall) carry the registering `current_task_id()`.
- **B export shape:** Prometheus export groups by resolved **name string** (one
  `# TYPE`/`# HELP` per name), then writes one line per (emitter, hart). Label rule,
  mirroring spans' conditional `thread.name`: `task="<thread name>"` if the emitter
  resolves via `thread_names`; `task="<id>"` if it's a real-but-unresolved id;
  **no `task` label** if `NO_EMITTER` (kernel-global — preserves existing series).
- **Rejected alternatives (B):** (1) `task_id` on every `Metric` frame — hot-path
  bloat for no gain. (2) emitter encoded into the metric name string — pollutes the
  intern table. (3) label by raw `name_id` — distinct but unfriendly (an opaque
  kernel id, not a process name).

## What's already in place (reuse)

| Piece | Where | Reuse |
|---|---|---|
| Per-process bounded name table pattern | `kernel_core::metric::MetricTable` (`is_full`, host tests, mutants) | model `SpanNameTable` on it |
| Fresh-id interning (no dedup) | `InternTable::register_or_lookup` (pointer-keyed) | leak → fresh `StringId` per process |
| task_id→name resolution | collector `thread_names` + `ThreadRegister` handling | emitter label source |
| Append-a-field-to-a-struct-variant precedent | `ThreadRegister.priority` ("appended at the END so postcard compat holds") | `MetricRegister.task_id` |
| Span quota refusal already asserted | itest `userspace-span-flood` (`SyscallRefused{Quota}`) | stays green (table-full == quota) |

---

## Part A — per-process span-name scoping (kernel-only) ✅ SHIPPED

> **Landed.** `kernel_core::span_name::SpanNameTable` (content-keyed, bounded;
> 6 host tests, mutants 6 caught / 1 unviable / **0 missed**). `Process` now holds
> `span_names: Mutex<SpanNameTable>` (replacing the `AtomicU32` counter + the
> `Process::MAX_SPAN_NAMES` const, now on the table). `span_open_bounded` resolves
> against the caller's own table — a new name (even one the kernel uses) leaks a
> *fresh* id. New itest `span-name-not-poisonable` (probe opens `"kernel.heartbeat"`,
> asserts a second distinct `StringRegister`); full suite **73/0**, span scenarios
> **40/40** on `--repeat 10`; clippy clean.
>
> **Deviation from plan:** kept `InternTable::lookup_by_content` (added a hazard
> doc note) rather than deleting it — the overflow-spill test uses it as a
> cross-region scan oracle (distinct-path coverage). The fix is that
> `span_open_bounded` no longer *calls* it; deleting the method is deferred cleanup.

### A1 — `SpanNameTable` (host-tested; pure data structure)

`kernel_core::user::span_name::SpanNameTable` (re-export `kernel_core::span_name`),
modelled on `MetricTable`. Holds `Vec<(&'static str, StringId)>`.
- `resolve(&self, name: &str) -> Option<StringId>` — content scan (O(n), n ≤ cap).
- `is_full(&self) -> bool` — at `MAX_SPAN_NAMES`.
- `insert(&mut self, name: &'static str, id: StringId)` — append (caller checked `!is_full`).
- `MAX_SPAN_NAMES: usize = 16` (the cap is the quota; the const moves here from
  `Process`).

**RED:** host tests — resolve-miss is `None`; insert then resolve returns the id;
two distinct names get distinct entries; the same content resolves to the same id;
`is_full` true exactly at the cap; insert up to the cap, never beyond.
**GREEN:** the table. **MUTATE:** `cargo mutants` (the content compare, the `>=`
capacity boundary).
> PR boundary: pure data structure. Like `MetricTable`'s 5a, this is host-green but
> the kernel won't build until A2 — so A1+A2 land together.

### A2 — wire `SpanNameTable` into the span path

`Process`: replace `span_names_registered: AtomicU32` with
`span_names: Mutex<SpanNameTable>` (behind the same `Mutex` discipline as `caps`/
`metrics`; never held across `sret`/`yield_now`). `span_open_bounded` takes the
process's span table instead of `(registered, max)`:
```rust
// under the process's span-name lock:
if let Some(id) = t.resolve(name) { id }                 // per-process repeat
else if t.is_full() { return None }                      // quota → SyscallRefused{Quota}
else { let leaked = Box::leak(...); let id = INTERN_TABLE.register_or_lookup(leaked, sink);
       t.insert(leaked, id); id }
```
Update `handle_span_open` (`syscall/span.rs`) to pass `&proc.span_names`. Delete
`Process::MAX_SPAN_NAMES` (moved to the table) and `InternTable::lookup_by_content`
+ its tests (last caller gone).
**Verification (integration):** add an itest `span-name-not-poisonable` — a probe
program opens a span named `"kernel.heartbeat"` (a name the kernel also uses); assert
the wire shows a **second** `StringRegister` for `"kernel.heartbeat"` with a distinct
id, and the program's `SpanStart` carries *that* id (not the kernel's). Existing
`userspace-span-flood` (`SyscallRefused{Quota}`) and all span scenarios stay green;
`--repeat 10`.
> PR boundary: A1+A2 are one coherent kernel change, suite-verified.

---

## Part B — collector emitter dimension for metrics (wire + collector) ✅ SHIPPED

> **Landed.** `Frame::MetricRegister` gained `task_id: u32` (appended; `PROTOCOL_VERSION`
> 3→4) + `protocol::NO_EMITTER = u32::MAX`. `InternTable::register_metric` threads
> the emitter; `register_user_metric` stamps `current_task_id()`, the kernel
> `register_counter/gauge/histogram` stamp `NO_EMITTER`. Collector: `metric_emitters`
> map + `metric_emitter_label` accessor; `format_metrics` rewritten to **group by
> name string** (one `# TYPE`/`# HELP` per name) with a per-series `task="…"` label
> (omitted for `NO_EMITTER`, numeric-id fallback when the emitter isn't yet named).
> 3 new collector tests (collision → one family/two series; kernel metric → no label;
> unnamed emitter → numeric id); `userspace-custom-metric` strengthened to assert a
> real emitter on the wire. Host tests green (collector 57, protocol 34, kernel-core
> 371); full itest **73/0**, metric scenarios **50/50** on `--repeat 10`; mutants
> **78 caught / 12 unviable / 0 missed** across `prom`/`state`/`intern`; clippy clean.

### B1 — `MetricRegister.task_id` (protocol; host-tested)

`Frame::MetricRegister { name_id, kind, task_id: u32 }` — append `task_id` at the
END (postcard compat, per the `ThreadRegister.priority` precedent). Add
`pub const NO_EMITTER: u32 = u32::MAX;` (with doc: "kernel-global metric, no emitter
label"). Update `OwnedFrame::from_borrowed` (`protocol/src/stream.rs`), the
`MetricRegister` roundtrip test, and **bump `PROTOCOL_VERSION`**.
**RED:** roundtrip test carries a non-zero `task_id`. **GREEN:** the field.

### B2 — `InternTable::register_metric` carries the emitter (kernel-core; host-tested)

`register_metric(name, kind, task_id, sink)` — thread `task_id` onto the emitted
`MetricRegister`. **RED:** update the intern tests to assert the `MetricRegister`
frame carries the passed `task_id`. **GREEN:** the param. **MUTATE:** kernel-core.

### B3 — kernel stamps the registering task (kernel; builds)

`tracing::register_user_metric` passes `current_task_id().0`;
`register_counter/gauge/histogram` (the `&'static` kernel path) pass
`protocol::NO_EMITTER`. (Per-task kernel metrics already disambiguate by name, so
they stay `NO_EMITTER` too.)

### B4 — collector: emitter map + group-by-name export (host-tested)

`state.rs`: add `metric_emitters: HashMap<u32 /*name_id*/, u32 /*task_id*/>`,
populated on `MetricRegister`. `prom.rs`: group the export by resolved **name
string** (one `# TYPE`/`# HELP` per name); for each (name_id, hart) line, attach
labels: `hart="N"` (as today) **and** `task="…"` per the label rule above
(`NO_EMITTER` → omit). **RED:** new collector tests — (1) two name_ids, same name,
two emitters → one family, two series distinguished by `task` (no duplicate
`# TYPE`); (2) a `NO_EMITTER` metric → unchanged, no `task` label (back-compat
pin, mirroring `span_attributes_omit_thread_name_when_unresolved`); (3) emitter
resolved via `thread_names` → `task="<name>"`, unresolved real id → `task="<id>"`.
**GREEN:** the map + grouped export. **MUTATE:** collector (`prom`/`state`).
> PR boundary: B1–B4 land together (the `MetricRegister` encoding changes, so the
> kernel emits and the collector decodes the new field in lockstep); the itest
> capture corpus regenerates. The collector collision is a *unit*-testable concern —
> B4's tests are the primary gate. An end-to-end itest needs two processes
> registering the **same** metric name; defer unless cheap (a `double-probe`
> workload) — note it, don't block on it.

---

## Ordering

Parts A and B are **independent** (A is kernel-only span interning; B is wire +
collector for metrics) and can land in either order or in parallel. Suggested: **A
first** (smaller, kernel-only, closes the security hole), then **B** (the
robustness fix the collector agent flagged as currently untested). Neither blocks
the other.

## Out of scope / follow-ons

- **Span-name GC.** Leaked `'static` span names are still never reclaimed (now
  bounded per-process by the quota, as before). Reclamation stays deferred.
- **End-to-end same-name-metric itest.** A `double-probe` workload (two processes
  registering `snitchos.probe.custom`) would prove B end-to-end; B4's collector unit
  tests cover the logic, so this is optional polish.
- **Emitter label for spans.** Not needed — spans already carry `task_id` →
  `thread.name`. Listed only to record it was considered.

## Pre-PR Quality Gate

1. `cargo mutants` on touched host crates (`kernel-core` — `SpanNameTable`,
   `register_metric`; `collector` — `prom`/`state`).
2. `cargo xtask itest --repeat 10` on touched scenarios (span + the new poison
   probe for A; metric/fs/userspace for B).
3. `cargo xtask clippy` (whole workspace).
4. Wire stability: `MetricRegister.task_id` appended at the END only;
   `PROTOCOL_VERSION` bumped; no reordered `Frame`/`MetricKind`/`Syscall` discriminants.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
