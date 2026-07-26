# `cond` is not a keyword

*Lab notebook. Building a corpus generator for a language with no corpus, and
discovering that most of what looked like model error was reference error.*

---

## The problem

Stitch has no training corpus, because Stitch has no users. To train a model to
write it, the corpus has to be manufactured — and the plan
([corpus-mvp.md](../plans/corpus-mvp.md)) is the obvious one: ask a local model
for programs, keep the ones that survive a gate, repeat until there are enough.

The gate is the part this project gets for free. `parse → typecheck → run the
program's own tests` is three function calls into a compiler I already own,
costing microseconds and never wrong. Most people doing this need a sandbox per
candidate, or a judge model that is noisy and has opinions.

So: eleven candidates by hand, then a harness, then a batch. What follows is
what the batch said, which is not what I expected it to say.

## Eleven candidates, pasted by hand

The first phase was deliberately manual — paste a prompt into LM Studio, read
the output, write down what broke. Slow on purpose: the point was to find out
what the prompt needed before automating the asking of it.

Each failure changed the prompt, and the prompt got five versions:

| v | Change | Because |
|---|---|---|
| 1 | first draft | — |
| 2 | `prelude.st` embedded verbatim | a prose rule saying "use `and`, not `&&`" was quoted back to me *and then ignored* |
| 3 | size counted in declarations, not lines | the model counted lines four times, got it wrong, and deleted a working function to hit a number |
| 4 | must-use identifiers removed | they became the function names — `overlaps` was renamed `birch` |
| 5 | the native functions listed | `filter`, `sort` and the whole `Str` module existed and were invisible, so the model refused to use them |

Version 2 is the one that taught me the most. The prompt said, in plain English,
that `&&` does not exist. The model's reasoning trace *restated that rule
correctly* — and then wrote `&&` in every conditional it produced. Four times.

A negative prose rule does not override a token prior. What fixed it was not
saying it louder; it was including the standard library verbatim, so the rule had
**code-shaped evidence** behind it. `acc or pred(x)` in a real function beats a
sentence about operators.

## The harness

Copy-pasting stopped being tolerable around candidate eight, so:

```
cargo xtask cram --gen --model qwen/qwen3.5-9b --count 5 --out corpora/batch1
```

It streams the model's output to the terminal as it arrives — not for
decoration, but because a candidate takes half a minute and a silent terminal
that long is indistinguishable from a hang. More usefully, a repetition spiral is
obvious on sight within seconds, long before the token cap ends it.

Then it extracts the program, runs the gate, and prints the funnel:

```
5 attempted → model errors 0 → empty 0 → parse 5 → type 0 → tests 0 → ok 0
154s, 7510 tokens, 48.7 tok/s
```

**The funnel, never one number.** The stage a candidate dies at is the diagnosis:
parse deaths mean the generator does not know the grammar, type deaths mean it
has the shape and not the semantics, test deaths mean it had the semantics and
got them wrong. A single yield percentage collapses three different next actions
into one shrug.

## The batch

Five for five at parse. Zero yield. Reading the error messages:

- `expected '(' after function name`
- `expected '|' in conditional`
- `expected '{' after match subject`
- `expected '(' after function name`
- `expected ')'`

Reading the actual source at those spans is a different story:

| # | Actually |
|---|---|
| 001 | `ext Time = Int` — a **type alias** |
| 002 | a multi-arm `=>` chain with no `\|` |
| 003 | bare `match` plus an invented `in` operator |
| 004 | `cond overlap(b1: Booking, b2: Booking) -> Bool` |
| 005 | `(c: TimeWindow) ->` — a type-annotated lambda parameter |

**Three of the five are defects in my reference, not the model.**

Stitch has no type aliases; the reference never said so. Lambda parameters are
never type-annotated; the reference never said so. And 004 is the one worth the
post title.

## `cond`

My syntax reference contained this line:

```
cond => thenValue | elseValue
```

`cond` was a **metavariable**. It meant "any expression that evaluates to a
boolean, followed by `=>`". I wrote it the way one writes a grammar rule, for a
reader who knows that `cond` is a placeholder.

The model read it as a keyword and wrote:

```
cond overlap(b1: Booking, b2: Booking) -> Bool = { … }
```

Which is a completely reasonable reading. Nothing in the line marks `cond` as
different in kind from `=>` or `|`. A reader who has never seen the language —
which is exactly the reader this document is for — has no way to tell the
placeholder from the syntax.

It now reads:

```
n > 10 => "big" | "small"
```

No metavariable to misread.

## The thing I keep relearning

This is the fourth time in eleven candidates:

- `&&` and `return`, because the reference showed no boolean operators.
- `@b.end` on a lambda parameter, because it showed no plain field access.
- `ext Room(…) = prod Room(…)`, because it showed `prod` and it showed `ext fn`,
  and never showed `ext prod`.
- `cond overlap(…)`, because it showed a placeholder as though it were syntax.

> **An omission from a reference does not produce a blank. It produces a
> confident wrong guess.**

And the model is disciplined in the other direction, which makes it worse. It
will *not* call a function it has not been shown — three separate traces show it
declining `filter`, declining `sort`, and abandoning a whole function on
discovering the prelude has `max(xs)` but no `max(a, b)`. So the reference is not
a hint. It is a **ceiling on program quality**, and every gap lowers it silently.

## What the checker overturned

Before the gate existed I had been reading candidates by eye and writing down
verdicts. When I finally built `stitch::gate::run` and swept the saved programs,
**two of five verdicts were wrong** — a 40% error rate on my own reading.

The two that mattered:

**The type checker catches nothing.** I had a program where `overlap` takes two
`Booking`s and the caller passes two `Time`s, and I confidently recorded it as a
type error. It type-checks clean — Stitch's typing is gradual, so diagnostics are
advisory. Across five programs: parse caught two, tests caught three, the type
checker caught **zero**. Which makes requiring a `test` block in every generated
program load-bearing rather than decorative.

**One program passes the entire gate and is wrong.** Once a single name error is
fixed, candidate 006 parses, type-checks, and passes all five of its own tests —
while only ever comparing the *first* booking against the rest. A three-element
list with the conflict away from the head is silently reported conflict-free.

Its tests do not catch it because the code and the tests were written from the
same misunderstanding. They agree with each other and both are wrong. That is the
concrete argument for scoring a suite by **mutants killed** rather than by
passing: a mutation swapping that scan for a full pairwise one survives the suite
untouched, which is exactly the signal that the suite is thin.

## What is next

Every failure in batch1 died at parse, and parse is the one stage a **grammar
mask** makes structurally impossible — the continuation oracle already knows the
legal token set at every position, so an invalid token can simply be removed from
the distribution. That does not fix `in`, or a wrong overlap predicate, or a
thin test suite. It fixes precisely the five things that happened here.

Two runs, in order:

1. **Unmasked, ungated.** For a first training run the corpus does not need to
   parse — the goal is only to beat uniform-over-legal on held-out NLL, and a
   program with three bad tokens in fifteen hundred is still overwhelmingly
   correct Stitch by token. ~500k tokens is about three hours at the measured
   rate.
2. **Then the mask**, and measure what the funnel looks like when parse is free.

One design note for run 1: the babble corpus was ~24M tokens. Comparing a
500k-token real corpus against that confounds quality with quantity, and the real
one could lose on volume alone while being better per token. The comparison that
answers the question is **same-budget** — babble-500k against real-500k, identical
config, only the corpus differing.

---

## Things worth remembering

- A prose rule loses to a token prior. Evidence in code wins.
- A reference's *metavariables* are part of its surface. If a reader cannot tell
  the placeholder from the syntax, it is syntax.
- The model will not use what it has not been shown, so the reference is a
  ceiling and not a hint.
- Gradual typing means the type stage catches almost nothing. Tests are doing the
  semantic work.
- A suite written from the same misunderstanding as its code agrees with the
  code. Pass/fail cannot see that; mutation scoring can.
- I read five programs by eye and got two wrong. Build the checker earlier than
  feels necessary.
