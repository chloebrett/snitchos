# Tab completion in the Stitch REPL (TDD plan)

**Status:** 🚧 **IN PROGRESS — increments 1, 2, 3 done, and increment 4's seam
with it.** `LineEditor::feed_with(bytes, &dyn Completer)` handles Tab; `feed`
delegates to it with a `NoCompleter`, so every existing caller behaves exactly
as before (Tab was already dropped with the other control bytes). 12
line-editor tests, 752/752 across the crate, clippy clean.

Increment 4 collapsed into 3 because the editor needs *something* to call, so
the seam had to exist immediately. What remained worth keeping from 4 is the
decoupling proof: a fake completer returning `Forced("!!!")` — an answer no
grammar would give — is inserted verbatim, which could not pass if the editor
consulted `stitch::complete` itself. That is what makes the model-backed
completer a **substitution into** the editor rather than a rewrite of it.

One plan prediction was wrong and is worth recording: *"Tab on an empty buffer
is inert"* — it is not. The oracle has a real answer for the empty line (every
declaration opener, plus every expression opener), so Tab there shows a menu.
That is better behaviour than the plan imagined, so it stands.

Where the round-trip discipline actually lives: **not** in the editor. The
editor asks its completer once per Tab; it is the *ranking* completer
(increment 5) that must resolve `Forced` from the grammar locally and only
consult the model when the choice is ambiguous. Putting that rule in the editor
would have leaked grammar knowledge into it.

**Increment 5 done (host half).** `Platform::complete(prefix, max_tokens) ->
Option<String>` with a `None` default — no endpoint is the *common* case, not
an error path, and it degrades to grammar-only. `ModelCompleter` composes the
two, and the round-trip rule lives there as designed: a `Forced` token or a
dead line is decided by the grammar alone and **never asks the service**
(pinned by a fake that counts requests and asserts zero). 757/757, clippy clean.

Two things the increment added beyond the plan:

- **`Completion::Suggested`** — a model's guess is legal but not certain, so it
  is a distinct variant from `Forced`. The line editor inserts both (pressing
  Tab *is* the request), but the distinction is now available to any surface
  that can render a guess differently.
- **The suggestion is validated locally.** kvetch only emits oracle-approved
  tokens — but a client that *assumed* that would be trusting another process
  to police its own output. `ModelCompleter` checks the suggestion leaves the
  line viable and falls back to the menu if not. A suggestion that kills the
  buffer is worse than no suggestion.

**Remaining for 5:** `RuntimePlatform::complete` over `kvetch_proto` — the
on-target half, which needs the second endpoint cap and so lands with
increment 6.
 `stitch::complete` returns
`Forced` / `Choices` / `None` over the union of both entries; 6 tests green,
clippy clean, full stitch suite 723/723. Real output:

```
"use M."   -> Forced("{")
"use"      -> Choices([Ident])
"greet"    -> Choices([And, Or, Plus, …, LParen, LBracket])   // 24
"let x = " -> Choices([Int, Float, Bool, Str, Ident, …])       // 17
```

Two findings worth carrying into increment 2:

1. **A forced *class* is not a forced *spelling*.** After `use`, exactly one
   class is legal (an identifier) — but only the user knows the module's name,
   and typing the oracle's probe lexeme (`x`) would be *inventing* code.
   `oracle::has_one_spelling` draws the line; `Bool` sits on the payload side
   (`true`/`false` are two spellings, not one).
2. **The union makes `Forced` rare, and that is the point.** `greet` is forced
   to `(` as a declaration but opens 24 continuations as an expression, so it is
   only ever offered as choices. Every `Forced` is therefore trustworthy — it
   holds under *both* readings. Both directions of the union are load-bearing
   and tested: drop the Expr half and `greet` wrongly becomes `Forced("(")`;
   drop the Program half and `use M.` wrongly becomes `None`.

**This is why increment 2 needs the cap**: a 24-item menu is noise at a prompt.

**Increment 2 done** — `complete::menu` renders choices via `oracle::describe`,
capped at `MENU_LIMIT` (8) with `… (N total)`, the same policy as
`ParseError::render`. 12 tests green. **Capping lives in rendering, not in
`complete`**: a ranker wants the whole legal set, only the display is bounded.

**The finding that matters, and it is a measurable one.** The grammar-only
menu is good at expression *openers* and bad at *continuations*:

```
"let x = "  an integer, a float, a boolean, a string, a name, a placeholder, `match`, `handle`, … (17 total)
"greet"     `and`, `or`, `+`, `-`, `*`, `/`, `%`, `==`, … (24 total)
```

The first is genuinely useful. In the second, the suggestions a person actually
wants after a bare name — `(` to call it, `.` for a field — are **buried past
the cap** behind `and`/`or`/arithmetic, because discriminant order happens to
front-load literals (good for openers) and keyword operators (bad for
continuations).

So ranking is not a nice-to-have: it is where the model first earns its place,
with a concrete target. `menu` renders whatever order it is handed, so a ranker
is a **pure pre-sort** — no change to the renderer, and the grammar layer keeps
having no opinion about likelihood. Note the ranker cannot live in `stitch`
(babble depends on stitch, not the reverse); it belongs at the REPL/kvetch
layer, which is increments 4–5. The first place a human meets the
completion stack. Realizes the division the design has been building toward:
**the grammar supplies what is *legal*, the model supplies what is *likely*** —
so the menu is never wrong, only variably helpful.

Deliberately staged so the first two increments ship real value with **no model
and no IPC**: where the continuation oracle admits exactly one token, the REPL
can complete it outright — correct by construction, zero latency. Only then does
`kvetch` get involved.

Related: [babble.md](babble.md) (the oracle, the server, `kvetch-proto`),
[../docs/llm-design.md](../docs/llm-design.md) (the four oracle consumers —
this is *affordances*, the second), [../docs/babble-design.md](../docs/babble-design.md),
[../docs/stim-design.md](../docs/stim-design.md) (where this goes next, at scale).

**Non-goals (explicitly later):** ghost text as you type (needs the
versioned-buffer protocol, deferred in babble); multi-line completion; UTF-8
input (the `LineEditor` is ASCII-only by standing limitation); ranking by a
*trained* model (babble ranks by bias table until a rung exists); stim.

---

## Design decisions (settle before increment 1)

**Entry point: the REPL is not program-entry.** `Repl::eval_line` tries
`parse_program` first and falls back to `parse` — so a line may be a
declaration *or* an expression, and the oracle must be asked accordingly
(`Entry::Program` vs `Entry::Expr`, added in babble increment 4b). Neither
alone is right: `let x = 1` is program-entry, `1 + ` is expression-entry.
**Decision: take the union of both entries' answers**, since the REPL genuinely
accepts either. Document that a token legal in only one reading is still
offered — the alternative (guessing which the user meant mid-line) is worse and
unpredictable. The union is also what makes the singleton case honest: it is a
forced token only if *both* readings force it.

**Tab is free.** `LineEditor::feed` currently drops all control bytes, so
`0x09` is unclaimed — no existing behaviour changes.

**Where the logic lives.** A new `stitch::complete` module: pure, host-tested,
a function of `(line, cursor)` returning a `Completion`. The REPL and (later)
stim both consume it; neither owns it. Same shape as `stitch::oracle`.

**Latency.** Each `valid_next` is one parse per token class (58). A REPL line
is short, so this is microseconds on the host; on-target it is a syscall-free
local computation. Measure at increment 4, before adding IPC on top.

## Increment 1 — the forced token: complete with no model at all

**RED** (`stitch/src/complete.rs`): after `use M.`, exactly one token is legal
(`{`), so `complete("use M.", 6)` yields `Completion::Forced("{")`; after
`greet`, `(` is forced. Where several tokens are legal (`let x = `),
`Completion::Choices(…)` instead; where none are (a dead prefix),
`Completion::None`.

**GREEN**: `complete(line, cursor)` over `valid_next_in` with the union of both
entries, plus `oracle::representative` for the lexeme to insert.

## Increment 2 — the menu

**RED**: `complete("let x = ", 8)` offers choices rendered by
`oracle::describe` (`an integer`, `a name`, `(`, …), capped and stable in
order; a choice list never contains a class the oracle rejects.

**GREEN**: the `Choices` arm, reusing `describe`. Cap the list the same way
diagnostics do (`SHOWN_CONTINUATIONS`) — a REPL line that offers two dozen
operators is noise.

## Increment 3 — wire Tab into the line editor

**RED** (`stitch/src/line_edit.rs` tests): feeding `0x09` with buffer `use M.`
appends the forced `{` and echoes it; feeding `0x09` with an ambiguous buffer
leaves the buffer unchanged and echoes the menu; feeding `0x09` on an empty
buffer is inert. Existing behaviour for every other byte is unchanged
(characterisation).

**GREEN**: a Tab arm in `feed`. **The editor must not depend on the completer**
— pass it in, so the pure editor stays testable and the model-backed version is
a substitution, not a rewrite.

## Increment 4 — the `Completer` seam

**RED**: `LineEditor::feed_with(&mut self, bytes, &dyn Completer)`; a
`GrammarCompleter` (increments 1–2, no model) and a `FakeCompleter` in tests
that returns a canned ranking. Assert the editor asks the completer only when
the choice is ambiguous — a forced token must **never** cost a round trip.

**GREEN**: the trait plus the grammar implementation. This is the seam the
kvetch client slots into; everything above it stays model-free forever.

## Increment 5 — the kvetch client in `stitch`'s platform

**RED** (host, against a fake): `Platform::complete(prefix, max_tokens) ->
Option<String>` returns `None` where there is no completion endpoint (the
`stitch-repl` workload, `FakePlatform`, the host CLI), and the fake's canned
answer otherwise. The trait default is `None` — no endpoint, no completion,
never a panic.

**GREEN**: default trait method; `RuntimePlatform` implements it over
`kvetch_proto::Complete` + `Endpoint::call`, mirroring `fs_read`'s attach-then-
call shape.

## Increment 6 — two endpoints, and the cap plumbing that implies

**The first process in the system to hold two endpoint caps.** `fs_read` today
reaches its FS endpoint through `snitchos_user::endpoint()` (the startup
endpoint); a second endpoint means the platform must *name* which one, so the
implicit "the endpoint" becomes an explicit handle discipline —
`delegated_handle(0)` for the FS, `delegated_handle(1)` for kvetch, ordered by
the workload's grant list.

**RED** (`kernel-boot`): `workload=stitch-kvetch` parses to its variant.

**GREEN**: a `ProgramSpec` granting the REPL both caps, a `LAYOUTS` entry
spawning `kvetch-server` + `fs-server-seeded` + `stitch_repl`, and the platform
reading each endpoint by its delegated slot. Flag for the manifest design: this
is exactly the positional-startup-ABI fragility that
[../docs/manifest-design.md](../docs/manifest-design.md) exists to kill — two
caps in, distinguished only by order.

## Increment 7 — the itest

**RED** (`xtask-itest/src/itest/scenarios.rs`): `workload=stitch-kvetch`; the
REPL is driven with a scripted prefix + `0x09`; assert (a) a
`kvetch.complete` span appears on the *server's* task id (the trace crossed the
boundary), (b) the REPL echoed a completion, (c) heartbeat survives. Runs under
snemu, joins the standard gate including `--scramble`.

**GREEN**: whatever console scripting shakes out; `console_echo` is the
precedent for feeding input to a userspace program under itest.

## Gate

`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`, plus
`cargo xtask clippy` and mutants over `stitch::complete`.

## What this sets up

- **Ghost text** becomes a protocol change (versioned buffer), not a rewrite:
  the `Completer` seam and the grammar/model division stay.
- **stim** inherits `stitch::complete` unchanged — the editor is a third
  consumer of one function, alongside the REPL and diagnostics.
- **The eval floor gets a human check**: if grammar-only completion already
  feels useful, that is the honest baseline any trained rung must beat, in
  exactly the way `unconstrained-parse%` is for generation.
