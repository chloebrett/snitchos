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

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation | Effort | Risk |
|---|-----|-----|---------|----------|----------------|--------|------|
| 1 | G | **med** | `HartRole`'s doc comment is garbled and `SwitchReason` has none. The block at `lib.rs:57-68` opens with *SwitchReason's* text ("Why the scheduler picked a different task. Carried on `Frame::ContextSwitch`…") then mid-comment switches to describing hart roles. `SwitchReason` (`lib.rs:70`) is left with no doc at all. | `lib.rs:57-82` — one doc block, two subjects; the enum below it bare. | Split: give `SwitchReason` back its first paragraph, leave `HartRole` only the hart-role text. Pure doc fix. | XS | none |
| 2 | C | **med** | `Frame::Event` is a wire variant with **no producer**. Kernel never constructs it (0 sites); collector explicitly parks it (`state.rs:241: Frame::Event { .. } => None, // not yet wired to OTLP`); harness only formats it for display. | `grep 'Frame::Event' kernel/ kernel-core/` → 0. `state.rs:241`. | Keep *or* drop — your call (wire format). It costs a discriminant slot and an arm in every match/test. If events aren't on the near roadmap, deleting it removes ~6 lines across `Frame`, `OwnedFrame`, `from_borrowed`, tests. | S | wire-format break (but no external captures) |
| 3 | C | **med** | 3 of 4 `SwitchReason` variants are speculative: kernel only ever emits `Yield`. `Preempt`/`Blocked` are doc'd "reserved for v0.5.x"; `Exit` has no such note and is also never emitted (v0.5 tasks are `-> !`). | `sched.rs:535,620` emit only `SwitchReason::Yield`; no other emit sites. | Keep `Preempt`/`Blocked` (documented placeholders, near-term). Reconsider `Exit` — either doc it as reserved like the others or drop it. Your call. | XS | wire-format (positional discriminants — see note) |
| 4 | D | med | `Frame<'a>` and `OwnedFrame` are parallel 11-variant enums; adding/changing a variant means editing **4 sites in lockstep**: `Frame`, `OwnedFrame`, the `from_borrowed` match, and the "canonical checklist" test list. The v0.6 `hart_id` add touched all four. The "edit-N-places" tell. | `lib.rs:85-107` ‖ `stream.rs:21-69` ‖ `stream.rs:151-163`. | **Likely intentional** — the borrow-vs-owned (`&str` vs `String`) and no_std-vs-std split is a real contract difference, and the duplication is the price of the kernel staying allocator-free. A macro or `Frame<S: AsStr>` generic would collapse it but cost clarity in the wire-format source of truth. Flagging the maintenance cost, not recommending a change. | M | readability of the contract |
| 5 | A | low | `try_decode_frame` is `pub` with **zero external callers** — used only by `decode_stream` internally and by tests. | `grep try_decode_frame` consumers → 0; callers = `stream.rs:95` + tests. | Demote to `pub(crate)`, *or* keep `pub` if it's deliberately offered as a decode primitive. Trivial either way. | XS | none (publish=false) |
| 6 | G | low | Stale comment: `MetricKind::Histogram`'s "bucket encoding **TBD** when we have a histogram-emitting site" — we now have one. Histograms are emitted (`tracing.rs:125`, `heartbeat.rs:242`) and fully decoded/exported (`collector/state.rs`). | `lib.rs:46-49` vs live histogram pipeline. | Drop the "TBD/when we have a site" framing; the site exists. | XS | none |

## Two lists

**Obvious wins (safe now, low risk):**
- **#1** — fix the garbled `HartRole`/`SwitchReason` doc (actively misleading; pure comment edit).
- **#6** — refresh the stale `Histogram` "TBD" comment.
- **#5** — `try_decode_frame` → `pub(crate)` (if you don't intend it as public API).

Mass: ~0 prod lines, doc-only + one visibility keyword.

**Needs your call (wire format / intentional design):**
- **#2** `Frame::Event` — delete the no-producer variant, or keep it parked?
- **#3** `SwitchReason::Exit` — document-as-reserved or drop? (Keep `Preempt`/`Blocked`.)
- **#4** `Frame`/`OwnedFrame` duplication — accept as the contract's honest cost, or collapse via macro/generic?

Mass if you delete #2: ~6 lines across 4 files. #3/#4 are judgement, not mass.

> **Wire-format note (applies to #2, #3):** postcard encodes enum discriminants
> *positionally*. `Event` and the `SwitchReason` variants sit **mid-enum**, so
> removing them shifts every later discriminant and breaks decode of any prior
> capture. Per CLAUDE.md the rule is "new variants at the END, never reorder."
> Deleting a mid-enum variant is the same hazard in reverse. Safe only because
> `publish = false` and no external v0.6 captures exist — but it would still
> invalidate any saved corpus. Bump `PROTOCOL_VERSION` (currently 2) if you do it.

## Verdict

`protocol/` is a small, well-tested, well-documented contract crate that's doing
exactly its job — no architectural debt, no dead private code, no lint drift. The
only *active hazard* is the garbled doc comment (#1). Everything else is a
deliberate-design question (speculative variants you may want to keep) or a
cosmetic cleanup. Not a session's worth of work; #1 + #6 are a 5-minute fix.
