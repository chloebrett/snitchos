# Stitch examples corpus — findings

Running notes from writing [stitch-examples-corpus.md](stitch-examples-corpus.md)'s
30 programs. One entry per program once it lands (or as it's in progress, if
something is worth capturing before the file is finished). Optimized for
"what would I want to know before writing the next one" — language
limitations, awkward constructs, stdlib/prelude gaps, checker surprises.
Fixed bugs and shipped workarounds belong in `.claude/CLAUDE.md` or
`docs/language-design.md` once they're load-bearing; this doc is the lab
notebook they get promoted from.

Gate: `stitch/tests/examples.rs` — parses + type-checks every file under
`examples/stitch/`, and asserts every `test` item in every file passes. Run
with `cargo nextest run -p stitch --test examples`.

---

## Template for each entry

```
### N. name.st

- Lines: NNN. Tests: N (N pass).
- What it exercises: ...
- Friction / limitations hit: ...
- Anything surprising: ...
```

---

**Update (same day):** the `Str<->Int` gap below was cheap enough to just
fix. `Str.parseInt(s) -> Maybe<Int>` and `toStr(x) -> Str` landed in
`stitch/src/natives.rs` (TDD: RED tests against the not-yet-existing natives,
then the two implementations — `parseInt` is `text.parse::<i64>()` wrapped
in `Some`/`None`; `toStr` just exposes the `Value::display` string
interpolation already used). Full `cargo xtask test` gate green, `no_std`
riscv64 lib build checked clean. `json.st` is left as-is (its hand-written
`digitValue`/`intToStr` still work and are decent teaching material for "here
is what you'd write without the native"), but **every later program that
needs numeric parsing should use `Str.parseInt`/`toStr` directly** — the
Int-only-numbers scope note in the entry below no longer applies to *future*
programs, only explains why *this one* is Int-only. The `Map`-construction
gap (next finding) got no such fix — it's a real design question (what
would "build a Map at runtime" even look like: `Map.fromList`? a `builder`?)
rather than a cheap missing native, so it stays a documented workaround for
now.

### 1. json.st

- Lines: 381 (was 399 before the `Str.parseInt`/`toStr` refactor below —
  net *shorter* despite added explanatory comments, since `digitValue`,
  `intToStr`, `digitsToStr`, and `digitChar` all went away). Tests: 13 (13
  pass, unchanged — the existing suite was the regression safety net for the
  refactor, not new tests; this was RED-GREEN-*REFACTOR*, not new
  behavior).
- What it exercises: recursive `sum` AST, hand-threaded parser state (no
  cursor/iterator type, so `(value, pos)` tuples thread by hand through
  `Result`), deep `match`/conditional nesting, tuple types `(Str, Json)` as a
  sum-variant field and as a return type, recursion (`scanStr`/`scanDigits`/
  `scanArr`/`scanObj` are all self-recursive, not folds — this is the first
  program in the batch where a `fold` over an existing collection doesn't fit
  because the *input* is a string being consumed character-by-character).

- **No `Str -> Int` or `Int -> Float` conversion exists anywhere in the
  stdlib.** Not `parseInt`, not `toFloat`, not even `Int -> Str` for printing
  a number back out. `plans/lang/samples.st` (the pre-implementation "feel"
  doc) writes `parseInt?` and `.toFloat()` as if they exist — they don't;
  that file is explicitly unvalidated. Consequence for this program: number
  parsing is scoped to `Int` only (a `3.14` is a documented parse error, not
  silent truncation), and both directions had to be hand-written — digit
  character → `Int` (`digitValue`, a 10-arm `match` naming every digit,
  since there's no character-code native to compute it) and `Int` → digit
  character (`digitChar`/`intToStr`/`digitsToStr`, peeling off `n % 10` /
  `n / 10` and doing the same 10-arm `match` in reverse). **This will recur**
  — `calc.st` and `semver.st` both parse numbers from text, `poker.st` may
  want to render ranks. Worth a shared `examples/stitch/lib/` helper once
  the second program needs it (text.st-style, per the corpus doc's note that
  examples may reuse `fs-image/lib/`-style helpers).

- **`Map<K, V>` cannot be built from a runtime-computed, variable-length
  sequence of pairs — only from a literal with a parse-time-fixed entry
  count** (`[k1: v1, k2: v2]`, keys/values are expressions evaluated at run
  time, but *how many* entries is fixed in the source text). There is no
  `insert`/`update`/`fromList`/spread-into-map. This is a hard blocker for
  "parse N key-value pairs into a dictionary" — exactly what a JSON object
  is. Worked around by representing `JObj` as `List<(Str, Json)>` (an
  association list) and writing `jsonGet` as the linear-scan lookup `Map`
  would otherwise give for free. **This is the single biggest finding of the
  batch so far** — it will hit `graph.st` (`Map<Str, List<Str>>` adjacency,
  built from parsed/generated edges), `trie.st` (`Map<Str, Trie>` children),
  `formula.st` (cell table), `template.st` (context), `markov.st`
  (transition table), `logstats.st`/`poker.st` (tallies/grouping) — anything
  that *builds* a dictionary rather than reading one written as a literal.
  Each of those will need to decide: association list (this program's
  choice — simple, honest, O(n) lookup), or find out whether there's a
  lower-level construction path this file didn't need (worth one focused
  check before assuming the same workaround every time).

- **Chained conditionals are a hard parse error, not just a style
  preference**: `cond1 => a | cond2 => b | c` fails with "chained
  conditionals aren't allowed — use `match` for more than two cases". Hit
  once in `skipWs`'s original form (skip-while-whitespace, the natural shape
  of which is "base case, else recurse-or-return"). Fixed by parenthesizing
  the nested conditional: `cond1 => a | (cond2 => b | c)`. Cheap once known,
  but the error only fires at the second `=>`, so it's easy to write three
  or four `=>`/`|` pairs in a row while drafting and get one error pointing
  at the last pair rather than a clear "here's your first offense" signal.
  Rule of thumb going forward: nest explicitly in parens (or reach for
  `match`) the moment a conditional's else-branch is itself conditional.

- **String interpolation's `{`/`}` escaping bit twice, and unhelpfully.**
  `"{expr}"` interpolates; a literal brace needs `{{`/`}}` (documented in
  `language-design.md`, easy to forget when the string content just *is*
  JSON, which is brace-heavy). The failure mode is bad: an unescaped lone
  `"{"` isn't a clean parse error at that string — the lexer treats it as an
  unterminated interpolation and keeps consuming (including subsequent `//`
  comments, which stop being comments once the lexer thinks it's inside a
  string) until it happens to hit a `"` inside a *later*, unrelated
  comment's own prose, which closes the runaway "string," after which the
  very next character is wherever the error actually gets reported. Both
  failures here reported at a location many lines past the real
  mistake (a backtick inside a doc comment, and a totally unrelated
  `match` clause) — the span pointed at collateral damage, not the cause.
  Diagnosis needed a scratch `stitch::lexer::lex` dump to find the real
  unterminated `{`. Worth a `lex-json-strings-full-of-braces`-shaped upstream
  issue: either a better lex error ("unterminated `{` interpolation opened
  at line N" instead of "unexpected character" at wherever it gives up), or
  nothing — but the diagnosis cost is worth flagging since every future
  program that builds strings containing `{`/`}` (any of the JSON-adjacent
  or templating ones — `template.st` especially) will hit this if it forgets
  the doubling.

- `Result`'s `Functor.map` (from the prelude) maps the `Ok` value alone —
  fine when a step returns a bare value, but every parse step here returns
  `Result<(value, pos), Str>`, and re-wrapping just the value half of that
  pair through `.map` reads worse than a three-line bespoke `mapOkPair`.
  Not a limitation, just a reminder that the general combinator and the
  problem's actual shape (value-plus-cursor) don't always line up.

- Nothing else fought the language. Recursion, `match` with guards-by-nesting,
  tuples, `Result`/`?`-free manual threading (didn't reach for `?` here since
  every step needs its cursor back, not just its value — `?` unwraps to a
  bare value) all read naturally once the two representational workarounds
  above were in place.

### 2. bank.st

- Lines: 307. Tests: 16 (16 pass).
- What it exercises: `mut` fields/methods, a user-defined `contract` (`Show`)
  + `on Account : Show`, capability rows on methods (`uses Telemetry`,
  `uses FsWrite`), `sum` variants carrying data (`TransferOut(Str)`), `?`
  through method calls, and — the actual point of the file — the gap between
  what `docs/language-design.md` says `mut` does and what the interpreter
  does.

- **Fixed (2026-07-26):** `docs/language-design.md` documented this wrong
  (Java/Kotlin reference semantics) in three places — the type-system
  intro, the "Mutability is opt-in" bullet, and the `on`-methods bullet.
  All three now describe value semantics with call-site write-back, cite
  the interpreter tests that prove it, and point at this file's `bank.st`
  as the worked example. What follows is why the fix was needed.

- **The single biggest finding of the whole batch so far: `mut` is value
  semantics with call-site write-back, not the Java/Kotlin reference
  semantics `language-design.md` used to document.** The design doc's `on` section
  says a `mut` field "is visible through all aliases (Java/Kotlin semantics,
  not value copies)" — i.e. "everything is a heap reference; two bindings
  that hold the same object see each other's mutations." The shipped
  interpreter does the opposite, **on purpose, with its own explicit tests
  asserting it**: `interp.rs::field_assignment_does_not_alias_a_copy` and
  `a_mut_method_does_not_alias_a_copy` both construct two bindings from the
  same starting record, mutate one, and assert the other is untouched. The
  mechanism (`eval_method_call` in `interp.rs`, and its own comment: *"a
  `mut` method binds `@` mutably so its body can reassign `@`/`@field`, and
  the result is written back to the caller afterwards (value semantics, so
  mutation isn't shared until we reassign the caller's place)"*): a `mut`
  method call rebuilds `@` inside its own call frame and then
  `assign_place`s the final value back onto **the exact expression the
  method was called on** (a `Var`, `@`, or a field path — checked by
  `is_assignable_place`, which is why `mut` methods refuse to run on a
  temporary at all: *"cannot call mut method `f` on a temporary — it has no
  place to write back to"*). Two bindings that happen to hold "the same"
  record at call time are not aliases of one heap cell; each is its own
  copy from the moment they diverged, and only the one actually named in
  the call expression gets updated.
  - **Consequence for any program with a collection of mutable objects**
    (this file's `Ledger.accounts: List<Account>`, and later `sched.st`'s
    task list, `inventory.st`'s stock, anything shaped like "a list of
    records I mutate one at a time"): an object pulled out of the list via
    `find`/`at`/pattern-matching is a copy. Mutating it does nothing to the
    list. Every one of `Ledger`'s methods has to explicitly find, mutate a
    *local* binding, then write the result back into `@accounts`
    (`replaceAccount`, a `map` that swaps the matching element) — the same
    discipline `List.set` already implies, just made unavoidable rather
    than a choice.
  - This was a documentation bug, not a language bug — the interpreter's
    own behavior is well-tested and (once you know it) is a perfectly
    coherent design (predictable, no spooky-action-at-a-distance aliasing).
    But `language-design.md`'s "Mutability is opt-in" paragraph was
    actively wrong about what opting in gets you, and anyone reading only
    that doc before writing `mut`-heavy code would build the wrong mental
    model, exactly as this file's first draft did. Now fixed, per above.

- **A second, related, and previously-mis-stated finding: the "mut receiver
  binding" discipline (calling a `mut` method needs `let mut x`, not plain
  `let x`) *is* enforced — just at runtime by the interpreter
  (`env.rs::Scope`'s `assign` refuses an immutable cell, surfacing as
  "cannot assign to immutable `x` (declare it with `let mut`)"), not
  statically by the checker.** json.st's entry didn't need this (no `mut`
  data there), so it went unchecked; writing `bank.st`'s tests with plain
  `let ledger = newLedger()` hit it immediately and uniformly across every
  test that called a `mut` method. Correction to a claim implicit in this
  doc's own earlier grep-based check.rs pass: "unenforced" was only true of
  the *static* checker — the *runtime* enforces it, with a clear message.
  Once known, cheap: every binding that will later take a `.mutMethod()`
  call needs `let mut` from the start.

- **`handle` is a reserved keyword** (`handle op with f { … }`, the
  effect-handler form — `TokenKind::Handle` in `lexer.rs`), so it can't be
  used as a parameter or variable name. Bit `auditLine`'s first parameter,
  which is exactly the name the native itself uses in its own doc comment
  (`fsWrite(fileHandle, text)` — actually already `fileHandle` there, which
  is what tipped off the fix). Diagnosing it cost more than it should have:
  the parse error ("expected parameter name") pointed at a *different*
  token than the actual `handle` occurrence, because the file's byte
  offsets and a naive Python `str[a:b]` slice diverge once the file has
  multi-byte UTF-8 (this file's comments are full of em dashes) — the
  fix was to re-slice the raw bytes, not the decoded string. Reserved
  words seen so far: the "structural" keywords in `language-design.md`
  (`prod`/`sum`/`contract`/`on`/`let`/`mut`/`use`/`ext`/`free`/`match`) plus
  three from the effects system that are easy to forget mid-flow because
  they read as ordinary English nouns/verbs: `handle`, `with`, `without`.
  Worth scanning identifier choices against `lexer.rs`'s `keyword()` table
  before committing to a name in any future program that touches capability
  or resource-handle vocabulary.

- **Piping into a call and then `?`-ing the *whole pipe* needs explicit
  parens: `(x |> f(a))?`, not `x |> f(a)?`.** `?` is parsed as a tight
  postfix operator in the same pass as call/index/field access (`parser.rs`,
  around the `TokenKind::Question` arm), so it binds to `f(a)` before `|>`
  ever sees it: `x |> f(a)?` parses as `x |> (f(a)?)`. Since `eval_pipe`'s
  first-argument-insertion trick only fires when the pipe's right side is
  *literally* a `CoreExprKind::Call` (`natives.rs`... actually
  `interp.rs::eval_pipe`), a `?`-wrapped call doesn't match that shape, so
  the fallback path runs instead: evaluate the right side as an ordinary
  value and apply the piped value to *that*. Evaluating `f(a)?` on its own
  calls `f` with just `a` — one argument short if `f` expects the piped
  value as its first parameter — which surfaces as a generic arity error
  ("function expects 2 argument(s), got 1") with no hint that precedence,
  not argument count, is the actual bug. Hit twice in this file
  (`Ledger.deposit`/`withdraw`'s `@findAccount(id) |> okOr(...)？` and
  `transfer`'s two validation lines) before the pattern was recognized.

- **The maximal-munch call-paren gotcha (already in project memory —
  `stitch_maximal_munch_call_paren.md` — but this is its first hit inside
  *this* batch) recurs the moment two statements are adjacent and the
  second starts with `(`.** `transfer`'s two validation lines,
  `(@findAccount(fromId) |> …)?` then `(@findAccount(toId) |> …)?` on the
  next line, fused into one expression — the first statement's result got
  *called* with the second statement's parenthesized expression as its
  argument, faulting with a mangled message ("cannot call a a record" — a
  minor doubled-article bug in the error text itself, `{}` `{}` where the
  second already includes its own article). Fixed by giving each line a
  `let _ = …` prefix, which starts the statement with a keyword instead of
  `(` and rules out the fusion. **Rule of thumb, promoted from a one-off
  gotcha to a habit**: never let a statement start with a bare `(` if the
  previous statement could grammatically be "called" — prefer `let _ = (…)`
  for a parenthesized expression kept only for its side effect (an
  early-return `?`, a `expect`, an effect call).

- Two things that *didn't* need a workaround, worth noting because they're
  easy to assume are broken given the density of findings above: `?`
  chaining through a `mut` method call (`acc.withdraw(amount)?`) composes
  cleanly with the write-back — a short-circuited `Err` leaves `acc`'s
  local binding unchanged (harmless no-op write-back) and propagates
  normally. And capability authority genuinely is per-call-boundary, not
  inherited *or* filtered by the caller: `transferAudited` declares only
  `uses FsWrite`, calls `@transfer` (which independently declares and gets
  its own `uses Telemetry`) with no Telemetry in `transferAudited`'s own
  row — and it works, exactly as `natives.rs`'s `shout()/main()` refusal
  test implies from the other direction. **Qualified by shell.st's entry
  below**: that's true at *runtime*, and true of the *static checker* for
  method calls specifically — but the checker requires transitive
  `uses`-propagation for plain function calls, an asymmetry this file
  didn't hit because `@transfer` is a method call.

### 3. vm.st

- Lines: 242. Tests: 16 (16 pass).
- What it exercises: a `sum`-typed instruction set, a self-tail-recursive
  `step` driver, `Result`-threaded error handling, a named `prod Popped`
  used instead of `at2`-style tuple access (see json.st's entry — first
  payoff of that "prefer a named product once a pair is read more than
  once" lesson).

- **Fixed (2026-07-26): a real interpreter bug, not a documentation one —
  `Env::bind` reset the self-tail-call marker on every `let`/match-binding,
  silently disabling the tail-call trampoline for almost any realistic
  recursive function.** This is the most consequential finding in the batch
  so far, more so than the `mut`-aliasing doc bug, because it's a
  correctness/scalability bug in the shipped interpreter, not a stale
  sentence in a design doc.

  - **Symptom:** `step`'s loop over `sumTo(5)`'s ~50-odd instructions faulted
    with `"call stack too deep"` (`MAX_CALL_DEPTH = 48`, `env.rs`) even
    though `step(prog, state) = match … { … => step(prog, next) }` is
    exactly the self-tail-recursive shape `interp.rs`'s own tests
    (`self_tail_recursive_function_via_match_runs_in_bounded_stack`) prove
    gets trampolined for a *million* iterations.
  - **Isolated by bisection** (`cargo nextest run` in a loop, narrowing a
    scratch repro added to `stitch/tests/examples.rs`): the difference
    between the working million-iteration test and my failing ~50-iteration
    one wasn't iteration count, nesting depth, or calling a helper function
    from the match *subject* — it was that every one of my match arms
    **bound a variable** (`Some(instr)`, `Ok(Continue(next))`, or a plain
    `let`), where the passing stdlib tests use a wildcard `_` for their
    recursive arm. `try_match`'s `Pattern::Binding` arm calls `env.extend`,
    which calls `Env::bind`; `Pattern::Wildcard` just clones the env.
  - **Root cause** (`env.rs::bind`, the function backing `extend`/
    `extend_mut`): hardcoded `self_closure: None` on every call. `bind` runs
    for *every* local variable binding — a `let`, or a pattern that names
    rather than discards a value — not just at real call boundaries. Any
    binding between the start of a self-tail-recursive function's body and
    its tail call wiped the "I am still executing my own tail position"
    marker, so the tail call fell back to an ordinary (stack-growing) call.
    Given that a wildcard-only recursive arm is the unusual case — almost
    every real recursive function destructures its next state by name, or
    binds an intermediate `let` — this bug was live for close to the entire
    space of realistic tail-recursive Stitch programs; the existing test
    suite's `count(n) = match n { 0 => 0  _ => count(n-1) }`-shaped tests
    happened to dodge it by using `_`.
  - **Fix**: `self_closure: self.self_closure.clone()` in `bind`, preserving
    (not resetting) the marker across a local binding — one line. The three
    *other* `self_closure: None` sites in `env.rs` (`globals_only`,
    `sibling_module`, `extend_cell`) are correct as-is: each represents a
    genuine call-boundary-like context (either immediately overwritten by
    `with_self_closure` during call setup, or a real scope change), verified
    by reading every call site before touching only the one that was wrong.
  - **Verification**: a new regression test,
    `interp.rs::self_tail_recursive_function_survives_a_let_binding_before_the_call`
    (same shape as the two existing trampoline tests, but with a `let`
    binding — and separately a named match arm — ahead of the tail call;
    both were the RED case before the fix). Full `cargo nextest run -p
    stitch --features testing` (811 tests), the `no_std` riscv64 lib target,
    and `cargo clippy -p stitch --lib` all clean after.
  - This is exactly the kind of bug that stays invisible in a codebase's own
    test suite forever: every existing tail-recursive stdlib/prelude
    function either doesn't recurse deep enough to need the trampoline, or
    happens to use `_`. It only surfaces once a program recurses **and**
    binds — i.e., once someone writes an actually-realistic recursive
    algorithm over actually-nontrivial input. json.st's `scanDigits`/
    `scanStr`/`scanArr` (recursive, binding-heavy) never hit it only because
    every test string was a handful of characters.

- **A second, unrelated bug — in my own `sumTo` program, not the
  interpreter — caught by simulating the instruction list in Python outside
  Stitch entirely, after the depth fix stopped masking it.** Once `step`
  could actually run to completion, `run(sumTo(5))` came back `Ok(0)`
  instead of `Ok(15)`. The loop's exit path assumed the spent (zero)
  counter would be on *top* of the stack for a plain `Pop`, but the loop
  invariant (`[acc, counter]`, established once and never actually
  re-derived at the exit point) puts `acc` on top there — so the original
  `Pop` was discarding the accumulator and keeping the dead counter. Fixed
  by inserting a `Swap` before the final `Pop`. Two lessons: (1) a stack
  machine's control-flow correctness is exactly the kind of thing that
  *looks* obviously right from the instruction list and is trivially wrong
  in a way native tests over a couple of small `n` values didn't catch
  either (`sumTo(0)` and `sumTo(1)` both happen to reach the buggy exit with
  an empty-enough stack that the bug's symptom — silently answering `0` —
  coincides with one of them, so weak coverage could have shipped this);
  (2) when native-test debugging stalls, dropping to an *external*,
  from-scratch reimplementation of the same semantics in a throwaway script
  is a fast, cheap way to get a second, independent opinion on "what should
  this actually compute" — cheaper here than continuing to hand-trace
  Stitch source by eye, which is exactly how the *first* version of this
  bug (an earlier, different ordering mistake in `Swap` itself) was caught,
  and exactly how this *second* one wasn't, until the tooling changed.

### 4. graph.st

- Lines: 246. Tests: 13 (13 pass). First program in the batch that passed
  its gate on the very first run — every earlier finding (association lists
  over `Map`, named `prod`s over tuples, parenthesize `(x |> f)?`, watch for
  a bare `(` starting a statement, `mut`'s real semantics) applied cleanly
  from the start instead of being discovered mid-file. That's the payoff
  this findings doc is for.
- What it exercises: `bfs`/`dfs`/`topoSort`/`connectedComponents` as
  worklist loops (queue vs. stack vs. Kahn's-algorithm frontier) — all
  self-tail-recursive with bindings ahead of the tail call, deliberately
  exercising the `Env::bind` fix from vm.st's entry on a program that
  actually needed it to be correct (a graph with a few hundred nodes would
  have blown `MAX_CALL_DEPTH` before that fix).
- No new language friction found. Worth recording as a data point on its
  own: once the association-list-over-`Map` and named-`prod`-over-tuple
  patterns are established habits (this file's `Edge`/`AdjEntry`/`Degree`),
  and once the `?`/`|>` precedence and maximal-munch gotchas are known to
  watch for, a `fold`/`filter`/`map`-heavy program over immutable data reads
  and writes essentially like idiomatic Rust or F# — the batch's friction so
  far has been concentrated in `mut`-heavy state (bank.st) and hand-rolled
  parsing/interpretation (json.st, vm.st), not in this "ordinary functional
  data processing" register.

### 5. markov.st

- Lines: 177. Tests: 10 (10 pass).
- What it exercises: `use <-` block sugar (`span("markov.generate")`, first
  use in the batch — `use <-` itself was already shipped/validated via
  `fs-image/primes.st`, just not exercised here yet), direct tuple
  pattern-matching (`match pair { (w1, w2) => … }`, distinct from the `at2`
  workaround — see the file's own header comment for why that's not a
  contradiction), a hand-rolled LCG as pure state-in-state-out (`Rng`/
  `Draw`), and a second, cleaner instance of the association-list-over-`Map`
  pattern (`List<Transition>`).

- **`take`/`takeWhile`/`drop`/`dropWhile` are `Seq`-only — they fault on a
  `List`, unlike `fold`/`filter`/`map`, which special-case both.**
  `buildChain` reached for `drop(words, 1)` (the obvious way to write
  "`words`, shifted by one, for zipping into bigrams") and got `"drop
  expects a Seq, got List"`. `natives.rs` confirms: `native_take`/
  `native_take_while`/`native_drop`/`native_drop_while` all hard-`match`
  on `Value::Seq` and error otherwise, while `native_map`/(presumably
  `filter`/`fold`, same shape) check `if let Value::Seq(_) = collection`
  first and fall through to a `List` path if not. There is also no `List ->
  Seq` conversion native (`toList` goes the other direction only), so
  there's no bridge — a `List` that needs `drop` semantics has to reach for
  a `List`-native equivalent instead. Fixed here with `List.removeAt(words,
  0)` (removing index 0 *is* "drop 1"). Filed alongside the `Map`
  limitation as a second "the eager/lazy split isn't uniform across the
  stdlib" finding — worth being alert for with `take`/`takeWhile` too, even
  though neither came up in this file.

- Otherwise clean — the LCG and generation loop worked exactly as
  hand-simulated in a scratch Python script before writing the `.st` (same
  methodology as vm.st's entry: cheaper to get a second opinion from an
  independent reimplementation than to keep re-deriving arithmetic by eye),
  and every expected sequence in the tests matched the interpreter's actual
  output on the first run once the `drop`/`use List` fixes landed.

### 6. sched.st

- Lines: 161. Tests: 8 (8 pass). Passed the gate on the first try.
- What it exercises: `contract`-only polymorphism as the actual mechanism
  (two zero-field marker `prod`s, `RoundRobin()`/`Priority()`, dispatched
  dynamically through an untyped `strategy` parameter — "the only
  polymorphism," demonstrated rather than just described), a
  self-tail-recursive simulation loop in the vm.st/graph.st worklist shape,
  and a saturated three-task scenario chosen specifically per
  `project_priority_scheduling_demo_lessons` (existing project memory):
  measure *completion order over a real run*, not just "who goes first,"
  and use enough contention that round-robin and priority actually produce
  different outputs — a two-task or unsaturated scenario would have let
  both strategies look identical.
- No `Map` needed this time (`removeByName`/`highestPriority` are linear
  scans over the pool `List<Task>` directly — there's no natural key to
  look up by, just "the whole pool," so the association-list pattern from
  json.st/graph.st/markov.st didn't even come up).
- No new language friction. Every trace (round-robin interleaving, priority
  drain order, per-task CPU-time totals) was hand-derived before writing the
  test assertions and matched the interpreter's actual output exactly on
  the first run — the second program in a row (after graph.st) to do so,
  now that "prefer a named `prod`", "watch `?`/`|>` precedence", and "don't
  start a statement with `(`" are habits rather than lessons still being
  learned.

- **Unrelated to Stitch, worth one honest note:** while finalizing this
  entry, `git status`/`git log` showed other work landing concurrently on
  `main` from outside this session (`fs-image/lib/text.st`, `snemu/*`,
  several `plans/corpus-prompts/v*.md` files, `docs/grammar-mask-design.md`)
  and `stitch/tests/canon.rs`'s native-suite-count assertion moving from
  `>= 6` to `>= 12` between two of this session's own gate runs — which is
  why the full `cargo nextest run -p stitch --features testing` total
  fluctuated (811 → 802) between the markov.st and sched.st entries above,
  with no failures either time. Not this batch's concern to fix or explain
  further, but worth naming so a fluctuating total isn't mistaken for a
  regression this work introduced.

### 7. calc.st

- Lines: 265. Tests: 13 (13 pass). Passed the gate on the first try.
- What it exercises: the classic two-stage pipeline (tokenize, then a
  precedence-climbing recursive-descent parse) as a deliberate contrast to
  json.st's single-pass character scanner; `Str.parseInt` used directly for
  the first time in the batch (json.st predates it and still hand-rolls
  digit parsing — see that entry); `Parsed(expr, pos)` as another named-
  product-over-tuple cursor-carrier, same shape as json.st's original
  `(value, pos)` pairs but done the way vm.st's entry recommended instead.
- Third program in a row (with graph.st, sched.st) to pass on the first
  try. The established-habits list now reliably includes: named `prod` over
  tuple, `(x |> f)?` parens, never start a statement with bare `(`,
  association list over `Map`, nest a conditional's else-branch in parens
  the moment it's itself conditional (used throughout `scanTokens`'
  digit/alpha dispatch here without incident).
- No new language friction. The one thing worth flagging as *absence* of
  friction: `Str.parseInt`/`toStr` (added for json.st) made number-token
  scanning a non-event here — exactly the "every later program should use
  these directly" prediction from the natives' own fix note paid off on the
  very next program that needed numeric parsing.

### 8. inventory.st

- Lines: 157. Tests: 10 (10 pass). Passed the gate on the first try (fourth
  in a row, with graph.st/sched.st/calc.st).
- What it exercises: a second, "business logic" `contract`-dispatch example
  distinct from sched.st's (three pricing strategies with actual field
  state — `BulkDiscount(threshold, percentOff)`, `Seasonal(percentAdjust)`
  — rather than sched.st's zero-field markers), `use <-`'s second outing
  (`report`'s `use <- span(...)`), and grouping-by-computed-key as an
  established, unremarkable pattern by now.
- **Minor correction to earlier entries' style, not a finding**: Stitch's
  unary `-` is an ordinary prefix operator
  (`plans/lang/01-grammar-and-precedence.md` lists it explicitly, and
  `parser.rs` parses `TokenKind::Minus` as `UnOp::Neg` in prefix position),
  so a negative literal like `Seasonal(-20)` just works. Earlier entries in
  this batch wrote negative *computed results* inside `expect` assertions
  as `0 - 7` rather than `-7` — that pattern started in json.st out of
  unverified caution and got carried forward by copying, not because `-7`
  doesn't parse. Not worth going back to fix (both are correct, `0 - 7` is
  just noisier), but worth naming so it doesn't read as "negative literals
  don't work" to someone skimming this doc.
- No new language friction.

### 9. life.st

- Lines: 171. Tests: 10 (10 pass). Passed the gate on the first try (fifth
  in a row).
- What it exercises: `Seq.iterate`/`take`/`toList` (first use of the `Seq`
  module's producer side in the batch — `bfs`/`dfs`/etc. so far all used
  hand-rolled worklists, not the stdlib's own lazy-sequence machinery), a
  cellular automaton as the immutable-data showcase (`step` never mutates,
  every generation is a fresh grid), and a real Conway pattern (a blinker)
  as the correctness oracle rather than an invented toy case — a period-2
  oscillator gives a much stronger test than an arbitrary hand-picked grid,
  since `step(step(g)) == g` is a real structural property, not just "I
  computed this by hand and it matches."

- **`use <-` breaks self-tail-call trampolining, confirmed empirically
  before committing to a design.** The obvious way to write "drive N
  generations, emit a span per tick" is a self-recursive loop with
  `use <- span("life.generation")` inside it. That doesn't work: `use <-`
  desugars to the rest of the block becoming a **callback argument** to
  `span`, so the recursive call ends up inside a lambda `span` invokes, not
  in the enclosing function's own tail position. `eval_tail_dispatch` only
  recognizes a *direct* self-call sitting in AST tail position (see vm.st's
  and this program's own `Env::bind` background) — a call reached through a
  native's callback doesn't qualify no matter how "tail-shaped" the source
  looks. Verified with a scratch program (`loop(n) uses Telemetry = n <= 0
  => 0 | { use <- span("tick")  loop(n - 1) }`, run for 1000 iterations)
  faulting with `"call stack too deep"` at `MAX_CALL_DEPTH` (48) before
  writing `life.st`'s real driver — same "get a second, independent
  opinion before trusting the derivation" instinct as vm.st's and
  markov.st's Python simulations, just aimed at a language-mechanics
  question instead of arithmetic.
  - **Not a bug** — this is the direct, correct consequence of how the
    trampoline is implemented (recognize a literal self-call in tail
    position; nothing more general), and `use <-`'s whole design *is*
    "the rest of the block becomes a callback." The two are simply in
    tension the moment the callback's body needs to keep recursing.
  - **The workaround generalizes past this file**: don't drive repeated
    computation with hand-written self-recursion when you also want a
    per-iteration effect. Compute the whole sequence first via a *native*
    loop (`Seq.iterate`/`take`/`toList`, or building a `List` some other
    way), which costs no Stitch-level call-stack depth no matter how long
    it is, then walk the already-built list with `each`/`fold` for the
    effects — those are native loops too, so a callback (`use <-` included)
    inside *that* lambda never touches the recursion-depth counter, since
    `each`/`fold` never recurse through the trampoline at all, they iterate
    in Rust. Any future program in this batch that wants "N repetitions,
    one effect per repetition" (a future `queue.st`, maybe others) should
    reach for this shape by default rather than rediscovering the fault.
- No other new friction — grid indexing (`cellAt`, off-edge = dead) and the
  `nextCell` rule read exactly like the textbook definition once written as
  `List.at` chains.

### 10. shell.st

- Lines: 153. Tests: 9 (9 pass). Last program of this ten-program round.
- What it exercises: `?.` safe navigation through a `Maybe<Profile>` field
  (`session.user?.name`, first genuine use in the batch — nothing earlier
  needed a `Maybe`-of-a-`prod` to chain through), a `sum Command` parsed
  from tokenized input with a graceful catch-all (`Unknown`) rather than a
  `Result`, and capability rows that genuinely differ per command
  (`runLs`'s `FsRead` vs. the rest of `runCommand`'s commands, which need
  none).

- **The static checker's capability-effect propagation is stricter than
  the runtime's — and inconsistent between plain function calls and method
  calls.** `runCommand` (a plain function) calling `runLs()` (also a plain
  function, declaring its own `uses FsRead`) failed the type-check gate:
  *`"performs an effect that needs `uses FsRead`, which is not declared"`*
  — on `runCommand`, even though nothing in `runCommand`'s own body
  performs an FsRead effect directly. `check.rs::walk_effects` (case C2,
  its own doc comment: *"a called user function propagates its declared
  `uses`"*) explicitly walks the call graph and requires every caller of a
  capability-needing function to redeclare that capability, transitively,
  for **plain function calls** (`CoreExprKind::Call` with a bare-name
  callee) — a strictly more conservative rule than the *runtime*, which
  resets authority to exactly a function's own declared `uses` at every
  call boundary regardless of caller (bank.st's finding, and
  `natives.rs`'s `shout()/main()` test, from the refusal side). But
  `walk_effects` only pattern-matches `CoreExprKind::Call`, not
  `CoreExprKind::MethodCall` — so bank.st's identically-shaped
  `transferAudited` (`uses FsWrite` only, calling `@transfer` which
  independently needs `Telemetry`) sailed through the checker clean,
  because `@transfer(...)` is a *method* call and the effect walker never
  looks inside one. Two real asymmetries stacked on each other: static
  vs. runtime (checker is stricter), and function vs. method call syntax
  (checker only propagates through one of them) — a program can dodge the
  static requirement entirely just by writing `@method()` instead of
  `plainFn()`, whether or not that's the receiver shape that actually makes
  sense for the call. Fixed here by being honest rather than routing around
  it: `runCommand` and `runLine` both widened to `uses Telemetry, FsRead`,
  even though `FsRead` is only exercised on the `Ls` branch. Given the
  checker is explicitly gradual and gaining rules over time
  (`docs/language-design.md`'s status note), extending C2 to method calls
  — or documenting the asymmetry as intentional, if dynamic dispatch
  genuinely makes it infeasible to do soundly — looks like a natural next
  increment; recorded here rather than fixed, since (unlike the `Str.parseInt`
  gap) this is a design tradeoff about a type checker's soundness/
  completeness boundary, not a missing three-line native.
- No other new friction.

### 11–15: regex.st, csv.st, trie.st, lru.st, elo.st (batch 2 opens)

All five passed the gate on the first try (regex.st, csv.st, trie.st,
elo.st) or after a self-caught `use List` omission fixed before running
(lru.st) — the run of "no new friction" programs that started with
graph.st continued through the whole back half of this session. Grouped
into one entry since none of them needed a dedicated deep-dive the way
json.st/bank.st/vm.st/markov.st/life.st/shell.st did; what's below is what
was worth recording from each.

- **11. regex.st** — 136 lines, 11 tests. A Kernighan-style recursive
  backtracking matcher (literals, `.`, `*`, `^`/`$`, `|`). Confirms
  top-level `|` alternation can be handled with a plain `Str.split(pattern,
  "|")` — safe specifically *because* this toy grammar has no grouping
  construct, so a literal `|` can never appear nested inside anything else.
  No `Map` needed (no dictionary anywhere in a regex matcher). No new
  friction — every nested conditional got parenthesized correctly from the
  first draft.

- **12. csv.st** — 124 lines, 9 tests. A `Result`-threaded quoted-field
  parser/writer, deliberately un-recursive-descent (no grammar, just
  comma/quote state machine) as a register shift from json.st/calc.st's
  parsers. The round-trip test (`parseCsv(writeCsv(rows)) == Ok(rows)`) is
  doing double duty as both a correctness check and — like life.st's
  blinker — a much stronger property than any single hand-picked example.
  No new friction.

- **13. trie.st** — 148 lines, 9 tests. The sharpest illustration yet of
  "no `Box` needed for a recursive type" (`Trie` contains `List<TrieEdge>`,
  `TrieEdge` contains `Trie`, mutually recursive, just works) and, in the
  same file, the sharpest illustration of the `Map` gap costing something
  real: a trie's whole point is fast keyed fan-out, and here every branch
  is a linear scan over `List<TrieEdge>` instead. Filed as a repeat data
  point for the json.st finding, not a new one — but worth flagging as the
  program in this batch where the missing native actually costs the
  *design*, not just verbosity.

- **14. lru.st** — 134 lines, 8 tests. An immutable-value cache
  (`get`/`put` return a new `Cache`) with memoized Fibonacci threaded
  explicitly through it — no global mutable cache, so `fibMemo` returns
  `FibResult(value, cache)` and the caller is responsible for carrying the
  updated cache into the next call, same shape as bank.st's `mut` write-
  back discipline but arrived at without needing `mut` at all. `fibMemo`
  is genuine (non-tail) recursion — two recursive calls combined
  afterward — so test `n` stayed under `MAX_CALL_DEPTH` (used up to 10;
  the depth cap from vm.st's/shell.st's/life.st's findings is a hazard for
  *any* non-tail-recursive function, not just deliberately deep loops, and
  a naive Fibonacci is the textbook example of one).

- **15. elo.st** — 152 lines, 12 tests, third program using `contract`-
  dispatched strategies (after sched.st, inventory.st) but the one that hit
  a stdlib ceiling head-on: **the real Elo formula needs `10^(diff/400)`,
  and there is no exponentiation, `sqrt`, or any `Float` math native in
  this interpreter at all** — checked `natives.rs` for `pow`/`exp`/`sqrt`
  under any name, all absent. Unlike the `Str<->Int` gap (fixed for
  json.st, two natives, an afternoon), this isn't a small missing native —
  real transcendental math would need either a `libm`-style dependency
  (the project's own `kvetch-model` crate takes exactly that dependency,
  for the same reason) or a from-scratch series approximation, both out of
  scope for an example-writing session. Worked around, not routed around:
  `expectedScorePerMille` is a deliberately-labeled **integer, piecewise-
  linear approximation** of the logistic curve (some real game engines do
  exactly this), and `marginMultiplierPerMille`'s staircase stands in for
  the textbook `sqrt(margin+1)` diminishing-returns curve the same way.
  Both are honest about being approximations in their own doc comments,
  not silently-wrong stand-ins. **This is the batch's second hard stdlib
  ceiling** (after `Map` construction) — worth remembering for any later
  program that reaches for real math (none currently planned need it, but
  worth flagging before one is written assuming it's available).

- **Not tested in-language, and worth recording why**: wanted a `bank.st`
  test proving a function *without* `uses FsWrite` is refused when it
  tries `fsWrite` (the negative case for the capability-boundary finding
  above). Couldn't write it as a native `test`/`expect` block — `expect`
  only asserts a `Bool`; there is no way to assert "this expression should
  fault" from inside Stitch itself, so a test that deliberately triggers a
  refusal just reports `Verdict::Failed`, which the batch's own gate
  (`stitch/tests/examples.rs`) treats as a real failure, not a pass.
  `native_test_runner.rs`'s existing "sneaks" test covers this at the Rust
  level instead. This looks like a real gap in `docs/stitch-testing-design.md`'s
  scope — worth a `test "…" fails { … }` or similar form if the testing
  design gets revisited — but is out of scope to fix here.
