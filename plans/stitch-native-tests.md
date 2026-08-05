# Native Stitch tests (TDD plan)

**Status:** 🟡 **Increments 1–8 DONE (2026-07-26 → 07-29). Increment 9 is all
that remains.** Implements
[../docs/stitch-testing-design.md](../docs/stitch-testing-design.md): `test` and
`expect` as structural keywords, the runner, and the migration of the suites
that were Stitch-in-Rust-strings.

Landed: `stitch::test_runner`, the `canon.rs` gate + anti-vacuity ratchet, and
the migration — `stim_fsm.rs` went 1021 lines / 61 tests → 61 lines / 5 `insta`
snapshots. The canon carries **89** native tests; `examples/stitch/` carries
**279** across 30 programs, and the corpus gate's run stage is built on this.

**Outstanding — increment 9 (below): the tests only ever run on the host.**
Nothing executes a canon suite under a booted kernel, so "validated by use, on
the metal" — the canon stratum's whole claim in
[../docs/generative-ladder.md](../docs/generative-ladder.md) — is currently
overstated with respect to the test suites themselves. Tracked as debt #17.

Unblocked increment 3 of [stage-0-validator-funnel.md](stage-0-validator-funnel.md)
(sift's run stage = "run the candidate's own tests").

**Keyword collision check (done):** neither `test` nor `expect` appears as an
identifier in the canon (`fs-image/`, `prelude.st`) or in babble's wordlist, so
promoting them to keywords breaks no shipped program and invalidates no cached
corpus.

---

## Increments

### 1. `test` lexes and parses

- **RED**: `parse_program(r#"test "adds" { 1 }"#)` yields one
  `Item::Test { name: "adds", uses: [], body }`.
- `TokenKind::Test` + `keyword()` arm; `Item::Test` variant; a parse arm
  alongside `prod`/`sum`/`contract`. Name is a **string literal** — the one
  declaration whose name is prose.
- `uses` clause parses the same way it does on `Func`.

### 2. The printer round-trips a `test`

`print.rs` has a round-trip contract and `tests/print.rs` enforces it over every
shipped `.st` file. A new item that does not print is a hole in that contract —
and the augmentation tier in sift depends on the printer being total.

- **RED**: a program containing a `test` item round-trips through
  parse → print → parse unchanged.

### 3. `expect` lexes, parses, and faults with both operands rendered

- **RED**: `expect 1 == 2` faults; the message renders `1` and `2` and carries
  the expect's span. `expect 1 == 1` evaluates to unit. `expect 3` (non-`Bool`)
  is a type error.
- `ExprKind::Expect { expr }`. It is a **form**, so lowering keeps the operand
  structure: when the inner expression is a comparison, the fault renders both
  sides via `render.rs`; otherwise it reports the expression's source span.

### 4. Tests type-check — **DONE**, including the checker gap it exposed

The canon gate's definition of "a program" widens to include `test` items —
which is precisely what tests-in-Rust were invisible to. `CoreItem::Test` now
routes through `check_callable`, so a test is checked exactly like a nullary
function.

**But `synth` does not descend into `Block`.** Found while writing increment 3's
type test; it predates tests entirely:

```
errors(r#"f() -> Int = "x""#)      // reported ✓
errors(r#"f() -> Int = { "x" }"#)  // silent ✗
```

Every real test body is a block, so until this is fixed the type stage only
holds for `= expr` bodies and the design's claim that test bodies are
type-checked is half-true. It also means the *canon gate itself* has been weaker than it
looks — `canon.rs` type-checks programs whose bodies are almost all blocks.

**Fixed.** `synth` gained a `Block` arm (statements synthesized for their own
errors, block's type = its result expression's, `Unit` when there is none), an
explicit `Without` arm (its body is absent from `child_exprs`, which the effect
walker owns), and a fallback that *descends* through un-typed forms instead of
shrugging at them — so an error nested inside a construct with no type rule yet
is still reported.

The feared blast radius did not materialise: the prelude, stim, and the whole
canon still type-check clean, and all 813 stitch+babble tests pass. The gate is
now real for block bodies, which retroactively strengthens `canon.rs` — it had
been checking almost nothing, since canon bodies are nearly all blocks.

Still gradual by omission: a `let` binding's type is not threaded into
`ctx.locals`, and `If`/`Match` still synthesize to `Dyn` rather than joining
their branches. Both are refinements, not gaps — they cost missed errors, never
false ones.

### 5. Authority: a test with no `uses` has none — **DONE** (see increment 6)

The point of the design. Enforced by the checker already — this increment proves
it and pins it.

- **RED**: a `test` whose body performs an effect without declaring the
  capability is a type error; the same body with `uses Telemetry` checks clean;
  `handle`/`without` inside a test body behave as they do anywhere.

### 6. The runner — **DONE**

`stitch::test_runner`: `run_tests(&[Item]) -> Vec<TestResult>`, plus
`run_tests_with_fuel`. Pure function over parsed items — no printing, no I/O, no
exit — which is what lets the host gate, the funnel's run stage, and an
on-target `stitch test` share one implementation.

- Every test runs, in source order; one failure does not stop the rest.
- `Verdict::{Passed, Failed { message, span }, Exhausted}`. Exhaustion is its own
  verdict because "never finished" and "was wrong" are different diagnoses —
  which is exactly the funnel's per-stage-death principle applied one level down.
- `DEFAULT_FUEL = 1_000_000` steps per test. The environment (prelude + natives +
  the program's declarations) is built **once** and refuelled per test via a new
  `Env::refuel`, so a file with many tests pays registration once.
- Classifying exhaustion needed the fault text; it is now
  `interp::FUEL_EXHAUSTED` rather than a literal repeated at the classifier.
- **Authority is enforced at runtime, both directions**: a test with no `uses`
  is refused when it performs an effect (the refusal names the missing cap), and
  one with `uses Telemetry` may. This is increment 5's runtime half; the
  checker's static half rides on `check_callable` from increment 4.

### 7. The Rust gate — **DONE**

Three tests in `stitch/tests/canon.rs`, which already owned "every shipped `.st`
file":

- `every_shipped_programs_native_tests_pass` — the gate. Rust *drives* the
  suite; the assertions live in the `.st` files.
- `the_native_test_gate_catches_a_failing_test` — the control, matching the type
  stage's existing one. Verified end-to-end by temporarily breaking a canon
  assertion: the failure names the file, the test, and both operands
  (`expect failed: 2 == 999`).
- `the_canon_carries_native_tests` — the **anti-vacuity ratchet**. The gate
  shipped green over zero tests, which is indistinguishable from green over a
  working suite. Floor of 6; raise it as tranches land, never lower it.

`cargo xtask itest` still 128/128 — `stats.st` ships into the ramfs image, and
`test` items are inert on the target (the registry binds no name for them).
Stripping them from the metal build is still worth doing for size, but nothing
depends on it.

### 8. Migration, in tranches

Each tranche is green in Stitch before its Rust file goes.

1. `lib/stats.st` + `lib/text.st` — **DONE**: 12 native tests, each beside the
   function it covers. Integer/string only, so they run on the metal (userspace
   FP is illegal). The ergonomic difference the design is about: the Rust
   versions needed a `format!`-assembled driver module, the native ones call
   `summarise`/`pad` directly because they are *inside* the module.

   **`canon_behaviour.rs` was reduced, not deleted.** All 11 of its behavioural
   assertions moved into Stitch, but one axis did not: a native test sits inside
   its module and calls unqualified, so it says nothing about whether `ext`
   exported anything or whether `stats.summarise` resolves from *another*
   module. Nothing else covered that — `builtin_module_use.rs` tests the
   built-in `Str`/`List` modules, and no canon program imports these two. So the
   file survives as two import smoke tests over the export boundary. Deleting it
   wholesale would have retired real coverage with every remaining test green.
2. `stim.st` — the 1021-line prize. **Motions tranche DONE** (8 tests: motion
   targets + wiseness, `h`/`l`, `j`/`k`, `0`/`$`, `^`, word motions, count
   accumulation, count repetition). `stim_fsm.rs` is down to 53 tests / 903
   lines; three helpers (`motion_rowcol`, `motion_wise`, `count_of`) went with
   them, since projecting a variant to a string tag is exactly what a native
   test does not need.

   **The port does not bloat the file** — the worry going in was that a ~1000
   line test suite would double a file that ships into the ramfs image. Measured:
   118 lines of Rust became ~104 lines of Stitch, and the two files together went
   1911 → 1897. The native form drops the `format!`, the doubled escaping, and
   the projection-to-string-tag, which pays for the extra assertions.

   Two forms are needed, not one: nullary variants compare with `==`
   (`effect == Redraw`) because they are singleton values, while a variant
   *carrying* a payload is a constructor when named bare, so `Save(_)` is still a
   `match`. Both pinned in `native_test_runner.rs`.

   **Operators tranche DONE** (12 tests: operator-pending, `d` + charwise, `dd`,
   `d` + linewise, `c`, `y`, `Y`, charwise paste, linewise paste, delete-feeds-
   register, cancel, counts-on-operators). `stim_fsm.rs` is down to 41 tests /
   634 lines; `pending_op` and `clipboard_wise` went with them. The two files
   together are now 1798 lines, down from 1911 at the start — the port keeps
   *shrinking* the total.

   Two small helpers live in `stim.st` for the tests: `pendingDelete` (a
   construction the operator tests repeat a dozen times) and `isEdit` (because
   `Edit(Str)` carries the buffer, so its tag is matched, not compared).

   **Text-objects tranche DONE** (10 tests: operators over word motions, `i`/`a`
   object-pending, `diw`, `ciw`/`yiw`, `daw`, the full `d`-`i`-`w` sequence,
   quote objects, `ci"`/`yi"`, the no-pair cancel, single quotes). `stim_fsm.rs`
   is down to 31 tests / 488 lines; `object_of`, `clipboard_text` and
   `pending_op_of` went with them — all five string-projection helpers are now
   gone, which was the file's whole reason for existing.

   **Modes / editing / save tranche DONE — `stim.st` migration complete.** All
   57 native tests now live in `stim.st`; `stim_fsm.rs` is **61 lines holding 5
   `insta` snapshot tests** and nothing else, down from 1021 lines / 61 tests.

   Both files together: **1583 lines, from 1911** — the port removed ~330 lines
   while *adding* assertions (the Rust tests often checked one coordinate where
   the native ones check both).

   **What deliberately stayed in Rust**: the snapshots. An `expect` compares two
   values someone wrote down; a snapshot records a whole structure nobody wants
   to write down — right for "the initial state is *this*, in full" and for the
   ANSI byte soup `renderFrame` emits, where being exhaustive-and-unwritten is
   the point. Native snapshot assertions remain deferred (they need a file
   convention + accept workflow), and that is now the *only* reason any stim test
   is in Rust.

   Gotcha for future renames: `insta` keys snapshots by test name, so renaming a
   snapshot test orphans its `.snap` file — `git mv` the snapshot alongside.

   Test-only helpers now in `stim.st`: `pendingDelete`, `pendingObject`, `at`,
   `isEdit`, `savedText`. The first three are constructions the tests repeat a
   dozen times; the last two exist because `Edit(Str)`/`Save(Str)` carry payloads
   and so are matched rather than compared.
3. **`prelude.st` gets tests — DONE (2026-07-29).** 20 native tests, taking the
   canon's suite count from 69 to 89 (`canon.rs::the_canon_carries_native_tests`
   ratcheted to match). The prelude was the last shipped `.st` file with no
   assertions of its own, and it is the one loaded into *every* program's globals.

   The cases were chosen so they can fail. Three carry the weight:

   - **`any`/`all` on the empty list** — false and true respectively. The vacuous
     pair is what a fold gets wrong by seeding the wrong identity, and it is the
     pair intuition argues with.
   - **`find` returns the first match, not the last**, over a list with three
     matches. A fold that overwrites `acc` on every hit passes any single-match
     test.
   - **`first`/`last` over three elements, both ends asserted.** They are one fold
     body apart, and swapping them passes on any one-element list. Verified
     falsifiable: rewriting `first`'s `Some(_) => acc` arm to `Some(_) => Some(x)`
     fails with `expect failed: 9 == 7` and names the test.

   Also covered: `count`/`total` (including the empty fold), `contains`,
   `min`/`max` (asserted with the extreme at both ends, since an accumulator that
   never updates looks right when the answer is already first), `flatten` (with an
   empty inner list — the case that separates "concatenate" from "collect"), the
   five Maybe helpers (`andThen` vs `mapMaybe` pinned on the one case that tells
   them apart: only `andThen` can turn a `Some` into a `None`), and both contract
   implementations — `Try` on Maybe/Result and `Functor` on Maybe/Result, the
   latter asserting `Err`'s payload survives a `map`.

   `each` is **not** covered: its whole effect is the side effect, and there is no
   double for it at prelude level. It wants an effect handler, which is the
   deferred fixtures/doubles work below.

   **Cost, measured rather than assumed** (the prelude is parsed at every program
   start, on target too): source 3505 → 8456 bytes, parse 517µs → 725µs (+40%),
   `build_env` unchanged at ~135µs — `Item::Test` lowers to a `CoreItem::Test` that
   nothing registers as a binding, so the cost is parse-only. That is the concrete
   number the "strip tests from the metal build" item below was missing.

`tests/{print,memory_churn,expressions}.rs` **stay in Rust**: their subject is
the runtime, not a Stitch program.

### 9. On-target + telemetry

- A span per test, an event per assertion — the collector already decodes them.
- A `stitch test` verb, and an itest scenario running the canon's suites under a
  booted kernel. This is what makes "validated by use, on the metal" cover the
  test suite too.

## Deferred

Snapshot assertions (need a file convention + accept workflow), fixtures/setup
(a test is a block; shared setup is a function call — ship nothing until the
canon asks), property-based testing (the generative machinery is right here,
which is exactly why it should wait).
