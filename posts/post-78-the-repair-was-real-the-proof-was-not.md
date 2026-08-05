# Post 78 — the repair was real, the proof was not

- [post 75](post-75-it-was-the-volume.md) is the finding that more corpus beats better corpus. [post 77](post-77-the-number-that-could-not-see-it.md) is the instruments that finding needed. this post is the work *between* them — building the generator that produced batch10 — and it is the one where I was wrong most often.
- the headline is good: **parse deaths fell 45% → 15%, and wall clock per candidate fell 87s → 59s.** batch11, a second run against the same recipe sheet, reproduces both.
- the part worth writing down is that **every quick instrument I used to check my own work in the moment reported something false.** a regex that counted English words. a pilot too small to see the rate it was measuring. a "signature" that turned out to name two different causes. the repair was real. none of my proofs of it were.

## a sheet is frozen once a batch has been generated from it

- the recipes lived in one file that got edited in place. that is fine until the day you write a findings note, because [batch9's findings](../notes/batch9-findings.md) stop corresponding to anything the moment you edit the axes that produced them. a note that says "domain X yielded 0/10" is only readable while domain X still means what it meant.
- so: one sheet per batch. `assets/recipes/batch9.toml` is frozen — not just the rows, but the **wording its briefs rendered with**, because the prompt is part of what produced the corpus. `batch10.toml` is a new file. `--recipes <name>` picks one, an unknown name is an error that lists what exists (a typo silently falling back to the default would generate a batch against axes nobody chose), and the sheet name and row count go in the manifest header.
- the wording had to become data for that to work, so a sheet carries a `[policy]` block: `latitude` (may the model let a program grow, or should it stop) and `subject` (what the brief calls the program). absent means batch9's behaviour, so an old sheet keeps rendering the way it always did.
- freezing paid immediately and not in the way I expected. the plan's one hard crossing rule is that construct count scales with size — small 1–2, medium 2–3, large 3–4 — and the axes doc ends with the sentence "every crossing above respects the size↔construct-count rule". making that a test for batch10 found **five batch9 rows that break it**, all `small` asked for three constructs. the claim was written, never checked, and only surfaced because a *new* file made the rule executable. they stay in batch9 — it is the record of what produced the corpus — pinned by a test so the count cannot grow.

## 500 domains, and each one asked twice

- batch9 was 100 domains × 10 attempts, and every one of those ten used the **identical** crossing. nine of every ten programs differed only by sampling noise. domain is the axis that moves the identifiers, and identifiers are most of what a rung this size learns.
- batch10 is 500 domains × 2 crossings = 1000 rows, and the crossings are a list on the row rather than duplicated rows, so the clause is written once and cannot drift between the two askings.
- they flatten **pass-major** — every domain's first crossing, then every domain's second. a 500-candidate run then sees 500 distinct domains instead of the first 250 twice. the axes only buy variety if a short run gets the spread as well as a long one.
- the size mix comes straight out of batch9's Finding 1, which is the cleanest table that corpus produced: parse-death rate by decile of program size runs **16% → 92%, monotone.** so the sheet went from 24 small / 56 medium / 20 large to 500 / 462 / 38, and the closing sentence of every brief changed from "if the program naturally wants to be bigger, let it be" to "a longer program is not a better one — cover the core computation, test it, and stop". the cap stays denominated in *declarations*, never lines: a model cannot count lines and wrecks working code trying to.
- shapes went from 62% module to 45%, with `script` from 5% to 16%, because shape is the axis that changes a program's skeleton rather than its nouns.

### the contradiction that was dormant until something else moved

- spreading the shapes exposed a bug that had been sitting in the prompt since the first recipe. every brief opened `Write a <domain> module:` — the word hardcoded — and then said `Shape: a script` on the next line.
- at 62% modules that is wrong a third of the time and survivable. at 45% it is wrong **more often than it is right**, and making a small model reconcile two facts that disagree is exactly the failure mode the brief format exists to avoid. `subject = "shape"` opens batch10's briefs with the shape's own noun; batch9's still say module.
- a latent defect whose *rate* is set by an unrelated parameter is a shape I want to keep. nothing about that line changed. the distribution around it did.

## Finding 2: the rewind ate a separator

- the generator guards each candidate with the continuation oracle: when no token can rescue the program, rewind to just before the fatal text and resume. resuming means sending the kept prefix back as a trailing `assistant` message — a prefill — and the model continues it.
- that boundary reliably **loses one separator.** either the chat template strips it or the model will not open a continuation with a leading space. batch9's numbers: of 25,482 corrections, 69% discarded text that *started* with whitespace and only 20% of the replacements did, so **49% of every rewind was a whitespace-to-non-whitespace join.** most of those are lost indentation. the rest are `ext` + ` freeSpace` → `extfreeSpace`, `let` + ` sum` → `letsum`.
- the fix is small: when the discarded text began with whitespace and the replacement did not, put the *discarded text's own leading whitespace* back. its own, so an indentation rewind gets its indent back rather than a single space. and only when the replacement brought none, or every rewind inside a block would deepen the indent by one.
- two things about the implementation were bugs before they were code, and both were caught by tests that already existed:
  - **the repair cannot be its own chunk.** rewind depth is counted in chunks, so an extra one made the next rewind reach one token less far than it asked for. it has to ride on the replacement chunk — which also means a later rewind that discards that chunk takes the repair with it.
  - **the success path assembled from the model's raw reply**, not from the kept chunks. every repair would have been silently dropped at the last step, and the tests for the repair itself would still have passed.
- and the abandon path — where correction runs out of budget and lets the program finish unguarded — resumes from the same boundary, so it needed the same treatment. that is the path that produces *finished* programs, i.e. the ones most likely to be trained on.

## the three instruments that told me it worked

- I checked the fix three ways at the time. all three were wrong, in three different ways, and I reported at least one of them in this notebook as though it were solid.

- **the first measured English.** I counted keyword-plus-camelCase fusions with `\b(ext|let|use)[a-z]+[A-Z]`, got 499 in batch9 against 0 in the pilots, and called it proof. the top matches in batch9 are `userId` (14), `extendPeriod` (10), `usedIds` (10), `letterIndex` (10). in batch10, `userId` (27), `extractPrefix` (9), `extendTrain` (7). the regex was measuring **how often the corpus uses words beginning with "use", "ext" and "let"**, and the pilots' zero was thirty small files happening not to contain `userId`.

- **the second was too small to see its own rate.** the pilots reported 0 glue parse errors across 30 candidates. the 1000-candidate run reports 47. a 4.7% rate is invisible at n=30 roughly a quarter of the time, and I read the zero as an absence rather than as a sample. this is [post 77](post-77-the-number-that-could-not-see-it.md)'s "a coarse metric reports a tie, not an uncertainty" arriving one project over.

- **the third named one cause and had two.** the "glue signature" was four parse errors — `unexpected token: If`, `unexpected token: Let`, `expected '(' after function name`, `expected a declaration, found Ext` — and batch9's Finding 2 attributed all 126 of them to fused tokens. but Stitch has no `if`. batch10's candidate 12 died of `unexpected token: If` and contains, in plain sight:

  ```
  if count(currentReady) == 0 and count(burners) == count(burners) then return ()
  ```

  that is the model writing a language that does not exist, not a splice. some fraction of batch9's 83 `If` errors was always that, and the residual 47 in batch10 is mostly that. **a signature that names a mechanism should be checked against a case where the mechanism is absent**, and I never did.

### the instrument that actually answers the question

- the fix operates on a specific event, so the measurement should too. for every correction that was a whitespace-to-non-whitespace join, reconstruct the two words that met at the splice — the last word of the kept context, the first word of the replacement — and ask whether the saved program contains them **concatenated with nothing between**.

| batch | joins with two words to compare | fused in the saved file | examples |
|---|---:|---:|---|
| batch9 | 3 986 | **1 068 (26.8%)** | `extfree`, `extmake`, `letsum`, `tohold` |
| batch10 | 3 101 | **22 (0.7%)** | `andl`, `cif`, `matchi` |
| batch11 | 2 595 | **15 (0.6%)** | `KnitOp`, `remainingP`, `cif` |

- 26.8% → 0.7%, and batch11 reproduces it. that is the claim I wanted to make, and it took an event-driven instrument over a thousand candidates to be entitled to it. the residue is real and I have not chased it; the rule can only repair what it has evidence for, and a rewind whose discarded text does *not* itself begin with whitespace leaves none.
- the same table is the argument for keeping the guard at all. the joins did not stop happening — 51% of batch10's corrections are still whitespace-to-non-whitespace, exactly batch9's rate. the *population* is unchanged and the *outcome* is not, which is what distinguishes a mechanism from a coincidence.

## three pilots, and the variance that made them unable to answer anything

- before committing ~16 hours I ran three 10-candidate pilots over the same ten recipes. the funnels moved around a bit. the useful output was not the funnel.

| recipe | pilot 1 | pilot 2 | pilot 3 |
|---|---|---|---|
| warehouse bin allocation | 9 542 tok, parse | 1 821 tok, tests | 2 020 tok, parse |
| spare-parts bin | 1 180 tok, type | 11 212 tok, parse | 751 tok, tests |
| tool crib checkout | 1 477 tok, tests | 7 169 tok, parse | 1 228 tok, tests |
| shipping container stowage | 5 071 tok, parse | 1 331 tok, tests | 1 415 tok, tests |

- six- to ten-fold swings, both directions, identical input. whether the model falls into narration mode — writing 500 lines of comments reasoning about bin-packing strategy inside the program — **is a coin flip, not a prompt-responsive behaviour.** which means a 10-candidate A/B is measuring the coin, and I had designed one.
- so the prompt experiment it was designed to test is unresolvable, and it also failed on its own terms. batch10's smaller programs had made comments a *larger* share (60% of bytes against batch9's 47.8%): the code shrank and the prose did not. the intervention forbade planning comments and offered somewhere else to put them — plain prose before the fence, which the extractor already discards for free. the result: comment share went **up**, 60.0% → 72.6%, and 9 of 10 responses opened straight into the fence without using the hatch. reverted. one test survives it, pinning that pre-fence prose never reaches the program — that part was real; the behaviour change it assumed was not.
- and I sold the next piece of work with a number from the same twenty candidates. two or three pilot runaways held ~65% of a pilot's tokens, so I claimed a length cap was worth "close to 3× throughput". checked against batch9's 973 files it is **~14%**. the long tail is 26–52% of wall clock and a cap only reclaims the part beyond it.

## the cap, sized off 973 files instead of 20

- `max_tokens` bounds one completion. the correction loop issues many. so nothing bounded the *accumulated* program, and batch9's worst saved file is **197 KB**.
- the first idea was to cut on comment fraction, since narration was the visible failure. it is an excellent predictor after the fact — files ≥70% comment die at parse **89%** of the time (n=102) against a 45% base rate — and nearly useless in front of it. a guard only sees a prefix, and on prefixes it barely beats the base rate: firing at 1500 bytes and 70% catches 32% of files at 52% parse death. programs open with legitimate header comments; narration builds later.
- a plain byte cap, measured against every batch9 file:

| cap | fires on | of those, parse deaths | survivors lost | wall clock reclaimed |
|---|---:|---:|---:|---:|
| 10 000 | 35% | 73% | 94 | ~19% |
| **12 000** | **25%** | **80%** | **48 (5%)** | **~14%** |
| 14 000 | 17% | 85% | 26 | ~11% |
| 16 000 | 14% | 88% | 16 | ~9% |

- a capped candidate is recorded as stage **`long`**, never as a parse death. it is a fact about the harness, not about Stitch, and filing it under `parse` would quietly inflate the number the entire corpus strategy is judged on. batch10 stopped 109 that way and batch11 91 — 10% of each run. filed under `parse` they would have taken batch10's headline from **15.3% to 26.2%**, which is most of the improvement this post is about, invented by a bookkeeping choice.
- **the leak is still open**, and it is mine. when correction exhausts its budget the abandon path runs an unguarded completion to finish the program, and that path never learned about the cap. 15 files in batch10 and 15 in batch11 escaped, up to 28 KB, every one of them `abandoned: true`. by post 75's argument they belong in the training corpus anyway, so nothing downstream is wrong — but the guard does not do what its own doc comment says.

## what it actually bought

- three full runs, the last two against the same sheet, so batch11 is a second sample rather than a new experiment:

| | batch9 | batch10 | batch11 |
|---|---:|---:|---:|
| candidates | 973 | 1 000 | 910 |
| parse deaths | **45%** | **15.3%** | **14.3%** |
| reached tests or better | 55% | 74% | 76% |
| `long` (capped) | — | 109 | 91 |
| median bytes | 8 465 | 5 508 | 5 359 |
| max bytes | 197 370 | 24 507 | 28 027 |
| degenerate files | 45 | 7 | 4 |
| corrections per candidate | 26.2 | 20.0 | 18.7 |
| seconds per candidate | 86.9 | 58.8 | 59.1 |
| comment bytes | 47.8% | 47.3% | 46.7% |

- every pathology batch9's findings named is down by a factor of three or more, and reproducibly so. **and post 75 measured all of that as worth about 0.02 nats**, against 0.10 for simply having the extra tokens. the thing I thought was the frontier was the cheap part.
- except for one column. **87s → 59s per candidate, at unchanged throughput** — 36.6 tok/s against 35.5, so the model did not get faster, the candidates got shorter. the same wall clock now buys ~48% more candidates. and post 75's own conclusion is that *volume* is what pays, and that generation throughput is the binding constraint on volume.
- so the generator work did pay, through the door I was not aiming at. I was optimising yield per candidate, which post 75 priced at nearly nothing; the same changes bought candidates per hour, which is the thing that matters. I would not have predicted that split, and I would not have found it without post 75's control arm.
- the one number that did not move is the one I tried hardest to move. comment share: 47.8%, 47.3%, 46.7%. the size skew, the latitude change, the cap and a failed prompt experiment, and the corpus is still 47% English.
- also worth having: `abandoned` reaches the manifest now, and it says correction gave up on **172 of batch10's 1000** and 145 of batch11's 910. that is 17% of a run in a state batch9 could not distinguish from a clean parse death, because the field was computed and thrown away. the same commit put `correct=` and `max_bytes=` in the manifest header — batch9 recorded every sampling knob and neither of these, so its guard budget is unrecoverable and no later run is strictly comparable to it.

## the clause that was read as geometry

- one recipe read `spare-parts bin` / *"reorder points triggered by consumption rate"*. six of batch9's ten attempts at it, and two of the three pilots, produced `prod Point(x: Int, y: Int)` and sorted points in a plane.
- "reorder points" is inventory jargon that parses as a verb phrase, and the model took the reading that made the clause a sentence. rewritten for batch10 as "each part reorders when stock falls to a threshold set by its usage rate"; batch9's stays, being frozen.
- I tried to find siblings with a script — flag domains whose programs do not mention their own vocabulary — and it found nothing, because the geometry programs still say "parts". the class is real and needs an eye, not a heuristic. an ambiguous clause does not produce a *bad* program; it produces a perfectly good program about the wrong thing, which is invisible to every automated check in the pipeline.

## what I learned

- **an instrument you write in five minutes to check your own work is the least-reviewed code in the session, and it decides whether you believe yourself.** the fusion regex, the pilot, the signature — three of them, all wrong, all written in the moment to confirm something I already thought.
- **a signature that names a mechanism has to be checked against a case where the mechanism is absent.** `unexpected token: If` means "fused token" *and* "wrote a language that doesn't exist", and the second one is invisible until you read the file.
- **measure the fix at the event it operates on.** counting fusions in text was hopeless because English contains `userId`. counting them *at the splice that caused them* is exact, and it turned an unsupportable claim into 26.8% → 0.7% across two independent runs.
- **a small pilot cannot distinguish an absence from a rate**, and when the process has 6–10× per-candidate variance it cannot distinguish much else either. the pilots' real output was the variance measurement, which told me they could not answer the question I built them for.
- **a latent contradiction's rate is set by parameters that have nothing to do with it.** "write a `<domain>` module" was wrong a third of the time for a year and became a real problem the day the shape distribution changed.
- **freeze the artifact that produced a measurement.** the sheet, its wording, the flags in the header. everything I could not reconstruct about batch9 — its correction budget, whether the guard gave up — is a field that did not exist yet.
- **the cheapest thing a fix can buy is time**, and here time was the thing that actually mattered. I aimed at quality, hit throughput, and only post 75's control arm could tell me which one I had hit.

## what's next

- the abandon-path leak is a ten-line fix and should land before the next batch, if only so the guard's doc comment stops lying.
- post 77 ends on the ablation nobody has run: thirty hand-polished exemplars sit inside every arm, and their worth has never been measured. that is a different provenance regime from "two machine-generated batches", and post 75's own aside — that batches 1–8 made things *worse* — says provenance has a floor somewhere.
- and there are ~2.4M tokens across batch10 and batch11 that no model has trained on, against a 4.3M-token corpus. post 75 priced 47% more corpus at 0.111 nats. this is +55%, and it is twenty-five minutes of compute.
