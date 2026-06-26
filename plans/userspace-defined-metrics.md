# Plan: Userspace-defined metrics (debt #2)

**Work lands on:** `main` (no feature branches — see CLAUDE.md). User handles commits.
**Status:** Steps 1–4 SHIPPED. Step 1: `kernel_core::metric::MetricTable` (host-tested, mutants all caught). Step 2: `RegisterMetric`/`EmitMetric` syscalls + runtime bindings + `userspace-custom-metric` itest. Step 3: FS server self-registers `snitchos.fs.denied`; kernel special-casing (`FS_DENIED_METRIC`, `CounterKind`, `run_ipc_counter`) deleted. Step 4: ergonomic `register_counter/gauge/histogram(name) -> Metric` free fns + inert-`Metric` no-op-on-refusal; `telemetry_total` stays pre-registered. **Step 5 SHIPPED (option C): `Invoke` removed + ABI renumbered (`Exit = 0` … `Wait = 18`); `TelemetrySink` is authority-only (`{ counter }` dropped); `telemetry_total` deleted in favour of per-process `snitchos.<task>.marker` metrics (12 programs + 14 scenarios migrated). Full suite 72/0; migrated scenarios 150/150 on `--repeat 10`. ALL FIVE STEPS DONE — debt #2 paid. This file can be deleted once committed.**

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

**Decision (option C): retire `telemetry_total` entirely; markers become
per-process custom metrics.** Investigation (2026-06): `snitchos.user.telemetry_total`
is the counter bound to every process's bootstrap `TelemetrySink`, written by the
legacy `Invoke` syscall. But despite the name **nothing uses it as a total/rate** —
all ~15 scenarios assert a *specific value* (`STAT_ROOT_OK`, `42`, `524288`, an
echoed RPC result…). It's a v0.7a **test-signal bus mislabeled as a counter**. So
we don't keep a shared `telemetry_total`: each program registers its **own** marker
metric on the Step-2 path and emits through the handle. This retires `Invoke` with
no shared-counter carve-out, deletes a misleading metric, and **sidesteps the
collector-emitter-dimension collision** (no shared name → no multi-series problem),
which is why it's preferred over keeping `telemetry_total` per-process.

`Invoke` is purely the telemetry path (`handle_invoke` → `invoke_telemetry` →
`emit_metric` to the bound counter), so retiring telemetry-via-`Invoke` retires
`Invoke` wholesale. `handle_invoke` and `worker`'s `sink.emit(progress)` are
included (a computed value migrates identically to a sentinel).

**Naming convention:** one counter per program, `snitchos.<task>.marker`,
registered once at startup; markers emit through its handle (a program emitting
several distinct sentinels uses one metric, many values — exactly as today).
Mapping: `hello`→`snitchos.hello.marker`, `faulter`→`snitchos.faulter.marker`,
`bad_ptr`→`snitchos.bad_ptr.marker`, `span_flood`→`snitchos.span_flood.marker`,
`heap_grow`→`snitchos.heap_grow.marker`, `spawner`→`snitchos.spawner.marker`,
`ipc_receiver`→`snitchos.ipc_receiver.marker`, `rpc_client`→`snitchos.rpc_client.marker`,
`badge_handout_server`→`snitchos.badge_handout.marker`,
`worker_a`/`worker_b`→`snitchos.worker_a.marker`/`…worker_b.marker`,
`fs_client`→`snitchos.fs_client.marker`. (Counter, for parity; Gauge is also
defensible since the value is "last marker," not a sum — impl call.)

**Acceptance criteria:** legacy `Invoke`/fused-counter path retired; `TelemetrySink`
becomes authority-only (`Object::TelemetrySink { counter }` → unit, mirroring
`SpanSink`); no `snitchos.user.telemetry_total`; every former marker emitter uses a
per-process metric on the register path; all affected scenarios stay green.

#### Sub-steps

> **Correction (sequencing):** 5a is host-test-green but **not** a green-tree
> checkpoint — dropping `{ counter }` breaks the kernel build until 5b. So 5a is
> not independently mergeable; all of 5a–5e land as one commit. The only
> full-green points are after 5a (kernel-core tests) and after 5e (build + suite).

**5a — cap layer authority-only (host-tested). ✅ DONE.**
`kernel_core::cap`: `Object::TelemetrySink { counter }` → unit variant;
`CapTable::bootstrap(counter)` → `bootstrap()` (bare telemetry + span caps);
replace `invoke_telemetry(table, handle) -> Result<StringId, Denied>` with
`authorize_telemetry(table, handle) -> Result<(), Denied>` (checks `EMIT` + the
`TelemetrySink` object, returns nothing). **RED:** update the cap.rs host tests
(`bootstrap`, the `TelemetrySink { counter }` constructions, `invoke_telemetry`
cases) to the new shapes. **GREEN:** the cap changes. **MUTATE:** cap.rs.
> PR boundary: pure data structure — lands alone, kernel catches up in 5b.
> **Landed:** 49 cap tests green (365 kernel-core total), clippy clean, mutants
> 34 caught / 16 unviable / **0 missed**. The `StringId` import is gone (sink
> carries no `protocol` type). Kernel build is intentionally red until 5b
> (callers still name `invoke_telemetry` / `bootstrap(counter)` / `{ counter }`).

**5b — kernel un-thread the counter + retire `Invoke`. ✅ DONE.**
`trap/user.rs`: deleted `USER_METRIC` + `user_metric_id()` + its `init_metric`
registration; `Process::bootstrap()` drops the `telemetry_counter` param; `run` /
`run_ipc` stop resolving a counter. `syscall/cap.rs`: deleted `handle_invoke`;
`RegisterMetric`'s cap-gate failure now bumps `cap.denied_total` (parity with
`MintBadged` — needed because `hello`'s adversarial probe moved off `Invoke`).
`syscall/metric.rs`: gates `RegisterMetric` via `authorize_telemetry`.
**`Invoke` was removed outright and the ABI renumbered** (`Exit = 0` … `Wait = 18`):
syscall numbers are a build-time register ABI — kernel + userspace rebuild from
`abi` together and nothing persists a number (the collector ignores the
`SyscallRefused` syscall byte), so renumbering on removal is safe, unlike the
postcard `Frame` discriminants. `from_usize` + the round-trip test updated.

**5c — runtime drop the legacy emit. ✅ DONE.**
`snitchos_user`: removed `TelemetrySink::emit` and the private `invoke()` helper
(only `emit` used it); `telemetry()` now yields a register-only authority. `Denied`
stays (IPC uses it).

**5d — migrate the marker emitters (12 programs). ✅ DONE.**
Each program calling `telemetry().emit(…)` / `sink.emit(…)` now `register_counter`s
its `snitchos.<task>.marker` once at startup and emits through the handle: `hello`,
`faulter`, `bad-ptr`, `span-flood`, `heap-grow`, `spawner`, `ipc-receiver`,
`rpc-client`, `badge-handout-server`, `worker_a`, `worker_b`, `fs-client`. `hello`'s
adversarial wrong-object probe moved from `TelemetrySink::emit` to
`TelemetrySink::from_raw_handle(1).register_metric(…)` (same `CapWrongObject` snitch).

**5e — repoint scenarios + verify. ✅ DONE.**
14 scenario assertions swapped `snitchos.user.telemetry_total` → the emitting
program's `snitchos.<task>.marker` (values unchanged): ipc-message-crosses,
rpc-round-trips, rpc-reply-recv, badge-demux, fs-{stat,create,write,lookup,remove,
readdir}, userspace-bad-ptr, userspace-emits-telemetry, heap-grows-on-demand,
spawn-delegates-to-child. **GREEN: full suite 72/0; the 15 migrated scenarios
150/150 on `--repeat 10`; clippy clean; host tests (kernel-core 365, abi, protocol)
green.**

> **PR boundary** — all of 5a–5e land as **one commit** (5a's `{ counter }` drop
> breaks the kernel build until 5b; removing `TelemetrySink::emit` breaks every
> marker emitter until 5d). Suite-verified. **Done last**, after Steps 2–4 proved
> the new path.

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

### Step 2 — `RegisterMetric` + `EmitMetric` syscalls + runtime bindings ✅ SHIPPED

> Landed. `abi::Syscall::{RegisterMetric = 17, EmitMetric = 18}` (round-trip test).
> Kernel handlers in `kernel/src/syscall/metric.rs`: `RegisterMetric` gated by
> `invoke_telemetry` (a `TelemetrySink` with `EMIT`), quota-checked via
> `MetricTable::is_full` *before* leaking the name, interns a fresh id
> (`tracing::register_user_metric` → no content dedup → distinct `StringId` per
> emitter), stores it in `Process.metrics: Mutex<MetricTable>`, returns the handle
> in `a0`. `EmitMetric` resolves the handle against the caller's own table →
> `emit_metric`; unregistered → `SyscallRefused{BadMetricHandle}`. Runtime:
> `snitchos_user::{MetricKind, Metric, TelemetrySink::register_metric}` (name
> crosses once; emit ships only the handle). Demo: `user/hello/src/bin/probe.rs` +
> `workload=probe` (`WorkloadKind::Probe`). Wire vocab: appended
> `RefusalReason::{BadMetricHandle, BadMetricKind}`. Itest `userspace-custom-metric`
> asserts `MetricRegister{…, Gauge}` + `Metric{42}` + the refusal — 10/10 on
> `--repeat 10`; `metric.rs` mutants all caught; clippy clean.

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

### Step 3 — First consumer: FS denial gauge self-registers (removes the `run_ipc_counter` hack) ✅ SHIPPED

> Landed. `user/fs/src/bin/fs-server.rs` registers `snitchos.fs.denied` (Gauge)
> at startup via `telemetry().register_metric(...)` and emits the packed `Denial`
> through the returned `Metric` handle. Kernel special-casing deleted from
> `kernel/src/trap/user.rs`: the `FS_DENIED_METRIC` static, `fs_denied_metric_id()`,
> its `register_gauge` line in `init_metric`, the `CounterKind` enum, and the
> per-program counter on `Launch::Ipc` — `run_ipc_counter` collapsed to `run_ipc`
> (every IPC program now bootstraps with the plain `snitchos.user.telemetry_total`
> sink). `fs-lookup-rights-gate` resolves the gauge by name+value, so the migration
> is transparent: all 8 `fs-*` scenarios 10/10; ipc/rpc/badge families green;
> default + itest builds + clippy clean.

**Acceptance criteria:** the FS server registers `"snitchos.fs.denied"` (gauge)
itself and emits via the new path; the kernel-side `FS_DENIED_METRIC` registration
**and** the `run_ipc_counter` special-casing are removed — the FS server gets a
plain bootstrap sink like every other IPC process. The existing
`fs-lookup-rights-gate` itest still asserts the structured denial under
`snitchos.fs.denied`.
**RED:** `fs-lookup-rights-gate` stays green after the migration; `--repeat 10`.
**GREEN:** FS server change + delete the kernel special-casing.
> **PR boundary** — the migration; pays down the FS-era debt.

### Step 4 (optional) — runtime ergonomics + telemetry_total migration ✅ SHIPPED

> Landed. Clean `snitchos_user` API: free `register_counter` / `register_gauge` /
> `register_histogram(name) -> Metric` (read the startup `TelemetrySink`
> implicitly — no cap or kind at the call site), and `Metric::emit(value)` is now
> fire-and-forget (`-> ()`). A refused registration returns an **inert `Metric`**
> whose `emit` is a no-op, mirroring an inert `Span` — so call sites drop the
> `if let Some(...)`. Adopted in `probe.rs` and `fs-server.rs` (the latter:
> `let denied = register_gauge("snitchos.fs.denied"); … denied.emit(…)`).
> `TelemetrySink::register_metric(name, kind)` kept as the explicit-cap primitive
> the free fns delegate to. **`telemetry_total` decision: stays kernel-pre-registered**
> — it's the legacy `Invoke` path's shared, genuinely-system-wide rate (the one
> metric where global registration is *correct*, not the poisonable-snitch
> anti-pattern); its migration onto the register path is Step 5's job, not churn
> here. `userspace-custom-metric` + all `fs-*` scenarios green (20/20 on the two
> API consumers); clippy clean.

**Acceptance criteria:** a clean `snitchos_user` API (`register_counter(name) ->
Metric`, `Metric::emit(value)`); decide whether bootstrap `telemetry_total` stays
pre-registered (for existing markers) or becomes runtime-registered. Additive.
> **PR boundary** — polish; only if the API proves clunky in Steps 2–3.

## Out of scope / follow-ons

- **Collector emitter dimension** — with no cross-process dedup, two processes
  naming a metric the same become two `StringId`s both labelled that name. A
  `task_id`/process label keeps them distinct in Prometheus. *Deferred*: no
  collision today (kernel per-task metrics already embed the task in the name; the
  FS gauge has a single emitter; Step 5's per-process markers use distinct
  `snitchos.<task>.marker` names by construction, so they don't trip this either).
  Add when a real two-process-same-name case lands.
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
