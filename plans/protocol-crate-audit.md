# Crate audit — `protocol/`

_Audited 2026-06-08. Evidence-backed; no edits made._

## Scope

- **Target:** `protocol/` — `lib.rs` (365 lines) + `stream.rs` (276 lines), 641 total.
- **Mandate:** the wire-format contract between kernel and host. Postcard-encoded
  `Frame` enum + the host-side (`std`) decode path. That's all it should be.
- **Publish status:** `publish = false`. Consumers: `kernel`, `kernel-core` (no_std,
  default features) and `collector`, `xtask` (std feature). **Decisive scoping fact:**
  not published + only in-repo consumers ⇒ for the `std`-only items, *unused by host
  crates == dead* (no external-API escape hatch). For the core `Frame`/id types the
  no_std consumers are the API.
- **Health:** clean. `cargo build -p protocol` and `--features std` emit zero
  warnings; pedantic clippy is enforced (`[lints] workspace = true`) and passes;
  the `stream` module is genuinely path-accessed (`protocol::stream::` ×5) so it
  can't be made private. No dead private code (everything is `pub`, so the compiler
  can't flag it — cross-referenced manually below).

## Resolution (post-review, 2026-06-08)

All six findings triaged. **#1, #5, #6 applied** (doc fixes + visibility demotion).
**#2, #3 investigated and kept** — both are reserved-by-design, not speculative cruft
(see corrected rows below). **#4 kept** — intentional. No further action; nothing in
this crate warrants a code change beyond what's already landed.

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation | Effort | Risk |
|---|-----|-----|---------|----------|----------------|--------|------|
| 1 | G | **med** | `HartRole`'s doc comment is garbled and `SwitchReason` has none. The block at `lib.rs:57-68` opens with *SwitchReason's* text ("Why the scheduler picked a different task. Carried on `Frame::ContextSwitch`…") then mid-comment switches to describing hart roles. `SwitchReason` (`lib.rs:70`) is left with no doc at all. | `lib.rs:57-82` — one doc block, two subjects; the enum below it bare. | **✅ DONE.** Split: `SwitchReason` got its paragraph back, `HartRole` keeps only the hart-role text. | XS | none |
| 2 | C | ~~med~~ → **keep** | `Frame::Event` is a wire variant with no producer. Kernel never constructs it; collector parks it (`state.rs:241: => None, // not yet wired to OTLP`). | `grep 'Frame::Event' kernel/ kernel-core/` → 0 (in any commit). `state.rs:241`. | **KEEP — reserved by design.** `docs/observability-design.md:21,27` locks 3 primitives (Span/**Event**/Metric, "profiling rides on Event") and states "all 7 frame types defined now; kernel uses 5 in v0.1." Defined-but-unemitted is the intended state, not debt. The `from_borrowed` arm + test are the deliberate cost. | — | n/a (keeping) |
| 3 | C | ~~med~~ → **keep** | Kernel only ever emits `SwitchReason::Yield`; `Preempt`/`Blocked`/`Exit` never emitted. | `sched.rs:535,620` emit only `Yield`. | **KEEP all four — reserved by design.** `plans/v0.5-threading.md:202-208` enumerates the full enum up front; `Exit` ↔ `TaskState::Exited` + the task-exit feature tracked in `plans/residual-race-investigation.md` (483,492,558). **✅ Applied** the only real gap: added a "reserved for task-exit" note to `Exit` for parity with `Preempt`/`Blocked`. | XS | none |
| 4 | D | med → **keep** | `Frame<'a>` and `OwnedFrame` are parallel 11-variant enums; a variant change edits 4 sites in lockstep (`Frame`, `OwnedFrame`, `from_borrowed`, the test checklist). | `lib.rs:85-107` ‖ `stream.rs:21-69` ‖ `stream.rs:151-163`. | **KEEP — intentional.** Borrow-vs-owned (`&str` vs `String`) is a hard contract requirement (kernel is `no_std`, can't allocate `String`). The exhaustive match + test list are a *designed* compile-time checklist (`stream.rs:18-19,146-150` say so). Collapsing via macro/generic obscures the wire-format source of truth — more cleverness, not less. | — | n/a (keeping) |
| 5 | A | low | `try_decode_frame` is `pub` with zero external callers — used only by `decode_stream` internally and by tests. | `grep try_decode_frame` consumers → 0; callers = `stream.rs:95` + tests. | **✅ DONE.** Demoted to `pub(crate)`. | XS | none (publish=false) |
| 6 | G | low | Stale comment: `MetricKind::Histogram`'s "bucket encoding **TBD** when we have a histogram-emitting site" — we now have one. | `lib.rs:46-49` vs live pipeline (`tracing.rs:125`, `heartbeat.rs:242`, `collector/state.rs`). | **✅ DONE.** Comment now describes the live (observed-via-`Metric`, host-bucketed) behavior. | XS | none |

## Outcome

**Applied (doc-only + one visibility keyword, zero behavior change):**
- **#1** — split the garbled `HartRole`/`SwitchReason` doc.
- **#3** — added a "reserved for task-exit" note to `SwitchReason::Exit` for parity.
- **#5** — `try_decode_frame` → `pub(crate)`.
- **#6** — refreshed the stale `Histogram` "TBD" comment.

Verified: `cargo build -p protocol`, `cargo test -p protocol --features std` (23 pass),
clippy all clean.

**Kept, deliberately (reserved-by-design, not debt):**
- **#2** `Frame::Event` — locked primitive (`docs/observability-design.md:21,27`); profiling will ride on it.
- **#3** `SwitchReason::{Preempt,Blocked,Exit}` — full enum defined up front per `plans/v0.5-threading.md`; `Exit` ↔ the planned task-exit feature.
- **#4** `Frame`/`OwnedFrame` duplication — the borrow-vs-owned split is a hard `no_std` constraint; the lockstep match/test list is a *designed* compile-time checklist.

> **Wire-format note (for any future deletion of #2/#3):** postcard encodes enum
> discriminants *positionally*. `Event` and the `SwitchReason` variants sit
> **mid-enum**, so removing one shifts every later discriminant and breaks decode
> of any prior capture (the inverse of the "new variants at the END, never reorder"
> rule in CLAUDE.md). Were the design to change, deletion would still need a
> `PROTOCOL_VERSION` bump (currently 2) and invalidate any saved corpus.

## Verdict

`protocol/` is a small, well-tested, well-documented contract crate doing exactly
its job — no architectural debt, no dead private code, no lint drift. The one
*active hazard* (the garbled doc comment, #1) is fixed. Everything flagged as
"unused" turned out to be reserved-by-design wire surface, confirmed against the
design docs and plans — correctly kept. Nothing further to do.
