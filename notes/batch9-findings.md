# batch9: what a 1000-candidate generation run taught us

Run: `qwen/qwen3.5-9b`, temp 0.7 / top_p 0.8 / top_k 20 / presence 0 / max_tokens 4096,
100 recipes × 10, guard on. Crashed at 973/1000 — the uncommitted incremental
`write_manifest` in `xtask-cram/src/generate.rs` is the only reason the verdicts survived.
**Commit that.**

23.5 hours wall clock, 3.09M model output tokens, 36.6 tok/s.

## The funnel

| stage | n | share |
|---|---|---|
| parse | 437 | 45% |
| tests (parsed + typed, some test failed) | 391 | 40% |
| type | 135 | 14% |
| ok (every test passed) | 10 | 1% |

batch5 (50 candidates, same model and settings) was parse 36% / tests 56% / type 6% / ok 2%.
**batch9 is worse**, and the reason is length, not the model.

## Finding 1 — parse death is a function of program length

Parse-death rate by decile of saved `.st` size:

| decile | ≤ bytes | parse | mean corrections |
|---|---|---|---|
| 1 | 5105 | 16% | 12.9 |
| 2 | 6238 | 22% | 17.0 |
| 3 | 7026 | 31% | 21.0 |
| 4 | 7777 | 34% | 24.6 |
| 5 | 8464 | 27% | 25.0 |
| 6 | 9334 | 39% | 26.7 |
| 7 | 10846 | 57% | 31.6 |
| 8 | 13068 | 56% | 33.2 |
| 9 | 17968 | 74% | 32.2 |
| 10 | 197373 | 92% | 37.5 |

Monotone, 16% → 92%. batch5's programs were half the size (1231 vs 2701 median model
tokens) and took 5.0 corrections each; batch9's take 26.2. The guard's per-rewind damage
compounds, so length is the independent variable that sets the yield.

## Finding 2 — the rewind eats a space

25 482 corrections. 69% discarded text that *started* with whitespace; only 20% of the
replacements start with whitespace. **49% of all corrections are a whitespace-to-
non-whitespace join.** The top pairs are the model re-emitting the same token minus its
separator:

```
663  ' if'    -> 'if'
482  ' let'   -> 'let'
349  ' ='     -> '='
302  'ext'    -> 'ext'
262  ' }'     -> '}'
213  '   if'  -> '  '        <- 3 spaces of indent become 2
171  '       []' -> '      ' <- 7 become 6
```

Cause: `run_once_corrected` resumes by sending the kept prefix as a trailing `assistant`
message. The prefill boundary loses one separator — either the chat template strips it or
the model will not open a continuation with a leading space. Either way, resuming a prefix
that ends mid-line reliably deletes one character of whitespace.

~2970 of those joins survive verbatim into the saved `.st` (~3 per file, present in 97% of
files). Most are indentation loss; a minority are real identifier glue (`ext` + `freeSpace`
→ `extfreeSpace`, `let` + `sum` → `letsum`), which is exactly the parse-error population:
83 × `unexpected token: If`, 18 × `unexpected token: Let`, 19 × `expected '(' after
function name`, 6 × `expected a declaration, found Ext`.

**Fix**: after a rewind, if the discarded text began with whitespace and the replacement
does not, re-insert the separator before splicing. Cheap, local, and it should move the
biggest single block of parse deaths.

## Finding 3 — smaller pathologies

- **199/973 hit the 4096-token cap.** 129 of the 437 parse deaths do not end in `}`, i.e.
  they are truncated rather than wrong.
- **45 files (5%) are degenerate loops** — a ≥6-character line repeated ≥15×. Worst is
  `195.st`: `|> unwrap` 1024 times. 4 of the 5 worst are `parse`. These are poison as
  training data whatever else we decide.
- **`extra_blocks > 0` on only 4 candidates** and zero `<think>` blocks — the prompt and
  `enable_thinking: false` are both working.

## Finding 4 — the recipe expansion added domains Stitch cannot express

Per-domain, 10 attempts each. Best: `soil moisture probe` 9/10 reaching tests,
`warehouse bin allocation` 8/10, `museum exhibit rotation` 8/10, `bird feeder log` 8/10.
Worst, all 0/10: `sudoku grid` (10/10 parse), `bowling scorecard` (10/10 parse),
`orienteering control points` (10/10 parse), `go territory scoring`, `darts scoring`,
`cribbage board`, `playlist shuffling`, `woodworking cut list`.

The dead domains all want indexed iteration over a grid or a running state machine — which
in a language with no loop keywords and no `xs[i]` becomes long recursive fold chains, i.e.
the long programs Finding 1 says will die. This is the same signal the uncommitted
`reference.md` addition ("Collections have NO methods and NO indexing", "`use List` is
required") is responding to.

## Finding 5 — the gate stages other than parse are informative

- type deaths: 28 undeclared effects, 24+ type mismatches, 13+ bad operator applications.
  Effect declarations are the single most common semantic miss.
- test deaths: median 1 passed / 5 failed; **174 of 391 had zero tests pass**. The model
  writes tests it cannot satisfy more often than it writes a nearly-right program.

## Token count

Tokenized with `checkpoints/drivel-9.vocab` (1024 entries), a faithful Python port of
`kvetch_vocab::{pre_tokenize, encode}`.

| corpus | files | bytes | tokens |
|---|---|---|---|
| batch1–8 | 101 | 453 884 | 317 336 |
| batch9 | 973 | 10 451 977 | 6 845 676 |
| **all `corpora/batch*`** | **1074** | **10 905 861** | **7 163 012** |

batch9 by gate stage:

| stage | files | tokens |
|---|---|---|
| ok | 10 | 111 273 |
| tests | 391 | 2 050 646 |
| type | 135 | 767 520 |
| parse | 437 | 3 916 237 |
| **parses cleanly (ok+tests+type)** | **536** | **2 929 439** |

Caveat: 1.52 bytes/token is poor compression — `drivel-9.vocab` was trained on babble, not
on this. A vocab trained on batch9 at 4–8k entries would land nearer 3–4 bytes/token and
the token count would fall by roughly half. **The number is only meaningful against a
named vocab.**

## Blockers to training now

1. **There is no way to train on a batch directory.** `xtask cram` calls
   `load_corpus(programs, layout)`, which generates or reuses *babble*. `--corpus-dir`
   (or equivalent) does not exist. This is the one hard blocker.
2. **No frozen vocab.** Every run trains a fresh probe vocab, so losses are not comparable
   across runs. Before a real training run, train one vocab on batch9 and freeze it.
3. **Decide what to include.** The 437 parse-dead files are 57% of the corpus *by tokens*
   and are ungrammatical Stitch by construction. Excluding them still leaves 2.93M tokens —
   about 6× the 500k target. Recommendation: exclude parse deaths, exclude the 45 degenerate
   files, keep `type` and `tests` deaths (they parse, and wrong semantics is still valid
   syntax to learn from).
4. **Held-out contamination is already handled** — `cram_eval::corpus::NOT_CORPUS` excludes
   `corpora/`, so `examples/` stays clean.
5. **Size ceiling.** 2.9–6.8M tokens feeds `drivel` (1M params) comfortably and `quip`
   (3M params) with epochs. `cliche` (10M params, ~200M tokens at Chinchilla) is out of
   reach. At 36.6 tok/s, generating 20× more corpus is ~470 hours — generation throughput,
   not the trainer, is what caps the ladder.

## What to do before batch10

- Commit the incremental `write_manifest`.
- Repair the rewind splice (Finding 2).
- Cap program size in the prompt — the recipes should ask for ~150-line programs, not
  whatever the domain suggests. Finding 1 says that alone roughly doubles yield.
- Drop or rewrite the ~8 domains that need indexed grid iteration.
- Record `abandoned` in `CandidateRecord`; it is currently computed and thrown away, so
  "the guard gave up" is invisible in the manifest.
