# batch10: three 10-candidate pilots before committing the run

Same 10 recipes each time (`batch10.toml` rows 1–10, all first-pass `small`),
`qwen/qwen3.5-9b`, temp 0.7 / top_p 0.8 / top_k 20 / max_tokens 4096,
`--correct 8`. Ten candidates is enough to check a mechanism and **not** enough
to measure a yield — see "the variance" below, which is the most useful thing
these runs produced.

| | pilot 1 | pilot 2 | pilot 3 |
|---|---|---|---|
| change under test | recipes + splice fix | + prompt change | + byte cap, prompt reverted |
| funnel | parse 3 · type 1 · tests 6 | parse 2 · type 1 · tests 6 · ok 1 | parse 1 · tests 8 · **long 1** |
| median `.st` bytes | 4926 | 4436 | 4427 |
| median model tokens | 1461 | 1335 | 1321 |
| mean `.st` bytes | 7006 | 9546 | **5282** |
| max `.st` bytes | 26817 | 35657 | **12001** |
| comment bytes | 60.0% | 72.6% | — |
| wall clock | 11.3 min | 12.5 min | 7.1 min |

Pooling the two runs that share a prompt (1 and 3, n=20): **15/20 = 75%
parse-clean against batch9's 55%**, at 55 s/candidate → ~15 h for 1000. Both
figures are suggestive rather than established, and every one of the 20 was a
`small` first-pass recipe; the medium/large half of the sheet is untested and
will be slower.

## The splice fix works, and this is not a sample-size claim

Across all 30 pilot candidates: **0 glue-signature parse errors** against
batch9's 126, and 0 keyword+identifier fusions in the saved files against
batch9's 501. The join itself still happens at the same rate — 45–50% of every
pilot's rewinds discarded leading whitespace whose replacement had none, exactly
batch9's 49% — so each one is a repair that would have been a fusion before.
That is a mechanism check, not a statistic: the population is the same, the
outcome is different.

## The variance is the finding

Three runs of the *same ten recipes* at the same settings:

| recipe | pilot 1 | pilot 2 | pilot 3 |
|---|---|---|---|
| warehouse bin allocation | 9542 tok, parse | 1821 tok, tests | 2020 tok, parse |
| spare-parts bin | 1180 tok, type | 11212 tok, parse | 751 tok, tests |
| tool crib checkout | 1477 tok, tests | 7169 tok, parse | 1228 tok, tests |
| shipping container stowage | 5071 tok, parse | 1331 tok, tests | 1415 tok, tests |

Six- to ten-fold swings, in both directions, on identical input. **Whether the
model falls into narration mode is a coin flip, not a prompt-responsive
behaviour.** Any A/B at n=10 is measuring that coin, which is why pilot 2's
verdict below is "no signal" rather than "worse".

## Pilot 2: a failed prompt experiment, reverted

batch9's Finding "half the training budget went on English prose" got worse
under batch10's smaller programs — pilot 1 was 60% comment bytes against
batch9's 47.8%, because the code shrank and the prose did not. With
`enable_thinking: false` the model has nowhere to plan but the file; pilot 1's
worst candidate was 642 lines of which 500 were comment-only, reasoning about
bin-packing strategy in 4–13 line blocks.

The intervention: forbid planning comments, and offer a place to put them
instead — plain prose before the fence, which `extract` already discards.

It failed on its own stated prediction. Comment fraction went **up**, 60.0% →
72.6%, and 9 of 10 responses opened straight into the fence without using the
hatch. The prompt was reverted to batch9's wording. Kept from the experiment:
one test pinning that pre-fence prose never reaches the program — that part was
real, the assumed behaviour change was not.

## The byte cap

`--max-bytes` (default 12000, 0 = off) bounds the **accumulated** program.
Nothing did before: `max_tokens` bounds one completion and `run_once_corrected`
issues many, so a repeatedly-rewound candidate grows without limit — batch9's
worst saved file is 197 KB.

Sized off batch9's 973 files rather than guessed:

| cap | fires on | of those, parse | survivors lost | wall clock saved |
|---|---|---|---|---|
| 10000 | 35% | 73% | 94 | ~19% |
| **12000** | **25%** | **80%** | **48 (5%)** | **~14%** |
| 14000 | 17% | 85% | 26 | ~11% |
| 16000 | 14% | 88% | 16 | ~9% |

Comment fraction was tried first and rejected: it predicts well *after the fact*
(≥70% comment → 89% parse death, n=102) but a guard only sees a prefix, and on
prefixes it is barely better than the base rate (fires on 32% of files at
N=1500/T=70% for 52% parse death against 45% overall). Files open with
legitimate header comments; narration builds later.

A capped candidate is recorded as stage **`long`**, never as a parse death — it
is a fact about the harness, not about Stitch, and filing it under `parse` would
inflate the number the whole corpus strategy is judged on.

## Also fixed

- `abandoned` reaches the manifest. It earned itself immediately: pilot 1 had
  2/10 where the guard gave up, indistinguishable from clean parse deaths in
  batch9's records.
- `correct=` and `max_bytes=` are in the manifest header. batch9 recorded every
  sampling knob and neither of these, so its guard budget is unrecoverable and
  no later run is strictly comparable to it.

## Open

- **Ambiguous clauses.** `spare-parts bin`'s "reorder points triggered by
  consumption rate" was read as *geometry* — `prod Point(x, y)` — by 6 of
  batch9's 10 attempts and by pilot 2 and 3. Inventory jargon that parses as a
  verb phrase. Fixed in batch10 only (batch9 is frozen). A vocabulary-overlap
  heuristic did **not** find any siblings, because the geometry programs still
  say "parts" — so the class needs an eye, not a script.
- **`mut` fields.** Two pilot candidates wrote `prod Point(mut x: Int, …)` and
  died on `unexpected token: Mut`. Possibly a `reference.md` gap.
- The second-pass (medium/large) crossings have never been generated.
