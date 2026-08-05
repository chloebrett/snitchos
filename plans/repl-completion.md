# Tab completion in the Stitch REPL (TDD plan)

**Status:** ✅ **DONE — all seven increments built, working end to end, and gated.**
`stitch-kvetch-completes` and `stitch-drivel-completes` are both registered
(`xtask-itest/src/itest.rs:217-218`). The kernel gap that blocked registration —
FP context switching — shipped as [floating-point.md](legacy/floating-point.md)
increment 4b on 2026-07-28. Two non-blocking defects found on the way are still
open; see "Two defects found on the way" below.

## Increment log

**Increments 1, 2, 3 done, and increment 4's seam
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

**Increments 6 + 7 landed, and found a blocker.** `workload=stitch-kvetch`
(kvetch server + REPL holding `SEND`), `RuntimePlatform::complete` over
`kvetch_proto`, and the REPL's editor wired to `ModelCompleter`. All of it
builds and boots. **Tab does not work on target**, for a reason worth
recording carefully.

### ⚠ ROOT CAUSE: userspace floating point is illegal on the metal

**Found by running the scenario under QEMU** (`--engine qemu`), which reported
what snemu swallowed:

```
Kernel panic: panicked at kernel/src/trap/mod.rs:203:18:
unhandled trap: UnknownException(2) (scause=0x2)
```

`scause=2` is **illegal instruction**. Nothing in the kernel ever sets
`sstatus.FS`, so it stays at its reset value (Off) and *every* floating-point
instruction traps — in userspace as much as in the kernel. The oracle probes
all 58 token classes including `Float`, whose token carries an `f64`; parsing
or even moving one emits FP. Hence: host-green, target-dead, and dead in the
`Forced` path too (every class is probed before the set is known).

**This is not a completion bug.** Confirmed by removing completion entirely
and typing a float at the prompt:

```
stitch> 1.5 + 1.5
Kernel panic: unhandled trap: UnknownException(2) (scause=0x2)
```

Three separate findings fall out, none of them about Tab:

1. **Stitch cannot use floats on target.** The language has full float support
   (`TokenKind::Float`, `ExprKind::Float`, float arithmetic) and none of it can
   run on the metal. It had simply never been exercised — every on-target
   Stitch program and fixture to date (`primes.st`, the REPL demo) is integer
   only. Fixing it means enabling FP for user mode: set `sstatus.FS` on user
   entry *and* save/restore the FP registers in `TaskContext` on context switch
   (they are not in it today). The kernel itself stays zero-FP, as designed —
   this is about what *userspace* may do.
2. **A user program can panic the kernel with one illegal instruction.** The
   `UnknownException` arm at `trap/mod.rs:203` panics regardless of privilege,
   even though the surrounding code already distinguishes `from_user`. An
   unhandled *user* trap should kill the process (as other user faults do), not
   take down the machine. That is a robustness hole independent of FP, and
   arguably the more serious of the two.
3. **snemu hid a kernel panic.** Under snemu the guest simply stopped emitting
   frames; under QEMU the same run printed the panic. The `panic-now` scenario
   exists precisely to assert that kernel panics reach the wire, so this is a
   real fidelity gap — either the trap is mis-executed or the panic telemetry
   never flushed. Worth a `snemu diff` investigation on its own.

**What this cost:** the earlier heap-exhaustion diagnosis was wrong, and so was
the instinct to keep bisecting under snemu. One `--engine qemu` run — the
documented "fidelity escape hatch" — answered in 60 seconds what an hour of
snemu-side inference did not. When the engine that gives *worse* signal is the
one being interrogated, switch engines earlier.

### Superseded diagnosis (kept for the lesson)

**Corrected diagnosis.** An earlier version of this note said the cause was
heap exhaustion (232 probes × ~68 KiB talc regions ≈ the 16 MiB
`Process::HEAP_MAX`). The arithmetic fit suspiciously well, and it was
**wrong**: the kernel refuses `MapAnon` with `RefusalReason::OutOfMemory` and
refusals snitch, and **no `SyscallRefused` frame is ever emitted**. The heap is
never exhausted. Recording this because the near-miss is instructive — a number
that lands on a known cap is a hypothesis, not evidence.

What is actually established:

| Evidence | Reading |
|---|---|
| `emit("probe", 7)\n` round-trips | console input, injection and the workload are fine |
| `use M.` alone echoes | partial-line input and echo are fine |
| `use M.\t` echoes *nothing* | the hang is inside `feed_with` — it returns echo at chunk *end*, so a hang on Tab swallows the preceding characters too |
| the client-side counter never fires | the REPL never reaches the IPC call: it is the **grammar**, not the wire |
| `Forced` (`use M.`) hangs too | not the model path — it is `stitch::complete` / `valid_next` itself |
| no `SyscallRefused` | not heap exhaustion |
| no exit-frame | the process is not killed |
| no panic/overflow `Log` | no *kernel* panic reported |
| **`kernel.heartbeat` stops too** | the whole guest stops producing frames, not just the REPL |
| unchanged at `--steps 4B` and a 120 s wait | not merely slow |
| the identical call is green on host | environment-dependent, not a logic bug |

Note one measurement was misread along the way: `View::guest_instret` reports
instret at the last *matched* frame, so "flat instret" means "no frames
arrived", not "the guest stopped executing". Do not read it as a progress
counter.

**Still unexplained:** what differs on target. Heap and refusals are ruled out;
stack depth, an opt-level-dependent miscompile in the userspace build (there is
a documented latent UB class there), and a snemu fidelity gap are all still
open. The next probes worth running: call `complete()` from a *non-console*
path on target (a boot self-test in the REPL) to remove the editor and console
from the picture entirely; and try `--opt hi`/`max` to see whether the
behaviour is opt-dependent.

### The allocation reduction (done anyway, and worth having)

One `complete()` makes **232 probes** — 58 token classes × 2 grammar entries —
and each probe allocates a `format!("{prefix} {lexeme}")` string, a fresh token
vector, and an AST. On the host that is microseconds. On target it exhausts the
**16 MiB per-process heap** (`Process::HEAP_MAX`): talc maps a fresh ~68 KiB
region per OOM, and 232 × 68 KiB ≈ 15.8 MiB lands almost exactly on the cap.
The REPL then hangs in talc's OOM path — in `snitchos_user`'s panic handler,
which spins, so **snemu reports the guest as idle** and the failure looks like
"nothing happened" rather than a crash.

How it was pinned down, since none of it was visible from the symptom:

| Observation | What it ruled out |
|---|---|
| `emit("probe", 7)\n` round-trips | console input, injection, the workload |
| `use M.` alone echoes | partial-line input, echo path |
| `use M.\t` echoes *nothing* | — and explains why: `feed_with` returns its echo at the *end* of a chunk, so a hang on Tab swallows the preceding characters too |
| a client-side counter never fires | the REPL never reaches the IPC call — the hang is in the *grammar*, not the wire |
| `Forced` (`use M.`) hangs too | the model path entirely; it is `valid_next` itself |
| process did not exit; instret flat at 15.4 M over a 20 s wait | computation — the guest is idle, i.e. blocked or spinning in a way snemu skips |

`valid_next_in` now lexes the prefix **once per query** and probes by appending
a candidate *token* (`oracle::Probes` + `parser::{parse_program_tokens,
parse_expr_tokens}`), instead of formatting a fresh source string and re-lexing
the whole prefix for each of 58 classes. That removes 232 string allocations
and 232 lexes per `complete()`, leaving the parses. Behaviour is unchanged —
773 stitch tests green, including
`the_single_class_query_agrees_with_the_full_set`, which pins the single-class
and full-set paths against each other.

It did **not** fix the wedge, which is how the heap hypothesis was falsified.
It is kept because it is a real reduction that any fix will want anyway, and
because the token-wise probe is the more honest formulation: the space
separator it replaces existed only to defeat maximal munch, a hazard that
cannot arise when tokens are appended directly.

A second lever, still available: the menu only ever displays `MENU_LIMIT`
entries, so probing could stop once enough classes are known — at the cost of
the exact "(N total)" count.

**The lesson worth keeping:** the design doc said "measure latency at increment
4, before adding IPC on top". I skipped it, and paid for it — not in latency,
but in a target-only failure discovered three increments later, with the whole
stack built on top of it.

### ✅ RESOLVED 2026-07-28: Tab works. The blocker is FP context switching.

The whole chain runs. A keystroke reaches the line editor, the grammar declines to
settle the position, the REPL calls the kvetch server, babble samples, and the answer
comes back and is inserted at the prompt:

```
stitch> let x = .. and ..= < "score" +
```

Verified under **both** engines (snemu and `--engine qemu`), with a negative control:
flipping the asserted span name to one that is never emitted fails the scenario, so the
assertions are not vacuous.

**What actually blocks registration is the *second* Tab**, and it is not in the
completion path at all. Both processes lex Stitch — the server to sample, the client to
re-validate what it was sent — so both eventually parse a float literal, and
`FpEnableDecision::RefuseBusy` permits one FP process at a time. The REPL wins the race,
the server is killed by an illegal instruction mid-request, and the REPL blocks forever
in `call` on an endpoint with no receiver. Full evidence and consequences:
[floating-point.md](legacy/floating-point.md) increment 4b.

**The lesson, again, and it is the same one.** The 2026-07-26 conclusion — "a second,
independent gap: the REPL never reaches the call" — was read off the bisect
scaffolding's own diagnostic and the console tail, both of which were sampled *after*
the wedge. The frame stream said otherwise the whole time: `completions_asked` fires,
`kvetch.complete` opens on the server's task id, and two `Log` frames name the kill.
Read the wire before believing the console.

**Two defects found on the way, neither blocking:**

1. `RuntimePlatform::complete` calls `register_counter` on **every** Tab. Metric names
   are a per-process quota of 16 (`MetricTable::MAX_METRIC_NAMES`) with no dedup, so
   after ~13 Tabs the registration is refused and `Metric::emit` silently no-ops — the
   client half of the round trip disappears from the wire exactly when a long session
   would want it. Register once, hold the handle (`kvetch::serve` already does this).
2. The same counter emits a constant `1` rather than a running total, so the wire
   cannot distinguish one completion from fifty.

**Status of the pieces:** the workload, the server, the protocol, the client
platform method and the scenario body all exist and work. `stitch-kvetch-completes`
was held unregistered while it wedged on tab 2, so the gate stayed green; the fix
landed in the kernel (FP context switching) and the scenario is **registered as of
2026-07-28**, alongside `stitch-drivel-completes`.

---

**Increment 1 done.** `stitch::complete` returns
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
