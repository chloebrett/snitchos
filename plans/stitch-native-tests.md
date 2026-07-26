# Native Stitch tests (TDD plan)

**Status:** 📐 **PLAN — starting.** Implements
[../docs/stitch-testing-design.md](../docs/stitch-testing-design.md): `test` and
`expect` as structural keywords, the runner, and the migration of the suites
that are currently Stitch-in-Rust-strings.

Blocks increment 3 of [stage-0-validator-funnel.md](stage-0-validator-funnel.md)
(sift's run stage = "run the candidate's own tests"). Increments 1–2 and 4–8 of
that plan are independent and can proceed in parallel.

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

### 4. Tests type-check — **blocked on a pre-existing checker gap**

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

- **RED (this increment)**: a mismatched body inside `{ … }` is reported;
  the canon and stim still type-check clean afterward.
- Fixing it means extending `synth` with a `Block` arm (walk statements, take
  the result expression's type) and probably `If`/`Match` arm bodies too. The
  blast radius is real: it may surface genuine errors in the canon *and* false
  positives from the gradual checker's blind spots. That is a decision worth
  making explicitly rather than as a side effect of adding a keyword.

### 5. Authority: a test with no `uses` has none

The point of the design. Enforced by the checker already — this increment proves
it and pins it.

- **RED**: a `test` whose body performs an effect without declaring the
  capability is a type error; the same body with `uses Telemetry` checks clean;
  `handle`/`without` inside a test body behave as they do anywhere.

### 6. The runner

- **RED**: a module with three tests (one passing, one failing, one faulting)
  produces three results with names, verdicts, and — for the failures — the
  message and location. A test that loops forever is reported as fuel
  exhaustion, not as a hang.
- Pure function over parsed items → results. Per-test fuel budget. No I/O.

### 7. The Rust gate

- **RED**: every `.st` file the repo ships has its native tests run, and a
  deliberately-broken fixture fails the gate. Same shape as `canon.rs`, wired
  into `cargo xtask test`.

### 8. Migration, in tranches

Each tranche is green in Stitch before its Rust file goes.

1. `lib/text.st` + `lib/stats.st` — port `canon_behaviour.rs`, delete it.
2. `stim.st` — the 1021-line prize. Motions, then operators, then modes.
3. `prelude.st` gets tests, which it has never had.

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
