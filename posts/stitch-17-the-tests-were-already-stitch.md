# Stitch 17 — the tests were already Stitch

- I sat down to define one thing: what "run the tests" means for the corpus funnel. The [validator funnel](../plans/stage-0-validator-funnel.md) grades a generated program by parse → type → **run** → dedup, and that third stage needed a definition, because Stitch had no test construct. The obvious move was a convention: require every candidate to carry a nullary `check()` returning `Bool`, call it, demand `true`. Ten minutes of work.
- I didn't do it, for a reason that only shows up if you think about what the corpus is *for*: **the corpus teaches whatever convention it contains.** Generate forty thousand programs that all end in `check()`, train on them, and `check()` is Stitch's test idiom forever — chosen by a funnel implementation detail, in an afternoon, by nobody.
- So the question changed from "what does the funnel call" to "what does testing look like in this language." This post is that, and the migration that followed: 1021 lines of Rust deleted, 57 tests moved into the file they test, and a gate that turned out to have been checking almost nothing.

## the file that gave it away

- `stitch/tests/stim_fsm.rs` was 1021 lines of Rust whose job was to write Stitch:

```rust
fn step_mode(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ let st = step({setup}, "{key}")  match st.state.mode {{ Normal => "Normal"  Insert => "Insert"  … }} }}"#
    ))
}
```

- That is a Stitch program. It is stored as a Rust string literal, and so it gets none of what this project has spent months building for Stitch programs. No diagnostics — faults carry `file:line:col` now ([stitch 13](stitch-13-the-fault-learns-to-point.md)), and a fault inside a `format!`-assembled string cites a location that does not exist. No type checking — the canon gate checks every shipped `.st` file, and the most-exercised Stitch in the repo was invisible to it. No printer, no editor, no completion: stim's own FSM could not open its own tests.
- And the tell that makes it obvious in hindsight: **that helper exists to turn a variant into a string.** Rust cannot name a Stitch `Normal`, so every assertion projects the value down to a tag it *can* compare. Five such helpers, and between them a third of the file.
- The tests were always Stitch. Rust was just where they were parked.

## what the language already had

- Here is why this was a week and not a quarter. I had assumed a test framework needs machinery — doubles, injection, isolation. Going through the list, every piece was already shipped and being used for something else:

| testing need | Stitch primitive |
|---|---|
| test doubles / mocks | `handle <op> with <handler> { … }` |
| "this must not touch the disk" | `without Cap { … }` |
| declaring what a test may do | `uses Cap` on the declaration |
| failure location | spanned faults + backtraces |
| reporting | telemetry frames, decoded by the collector |
| discovery | we own the AST |

- Effect handlers are the headline. `FakePlatform` exists in Rust because Stitch tests could not install their own doubles; in Stitch they can, and **the double is written in the language under test** rather than in the host's. `without Fs { … }` asserts the negative — that a path *cannot* reach the disk — and fails at the perform site with a span, which is a much stronger claim than "no writes were recorded."
- The work was a keyword, a runner, and a migration. Not a framework.

## the shape

```stitch
test "pad widens to the column and never truncates" {
  expect pad("ok", 5) == "ok   "
  expect pad("overlong", 3) == "overlong"
}
```

- `test` joins `prod` / `sum` / `contract` / `on` as a **structural** keyword, which is the rule the grammar already stated: a function carries no keyword because it is the lightest declaration; the structural forms carry one. A test is structural — nothing calls it by name, it takes no parameters, it returns nothing.
- The name is a **string, not an identifier**, and it is the only declaration in the language where that's true. Test names are prose; they end up in reports. Forcing them through identifier syntax produces `pad_widens_to_the_column_and_never_truncates`, which is a sentence wearing a costume.
- Two decisions that took longer than they look:

- **A test's `uses` clause *is* its authority.** No clause, no authority: no telemetry, no console, no clock. Pure by construction, therefore deterministic by construction — and the capabilities-as-effects checker ([stitch 08](stitch-08-the-language-learns-to-say-no.md)) already enforces it, so a test that performs an undeclared effect is a *type error*, not a runtime surprise. This inverts the usual arrangement, where tests inherit the whole process's authority and isolation is a convention. Here the pure case is the default and every escape is written down. A side effect I like more than I expected: **the `uses` clause of a test reads as a specification of what the code under test needs.**

- **`expect` is a form, not a function.** A native taking a `Bool` has already lost the information by the time it runs — it can only ever say "expected true, got false." The form keeps the unevaluated operands, so a comparison is evaluated operand-wise and the failure names both sides:

```
expect failed: 2 == 999
```

- Each operand is evaluated exactly once and the comparison applied to the results, so an effectful operand isn't run twice to build the message. One form, not a family: `==` and pattern matching cover the cases, and a large assertion vocabulary is exactly the surface area a 30M model would have to memorise.

## the gate that had been checking almost nothing

- Writing the type test for `expect` turned up something that predates all of this. `expect 3` should be an error — the operand must be `Bool`. It was reported. Then I wrote the same assertion inside a test body, which is a block, and it was silent.

```
f() -> Int = "x"        // reported ✓
f() -> Int = { "x" }    // silent  ✗
```

- **`synth` never descended into `Block`.** Not for tests — for anything. Every function body anyone actually writes is a block, so the gradual checker had been reporting type errors only in one-line `= expr` bodies for its entire life.
- Which means `canon.rs` — the gate whose whole job is "every shipped program type-checks clean," written specifically so the canon can't rot into something no model should imitate — had been passing on programs whose bodies it never entered. It was green. It had always been green. That's the problem.
- The fix was a `Block` arm (statements synthesized for their own errors; the block's type is its result expression's, `Unit` when there is none), an explicit `Without` arm — its body is deliberately absent from `child_exprs`, which the effect walker owns, so it was the one place an error could still hide — and a fallback that *descends* through un-typed forms instead of shrugging at them.
- I expected a blast radius and prepared to argue about false positives. There wasn't one: prelude, stim, and the whole canon still check clean. The gate simply became real.
- [Post 69](post-69-the-bug-was-where-no-test-could-reach.md) is this post's sibling and got there first: the compiler, the review, the test suite — none of them check what you've put out of their reach, so the danger lives exactly there. A checker that stops at `{` has put every real body out of its own reach, and a passing suite says nothing about it.

## the runner, and two verdicts

- `stitch::test_runner` is 105 lines and a **pure function over parsed items**: items in, verdicts out. No printing, no I/O, no process exit. That's what lets three callers that share nothing else use one implementation — the host gate, the corpus funnel's run stage, and a future `stitch test` on the metal, where there is no stdout to print to.
- Every test runs, in source order. A suite that stops at the first failure reports one problem when you have three.
- `Verdict::{Passed, Failed { message, span }, Exhausted}`. **Exhaustion is its own verdict, not a fault.** A generated candidate can loop forever and the funnel must not — but more than that, "this test never finished" and "this test was wrong" call for different fixes. That's the funnel's own principle (the stage a candidate dies at is the diagnosis) applied one level down.
- The budget is 1,000,000 evaluation steps per test. The environment — prelude, natives, the program's own declarations — is built **once** and refuelled per test, because the fuel counter is run-shared, so refilling the cell covers the whole call tree. A file with ninety tests pays prelude registration once instead of ninety times.

## a green gate over zero tests

- The Rust gate went into `canon.rs`, which already owned "every shipped `.st` file." Rust *drives* the suite; the assertions live in the `.st` files. Then a control test, because the type stage has one for the same reason: a gate that passes on arrival is only reassuring if it can fail.
- And then a third test I didn't plan, because when I first ran the gate it was green — over **zero tests.** No canon program had any yet.
- That state is indistinguishable from green over a working suite. It is precisely the failure mode this whole design accuses tests-in-Rust of. So: an anti-vacuity ratchet, asserting the canon carries at least *N* native tests. Floor of 6 then, 89 now; raise it as tranches land, never lower it.
- I also verified the gate end-to-end rather than trusting it, by breaking a canon assertion on purpose:

```
native tests failed:
fs-image/lib/stats.st: mean truncates rather than rounding
  — Failed { message: "expect failed: 2 == 999", span: Some(…) }
```

- File, test name, both operands.

## the migration, and a number I got wrong

- `stats.st` and `text.st` first, then `stim.st` in four tranches: motions, operators, text objects, modes.
- I went in worried about size. `stim.st` ships into the ramfs image, and ~1000 lines of tests would double it. Measured after the first tranche: 118 lines of Rust became ~104 lines of Stitch, and the two files *together* went 1911 → 1897. By the end: **1583 lines, from 1911** — the port removed ~330 lines while *adding* assertions, since the Rust tests often checked one coordinate where the native ones check both. The native form drops the `format!`, the doubled escaping, and the projection-to-a-string-tag, and that pays for a lot.
- `stim_fsm.rs`: **1021 lines / 61 tests → 61 lines / 5 tests.**
- One thing to check before porting, which I did check rather than assume: **nullary variants compare with `==`** — they're registered as singleton values, so `effect == Redraw` means what it looks like. A variant *carrying* a payload is a constructor when named bare, so "is it a `Save`, whatever it holds" is still a `match`. Two forms, not one, both pinned as tests.
- The difference in the read is the whole argument:

```rust
assert_eq!(motion_wise(r#"initialState("hello")"#, "$"), s("Charwise"));
```

```stitch
expect match motionTarget(initialState("hello"), "$") {
  Some(t) => t.col == 4 and t.wise == Charwise
  None => false
}
```

## over-parenthesising is not the safe direction

- Adding `expect` broke the printer, and babble's round-trip fuzzer found it before I did. Seed 27: `(expect 1)?.field` reprinted as `expect 1?.field`. `expect` takes its operand at binding power 0, so everything to its right is part of the operand — in a tighter position it must be parenthesised or it swallows the context.
- The obvious generalisation is that anything ending in a subexpression needs the same treatment, so `handle` and `without` got it too. Seed 2 said no:

```
port ( ) -> buffer { expect $a == { } without delta { } . items }
```

- `without delta { }.items` parsed fine unparenthesised. The printer, now treating `without` as loose, emitted `(without delta { }).items` — and that leading `(` was read as a **call of the previous statement.** `handle` and `without` close with a delimited `{ … }`, so nothing to their right can be drawn in; they never needed the parens, and adding them was a correctness bug.
- This is [stitch 16](stitch-16-the-gap-was-doing-work.md)'s lesson arriving from the other side. That post found that a stray `(` changes what the previous line means. This one found that the *fix* for a real swallowing bug will happily manufacture stray `(`s if you over-apply it. In a grammar where juxtaposition is application, there is no safe direction to round in — only the correct one.

## what stayed in Rust, and why

- Five `insta` snapshot tests. An `expect` compares two values someone wrote down; a snapshot records a whole structure nobody wants to write down. That's the right tool for "the initial state is *this*, in full" and for the ANSI byte soup `renderFrame` emits, where being exhaustive-and-unwritten is the point. Native snapshot assertions are deferred, not rejected — they need a file convention and an accept workflow.
- `print.rs`, `memory_churn.rs`, `expressions.rs` stay too. **The line is whether the subject is Stitch code or the Stitch runtime.** Round-tripping, `Rc` behaviour, parser edge cases — those are tests *of the implementation*, and they belong in the implementation's language.
- `canon_behaviour.rs` was the interesting call. All 11 of its behavioural assertions moved into Stitch, so the obvious move was to delete it. But a native test sits *inside* its module and calls `summarise(…)` unqualified — which says nothing about whether `ext` exported anything, or whether `stats.summarise` resolves from *another* module. Nothing else covered that: `builtin_module_use.rs` tests the built-in `Str`/`List` modules, and no canon program imports these two. Deleting it would have retired real coverage of the export boundary with every remaining test green. It survives as two import smoke tests, 142 lines down to 53.
- (A small gotcha, recorded so the next person doesn't lose ten minutes: `insta` keys snapshots by test *name*, so renaming a snapshot test orphans its `.snap` file. `git mv` the snapshot alongside.)

## where it went

- The canon carries **89 native tests** — `prelude.st` 20, `stim.st` 57, `stats.st` and `text.st` 6 each. The prelude was the last shipped file with no assertions of its own, and it's the one loaded into every program's globals.
- The corpus effort took it and ran: **279 native tests across 30 example programs** in `examples/stitch/`. The gate that [post 66](post-66-cond-is-not-a-keyword.md) and [post 75](post-75-it-was-the-volume.md) describe — the thing grading a manufactured corpus — runs on `test` and `expect`. It caught a real bug in a brand-new example on its first run, which is the cheapest possible evidence the feature was worth building.
- And the recipe got better than the funnel needed. "Write a program and the tests that prove it" produces a self-validating artifact, gives the run stage something real to do instead of "did `main` fault", and — because the corpus teaches whatever it contains — teaches the model the idiom *this is how you state what this function does*. Which is plausibly the most valuable thing a synthetic corpus could carry.
- Two hazards that came with it, both real and neither solved: **degenerate tests** (`expect true` passes; the defence is mutation testing over the candidate's own AST, which is cheap because we own it) and **tests that agree with a wrong program** (unfixable in general; bounded by the recipe stating the postcondition in prose, so the test has an external referent).
- Still open: [increment 9](../plans/stitch-native-tests.md) — a span per test, an event per assertion, and a `stitch test` verb with an itest scenario. The collector already decodes those frames, so the test runner becomes a collector and results land in Tempo beside the kernel's own spans. That's also what would make "validated by use, on the metal" finally cover the test suite itself — which, given that's the canon's whole claim, it currently does not.

## the shape of the week

- The design took an afternoon and was mostly *subtraction*: every mechanism I reached for was already in the language, doing another job. The implementation took a few days. The migration took longer than both and taught me the most, because porting a thousand lines of assertions is a slow, close read of what each one was actually claiming — and about a dozen times the answer was "less than it looks."
- The thing I'd tell myself at the start: the reason to move tests into the language isn't tidiness or dogfooding. It's that **a test written in the host language is exempt from every gate the guest language has.** Ten months of work — spanned faults, a type checker, a printer, a completion oracle — and the most-exercised Stitch in the repo was outside all of it, because it was inside a string.
