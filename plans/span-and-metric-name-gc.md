# Span- and metric-name GC (reclaim per-process leaked names)

**Status:** **Option B COMPLETE** (2026-06-28). Option A (document the bound)
shipped 2026-06-26. Reclaim-on-exit now implemented across all 5 increments
(below), host-tested + proven end-to-end by the `spawn-reclaims-names` itest
(`strings_released_total ≥ 30` across the reaper's 30 reap cycles; stable 10/10).
*This file can be deleted once the work is committed.*

## Increment chain (TDD, each RED→GREEN)

1. ✅ **`intern.rs` foundation** — `register_owned(Box<str>)` + `register_metric_owned`
   (table owns the name) + `release(StringId)` (tombstone the slot, drop the bytes,
   never reuse the id). Inline + overflow both tombstonable; `push` stays monotonic.
   `count()` now reports *live* names. *Fork-independent.* (3 new host tests.)
2. ✅ **`span_name.rs` / `metric.rs`** — `ids()` on both. `SpanNameTable` now owns
   its name (`Box<str>`, the fork below). (2 new host tests + signature update.)
3. ✅ **`tracing.rs`** — `span_open_bounded` / `register_user_metric` route through
   the owned paths (no `Box::leak`); kernel `&'static` literals unchanged. Added
   `release_names`.
4. ✅ **`sched::reap_task`** — gathers the exiting process's span + metric ids and
   releases them before dropping the `Process` (ids collected first → no nested
   lock under `INTERN_TABLE`). Validated against existing reap + userspace-metric +
   span itests (all green).
5. ✅ **itest `spawn-reclaims-names`** — end-to-end reclaim proof. Added a
   `snitchos.intern.strings_released_total` counter (`release` now returns whether
   it freed a live entry; `release_names` sums the trues and bumps the counter,
   drained by the heartbeat). `memhog` names one metric per spawn; the scenario
   asserts the counter reaches ≥ 30 after `reaper.done` — reclaim fires on every
   reap. A genuinely useful "watch reclamation happen" observable, not just a test.

   *Why not assert on `strings_used` / max `StringId`:* the live gauge needs a
   baseline to claim "stayed bounded," and max id always climbs (ids never reused,
   by design). A dedicated released-total counter is the clean monotonic signal.

**Fork (increment 2):** `SpanNameTable` stores its own `Box<str>` copy rather than
coupling `resolve` to the intern table — keeps the two kernel-core tables
independently host-testable. Negligible space (≤16 names/process, both copies freed
on exit).

## Why this exists now

The per-process security fixes deliberately removed cross-process name dedup:
- **Metrics** (debt #2 Step 5): `register_user_metric` leaks a fresh `'static`
  name per process → distinct `StringId` per emitter (the forgery boundary).
- **Span names** (span-poisoning Part A): `span_open_bounded` leaks per-process via
  the [`SpanNameTable`] → a process gets its own id for any name (no poisoning).

Both are **correct and required**. The side effect: a program's names are no longer
interned once globally and reused — **each spawn re-leaks** its span + metric names.
So the leak is now **O(spawns × names-per-program)**, not O(distinct programs).
A long-running **shell (v0.13)** that spawns repeatedly is what makes it bite.

Today the practical bound is still O(distinct names ever registered) — tens of bytes
each, per-process quota 16 — so it is slow to matter. Leaked names are never
reclaimed (the global intern table is append-only; `Box::leak`'d `&'static str`).

## Decision

- **Option A — now.** Accept the bound; **document** it where the per-process tables
  enforce the per-spawn cap ([`SpanNameTable`], [`MetricTable`]) and note that
  reclamation is deferred to v0.12 teardown. Zero runtime work.
- **Option B — later (with v0.12 Exit/teardown reclaim).** Reclaim a process's
  leaked names when it exits. Deferred because (a) it shares the teardown lifecycle
  hook with address-space reclaim and kernel-stack guard-pages Tier B, and (b) the
  shell is what makes it relevant.
- **Option C (don't store userspace names globally at all) — rejected.** More
  invasive to the intern table (decouples id-allocation from name-storage, complicates
  the pointer-identity used for kernel `&'static` names) for no gain over B.

## Option B design sketch (for when v0.12 teardown lands)

The per-process tables already hold exactly which `StringId`s the process owns —
`SpanNameTable` (`(name, StringId)` pairs) and `MetricTable` (`StringId`s) — so
exit-time reclaim has its data for free. On process teardown:

1. **Intern entries own their userspace names** (`Box<str>`/`String` instead of a
   `Box::leak`'d `&'static str`), so they can be dropped. Kernel-registered
   `&'static` literals (`span!("kernel.boot")`, `register_counter("snitchos.…")`)
   stay as-is — bounded program literals.
2. Walk the exiting process's `SpanNameTable` + `MetricTable` ids; **drop** those
   intern entries → frees the dominant cost (name bytes).
3. **Tombstone** the freed id slots (set `None`, **never reuse**) — wire-id
   stability: frames and the collector reference `StringId`s, so a freed id must
   never alias a new name. The inline region is already `Option<InternEntry>`; the
   overflow `Vec<InternEntry>` becomes `Vec<Option<InternEntry>>`.

**No wire/collector change** — the collector keeps its id→name map harmlessly; a
tombstoned id simply stops appearing in new frames (and the map resets on `Hello`).
GC is purely kernel-side reclaim.

[`SpanNameTable`]: kernel_core::span_name::SpanNameTable
[`MetricTable`]: kernel_core::metric::MetricTable

---
*Delete this file when the plan is complete.*
