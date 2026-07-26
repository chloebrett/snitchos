# Native testing in Stitch

**Status:** 📐 **DESIGN — not started.** Stitch programs are currently tested
from Rust. This page argues that the tests belong in Stitch, designs the form
they take, and shows why the language already has every primitive the design
needs — the work is a keyword, a runner, and a migration, not a new mechanism.

Related: [language-design.md](language-design.md) (the grammar, `uses`,
`handle`/`with`/`without`, contracts), [generative-ladder.md](generative-ladder.md)
(the canon stratum, the Stage 0 funnel),
[../plans/stage-0-validator-funnel.md](../plans/stage-0-validator-funnel.md)
(sift — the immediate consumer),
[observability-design.md](observability-design.md) (the frame wire format a test
run reports on).

---

## The problem, in one file

`stitch/tests/stim_fsm.rs` is 1021 lines of Rust whose job is to write Stitch:

```rust
fn step_mode(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ let st = step({setup}, "{key}")  match st.state.mode {{ Normal => "Normal"  … }} }}"#
    ))
}
```

That is a Stitch program. It is stored as a Rust string literal, so it gets none
of what this project has spent months building for Stitch programs:

- **No diagnostics.** Faults carry `file:line:col` and a backtrace now; a fault
  inside a `format!`-assembled string cites a location that does not exist.
- **No type checking.** The canon gate type-checks every shipped `.st` file.
  These programs are invisible to it — the most-exercised Stitch in the repo is
  the least checked.
- **No printer, no editor, no completion.** `print.rs` round-trips real files;
  stim's own FSM cannot open its own tests.
- **Doubled escaping.** `{{` and `\"` throughout, and maximal-munch hazards get
  worked around in *Rust* comments ("Binding `rc` keeps the trailing `[…]` off
  the match").
- **Host-only.** The interpreter runs on riscv64. These tests never do, so the
  one thing the canon claims — *validated by use, on the metal* — excludes its
  own test suite.

`canon_behaviour.rs` shows the same shape more politely: it builds a driver
module in Rust (`format!("use text\nuse Str\n\nmain() = {body}")`) to call into a
Stitch library. The driver is Stitch. Only its residence is Rust.

**The tests were always Stitch. Rust is just where they were parked.**

## What the language already has

This is the reason to design it now rather than later: nothing below needs a new
runtime mechanism.

| Testing need | Stitch primitive | Status |
|---|---|---|
| Test doubles / mocks | `handle <op> with <handler> { … }` | shipped |
| "This test may not touch the disk" | `without <Cap> { … }` | shipped |
| Declaring what authority a test grants | `uses Cap` on the declaration | shipped, checked |
| Failure location | spanned faults + backtraces | shipped |
| Reporting | telemetry frames + the collector | shipped |
| Discovery | we own the AST | free |
| Runaway tests | interpreter fuel + depth guards | shipped |

Effect handlers are the headline. `FakePlatform` exists in Rust because Stitch
tests could not install their own doubles; in Stitch they can, and the double is
written in the language under test rather than in the host's.

## The design

### A test is a declaration

```stitch
test "pad widens to the column and never truncates" {
  expect text.pad("ok", 5) == "ok   "
  expect text.pad("overlong", 3) == "overlong"
}
```

`test` joins `prod` / `sum` / `contract` / `on` as a **structural** keyword,
which is exactly the rule the grammar already states: a function carries no
keyword because it is the lightest declaration; the structural forms carry one.
A test is structural — the runner enumerates it, it is not called by name, and
it has no parameters or return type.

The name is a **string, not an identifier**. Test names are prose, they end up
in reports and telemetry, and forcing them through identifier syntax produces
`pad_widens_to_the_column_and_never_truncates` — which is a sentence pretending
to be a name. Stitch has no other declaration whose name is for humans only;
this one is.

### Authority is granted, not ambient

A `test` with no `uses` clause has **no authority**: no telemetry, no console,
no filesystem, no clock. It is pure by construction, therefore deterministic by
construction, and the type checker already enforces this — capabilities-as-effects
is checked, so a test that calls an effectful function without declaring the
capability is a *type error*, not a runtime surprise.

```stitch
test "the writer emits one span per save" uses Telemetry {
  …
}
```

This inverts the usual arrangement, where tests run with the full authority of
the host process and isolation is a convention. Here the pure case is the
default and every escape is written down. It also means **the `uses` clause of a
test is a readable specification of what the code under test needs** — which is
the project's whole thesis applied to its own test suite.

### Doubles are handlers

```stitch
test "save writes the buffer, once" uses Fs {
  handle fsWrite with (path, bytes) -> record(path, bytes) {
    expect save(buffer) == Ok
  }
}
```

No mock framework, no injection ceremony, no `FakePlatform` in a second
language: the substitution mechanism is the language's own dynamically-scoped
handler, and the double is ordinary Stitch. `without Fs { … }` asserts the
negative — that a path *cannot* touch the disk — and fails at the perform site
with a span, which is a far stronger statement than "no writes were recorded".

### `expect` is the assertion, and it renders both sides

```stitch
expect motionTarget(st, "w") == Some(Target(row: 0, col: 4))
```

On failure the report needs both operands rendered, which `render.rs` already
does. So `expect` is a **form, not a function** — it must see the unevaluated
operands to report them; a native taking a `Bool` has already lost the
information by the time it is called. It lowers to a conditional fault carrying
the span, the rendered left, and the rendered right.

One form, not a family (`expectEq` / `expectSome` / …): `==` and pattern
matching cover the cases, and a large assertion vocabulary is exactly the kind
of surface area a 30M model would have to memorize.

### Tests live beside the code they test

Same file, `test` items interleaved with declarations. Three reasons, in
increasing order of importance:

1. One artifact per program — nothing to keep aligned.
2. The test documents the function, next to the function.
3. **The corpus.** A generated candidate that carries its own tests is one
   validated unit; a candidate plus a sidecar is two artifacts that can disagree.
   Same-file also makes the test *training signal about the code* — the model
   learns the idiom "this is how you state what this function does", which is
   the single most valuable thing it could learn from a synthetic corpus.

Tests are stripped before the metal build the same way any other tooling
artifact is — they are items in the AST, so this is a filter, not a
preprocessor.

### A test run is telemetry

The runner opens a span per test, emits an event per assertion, and closes with
a verdict. The collector already decodes these frames. Consequences worth having:

- `stitch test` on the board reports into Tempo and Prometheus with no new wire
  format — test results land beside the kernel's own spans.
- A flaky test is visible as a *distribution*, not an anecdote.
- The test runner is a collector, which is the same claim the diagram system
  makes and the same claim the funnel makes. One idea, three places.

### The Rust side becomes a runner, not an author

`cargo xtask test` must still fail when a canon program regresses, so a thin
Rust gate — the shape `canon.rs` already has — discovers every `.st` file,
runs its native tests, and asserts they pass. Rust keeps the job it is good at
(driving CI) and loses the job it was bad at (writing Stitch in string
literals).

`stitch/src/testing.rs` stays: it is the harness that *hosts* the interpreter,
and the native runner is built on it.

## What this unblocks

**sift's run stage** ([../plans/stage-0-validator-funnel.md](../plans/stage-0-validator-funnel.md))
becomes "run the candidate's own `test` items" — which is why this design comes
first. The alternative on the table was a `check()` convention invented for the
funnel alone: a shadow of native tests, with none of the authority story, that
the corpus would then have taught the model to write forever. **The corpus
teaches whatever convention it contains, so a convention adopted for expedience
becomes the language's de-facto test idiom.** That is the argument for settling
this before generating a single candidate.

It also sharpens the recipe: "write a program and the tests that prove it" is a
better prompt than "write a program", produces a self-validating artifact, and
gives the funnel a real run stage instead of "did `main` fault".

Two hazards the funnel must then handle, both real:

- **Degenerate tests.** `expect true` passes. The defence is mutation testing
  over the candidate's own AST (we own it, so there is no compile-per-mutant) —
  a test suite that survives every mutation of its program tested nothing.
- **Tests that agree with a wrong program.** Unfixable in general; bounded by
  the recipe stating the postcondition in prose, so the test has an external
  referent.

## Migration

Ordered so the suite is never dark:

1. `fs-image/lib/text.st` and `stats.st` — small, pure, no authority. Port
   `canon_behaviour.rs` and delete it. Proves the form on real code.
2. The Rust gate that runs native suites, wired into `cargo xtask test`.
3. `stim.st` — 1021 lines, the whole point. Port in tranches (motions, operators,
   modes), keeping the Rust file until its tranche is green in Stitch.
4. `prelude.st` gets tests, which it has never had.
5. On-target: a `stitch test` verb and an itest scenario that runs the canon's
   native suites under a booted kernel.

`stitch/tests/{print,memory_churn,expressions}.rs` stay in Rust: they test the
*implementation* (round-tripping, `Rc` behaviour, parser edge cases), not
programs written in the language. The line is whether the subject is Stitch code
or the Stitch runtime.

## Open questions

1. **`test "name" { … }` vs `test name() = …`.** Recommended: the string form,
   above. The counter-argument is that every other declaration binds an
   identifier, and a string name cannot be referenced — though nothing needs to
   reference it, and `--filter "pad widens"` is the only access anyone wants.
2. ~~**Is `expect` a keyword or a lowering of an existing form?**~~ *Settled
   (2026-07-26): a keyword.* It needs unevaluated operands either way, and the
   grammar has no macro concept — inventing one for this is a much larger
   commitment than one keyword.
3. **Setup/fixtures.** Deliberately omitted above. A test is a block; shared
   setup is a function call. Recommended: ship nothing, and see whether the
   canon actually wants more.
4. **Snapshot/`insta`-style assertions.** The printer makes them cheap and the
   FSM tests would use them heavily. Deferred, not rejected — they need a file
   convention and an accept workflow.
5. **Do `test` items type-check under the canon gate?** They should, which means
   the gate's definition of "a program" widens. Cheap, but it is the reason
   tests-in-Rust were invisible, so state it explicitly.
6. **Property-based testing.** The generative machinery is right here, and
   `babble` already samples from the oracle. Out of scope; noted because it will
   look obvious in six months.
