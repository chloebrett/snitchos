# Corpus MVP — Increment 0, the spike

**Status: 📐 PLAN — not started.** Increment 0 of
[corpus-mvp.md](corpus-mvp.md). Related:
[corpus-recipe-axes.md](corpus-recipe-axes.md) (the recipes used below),
[../docs/language-design.md](../docs/language-design.md) (source for the
cheat-sheet), [stitch-examples-corpus.md](stitch-examples-corpus.md) (where
better exemplars come from).

**The deliverable is a decision and four numbers, not code.** Roughly two hours,
most of it pasting.

---

## What this answers

1. **Which model.** The primary output.
2. **Is the plan viable at all** — the floor check.
3. **Build order** — do Increments 7 and 8 come before the first real run?
4. **Prompt v2** — what obviously fails in v1. Most of the value is here, because
   yield dominates every downstream cost.

## What it cannot answer

**The yield number.** Twenty samples per model puts binomial noise at roughly
±10 percentage points, so this cannot distinguish 20% from 35%. It gives an order
of magnitude and a floor. Precise yield comes from the first real batch; do not
tune anything against these numbers as if they were tight.

---

## Setup

### S1. Models — fix the family, vary only size

Install **Qwen3-4B, Qwen3-8B and Qwen3-14B** (or another single family across
three sizes). Holding the family fixed makes this a controlled comparison about
*size*; mixing families confounds size with training data. Family is a separate
axis and would need its own twenty.

**Use the text-only variant, not `-vl`.** Vision-language builds spend a large
share of their post-training on multimodal data, which dilutes code ability
against the text-only sibling at the same parameter count. There are no images
here, so the VL tax buys nothing — and if one size in the comparison is VL and
another is not, the size comparison is confounded.

Configuration, all easy to get wrong and expensive:

- **Thinking mode off — and verify it actually is.** Thinking tokens are
  discarded here and the cost is unbounded, not merely tripled (Findings 002 spent
  2.5 minutes thinking and never emitted a program). If the UI has no switch, in
  rough order of reliability: set `chat_template_kwargs: {"enable_thinking":
  false}` if the runtime exposes it; append `/no_think` to the system message
  (the Qwen3 convention); or **prefill the assistant turn with an empty
  `<think></think>` block**, which forces it closed in any runtime that allows an
  assistant prefix. Confirm by checking that the response contains no reasoning
  preamble — do not assume a toggle worked.
- **Sampling: temp ≈ 0.7, top_p ≈ 0.8, top_k ≈ 20** (Qwen3's non-thinking
  recommendations), plus **repetition penalty ≈ 1.05–1.1**. Near-greedy decoding
  is the classic route into a repetition collapse — see Findings 001.
- **`max_tokens` ≈ 1200.** A 40–70 line program is ~600–900 tokens. Without a cap,
  a looping candidate consumes whatever context remains.
- **Pin all of the above now**, and write them on the record sheet. Later
  comparisons against a bulk run are meaningless if these drift.

### S2. Exemplar pool

Usable today: `stats.st` (111), `text.st` (70), `prelude.st` (108),
`double.st`, `primes.st`. **Exclude `plans/lang/samples.st`** (illustrative
fragments, not programs). **`stim.st` (890) and `json.st` (381) are too large to
paste** — they would dominate prefill.

That leaves a thin pool, which is the point of S3.

### S3. The cheat-sheet

**Written — it is the reference block inside Prompt v1 below.** Syntax verified
against [../docs/language-design.md](../docs/language-design.md) rather than
inferred from the exemplars, because a cheat-sheet that teaches wrong syntax is
worse than none.

**This is the highest-value artifact in the spike.** With the pool in S2, most
constructs have no exemplar demonstrating them, so the cheat-sheet carries nearly
all the syntax load until the 30 programs land. Example-based, not prose — models
follow examples better than descriptions.

### S4. The one piece of code allowed

`stitch` has no `src/bin`, so there is no `stitch check foo.st`. The only
zero-code path is dropping candidates into `examples/stitch/` and running the
test suite, which is all-or-nothing and pollutes the repo.

Pull forward the first half of Increment 1: **`verdict(src) -> Verdict` plus a
~20-line main that reads a path.** Sixty manual inspections become sixty
one-line commands.

> **Tripwire.** If this grows candidate extraction, a recipe loader, or anything
> that calls a model, the harness has arrived early through the side door. Stop
> and go back to pasting. The budget is the gate function plus a `main`.

---

## Prompt v1

Rendered for recipe **#69 sauna booking** — chosen as the first paste because
`prod`, `Maybe` and `|>` are all visible in both exemplars, making it the fairest
possible first test. Swap the final section to run any other recipe; everything
above it is invariant and belongs in the cached prefix.

**Exemplars are `text.st` + `stats.st` (181 lines), pasted verbatim below.** Not
`bank.st` or `json.st`: at 635 lines those two put the prompt near 9–10k tokens,
which made prefill roughly equal to decode per candidate, and their multi-line
rationale comments are the most extreme style in the corpus — the suspected cause
of Findings 001's comment spiral. This pair keeps the prompt near ~2.6k and shows
a plainer commenting register.

**The current prompt is [prompt v5](corpus-prompts/v5.md)** —
system prompt and user message, ready to copy. Kept in its own file so there is
one canonical copy to paste from rather than two that drift.
[v1](corpus-prompts/v1.md) and [v2](corpus-prompts/v2.md) are retained for
comparison; what changed and why is in Findings 003 and 004.

### Reading the first result

- **Judge meaning, not validity.** Does it actually detect interval overlap, or
  is it a record store with a filter? A parse failure is the *least* interesting
  outcome; a program that parses and typechecks while being about nothing is the
  result that should worry you.
- **The three jargon words are a live test** of the must-use-words finding.
  `cooldown` lands easily and `cedar` is a plausible room name, but `loyly` has no
  home unless the model reaches into the domain. `let loyly = 0` dead filler on
  prompt one would confirm the failure mode early and cheaply.
- **Time prefill separately from decode.** Reference plus two exemplars is
  ~2,600 tokens, and that number is exactly what Increment 4's prefix caching
  exists to amortise.

---

## The eight recipes

Drawn from [corpus-recipe-axes.md](corpus-recipe-axes.md), balanced two-per-shape
— **deliberately over-sampling `script`**, which is only 5 of 100 in the seed
table and would otherwise go untested.

| # | Domain (clause) | Constructs | Size | Shape | Words |
|---|---|---|---|---|---|
| 28 | taxi meter — state machine over distance and waiting time, with tariff changes | sum, prod, uses Telemetry | small | script | fare, surcharge, hiring |
| 88 | brewery fermentation — time series against target curves, with alerts | prod, contract+on, uses Telemetry | large | script | krausen, gravity, pitch |
| 69 | sauna booking — exclusive occupancy, so overlapping intervals must be detected | prod, Maybe, \|> | small | module | cedar, loyly, cooldown |
| 58 | go territory scoring — flood-fill regions; distinguish alive groups from dead | sum, recursion, Map, \|> | large | module | liberty, seki, atari |
| 27 | level crossing barrier — a safety interlock state machine | sum, contract+on, uses Telemetry | medium | server loop | interlock, approach, wigwag |
| 98 | laundromat machine status — cycle timing and queue notification | sum, prod, uses Telemetry | medium | server loop | drum, lint, cycle |
| 46 | petty cash box — receipts must reconcile the float | prod, Result+? | small | library-with-heavy-tests | chit, imprest, float |
| 54 | bowling scorecard — strikes and spares pull forward later frames | sum, recursion | medium | library-with-heavy-tests | frame, spare, tenth |

**Run each two to three times per model** (≈20 per model, 60 total). Repeats are
not waste: within-recipe variance is itself a finding. A recipe that goes 0/3 on
every model is a recipe problem; 1/3 everywhere is noise you need to know about
before reading anything else.

Render each as a **brief**, not an axis list — per Increment 4:

> Write a tides API: a server loop that answers queries against a tide table.

not

> Domain: tide table / Shape: server loop

---

## Procedure

For each of 60 candidates:

1. Build the prompt: system + cheat-sheet + 2–3 exemplars + rendered brief.
2. Paste, generate, note **decode tok/s** as reported by the runner.
3. Save the extracted program to a file; run the S4 checker.
4. Record the row.

### Record sheet

One table, kept comparable across models. Note the sampling params once at the
top.

| id | model | recipe | tok/s | verdict | death detail | note |
|---|---|---|---|---|---|---|
| 01 | qwen3-4b | 28 | | ok / type / parse / extract | | |

`extract` means no usable fenced block came back — a prompt failure, not a
Stitch failure, and worth separating because the fix is different.

### Also record, once per model

- Ten programs eyeballed: **does this mean anything, or is it Stitch-shaped and
  saying nothing?** This is the judgement no metric substitutes for and it is the
  deciding input.
- Anything the model got wrong *consistently* — that is prompt v2's backlog.

---

## Decision rules — fix these before looking at results

Pre-registered so the numbers cannot be rationalised after the fact.

**Floor check.** At least one program parses across all 60 → proceed. Zero → stop
and reconsider; per corpus-mvp §7, a program parse rate of zero implies per-token
legality low enough that constrained decoding would puppet an ignorant model
rather than confirm a capable one.

**Model choice.** Highest **type-pass** count wins. Ties go to the smaller model
on throughput. Parse-pass is not a tiebreaker — Increment 7 flattens it to ~100%
for every model, so it cannot discriminate.

**The size override.** If the 14B's type-pass is more than ~2× the 4B's, the
yield gap may beat the ~7× speed gap. Recompute the validated-tokens-per-second
table with *measured* numbers rather than assuming small wins. Memory is not a
constraint at 64 GB, so every size stays available.

**Build order.** Compute `500_000 / yield / throughput`. Beyond ~16 hours, build
Increments 7 and 8 before the first real run — 7 first, since it is the larger
lever and removes the wasted candidates 8 would otherwise carry.

**The semantic red flag.** High parse-pass with near-zero type-pass everywhere
means the model has the shape and not the meaning. That is a cheat-sheet and
exemplar problem, and it must be fixed before any harness — a harness would only
industrialise the production of Stitch-shaped nonsense.

---

## Findings

Running log. One entry per candidate worth learning from — not all sixty.

### 001 — qwen3-vl-4b, recipe #69 sauna booking

| | |
|---|---|
| Model | qwen3-vl-4b, 16k context |
| Exemplars | `bank.st` + `json.st` — 635 lines, **prompt ≈ 9–10k tokens** |
| Throughput | **58.6 tok/s** single instance |
| Generated | 1589 tokens, did not stop on its own |
| Verdict | **parse** — but see below |

**What went right, and it is most of the interesting part.** The first ~25 lines
are structurally sound Stitch and semantically on-target:

```
prod Booking(start: Int, end: Int)
prod Room(mut bookings: List<Booking>)
on Room {
    mut book(start: Int, end: Int) -> Maybe<Booking> =
        let overlaps = find(@bookings, b -> start < b.end and end > b.start)
```

`start < b.end and end > b.start` is the textbook interval-overlap predicate. It
picked the right data shapes, used `mut` correctly, put behaviour on an `on`
block, and reached for a combinator rather than a loop. **The distinguishing
clause worked** — this is the domain's real computation, not a record store with
a filter.

**What went wrong.**

1. `prod Booking(start: Int, end: Int) = // …` — a spurious `=` after a `prod`
   declaration, twice.
2. `@b.end` where `b` is a lambda parameter — conflated the receiver sigil with
   general field access. `@` is only ever the receiver.
3. Inverted logic: both branches of the overlap check return `Some`, and the
   branch that appends is the one taken *when an overlap exists*.
4. **Terminal repetition loop** — ~1300 tokens of one comment repeated verbatim.

**Diagnosing the loop, in order of likely contribution:**

- **Sampling.** Near-greedy decoding with no repetition penalty and no
  `max_tokens`. With 16k context and a ~4k prompt it had ~12k tokens of room to
  spiral. Fixed in S1.
- **The comment instruction is hostile here.** "Comments explain *why*, never
  *what*" combined with `bank.st`, whose comments are multi-line rationale
  essays, taught *write extended prose justification*. Prose carries no syntactic
  obligation to terminate; a function body must close.
- **Comments are the escape hatch when the model is lost.** It derailed
  immediately after writing `mut book(...) = let newBooking = …` — a bare `let`
  sequence with no `{ }` block, a form the exemplars never show. Having produced
  an ungrammatical body it had no legal continuation in mind, and fell back on the
  one construct that is always legal. The loop is what a syntax dead-end looks
  like, not a comment problem.

**Bound on Increment 7, worth recording before it is built:** constrained
decoding would **not** have stopped this loop, because comments are grammatically
legal everywhere. It would have prevented the spurious `=` and the malformed body
that caused the dead-end — so it addresses the cause and not the symptom.
Repetition penalty and a token cap are needed regardless of the mask.

**The per-token legality read is good, and it is the reason to proceed.** Roughly
4–6 bad tokens out of ~300 code tokens ≈ **98% per-token legality** — the band
where the mask confirms a capable model rather than puppeting an ignorant one. By
corpus-mvp §7's table, 98% per-token predicts a program-level parse rate near
zero at this length, which is exactly what was observed. A parse failure here is
the amplification effect working as described, not evidence against the approach.

**Throughput confirms the model.** 58.6 tok/s sits inside the predicted 50–90
band, so the bandwidth model holds and the wall-clock table stands (~12 h for 500k
validated at 20% yield, single stream).

**Prompt size is a first-order cost, and the exemplar choice sets it.** Pasting
`bank.st` + `json.st` put the prompt at ~9–10k tokens rather than the ~2.6k the
prompt was designed around. At M1 Max prefill rates that is ~20–30 s per
candidate — roughly equal to the decode it was paying for, and it would *dominate*
once `max_tokens` caps decode at ~1200. Two consequences:

- Exemplar selection is a throughput decision as well as a quality one. Prefer
  the smallest exemplars that demonstrate the required constructs; `stim.st` (890)
  and `json.st` (381) are effectively unusable as prompt content, exactly as S2
  says.
- Prefix caching is worth **more** than corpus-mvp §Napkin's ~1.5× estimate when
  the invariant block is large. The fix here is a smaller prompt; the general
  lesson is that the caching design in Increment 4 earns most of its keep on the
  exemplar block, not the system prompt.

**Actions before candidate 002:**

- Apply the S1 sampling block (temp/top_p/top_k, repetition penalty,
  `max_tokens`).
- Switch to the **text-only** 4B, not `-vl`.
- Swap `bank.st` for `stats.st` + `text.st` — smaller, and less extreme comment
  style. Change one thing at a time: exemplars *and* sampling together would
  leave the cause ambiguous.
- Soften the comment line in the system prompt toward brevity.
- **New harness item, carried to Increment 5:** an n-gram repetition detector that
  kills generation early. This candidate burned ~1300 tokens after it was already
  dead; at scale that is a meaningful share of a run.

### 002 — qwen3.5-4b, thinking on, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, 16k context, **thinking mode on** (no switch found) |
| Result | 2.5 minutes of reasoning, **no program emitted** — looped inside the think block |
| Verdict | **extract** — nothing to check |

**The loop had a single, identifiable cause: the word `loyly`.** The reasoning
trace shows the model trying to emit it as an identifier, typing `loyalty`
instead, noticing the mismatch, and re-entering the correction cycle —
indefinitely. Roughly forty repetitions of "I will write `let loyalty = 30` but
the variable name must be `loyly`."

**The mechanism, and it generalises.** `loyly` is OOV and shatters into BPE
fragments; `loyalty` is a single high-frequency token one small edit away. The
prior pulls the model to `loyalty` every time, its self-check rejects it, and
with thinking mode granting unbounded room there is no exit. **A jargon word is a
trap when it is rare *and* has a high-frequency near-neighbour** — rarity alone is
fine (`krausen`, `boneyard`, `dunnage` have no dominant attractor), it is the
attractor that kills.

Three fixes, in order of importance:

1. **Make the words a preference, not a requirement.** A hard constraint a small
   model cannot satisfy can deadlock; a soft one degrades gracefully. The task
   block now reads "where they fit naturally, prefer these names… if one does not
   fit, leave it out." Whether the words actually landed is a **post-hoc harness
   metric**, not something the model must enforce against itself.
2. **Screen jargon for near-neighbours** before it enters the axis list.
3. `loyly` → `birch` in recipe #69.

**Thinking mode is not a cost problem here, it is a correctness problem.** S1 said
it triples time-per-candidate; that understated it. The think block is where the
loop lived, and depending on runtime it may not be subject to the same
`max_tokens` and repetition-penalty settings as the answer. S1 now carries three
concrete ways to force it off, including the assistant-prefill trick.

**The cheat-sheet has gaps, and gaps produce inconsistent guessing.** The trace
reached for `&&`, `||`, `return`, and `++` — none of which are Stitch. Verified
against `parser.rs:219` and `primes.st`: the operators are `and` / `or`, there is
no `return`, and a block's value is its last expression. Candidate 001
independently guessed `@b.end` for field access on a lambda parameter. **Every one
of these is a construct the cheat-sheet never showed**, so the model filled the
hole from C-family priors — differently each time. Added to Prompt v1: boolean
operators, field access vs. the `@` receiver, ranges and list literals, and an
explicit "there is no `return`."

**The encouraging part, again.** Before deadlocking, the model derived
`s1 < e2 and s2 < e1` — the correct overlap predicate, independently, for the
second time in two candidates. It also correctly worried that `const` is not a
Stitch keyword, that there is no `main` in a module shape, and that `overlaps`
takes two `Booking`s so strings cannot be passed directly. **The model can do this
task.** Both failures so far have been prompt and configuration defects, not
capability defects.

### 003 — qwen3.5-4b, prompt v1, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, thinking on, ~4 minutes |
| Result | **A complete, coherent, on-topic 65-line program with three tests.** Terminated cleanly. |
| Verdict | **parse** — but close, and every error is traceable |

The 001/002 failure classes are gone: no repetition spiral, no deadlock. What
remains is a program with a good semantic core and a handful of *systematic*
errors, each of which the reasoning trace explains exactly.

**The semantic core keeps being right.** Third candidate running to derive
`start1 < end2 and start2 < end1` independently, this time with half-open
interval reasoning *and* an unprompted `sameRoom` check — two bookings in
different rooms do not conflict, which the brief never said. The distinguishing
clause is doing its job.

#### The headline: a prose rule loses to a token prior

The v1 reference contained, verbatim:

```
a and b     // NOT &&
a or b      // NOT ||
```

The trace shows the model **reading and correctly restating that rule** — *"it
says 'Booleans are words, not symbols'. `a and b`"* — and then writing `&&` in
every conditional it produced. Four times.

Two lessons, and the second is the important one:

1. **A negative prose rule does not override a strong token-level prior.** At the
   moment of emitting `b1.start < b2.end `, the code-context prior for `&&` is
   overwhelming, and a line of English fifty tokens earlier does not move it.
2. **The rule had no code-shaped backup.** Neither exemplar contains a single
   `and` or `or` — `text.st` and `stats.st` use only `=>`/`|` conditionals. The
   one form of evidence the model actually follows was absent from the prompt.

**This is the strongest argument yet for Increment 7.** Constrained decoding is
not merely a yield optimisation for this failure class — it is the *only*
reliable fix. A token prior cannot be prompted away; a grammar mask makes `&&`
unrepresentable, absolutely and for free.

#### Confirmed from the inside: the `ext prod` gap

The trace derives the error step by step — it records `prod Name(fields...)` and
`ext name(params) -> Type = body` as two separate rules, finds no rule for
*exporting a type*, and composes them into `ext Room(...) = prod Room(...)`
(three times, for Room, Booking and Conflict). `stats.st` has `ext prod
Summary(…)` right there, but one occurrence in an exemplar lost to two explicit
rules in the reference.

#### The `fold` thrashing was a prompt defect

Roughly a quarter of the four minutes went on reconciling `xs |> fold(0, f)`
(reference) against `fold(1..=n, "", f)` (exemplar) and never resolving it; the
output `fold([], [], (acc, b) -> …)` is a fusion of both readings. They are
consistent — a pipe supplies the first argument — but **nothing in the prompt
stated the desugaring**, so two correct examples read as a contradiction.

#### The most useful behavioural finding

**The model treats the reference as the boundary of the language and stays
inside it, deliberately:**

> *"`filter` is not in the reference examples… I should avoid `filter` to be safe."*
> *"`sort`? Not shown. I'll use bookings directly."*
> *"Int to Str is `Str.toStr`? Reference doesn't show conversion… I'll use Int in `Conflict`."*

It changed a field's type from `Str` to `Int` purely to avoid a conversion it had
not been shown. So **the reference's coverage bounds the program space the model
will attempt** — every missing combinator narrows what it can write, and it will
not guess its way out.

That is what makes `prelude.st` the right fix rather than a hand-written
signature list (below).

#### Remaining errors, all downstream of the above

- `acc |> all(a -> overlaps(a, b))` should be `any` — `any` exists in the
  prelude, which was not in the prompt. `all` on an empty list is vacuously true,
  so as written it reports conflicts everywhere.
- `findConflicts` declares `Maybe<List<Conflict>>` but the fold returns a bare
  list; no `Some(…)` on the success path.
- `Conflict(b.roomId, b.roomId, 0)` — same room twice, so the record carries no
  information.
- Positional construction throughout (`Booking(1, 10, 12)`) where the reference
  and `stats.st` both show named fields.

#### Cost, and the must-use words

**Four minutes for 65 lines, mostly thrashing** — `findConflicts` was rewritten
about eight times and the shipped version still has the fold bug. Separately, the
must-use words consumed roughly ten paragraphs of deliberation to produce
`cedar`/`birch` in *comments and test names only* and a `cooldown` field that is
always 0 with a comment admitting it means nothing.

The softened wording prevented 002's deadlock but not the spend: **for a thinking
model, a soft constraint still buys full deliberation.** The words axis is a
candidate for removal at small scale — it costs a large share of the reasoning
budget and has yet to produce a single meaningful identifier.

#### Prompt v2

[prompt v2](corpus-prompts/v2.md). Changes, each tied to
evidence above:

- **`prelude.st` included verbatim as a "Standard library" section.** This is the
  stdlib written in Stitch itself, 108 lines, and it is a better answer than a
  hand-written signature list for four reasons at once: it *lists* what exists
  (`any`, `find`, `contains`, `unwrapOr`, `flatten`, …); it shows `and`/`or` in
  **real code**, giving the boolean rule the code-shaped backup it lacked; it
  shows `match m { Some(v) => … }` — the pattern form, which v1's reference never
  covered at all; and it shows the unpiped 3-arg `fold(xs, init, f)`. It also
  cannot drift from the language, because it *is* the language.
- **`ext prod` / `ext sum` added**, including that each field needs its own `ext`,
  and that `ext` is invalid on `contract`, `on`, and `use` (verified against
  `parser.rs:818`).
- **The pipe desugaring stated explicitly**, with `fold(xs, 0, f)` and
  `xs |> fold(0, f)` shown as identical.
- **Both `match` forms** — guard and pattern.
- The boolean and no-`return` rules **promoted into the system prompt**, where
  they are closer to the generation point, while relying on the prelude for the
  actual evidence.

Prompt v2 is ~3.5k tokens, up from ~2.6k. That is a deliberate trade: the added
900 tokens are invariant, cacheable, and target three confirmed failure modes.

### 004 — qwen3.5-4b, prompt v2, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, thinking on |
| Result | Interrupted mid-reasoning — the model was iterating on line count, not on the program |
| Verdict | n/a |

**Prompt v2 fixed three confirmed failure modes.** In this trace, all correct:

- `s1 < e2 and s2 < e1` — **`and`, not `&&`**, everywhere. The prelude giving the
  rule code-shaped backup did what a prose rule alone could not.
- `match acc { Some(_) => acc   None => … }` — the pattern form, used correctly.
- `prod TimeWindow(start: Int, end: Int)` — no `ext X(...) = prod X(...)`.
- `Booking(id: 99, room: "cedar", window: TimeWindow(start: 0, end: 120))` —
  named-field construction.

That is 003's entire error list, gone. The v2 changes are validated.

#### The new dominant cost: the model cannot count lines, and tries anyway

The trace counts lines manually **at least four times**, gets it wrong ("This is
85 lines" for a program of roughly 78), and then *degrades the program to fit*:
it deletes the `book` function, deletes a test, and strips comments — while
noting each deletion is only to hit the size. Several full rewrites are spent
entirely on this.

Compounding it, the size was ambiguous about tests, so the model counted its own
test blocks against the budget and squeezed the module to compensate.

**The general rule, now on its third instance:**

> **Do not make the model enforce a constraint the harness can measure.**

Must-use words (002, 003) and size (004) are the same failure: the model spends a
large share of its budget self-enforcing, does it unreliably, and *reduces
program quality* in the attempt — for a property that is one line of Rust to
measure after the fact. Express the intent qualitatively, let the model write, and
measure the outcome in the funnel.

#### Still unfixed: `if`/`then`, and it is the `&&` pattern again

The trace writes `if overlaps(window, b.window) then Some(b) | None` inside a fold
lambda, despite the system prompt's "There is no if/else."

Same class as 003's `&&`: a **negative prose rule losing to a token prior in a
specific syntactic position**. Note `text.st` *does* show a `=>`-conditional inside
a `map` lambda, so an example existed — it was just not in the same position
(fold accumulator) where the `if` prior is strongest. v3 adds the counter-example
in exactly that position, but this failure class is the standing argument that
some priors are only fixable by a grammar mask (Increment 7).

Also still guessing at conversions: `b.id.toString()`, method-style. The natives
are free functions (`toStr`, `Str.parseInt`) and neither appears in the prelude,
so the reference had no answer to give.

#### Prompt v3

[prompt v3](corpus-prompts/v3.md):

- **Size is counted in declarations, not lines**, over a wide range: "a small
  module — 1 to 4 types and 2 to 6 functions. Tests are extra and do not count
  toward that. This is a rough guide, not a limit: never delete working code or
  drop a test to hit it, and if the program naturally wants to be bigger, let it
  be." Actual line counts become a harness metric.

  A first attempt said "a couple of types and three or four functions" — which
  merely **moves the counting target** rather than removing it; the model would
  have trimmed to hit *that*. Two properties make the replacement safe: types and
  functions are countable at trivial cost (unlike lines), and the range is wide
  enough that a reasonable program satisfies it on arrival, so there is nothing
  to optimise toward. Per-bucket ranges live in
  [corpus-recipe-axes.md](corpus-recipe-axes.md); they overlap deliberately, so
  they signal scale without becoming a sharp edge.
- **A no-`if` counter-example in the failing position** — a `=>` conditional inside
  a lambda, and a `match` inside a fold accumulator.
- **Conversions added**: `toStr(n)` with an explicit "NOT `n.toString()`", and
  `Str.parseInt(s)`.

#### Cheap experiment worth running on v3

The must-use-words line has now cost budget in every candidate and produced no
meaningful identifier. Run v3 twice — once as written, once with that line
deleted. If the program is as good and arrives sooner, the axis has failed its own
test and should come out of [corpus-recipe-axes.md](corpus-recipe-axes.md).

### 005 — qwen3.5-4b, prompt v3, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, thinking on, ~4 minutes |
| Result | **The first plausibly-valid program.** 1 type, 2 functions, 2 tests |
| Verdict | expected **parse ✓ · type ✓ · tests ✗** |

Syntax is clean throughout: `ext prod Booking(ext start: Int, ext end: Int)`,
`and` not `&&`, `any(xs, pred)` matching the prelude signature exactly, named-field
construction, no `if`, no `return`. **v3's size fix worked completely** — the trace
contains *zero* line counting, where 004 counted four times and deleted working
code. 1 type and 2 functions, comfortably inside the small bucket.

#### The bug, and how it got there

```
ext cedar(bookings: List<Booking>) -> Bool =
    any(bookings, b -> any(bookings, x -> birch(b, x)))
```

Every booking is compared **against itself**. `birch(b, b)` is
`b.start < b.end and b.start < b.end` — true for any valid booking. So `cedar`
returns `true` for any non-empty list, and the program's own second test
(`cedar([b1, b2]) == false` for disjoint bookings) **fails**.

The trace is the interesting part. The model *found this exact bug*:

> *"x = b1. `overlaps(b1, b1)` -> True. So checkOverlaps returns true even if
> there's no real conflict…"*

and then argued itself out of it:

> *"Wait, if I have `[b1, b1]`, do they conflict? Yes. So checking all pairs is
> fine for 'exclusive use'."*

It conflated *two identical bookings in a list conflict* (true) with *a booking
compared to itself conflicts* (a bug). **More reasoning made the program worse:
it detected the defect and constructed a justification for shipping it.** Worth
holding onto — self-correction is not monotonic, and a thinking model can talk
itself past a correct observation.

#### This validates the oracle ladder

Parse ✓, typecheck ✓, **tests ✗**. That is precisely the case
[corpus-mvp.md](corpus-mvp.md) §3's rung 3 exists for, and the first candidate to
demonstrate that rungs 0–2 are not sufficient. A syntax-only gate would have
accepted this program into the corpus.

**And it was caught for free, by the model's own tests.** No held-out suite was
needed: a model that writes a test its own code fails has produced *detectably*
wrong code at zero marginal cost. That is weaker than supplied tests (nothing
stops vacuous ones — hence the mutation-kill reward in §7) but it is a real
semantic signal available immediately, and it should be in the funnel from the
first batch rather than deferred with the rest of rung 3.

#### The must-use words finally landed, and the result is bad enough to kill the axis

The words became the **function names**: `birch` is the overlap predicate,
`cedar` is the conflict checker. Not decoration this time — they **displaced the
meaningful names**. `overlaps` became `birch`; `hasConflicts` became `cedar`. A
reader cannot tell what `birch(b1, b2)` does.

That is worse than 003's dead field, because the corpus's purpose is to teach
good Stitch, and this teaches *name your functions after trees*. Five candidates
of evidence, no successes:

| | Outcome |
|---|---|
| 002 | deadlock on `loyly`; no program at all |
| 003 | `cooldown` a permanently-zero field; `cedar`/`birch` in comments only |
| 005 | words displace meaningful function names |

Every candidate also spent a large share of its reasoning budget deliberating
where to put them.

**The root cause is that the mechanism does not port.** TinyStories' random-word
trick works because prose absorbs an arbitrary noun without cost. **Code
identifiers carry semantics; a name that does not describe its referent is a
defect, not a variation.** Lexical diversity in a code corpus has to come from
domains, constructs and shapes — the axes that change *what the program does*.

**Decision: the must-use-words axis is removed** from
[corpus-recipe-axes.md](corpus-recipe-axes.md). Prompt v4 is v3 with that line
replaced by "Name things for what they do."

#### The prelude discipline is working — against an incomplete reference

Three separate times the model checked the prelude and stayed inside it:
declining `filter` and `sort` as unlisted, and abandoning a `cooldown` function
entirely on discovering `max(xs)` but no `max(a, b)`.

**But `filter` and `sort` both exist.** `prelude.st` is only the *derived* layer —
its own header says it is built "from the native core (fold/map/filter)". The
natives are implemented in Rust and appear nowhere in the file, so the model
could not see them. Missing from v4's prompt entirely: `filter`, `sort`, `sortBy`,
`zip`, `enumerate`, `take`, `drop`, `takeWhile`, `dropWhile`, `flatMap`,
`foldWhile`, `concat`, `reverse`, `List.at`/`set`/`insert`/`removeAt`, the whole
`Str` module, and `toStr`.

So the discipline is real and the reference was wrong, which is the more expensive
combination: **the model writes deliberately worse programs than it could, and
does so silently.** `cedar`'s O(n²) self-comparing implementation is plausibly a
direct consequence — `sort`, `zip` and `enumerate` were all available and all
invisible.

This also corrects the framing in Findings 003. "The model stays strictly inside
the reference" is a *capability* observation, but its cost was under-stated: the
reference is not merely a boundary on what the model attempts, it is a **ceiling
on program quality**, and every omission lowers it.

### 005a — prompt v5: the natives

[prompt v5](corpus-prompts/v5.md) adds a **Built-in functions**
section ahead of the prelude, generated from the `NativeFn` table in
`stitch/src/natives.rs` so names *and arities* are accurate rather than guessed.
It also states that the two lists together are complete, so an unlisted function
genuinely does not exist — and that a listed one should not be avoided.

Worth automating later: the natives table is a literal array in one Rust file, so
the prompt section can be generated at build time and cannot drift. Same argument
as embedding `prelude.st` verbatim rather than paraphrasing it — the prompt should
be *derived* from the language, never maintained alongside it.

#### Still open

Four minutes for 25 lines. The tail of the trace is heavily repetitive
("One more check on…", "Wait, I should check…") without adding information — a
mild version of 001's checking loop. Thinking is buying design quality
(`sameRoom` in 003, the prelude discipline here) and paying for it in a way that
would be unaffordable at 500k tokens.

### 006 — qwen3.5-4b, prompt v4, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, thinking on, ~3.5 minutes |
| Prompt | **v4** — v3 with the must-use-identifiers line removed. **v5's natives section was not in play.** |
| Result | 1 type, 3 functions, 5 tests |
| Verdict | expected **parse ✓ · type ?** · logically wrong, **and its own tests do not catch it** |

**The axis removal is validated.** Every name is meaningful: `overlaps`,
`findFirstOverlap`, `hasConflicts`. Compare 005's `birch` and `cedar`. Removing
the words cost nothing and recovered the naming.

**005's self-comparison bug is fixed**, and deliberately — the trace reasons about
whether an element can conflict with itself and solves it structurally:

```
let rest = removeAt(bookings, 0)
let conflicts = find(rest, (x) -> overlaps(b1, x))
```

#### The new bug, and why it matters more than 005's

`findFirstOverlap` compares **only the head against the tail**. For
`[a, b, c]` where `b` and `c` overlap but `a` overlaps neither, it returns `None`
— and `hasConflicts`, which is documented as "check if the list contains any
overlaps", inherits the lie.

**Its own tests do not catch this.** The five tests cover two-element lists and
the empty list; none has three elements with the conflict away from the head. The
suite passes on broken code.

That is a sharp refinement of the 005a claim that self-written tests are a free
semantic oracle:

> **Self-written tests catch inconsistency between the code and the model's own
> intent. They do not catch gaps in that intent.**

005 wrote a test its code failed — a genuine catch. 006 wrote code and tests from
the *same* wrong mental model, so they agree with each other and both are wrong.
The oracle is real but partial, and its blind spot is exactly the case where the
model misunderstood the problem — which is the case that matters most.

**This is the argument for scoring suites by mutants killed rather than by
passing** ([corpus-mvp.md](corpus-mvp.md) §7). A mutation that swaps
`findFirstOverlap`'s tail-scan for a full pairwise scan would survive this suite,
which is precisely the signal that the suite is thin.

#### Two notes on the reference boundary

**Tuples were invented, correctly.** `Maybe<(Booking, Booking)>` and
`Some((b1, x))` appear nowhere in the prompt, and tuples do exist — verified in
`ast.rs`, which has `Type::Tuple`, `Expr::Tuple` and `Pattern::Tuple`. So the
Findings 003 claim needs narrowing: the model stays strictly inside the reference
for **library surface** (it will not call a function it has not seen) but freely
extrapolates **syntax**. That asymmetry is sensible — library membership is a fact
it cannot derive, whereas syntax is a pattern it can.

**`removeAt(bookings, 0)` is called bare**, and the trace spends real time
agonising over whether it exists ("`drop` is NOT explicitly listed… I must
implement it or avoid it"). It is `List.removeAt`, module-qualified, arity 2 — so
the bare call may not resolve. This is precisely the gap v5 closes, and 006 is
the last candidate that should hit it.

Minor: `ext prod Booking(room: Int, ext start: Int, ext end: Int)` exports
`start` and `end` but not `room`, with no apparent reason.

#### Prompt-version accounting

**v5 is still untested.** 006 ran v4. The next candidate should be the first to
see the natives list, and the specific things to watch are whether `drop`,
`sort`, `zip` and `List.removeAt` get used without deliberation, and whether the
saved reasoning budget shows up as a shorter trace.

### 007 — qwen3.5-4b, prompt v5, recipe #69

| | |
|---|---|
| Model | qwen3.5-4b, thinking on, ~3.5 minutes |
| Result | 2 types, 3 functions, 7 tests |
| Verdict | expected **parse ✓ · type ✗** — a signature mismatch |

```
ext overlap(b1: Booking, b2: Booking) -> Bool = …
…
bookings |> fold(true, (avail, b) -> avail and not(overlap(b.time, time)))
                                                       ^^^^^^  ^^^^
```

`overlap` takes two `Booking`s; `isAvailable` calls it with two `Time`s.

**The trace shows precisely how.** An early draft had a second helper:

```
ext timesOverlap(t1: Time, t2: Time) -> Bool = t1.start < t2.end and t2.start < t1.end
```

While consolidating to fit the function budget, the model **deleted the helper and
did not update the call site**. A textbook refactoring artifact — and one no
amount of local reasoning catches, because each line is individually plausible.

#### Three candidates, three bug classes, three different rungs

| | Bug | Caught by |
|---|---|---|
| 005 | self-comparison — every booking conflicts with itself | its own tests (rung 3) |
| 006 | head-vs-tail only — misses conflicts away from the head | **nothing** — tests share the wrong intent |
| 007 | signature mismatch after a refactor | the type checker (rung 1) |

Each survives the rung below it. Parse alone would have accepted all three.
**This is the graded oracle ladder ([corpus-mvp.md](corpus-mvp.md) §3) paying for
itself on the first six candidates** — and 006 remains the argument that even
rung 3 needs mutation scoring rather than pass/fail, since a suite written from
the same misunderstanding as the code agrees with it.

#### The v3/v4 fixes are holding

- **No line counting.** 2 types, 3 functions — in range, no deliberation.
- **All names meaningful**: `overlap`, `isAvailable`, `tryBook`.
- **It self-corrected an `if`**: *"Wait, I used `if` in tryBook. I cannot use
  `if`."* — and replaced it with `match`. v3's counter-example appears to be
  converting a shipped error into a caught one.
- **The comment-length instruction worked**, explicitly: *"My comments are a bit
  long. I'll shorten them."*

#### v5's natives are still untested

The program needed only `fold`, so `sort`/`zip`/`drop`/`Str.*` went unexercised.
But the *negative* signal is real: **zero deliberation about what exists**, against
006's extended agonising over whether `drop` and `removeAt` were available. A
recipe that actually needs list surgery is required to test this properly.

#### New: the construct axis has the words axis's failure mode, milder

`tryBook` returns `Maybe<Bool>` — where `Some(true)` is the only inhabited
success case, so it carries no information a plain `Bool` would not. The trace
gives the reason outright:

> *"I haven't used `Maybe` yet… I should probably include a function that returns
> `Maybe<Booking>` or similar **to satisfy the requirement to use Maybe**."*

That is the same pathology that killed the must-use words: a required token
inserted for compliance rather than because the program wanted it. The difference
in severity matters, though — **a forced construct produces a design smell; a
forced name produces a lie.** `Maybe<Bool>` is redundant but honest;
`birch(b1, b2)` misdescribes its function.

Constructs also earn their place in a way words never did: the corpus genuinely
needs coverage of `sum`, `contract`, `uses` and the rest, and there is no other
axis that delivers it. So this is a **softening** candidate rather than a removal
one — "use these where they fit, and if one does not fit, leave it out", the
phrasing that at least prevented 002's deadlock. Whether that costs too much
construct coverage is an open question for the first real batch, where per-recipe
construct-hit rate is measurable.

### 008 — qwen 9b, **thinking OFF**, prompt v5, recipe #69

| | |
|---|---|
| Model | qwen 9b — first non-4B, first with thinking successfully disabled |
| Result | 2 types, 6 functions, 14 tests — far more ambitious than any 4B output |
| Verdict | expected **parse ✗** (`if`/`else`) |
| Companion | qwen 9b with thinking **on** did not finish in 4 minutes and was stopped |

#### The headline: thinking-off ships the errors thinking catches

```
ext reportConflicts(bookings: List<Booking>) -> Str = {
    let conflicts = findConflicts(bookings)
    if count(conflicts) == 0 {
        "No conflicts found."
    } else { … }
}
```

`if`/`else`, with braces, despite the system prompt and v3's two counter-examples.

**Candidate 007 made the identical mistake and caught it** — *"Wait, I used `if`
in tryBook. I cannot use `if`."* — because thinking gave it a self-correction
pass. 008 has no such pass, so the error ships.

That makes the trade concrete for the first time:

| | Thinking on (007, 4B) | Thinking off (008, 9B) |
|---|---|---|
| Cost | ~3.5–4 min/candidate | seconds |
| Ambition | 1 type, 3 functions | 2 types, 6 functions, 14 tests |
| Rule-violation errors | **caught and fixed** | **shipped** |
| Semantic errors | present anyway | present anyway |

**And the resolution is Increment 7.** The errors thinking catches here are
*syntactic* — `if`, `&&`, the things the model can recite the rule for and violate
anyway. Those are exactly what a grammar mask makes **unrepresentable**. So
constrained decoding substitutes for thinking on the precise class where thinking
earns its keep, at none of the cost.

> **Turn thinking off, add the mask, and you get the speed of 008 with better
> syntactic correctness than 007.** This reframes Increment 7 from a yield
> optimisation into the thing that makes thinking-off affordable — which is in turn
> what makes 500k tokens reachable at all. At ~4 min/candidate, thinking-on is
> arithmetically out of reach regardless of quality.

#### v5's natives are validated

`filter`, `concat`, `Str.join`, `any`, `count` all used **without deliberation** —
against 006's extended agonising over whether `drop` and `removeAt` existed. The
built-ins list is doing its job and the program's reach is visibly wider for it.

Not a complete fix: `head` was invented (the prelude has `first`), so the list
raises the ceiling without eliminating hallucination.

#### The semantic errors are worse, and none is mask-fixable

- **`findConflicts` builds a nested list.** `bookings |> map(b1 -> bookings |>
  map(b2 -> (b1, b2)))` is `List<List<(B, B)>>`; the following `filter` then
  destructures each element as a tuple. Needed `flatMap` — which *is* in v5's
  list and was not reached for.
- **`"Conflict: " + b1.id`** concatenates `Str` with `Int`. Per
  [../docs/language-design.md](../docs/language-design.md), `1 + "x"` is a type
  error rather than coercion. `toStr` is in v5's reference, explicitly with
  "NOT `x.toString()`", and went unused.
- **`.contains(…)` and `.unwrapOr(…)` method-style** on `Str`/`Maybe`; both are
  free functions.
- **A test asserts a substring the format string cannot produce**: the code emits
  `"Conflict: 1 (Alpha) overlaps with 2 (Beta)"`, the test expects
  `"Alpha overlaps with Beta"`. Rung 3 territory, and another instance of 006's
  pattern — code and tests written from one mental model, agreeing with each
  other and both wrong.
- `prod Bookings(List<Booking>)` — an unnamed field, and the type is never used.
- `use Maybe` — built-in; the import is at best a no-op.

So the division of labour is now visible in one candidate: **the mask handles the
syntactic class, the type checker (rung 1) handles the arity/coercion class, and
tests-plus-mutation (rung 3) handle the intent class.** Three rungs, three
distinct error populations, none subsuming another.

#### Size

6 functions — the ceiling of the small bucket's 2–6, plus 14 tests. It obeyed the
range and spent all of it, with no counting deliberation. Worth watching whether
thinking-off consistently runs to the top of the range; if so the buckets are
doing more work than they look.

### 009 — self-repair without a diagnostic

| | |
|---|---|
| Setup | 008's own broken program handed back with "correct the errors and output a new program that is otherwise the same" |
| Prompt | v5 reference + built-ins + prelude + exemplars, unchanged |
| Result | **2 errors fixed of ~9. Both syntactic. Nothing else touched.** |

**Fixed:**

- `prod Booking(…)` → `ext prod Booking(…)` — the missing export.
- `if count(conflicts) == 0 { … } else { … }` →
  `count(conflicts) == 0 => "No conflicts found." | { … }`.

**Missed, all of it:**

- `findConflicts` still builds `List<List<(B, B)>>` via nested `map` and then
  `filter`s it as if the elements were tuples — the program's central bug.
- `head` still hallucinated (the prelude has `first`).
- `unwrapOr(None)` still type-confused.
- `"Conflict: " + b1.id` still concatenates `Str` with `Int`.
- `.contains(…)` still method-style on `Str`.
- The test still asserts `"Alpha overlaps with Beta"`, which the format string
  cannot produce.
- `use Maybe` (built-in) and the unused, unnamed-field `prod Bookings(List<Booking>)`
  both untouched.

#### What this says

**Self-repair without a diagnostic fixes the syntactic class and nothing else** —
which is precisely the class Increment 7's grammar mask makes *unrepresentable in
the first place*. So on top of mask-plus-oracles, naive self-repair adds
approximately zero: it corrects what the mask already prevents and misses
everything the type checker and tests would catch.

That is the whole taxonomy again, from a third angle:

| Error class | Mask | Type checker | Tests+mutation | **Self-repair, no diagnostic** |
|---|---|---|---|---|
| syntactic (`if`, `&&`, missing `ext`) | ✓ | ✓ | — | **✓** |
| arity / coercion / hallucinated name | — | ✓ | — | **✗** |
| wrong intent | — | — | ✓ | **✗** |

**But the experiment omitted the load-bearing input.** The model was told
"there are errors" and asked to find them — i.e. asked to *be* the type checker,
which it is not. [kvetch-rl-design.md](../docs/kvetch-rl-design.md) §5 specifies
repair traces as *broken state → **the checker's complaint** → fix*, and this run
had no complaint in it.

So the correct reading is **not** "repair doesn't work". It is:

> **The diagnostic is the load-bearing part of a repair trace, not the model's
> introspection.**

That is a real and useful validation of the §5 design — it centred the checker's
complaint for exactly this reason — and it makes a sharp prediction: the same
setup *with* the error message attached should fix a large share of the seven
misses, because each one is something the checker can name precisely.

**The follow-up is two minutes once S4 exists**: run 008 through the checker,
paste the actual error text alongside the program, ask for the same repair, and
compare against this run. If diagnostics move it from 2/9 to most-of-9, the
repair-trace branch of the RL design is validated cheaply and early. If they do
not, that branch is in trouble and it is better to know now.

**One thing that went right and is worth recording:** the model introduced **no
new errors**. The output is otherwise byte-identical to the input. Repair is
conservative here, which is the necessary precondition for it being useful at
all — a repair pass that damages working code is worse than none.

### 010 — the checker lands, and overturns two findings

S4 is built: `stitch::gate::run(src) -> Outcome` (`stitch/src/gate.rs`, five RED
tests in `stitch/tests/gate.rs` first) plus `stitch/src/bin/check.rs`. The chain
matches `tests/canon.rs` exactly, so a candidate faces the gate the shipped
corpus already passes. `Outcome` keeps the death *stage* rather than collapsing
to a bool.

```
$ cargo run -p stitch --bin check -- plans/corpus-candidates/*.st

003.st: parse — unexpected character `&`
005.st: tests — 1 passed, failed: cedar allows disjoint
006.st: tests — 3 passed, failed: findFirstOverlap finds overlap, hasConflicts returns true
007.st: tests — 4 passed, failed: availability finds conflict, …
008.st: parse — expected binding name

0/5 accepted
```

**Predicted vs actual — I was wrong twice, and wrong about a third reason:**

| # | Predicted | Actual | |
|---|---|---|---|
| 003 | parse ✗ | parse ✗ (`&`) | ✓ |
| 005 | tests ✗ on "cedar allows disjoint" | exactly that | ✓ |
| 006 | tests **pass** on broken code | tests ✗ | ✗ — but see below |
| 007 | **type ✗** | type ✓, tests ✗ | ✗ |
| 008 | parse ✗ (`if`/`else`) | parse ✗ (`let (b1, b2) =`) | right verdict, wrong cause |

#### The big correction: the type rung caught nothing

**007 type-checks clean.** `overlap` takes two `Booking`s and `isAvailable` calls
it with two `Time`s — and the checker does not object. Stitch's typing is
*gradual*; the diagnostics are advisory, and this class slips through.

So Findings 007's "three bug classes, three different rungs" is **wrong**. The
actual distribution over five candidates:

| Rung | Caught |
|---|---|
| parse | 003, 008 |
| **type** | **none** |
| tests | 005, 006, 007 |

**Tests are doing all of the semantic work.** That makes requiring a test block in
every program load-bearing rather than decorative — without them, three of these
five would have passed the gate — and it means the funnel's `type` stage may be
close to empty in practice. Worth measuring rather than assuming, but the
provisional read is that rung 1 is much weaker than
[corpus-mvp.md](corpus-mvp.md) §3 implies.

#### 006 is the mutation-scoring argument, now verified

006's raw failure was a **bare `removeAt`** — it is `List.removeAt`, and the bare
name does not resolve (checked directly). That is a different bug from the one I
attributed the failure to.

Patching only that one name:

```
$ cargo run -p stitch --bin check -- 006-patched.st
006-patched.st: ok — 5 tests passed
```

**It passes the entire gate — parse, type, and all five of its own tests — while
still comparing only the head against the tail.** A three-element list with the
conflict away from the head is silently reported as conflict-free, and
`hasConflicts` says so.

The Findings 006 claim is therefore **confirmed rather than refuted**, and now on
evidence: a suite written from the same misunderstanding as the code agrees with
it, and the whole ladder waves it through. This is the concrete case for scoring
suites by **mutants killed** ([corpus-mvp.md](corpus-mvp.md) §7) — pass/fail
cannot see it, by construction.

#### Two language facts the candidates found

- **`let (b1, b2) = pair` does not parse.** `Pattern::Tuple` exists and works in
  `match`, but not in a `let` binding. 008 died here at line 63 — the `if`/`else`
  I blamed is at line ~140 and was never reached. The model reached for
  destructuring naturally; whether Stitch should support it is a language
  question, not a prompt one.
- **Bare `removeAt` does not resolve** — module-qualified only. v5's built-ins
  list shows `List.removeAt(xs, i)` correctly, and 006 predates it.

#### What this changes

The checker paid for itself immediately: **two findings overturned, one promoted
from assertion to evidence, and two language facts established** — all from five
programs that already existed. Every verdict before this entry was an eyeball
estimate, and the error rate on those was 40%.

Nothing further should be tuned against my reading of a program when
`cargo run -p stitch --bin check` is one command.

### 011 — qwen3.5-27b, thinking on, prompt v5 — **the first accepted candidate**

| | |
|---|---|
| Model | qwen3.5-27b (GGUF), thinking on — **no off-switch on this build** |
| Result | 2 types, 2 functions, 4 tests |
| Verdict | **`ok — 4 tests passed`** — checked, not estimated |
| Cost | **~9 minutes of reasoning** |

```
$ cargo run -p stitch --bin check -- plans/corpus-candidates/011.st
plans/corpus-candidates/011.st: ok — 4 tests passed
```

**And it is not merely gate-passing, it is correct.** The other candidates all
modelled the problem as *find conflicts within a list*, which is what produced
005's self-comparison bug and 006's head-vs-tail bug. This one models it as
*does a candidate conflict with an existing schedule*:

```
ext findConflict(schedule: List<Booking>, candidate: Booking) -> Maybe<Booking> =
    schedule |> find(b -> overlaps(candidate, b))
```

The candidate is not a member of the schedule, so self-comparison cannot arise;
`find` scans the whole list, so nothing is missed. **A better problem model
dissolved both bug classes rather than avoiding them.**

Three things from the trace worth keeping:

- **The prelude was used as an authority to settle a rule.** It worried that
  "booleans are the words `and`/`or`/`not`" might forbid the literals `true` and
  `false`, checked, found `fold(xs, false, …)` in the prelude, and concluded the
  rule was about operators. Exactly what including `prelude.st` verbatim was for.
- **It considered `use Maybe` and rejected it** as built-in. 008 (9B) got that
  wrong and imported it.
- **Comments were revised toward "why" deliberately** — it rejected
  `// Check if two bookings collide.` as "what" and replaced it with
  *"Exclusive use requires non-overlapping intervals; this checks the
  intersection condition."*

#### The model comparison, finally with data

| Model | Quant | Thinking | Verdict | Cost |
|---|---|---|---|---|
| qwen3.5-4b | GGUF | on | tests ✗ (007) | ~3.5 min |
| qwen3.5-9b | MLX | **off** | parse ✗ (008) | seconds |
| qwen3.5-27b | GGUF | on | **ok ✓** (011) | **~9 min** |

Read it carefully, because the confounds are severe: **n = 1 per cell**, thinking
is **confounded with size** (only the 9B build had a working off-switch), and the
9B was MLX where the others were GGUF — different quantiser and runtime, which is
also the likely explanation for why only it exposed the toggle.

What survives the confounds: **27B clears the gate and 4B does not**, and that is
the first evidence on the question the spike exists to answer.

#### But 9 minutes is unusable, and that is now the blocking question

At ~9 min/candidate, 500k validated tokens is roughly **100+ hours** even at a
generous yield. 27B-with-thinking is not a pipeline, it is a demonstration.

So the decisive experiment is **27B with thinking off** — and the GGUF build has
no toggle. The 9B/MLX observation is the lead worth following: **try an MLX build
of the 27B**, since that is the one stack that exposed the switch. Failing that,
the template edit from S1.

The three outcomes and what each means:

- **27B-off clears the gate too** → that is the pipeline. Build the harness around
  it and the spike is answered.
- **27B-off drops to 008-like syntactic failures** → those are precisely the class
  Increment 7's mask makes unrepresentable, so build the mask and re-test before
  concluding anything.
- **27B-off degrades semantically** → thinking is buying real reasoning at this
  scale, and the corpus needs a slower, smaller run or a different approach.

Only the third is bad news, and none of the three can be guessed at.

---

## Not doing

**Building the runner.** The pull is real and it is a trap: sixty pastes is an
hour, the runner is a day, and the runner cannot answer anything the pastes
cannot. Building it after means building it against known numbers instead of
guesses.

**Tuning the prompt mid-spike.** Note what fails, finish the sixty, then write
prompt v2 once. Editing between candidates makes the sixty incomparable and
destroys the only measurement this increment exists to produce.

**Judging the recipes.** A program that fails here is data about the model or the
prompt. The axes are not on trial in this increment.
