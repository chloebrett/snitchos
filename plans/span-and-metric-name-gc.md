# Span- and metric-name GC (reclaim per-process leaked names)

**Status:** Decided (2026-06-26) — **Option A now** (accept + document the bound),
**Option B later** (reclaim on process exit, folded into the v0.12 Exit/teardown
reclaim milestone). Not started.

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
