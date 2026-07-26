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
  test implies from the other direction.

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
