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

### 1. json.st

- Lines: 399. Tests: 13 (13 pass).
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
