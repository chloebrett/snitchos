# Stitch — language improvements proposed from the 30-program corpus

Derived from [stitch-examples-findings.md](stitch-examples-findings.md) (the lab
notebook for [stitch-examples-corpus.md](stitch-examples-corpus.md)'s 30
programs, ~6,300 lines, 280+ tests). Every claim below was re-verified against
the shipped source before being written down — file and line references are to
`stitch/src/` as of 2026-08-06.

**This is a proposal document, not a plan.** Nothing here is scheduled or
sequenced into increments yet; the last section suggests an order. No code
changes accompany it.

---

## Why a corpus is the right instrument

The findings doc's own arc is the argument. Programs 1–3 (`json.st`,
`bank.st`, `vm.st`) each found something load-bearing — including a **real
interpreter bug** (`Env::bind` resetting the tail-call marker, which disabled
the trampoline for essentially every realistic recursive function) that the
interpreter's own 800-test suite had never caught, because every existing
tail-recursive test happened to use `_` in its recursive arm. Programs 4–10
found progressively less. Programs 23–30 found almost nothing new.

That decay curve is the signal: **the corpus is now close to exhausted as a
discovery instrument**, and what it surfaced has stopped being random. The
findings cluster into five recurring shapes, and those shapes — not the
individual annoyances — are what this document proposes fixing.

| # | Proposal | Evidence in the corpus | Cost | Priority |
|---|---|---|---|---|
| 1 | Buildable `Map` (and decide `Set`) | 12 of 30 files hand-roll one | Small–medium | **Highest** |
| 2 | Statement/arm boundaries (newline-aware postfix) | 4 files + the prelude itself | Medium | **Highest** |
| 3 | Effect propagation: close the method/higher-order holes | 3 files; the headline feature is dodgeable | Medium | High |
| 4 | `test … fails` — assert a fault | Capability refusals are untestable in-language | Small | High |
| 5 | Uniform `List`/`Seq` split | 3 files | Small | Medium |
| 6 | `Str.chars` | Silent data corruption in 1 file, near-miss in 2 | Tiny | Medium |
| 7 | `Math` module + `Int`↔`Float` | `elo.st` shipped an integer approximation | Tiny | Medium |
| 8 | Ordered tuples → multi-key `sortBy` | `semver.st` hand-wrote insertion sort | Small | Medium |
| 9 | `Either` exists in the doc, not the runtime | 1 file | Tiny | Low (coherence) |
| 10 | Recursion depth + the `use <-` / trampoline tension | 4 files bounded their own test inputs | Small now, VM later | Medium |
| 11 | Lexer: unterminated `"` and `{` emit **no error at all** | 2 files, both diagnosed many lines away | Tiny | Medium |
| 12 | Raw strings (kill the `{{{{` trap) | `template.st` silently mis-behaved | Small | Medium |

---

## 1. `Map` you can build — the single biggest finding

### Evidence

`Value::Map` is `Rc<Vec<(Value, Value)>>` (`value.rs:37`) — already an
association vector. It has exactly two operations: a literal (`interp.rs:603`)
whose entry *count* is fixed in the source text, and indexed read
(`eval_index`, `interp.rs:1207`, a linear `find`). There is no `insert`,
`remove`, `update`, `fromList`, `keys`, or `values` anywhere in `NATIVES`
(`natives.rs:17-65`).

So a `Map` cannot be *built* from runtime data. Twelve of thirty programs
worked around it by declaring an ad-hoc key/value `prod` and hand-writing the
lookup:

```
calc.st       prod Binding(name, value)          json.st    List<(Str, Json)>
formula.st    prod CacheEntry(name, value)       lru.st     prod Entry(key, value)
graph.st      prod AdjEntry(node, neighbors)     markov.st  prod Transition(word, nexts)
logstats.st   prod LevelCount / prod WordCount   huffman.st prod Freq(char, count)
template.st   prod Binding(key, value)           poker.st   prod RankCount(rank, count)
inventory.st  prod CategoryTotal(category, …)    trie.st    prod TrieEdge(char, node)
```

`inventory.st:69-72` and `logstats.st:38-41` are the *same eight tokens of
logic*, written twice in two unrelated files:

```
fold(entries, [], (acc, e) -> match find(acc, lc -> lc.level == e.level) {
    None    => concat(acc, [LevelCount(e.level, 1)])
    Some(_) => map(acc, lc -> lc.level == e.level => LevelCount(e.level, lc.count + 1) | lc)
})
```

That is O(n²) and five lines for what is one line in every language with a
dictionary. `trie.st` is the sharpest case: a trie's entire point is keyed
fan-out, and every branch there is a linear scan — the gap cost the *design*,
not just verbosity.

### Proposal

Add a `Map` module mirroring the existing `List` module's shape
(`natives.rs:62-65`):

```
Map.get(m, k)     -> Maybe<V>        // same as m[k]; named for pipelines
Map.insert(m, k, v) -> Map           // replaces an existing key
Map.remove(m, k)  -> Map
Map.update(m, k, default, f) -> Map  // the tally shape, see below
Map.has(m, k)     -> Bool
Map.keys(m)       -> List<K>
Map.values(m)     -> List<V>
Map.entries(m)    -> List<(K, V)>
Map.fromList(pairs: List<(K, V)>) -> Map
Map.size(m)       -> Int
```

`update` earns its place on evidence: it is the operation six of the twelve
files were actually reaching for. Both fold-tallies above collapse to

```
fold(entries, [:], (acc, e) -> Map.update(acc, e.level, 0, $ + 1))
```

**Keep the association-vector representation for now.** Lookup stays O(n);
that is not what these programs were blocked on. The blocker was *expressibility*,
and swapping the representation for a hash or B-tree is an interior change the
bytecode-VM stage is the natural home for. Shipping the API first means the
corpus can be rewritten against it and the representation change is later
invisible.

### The `Set` decision — make one

`Set<T>` is documented in `plans/lang/01-grammar-and-precedence.md` ("eager; no
literal, `[1,2,3] |> toSet`") and **does not exist**: no `Value::Set`, no
`toSet`, nothing in `registry.rs` or `natives.rs`. `spellcheck.st` used
`List` + `contains` and lost nothing, because a spellchecker only needs
membership.

Two honest options, and either beats the status quo:

- **Build it** as `Map<T, ()>` internally, with `union`/`intersection`/
  `difference` — the operations a `List` genuinely can't fake cheaply.
- **Cut it from the design doc.** A `List` plus `contains` covers membership;
  set algebra has not come up once in 30 programs.

**Recommendation: cut it**, and revisit if a program ever needs set algebra.
The corpus is evidence that nothing does. What is not acceptable is leaving a
design doc describing a type that was never built — that is the same class of
error as the `mut`-semantics doc bug `bank.st` found.

### The `Key` contract is unenforced

`docs/language-design.md:209` says using a value with any `mut` field as a
`Map` key is "a **compile error**", designed out via a `Key`/`Hashable`
contract. There is no such contract, and no such check. Either implement it
alongside the `Map` API or downgrade the claim in the doc. (With an association
vector and structural equality it is currently a *silent* footgun: a mutated
key does not corrupt a hash bucket, it just fails to compare equal.)

---

## 2. Statement and arm boundaries — the maximal-munch family

### Evidence

This is one root cause with four discovered symptoms, and it is the only
finding in the corpus that **recurred after being written down**.

The lexer emits no newline token and tracks no lines (`lexer.rs` — `Token` is
`{kind, span}`, `lexer.rs:62`). `parse_block` (`parser.rs:729-758`) separates
statements with *nothing*: it parses expressions until `}`. `parse_postfix`
(`parser.rs:542-582`) then extends a postfix chain across whatever follows,
newline or not. So any construct whose next element can begin with `(` fuses
with the previous one:

| Where | Program | Symptom |
|---|---|---|
| Adjacent statements | `bank.st` `transfer` | `"cannot call a a record"` |
| After a `\|>` pipe's RHS call | `tictactoe.st` | `"function expects 2 argument(s), got 1"` |
| Between `match` arms | `semver.st` | `"expected '\|' in conditional"` at arm 3 |
| Designed around pre-emptively | `poker.st` | (restructured to avoid it) |

Every one of those errors points somewhere other than the mistake. The
`semver.st` case is the worst: arm 2's body `1` became the call
`1(Left(_), Right(_))`, which then ate arm 3's `=>` as a conditional and failed
on arm 3's fat arrow — a message naming a token two arms away from an error
that is really "the arms need a separator."

The workarounds are visible in the source as scar tissue. `bank.st` carries
three `let _ = (…)` prefixes that exist purely to start a statement with a
keyword. And the **prelude itself** documents the corner (`prelude.st:48-49`):

```
// Note: written as a bare fold body, not `{ f(x)  () }` — `f(x) ()` would
// parse as a call of f(x)'s result (the maximal-munch corner).
each(xs, f) = fold(xs, (), (_, x) -> f(x))
```

When the standard library has to contort around a parser rule, that rule is
wrong.

### Proposal

**A `(` or `[` that begins a new source line does not continue a postfix
chain.**

This is a two-line change in spirit: tokens already carry byte spans, so
"is there a `\n` between the previous token's `end` and this token's `start`?"
is answerable without touching the lexer's output shape. Cleanest is to add
`preceded_by_newline: bool` to `Token` at lex time and gate the `LParen` and
`LBracket` arms of `parse_postfix` on it.

All four symptoms die at once. `let _ = (…)` disappears from `bank.st`, the
prelude's comment becomes obsolete, and tuple-pattern `match` becomes writable
the way `semver.st` first tried to write it.

**Rejected alternatives:**

- *Semicolons.* Directly contradicts the design doc's "no semicolons,
  expression-oriented throughout" (`language-design.md:99`), and would be the
  single most visible regression in the language's feel.
- *Significant indentation.* A much larger commitment than the problem needs,
  and off-brand for a Rust/Kotlin-shaped surface.
- *Require a separator only in `match` arms.* Fixes one of four symptoms.
- *Leave it, document the habit.* Already tried. It recurred in `semver.st`
  after being documented twice, and `poker.st` had to be *designed* around it.
  A gotcha that changes how you structure code is a language defect, not a
  style note.

**Precedent:** Go's semicolon insertion, Swift, and Kotlin all make a
line break meaningful at exactly this boundary. This is a well-trodden rule,
not an experiment.

**Risk:** a program that today wraps a genuine call's argument list onto the
next line would change meaning. Grep of the corpus and prelude found no such
site — the language's dominant multi-line idiom is a leading `|>`, not a
dangling `(`. The `--check`-able examples gate makes this cheap to verify.

### Related: the chained-conditional error

`parser.rs:418-422` rejects `a => b | c => d | e` with *"chained conditionals
aren't allowed — use `match` for more than two cases"* — but only fires at the
**second** `=>`, so a drafted four-way conditional reports at its last pair.
Keep the ban (it is a real coherence rule: `=>` means "case", `match` means
"many cases"), but the message should point at the *first* `=>` and name the
concrete fix: `wrap the else-branch in parens, or use match`.

---

## 3. Capability-effect propagation has holes the corpus found by accident

### Evidence

`walk_effects` (`check.rs:728-765`) propagates a callee's declared `uses`
**only** for `CoreExprKind::Call` whose callee is a bare `Var`. Consequences,
all three found by writing ordinary programs:

- **Method calls do not propagate.** `bank.st`'s `transferAudited` declares
  only `uses FsWrite`, calls `@transfer` which needs `Telemetry`, and passes
  the checker clean — because `@transfer(…)` is a `MethodCall`.
- **Plain function calls do propagate, transitively.** `shell.st`'s
  `runCommand` had to widen to `uses Telemetry, FsRead` for a capability
  exercised on one branch.
- **Higher-order calls do not propagate.** `handlers.st`'s `captureEmits(work)`
  calls `work()` through a parameter; invisible to `funcs.get(name)`.
- **But a lambda's *body* does propagate, even unevaluated.** Every test
  writing `() -> sumSquares(…)` inline needed the capability, because the walk
  is plain syntactic recursion into `child_exprs`.

Stack those and you get the finding that matters: **the language's headline
feature is dodgeable by writing `@m()` instead of `m()`** — a purely syntactic
choice that has nothing to do with authority.

`check.rs` knows. Its own C3 comment (`check.rs:909-911`) says *"Conservative:
`required` under-approximates, so effects reached only via methods/higher-order
calls could over-warn."* The hole therefore has two symptoms: missed
requirements (unsound C1/C2) **and** false least-authority warnings (C3).

### Proposal

Three parts, in decreasing confidence:

**(a) Propagate through method calls.** The `on`-block registry already knows
each method's declared `uses`. Dynamic dispatch means the callee may not be
statically known — take the **union of the `uses` of every method with that
name on every conforming type**, which is sound (over-approximating) and
matches how the language already treats capabilities as a coarse row, not a
precise set. Closes the dodge.

**(b) Effect-polymorphic higher-order parameters.** The principled fix is for
a function parameter's type to carry a `uses` row (`f: () -> T uses E`, with
`E` a row variable) so `captureEmits(work)` requires whatever `work` requires.
This is a real type-system increment — it is what Koka/Unison do, and the
design doc already names them as the lineage (`language-design.md:305`). If
that is too large for now, the *documented* interim rule should be stated
explicitly: **authority is not propagated through a value-carried function;
the callee runs with its own declared row** (which is what the runtime already
does), and the checker's under-approximation there is known and accepted.

**(c) Document the lexical-lambda rule as intentional.** "Any lexically
present named-function call propagates, whether or not it is ever invoked" is
a defensible conservative rule. It is currently undocumented, so it reads as a
bug.

### Also: one capability table, not two

`check.rs:711-720`'s `native_cap` maps native → capability, with the comment
*"Mirrors the runtime authority gate in `natives.rs` — keep them in sync."* A
comment is not a mechanism. Add the capability to the `NativeFn` struct
(`natives.rs:17`) and have the checker read it, so the two cannot drift. Any
new effect native currently arrives half-checked by default.

---

## 4. `test … fails` — the refusals are untestable in their own language

### Evidence

The findings doc ends on this, and it is the most pointed thing in it:

> wanted a `bank.st` test proving a function *without* `uses FsWrite` is
> refused when it tries `fsWrite` … `expect` only asserts a `Bool`; there is
> no way to assert "this expression should fault" from inside Stitch itself.

So **the negative case for the language's headline feature cannot be written in
the language**. It is covered at the Rust level (`native_test_runner.rs`'s
"sneaks" test), which means the one thing Stitch exists to demonstrate is
demonstrated in Rust. `handlers.st` hit the same wall independently.

### Proposal

Add a fault-asserting form. Two candidate shapes:

```
test "a function without FsWrite cannot write" fails {   // whole test must fault
    sneaky()
}

expect fault { sneaky() }                                // per-assertion (preferred)
expect fault "requires `uses FsWrite`" { sneaky() }      // ...matching a message
```

**Prefer the `expect fault` form.** It composes (a test can assert both a
success and a refusal), and it can carry the expected message — which is what
you actually want for a capability refusal, since "it faulted" is a much weaker
claim than "it faulted *because* the capability was withheld."

`Item::Test` already carries a `uses` row (`parser.rs:983-988`), and
`Verdict` (`test_runner.rs:40`) already distinguishes `Failed` from
`Exhausted`, so the runner has the structure. This is a small, high-leverage
change: it turns `examples/stitch/` from a corpus that demonstrates
capabilities into one that *proves* them.

---

## 5. The `List`/`Seq` split is not uniform

### Evidence

Three separate incidents, one cause — every collection native independently
decides what it accepts:

- `take` / `takeWhile` / `drop` / `dropWhile` hard-match `Value::Seq` and
  fault on a `List` (`natives.rs:166, 769, 806, 1041`), while `map`/`filter`/
  `fold` accept both. `markov.st` wanted `drop(words, 1)` and got *"drop
  expects a Seq, got List"*.
- There is **no `List -> Seq` conversion** — `toList` goes one way only. So
  there is no bridge; `markov.st` had to reach for `List.removeAt(words, 0)`.
- `map` over a range stays a `Seq` (ranges are lazy), and `List.at` then
  faults with *"List.at expects a List, got Seq"*. `diff.st` hit this building
  a DP table row; `huffman.st` and `spellcheck.st` hit the same thing while it
  was still fresh. Every DP-table program needs a `|> toList` nobody predicts.
- `logstats.st` hand-wrote `firstN` rather than use `take`.

### Proposal

State and enforce one rule: **eager in → eager out, lazy in → lazy out.** Make
`take`/`takeWhile`/`drop`/`dropWhile` accept a `List` and return a `List`,
exactly as `map`/`filter`/`fold` already do. Add `toSeq` for the other
direction.

That leaves the `diff.st` trap (a range is lazy, so `map` over it is lazy).
Two options: have `List.at` accept a `Seq` by forcing a prefix, or leave the
error and improve it to *"List.at expects a List, got Seq — add `|> toList`"*.
**Prefer improving the error**: silently forcing a lazy sequence inside an
indexing operation is how you get an accidental infinite loop, and the whole
point of the `Seq` split is that forcing is explicit.

---

## 6. `Str.chars` — a Rust semantics leak that silently corrupted a program

### Evidence

`Str.split` is a pass-through to Rust's `str::split` (`natives.rs:588`), and
Rust's empty-pattern behaviour yields boundary empties: `"abc".split("")` is
`["", "a", "b", "c", ""]`, and `"".split("")` is two empty pieces.

`huffman.st` used `Str.split(s, "")` as "the characters of s" and got a bogus
`Freq("", 2)`, a 2-leaf tree for a single-symbol input, and `Some(Leaf("", 2))`
where `None` was correct. `json.st:290` does the same thing and is correct only
by luck — it maps-and-rejoins, so the extra empties are invisible. Two files
(`huffman.st:25`, `spellcheck.st:23`) now carry a byte-identical hand-written
helper:

```
chars(s: Str) -> List<Str> = Str.split(s, "") |> filter(c -> c != "")
```

### Proposal

Add `Str.chars(s) -> List<Str>`. Additionally, consider making
`Str.split(s, "")` an **error** rather than exposing Rust's boundary-empty
behaviour — no Stitch program has ever wanted it, and its only observed effect
was silent data corruption. That is a judgement call worth making explicitly
rather than inheriting from the host language.

(This is the same class as the `mut`-semantics doc bug: an implementation
detail of the host leaking through as language semantics nobody chose.)

---

## 7. `Math` — there is no floating-point math at all

### Evidence

`Float` arithmetic exists (`ops.rs:52-56`, IEEE semantics including `/0.0`),
but there is no `sqrt`, `pow`, `exp`, `ln`, `floor`, `round`, or `abs`
anywhere, and no `Int`↔`Float` conversion in either direction. `elo.st` needs
`10^(diff/400)` and shipped `expectedScorePerMille` — a deliberately labelled
integer piecewise-linear approximation of the logistic curve — plus a staircase
standing in for `sqrt(margin+1)`.

### Proposal

Add a `Math` module: `sqrt`, `pow`, `exp`, `ln`, `abs`, `floor`, `ceil`,
`round`, `min`/`max` on two numbers; plus `toFloat(i)` and `Float.toInt(f)`
(truncating, named to make the lossiness visible). `libm` is already a
workspace dependency (`kvetch-model` takes it for exactly this reason and for
exactly this target), so the `no_std` riscv64 build stays clean.

This is cheap and unblocks a whole register of programs — statistics,
geometry, anything physical — that the corpus could not write.

---

## 8. `sortBy` cannot express a multi-field sort

### Evidence

`native_sort_by` (`natives.rs:861`) takes a **key function**, computes one key
per element, and orders by `value_order` — which handles only two Ints, two
Floats, or two Strs (`ops.rs:77-88`). `semver.st` needs major-then-minor-then-
patch-then-prerelease and hand-wrote an insertion sort against
`compareVersions`.

### Proposal

**Give tuples a lexicographic ordering** in `value_order`, so
`sortBy($ -> (v.major, v.minor, v.patch))` works. This is a smaller surface
than adding `sortWith(xs, cmp)` and an `Ordering` sum, composes with the
`sortBy` that already exists, and is what every language with tuple ordering
does. `maze.st` already established that `(row, col)` tuples are idiomatic
Stitch, so ordered tuples fit the grain.

`sortWith` + `Ordering` remains the fallback for genuinely non-key-expressible
comparisons; the corpus produced no example needing one.

---

## 9. `Either` is documented but does not exist

`language-design.md:143` and `:205` present `Either<A, B>` alongside
`Maybe`/`Result` as one of the "just sums." `Maybe`/`Result` are hardcoded in
`registry.rs::register_builtin_types`; `Either` is in neither `registry.rs`,
`interp.rs`, nor `check.rs`. `semver.st` declared it locally in three words
and it worked identically.

Fix either direction — add the three-word declaration to the prelude, or say
in the doc that `Either` is an *example* of the algebra rather than an ambient
type. Given that `?` deliberately does not apply to `Either`
(`language-design.md:150`) and the prelude has no `Either` contract instances,
adding it to `prelude.st` is trivially cheap and makes the doc true.

**Pattern worth noting:** this is the third doc-vs-implementation divergence
the corpus found, after the `mut`-semantics bug and `Set`. All three were found
by someone trusting the design doc. A `docs/language-design.md` claim about
what exists is a contract nothing compiles — the same class of problem
`cargo xtask links` was built to solve for markdown links. A cheap mitigation:
a test that asserts every type the design doc names as ambient resolves in a
fresh program.

---

## 10. Recursion depth and the `use <-` / trampoline tension

### Evidence

`MAX_CALL_DEPTH = 48` (`env.rs:190`). Non-tail recursion therefore bottoms out
at 48 frames, and four programs had to bound their own inputs because of it:

- `lru.st`'s memoized Fibonacci is genuine (two-branch) recursion — tests
  capped at n=10.
- `tictactoe.st` never runs minimax from an empty board.
- `json.st`'s recursive scanners are only ever tested on strings of a few
  characters.
- `life.st` verified with a scratch program that `use <- span(…)` inside a
  self-recursive loop faults at 48 — because `use <-` desugars the rest of the
  block into a **callback**, so the recursive call is no longer in the
  enclosing function's tail position. `eval_tail_dispatch` recognises only a
  *literal* self-call in AST tail position.

Note also that the trampoline handles **self**-recursion only, so `json.st`'s
mutually recursive scanner family (`scanArr` ↔ `scanObj` ↔ `scanValue`) never
trampolines at all, at any depth.

### Proposal

Three parts, honestly scoped:

**(a) Raise the cap and make it configurable.** 48 is very low — deep enough
to catch runaway recursion, too shallow for a naive Fibonacci at n=20. Fuel is
already configurable per run (`test_runner.rs:36`); depth should be too. This
is the change with real payoff and near-zero risk.

**(b) Improve the diagnostic.** `"call stack too deep"` is what `vm.st` chased
for a whole debugging session. It should say which function, and — where it can
tell — *why the call was not trampolined*: "not in tail position", "mutual
recursion is not trampolined", "reached through a callback (`use <-`)". The
information is available at the fault site and would have collapsed three of
the incidents above into one glance.

**(c) Document the sanctioned "N repetitions with an effect" idiom.** Build
the sequence with a native loop (`Seq.iterate |> take |> toList`), then walk it
with `each`/`fold` for the effects — native loops cost no Stitch call depth.
`life.st` and `queue.st` both landed on this. It belongs in
`language-design.md`, not only in a findings doc.

Proper mutual-tail-call and callback-transparent recursion are **bytecode-VM
work** — the VM stage owns the call frames and can do this correctly. Do not
attempt it under the tree-walker; (a) and (b) are the right scope now.

---

## 11. The lexer reports nothing for an unterminated `"` or `{`

### Evidence — verified, and worse than the findings doc assumed

`lex_string` (`lexer.rs:314-316`) treats end-of-input identically to a closing
quote: `None | Some('"') => break`. `read_interpolation` (`lexer.rs:361-363`)
does the same: `None => break`. **Neither emits a `LexError`.** An unterminated
string or interpolation silently swallows the rest of the file and the parser
then fails wherever the wreckage happens to become ungrammatical.

That is exactly what `json.st` paid for: an unescaped `{` consumed subsequent
`//` comments (which stop being comments once the lexer thinks it is inside a
string) until a stray `"` inside an unrelated comment's prose closed the
runaway literal. Both failures reported many lines past the real mistake — one
at a backtick in a doc comment, one at an unrelated `match` arm. Diagnosis
required dumping `stitch::lexer::lex` by hand.

### Proposal

Emit a `LexError` anchored at the **opening** delimiter:

- *"unterminated string literal opened here"*
- *"unterminated `{` interpolation opened here — did you mean `{{` for a
  literal brace?"*

`LexError` already carries a message plus span (`lexer.rs:77-79`) and errors
already flow through `LexOutput`, so this is a small change with an outsized
diagnostic payoff. It is the cheapest item in this document per unit of pain
removed.

---

## 12. Raw strings — retire the `{{{{` trap

### Evidence

`template.st`, a program whose entire subject is `{{ }}` template syntax, was
silently wrong throughout its first draft. In Stitch source, `"{{"` denotes the
single character `{`, so a literal `{{` delimiter must be written `"{{{{"`.
The file parsed cleanly and behaved as if every tag were single-braced. The
symptoms — `"Hello, {{name}}!"` rendering as `"Hello, Ada"` with the `!` eaten,
and an unrelated `"no match arm matched"` — point nowhere near the cause.

The findings doc had **already predicted this exact failure** in `json.st`'s
entry, and knowing about it in advance was not enough to avoid it.

### Proposal

Add a raw string form that disables interpolation and brace escaping —
`r"…"` (Rust's, matching the language's stated Rust lineage) or `"""…"""`.
One token of syntax removes the entire class: a template program writes its
delimiters literally, a regex program writes backslashes literally, and a
JSON-emitting program writes braces literally.

Combined with proposal 11's error, the brace footgun stops being a silent
wrong-answer bug and becomes either impossible or loudly diagnosed.

---

## What the corpus validated (worth stating)

The findings doc is a list of problems by construction, which distorts the
picture. Five programs in a row (`graph.st` → `calc.st`) passed their gate on
the first try, and the closing entry for `logstats.st` records it as "the
cleanest to write" of all thirty. What did *not* need fixing:

- **Recursive sums without `Box`** — `trie.st`'s mutually recursive
  `Trie`/`TrieEdge` "just works," as the design doc promised.
- **`contract` as the only polymorphism** — three programs (`sched.st`,
  `inventory.st`, `elo.st`) dispatch strategies through it, including
  zero-field marker types and stateful ones. No one wanted inheritance.
- **Value-semantic `mut` with call-site write-back** — once documented
  correctly, it produced a *better* discipline than reference semantics would
  have (`bank.st`'s explicit `replaceAccount`), and `lru.st` reached the same
  shape without needing `mut` at all.
- **`use <-`, `?.`, pipes, placeholder sugar, structural equality, the `?`
  family** — all used across the corpus without complaint.
- **`prod`-over-tuple as a habit** — with `maze.st`'s `(row, col)` establishing
  the honest boundary case.

The proposals above are therefore about the *stdlib and the parser*, not about
the language's identity. Nothing in 30 programs argued against a core design
decision.

---

## Suggested sequencing

Grouped so each stage is independently shippable and leaves the examples gate
green. Each item is TDD as usual — a failing test in `stitch/tests/` first.

**Stage 1 — cheap, high-ratio (a session).** 11 (lexer errors), 9 (`Either`),
6 (`Str.chars`), 7 (`Math`), 10a/10b (depth cap + diagnostic). All small,
independent, and each removes a documented incident.

**Stage 2 — the two big ones.** 1 (`Map`) and 2 (newline-aware postfix). Both
want their own increment. `Map` first: it is additive and cannot break existing
programs, while the parser change wants the examples corpus as its regression
net — and after `Map` lands, rewriting the twelve association-list files
against the new API is itself an excellent test of both changes.

**Stage 3 — the capability story.** 4 (`expect fault`) **before** 3
(propagation), because 4 is what lets 3 be test-driven: you cannot TDD a
refusal rule without being able to assert a refusal. Then 3a (method calls) and
the single-native-capability-table cleanup. 3b (effect-polymorphic parameters)
is a genuine type-system increment — plan it separately.

**Stage 4 — polish.** 5 (`List`/`Seq` uniformity), 8 (ordered tuples), 12 (raw
strings), and the `Set` decision from 1.

**Deferred to the bytecode VM:** mutual-tail-call elimination, callback-
transparent recursion, and swapping `Map`'s association-vector representation
for a real hash or tree.

---

## References

- [stitch-examples-findings.md](stitch-examples-findings.md) — the lab notebook
  every claim here derives from.
- [stitch-examples-corpus.md](stitch-examples-corpus.md) — the 30-program plan.
- [../docs/language-design.md](../docs/language-design.md) — design rationale;
  proposals 1 (`Set`, `Key`), 3 (effect rules), 9 (`Either`), and 10c each
  imply an edit here.
- [lang/01-grammar-and-precedence.md](lang/01-grammar-and-precedence.md) — where
  `Set` is specified but unbuilt.
- [lang/04-standard-library.md](lang/04-standard-library.md),
  [lang/05-lazy-seq.md](lang/05-lazy-seq.md) — the `List`/`Seq` split
  proposal 5 asks to make uniform.
