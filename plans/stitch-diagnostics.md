# Stitch runtime diagnostics: SourceMap + stack traces

> Follow-on from Phase C (core redesign). C5 gave runtime faults a `Span`
> (`RuntimeError::Fault { message, at: Option<Span> }`); this plan makes that span
> *user-visible* (`file:line:col` + caret) and then adds **stack traces**. Two
> phases, in order — a backtrace is only useful once spans render, so SourceMap
> lands first.

## The problem SourceMap solves

A fault's `at` span is a byte range, but byte offsets are meaningless without
knowing **which source** they index — and Stitch parses many sources independently,
each starting at offset 0:

- the **prelude** (`prelude_items()` parses `PRELUDE`)
- the **user program** (`eval_program`)
- **REPL** defs (`:load`) and the current line (`eval_line`)
- **cross-pipe stage** files (`<name>.st`)
- **each module** (`eval_modules`)

So `Span { start: 20, end: 25 }` could be in the prelude *or* the user program.
`ParseError::render(src)` works only because parsing is always against one known
`src`; a runtime fault has no such anchor. We need a **SourceMap**: a registry of
sources that a span can be resolved against.

---

## Phase 1 — SourceMap (render a fault's location)

**New types** (`src/source.rs`):
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SourceId(u32);            // 0 = the "unknown/synthetic" source

pub struct SourceMap { sources: Vec<SourceEntry> }   // SourceId indexes this
struct SourceEntry { name: String, text: String }

impl SourceMap {
    pub fn register(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId;
    pub fn render(&self, source: SourceId, span: Span) -> String;  // reuse ParseError::render's line/col/caret logic
}
```

**Design decision (2026-07-10): (B) — tag closures + stamp source in `apply_values`.**
Chosen over (A) [`SourceId` in `Span`] not just for the one-time churn but because
(B) gives **better provenance for synthesized/desugared nodes** (a fault in an
invented node — `operator_lambda`, `use <-` callback, `?.` accessor — is attributed
to the running closure's source rather than `source: 0`), keeps the **lexer/parser
API pure** (no `SourceId` threaded into `parse`), and avoids **re-entangling `Span`
equality** (C2 deliberately made spans metadata). (A)'s one real edge — uniform
rendering for non-closure eval (e.g. a bare REPL line) — is handled by the render
caller supplying the line's source; its headline benefit (spans self-describing
across source *movement*) is unused (Stitch has no macros / span relocation).

Rejected alternative — **(A) `SourceId` in `Span`**: every token span carries its
source, flowing through AST → CoreExpr → fault for free, but the lexer must stamp it
on every `Span`, `Span` grows ~8 bytes on *every* node, synthesized nodes need a
threaded source or fall back to unknown, and `Span { start, end }` literals churn.

Approach (B) in detail — keep `Span`
  a pure byte-range. Add `ClosureData.source: SourceId` (the source its body was
  lowered from). `apply_values`, on entering a closure, stamps the fault's source
  the same "innermost wins" way `eval` stamps the span — the innermost *closure*
  containing the faulting node. So:
  - `Fault { message, span: Option<Span>, source: Option<SourceId> }` (split: `eval`
    stamps `span`, `apply_values` stamps `source`).
  - `eval` stamp unchanged (C5). `apply_values` adds
    `.map_err(|e| e.with_source(closure.source))` (no-op once set).
  - Closure creation sets `source`: registered functions get the program/module's
    `SourceId` (threaded into `register_items`); lambdas inherit the currently
    running source; the `eval_safe_field` accessor uses `SourceId::default()`.
  - REPL/line and top-level faults (evaluated *not* through a closure) are rendered
    by the caller, which knows the line's source directly.

  *Why B:* localizes the change to the fault/closure path (no `Span` churn, no test
  churn), at the cost of a `source` field on `ClosureData` + `Fault` and a
  `SourceId` param on `register_items`/lowering.

**Wiring:** a run-shared `Rc<RefCell<SourceMap>>` on `Env` (like telemetry/platform).
Each parse-and-register site registers its source and gets a `SourceId` to tag the
lowered items with. `RuntimeError::render(&SourceMap)` produces the `file:line:col`
string; the REPL and `eval_program` error paths call it.

**TDD:** `SourceMap::render` unit test (offset → line:col+caret); a fault in a
`:load`ed def renders against the *def's* source, not the line's; a fault in the
prelude names the prelude. Keep 567 green.

**Acceptance:** a runtime fault prints `name:line:col: message` + the offending
source line + caret, resolved to the correct source among prelude/program/REPL.

### Progress

- **1a ✅ DONE (2026-07-10)** — `src/source.rs`: `SourceId` (0 = synthetic,
  location-free) + `SourceMap` (register/render), shared `caret_render(src, span,
  message)`; `ParseError::render` refactored onto it. 572 green.
- **1b ✅ DONE (2026-07-10)** — the fault-source mechanism, plumbed via a **`source:
  SourceId` field on `Env`** (the unifying choice): `eval`/`eval_tail` stamp
  `env.source()` onto a fault (innermost-closure-wins, no-op once set) alongside the
  span; `apply_values` sets `call_env.source = closure.source`; `ClosureData` gains
  `source`, set from `env.source()` at every creation site (Lambda arm, `?.`
  accessor, `register_items` for functions). `Fault { message, at, source }` +
  `with_source`/`source()`. Test: a closure tagged source 7 whose body faults yields
  `err.source() == Some(7)`. 574 green, clippy clean. **Bonus:** because
  `register_items` reads `env.source()`, 1c only needs `build_env` to *set* the env's
  source — functions then inherit it automatically.
- **1c ✅ DONE (2026-07-11)** — the render path, end to end. Key correctness point:
  prelude and user bodies carry spans into *different* texts, so registration is
  **split per source** — `build_env_in` was refactored into `build_env_batches(env,
  &[(items, SourceId)], uses_from)` that registers each batch under its own source.
  New `eval_program_located(items, &mut SourceMap, user_src)` registers the prelude
  as `<prelude>` and the user items as `user_src`, so a fault in a prelude function
  resolves against prelude text and a user fault against the program.
  `RuntimeError::render(&SourceMap)` renders a located fault; unlocated/synthetic →
  message alone. Wired into `run_program_source` (registers `<program>`, faults print
  `<program>:line:col: msg` + line + caret) and the REPL `eval_line` (per-line local
  `SourceMap`, faults print `<repl>:line:col`). Parse errors now render with caret
  too. Tests: a fault in `bad()` on line 2 cites `<program>:2:9`; REPL `1/0` cites
  `<repl>:1:1`. **577 lib + 26 integration green, clippy clean.**

  **Follow-ups (noted, not blocking):** REPL faults *inside loaded defs* render
  message-only (needs keeping the def source text — `load_source` currently drops it);
  multi-module (`run_module_files`/`eval_modules`) faults render message-only (pass an
  empty `SourceMap` today) — each module would register its own source.

**Phase 1 is complete.** Runtime faults cite `file:line:col` for single-program and
REPL-line code. Phase 2 (stack traces) builds on this.

---

## Phase 2 — Stack traces (the frame stack)

Today `apply_values` tracks only a **depth counter** (`depth: Rc<Cell<u32>>` +
`CallGuard` RAII) as the recursion backstop — it knows *how deep* but not *what's on
the stack*. Replace it with a **frame stack** that records the call chain, so a fault
can carry a backtrace.

**Design:**
```rust
struct Frame { name: Option<String>, call_site: Span, source: SourceId }
// Env: frames: Rc<RefCell<Vec<Frame>>>   (replaces `depth`)
```
- `enter_call` pushes a `Frame`; `CallGuard::drop` pops it. `frames.len()` *is* the
  depth guard — the `MAX_CALL_DEPTH` check becomes `frames.len() >= MAX`, so the
  counter is subsumed, not duplicated.
- **Frame needs a name** → `ClosureData.name: Option<String>` (set to the function
  name at registration; `None` for lambdas → renders as `<lambda>`). This is the one
  extra bit of state the trace needs.
- **Frame needs the call site** → `apply_values`/`eval_call` already have the call
  `CoreExpr`; pass its span into `enter_call`.
- On a fault, snapshot `frames` into the error:
  `Fault { message, span, source, trace: Vec<Frame> }` (captured at the raise point,
  before the guards unwind). `RuntimeError::render_trace(&SourceMap)` formats each
  frame as `in <name>  <file:line:col>` using Phase 1's renderer.

**Interaction with the trampoline:** self-tail-calls (`TailCall`) loop in
`apply_values` without growing the Rust stack — and correctly *shouldn't* grow the
logical frame stack either (tail calls replace the frame). So a tail iteration pops
+ pushes (or updates in place) rather than nesting — the trace shows one frame for a
tail-recursive function, matching its constant stack usage. Worth an explicit test.

**TDD:** a 3-deep call chain that faults reports all three frames in order with
correct names + locations; a tail-recursive fault shows a single frame; the depth
guard still fires at `MAX_CALL_DEPTH` (now via `frames.len()`).

**Acceptance:** a fault renders a full backtrace (function names + `file:line:col`
per frame), and the depth-guard behavior is preserved.

---

## Order & scope notes

- **Phase 1 before Phase 2** — a backtrace of bare offsets is useless; frames render
  via the SourceMap.
- Both are **behavior-additive** — no change to evaluation semantics, only to what a
  fault carries and how it's displayed. The 567-test suite stays the oracle.
- Deferred/adjacent: a `Display`/pretty error surface for the CLI; mapping traces
  into telemetry frames (a fault could emit its backtrace on the wire). Out of scope
  here.
