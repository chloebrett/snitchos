# Post 81 — what the total hid

- two arcs this session, and they turned out to have the same shape. one was making drivel faster on the VF2; the other was making Tab at the prompt stop misbehaving. both were a stack of things hiding under other things, where **fixing the top one was what made the next one visible**.
- the through-line is uncomfortable and worth stating first: every time I reasoned about where the cost was, I was wrong. every time I built a small thing that could check itself, it told me something I would not have guessed. that is [post 80](post-80-the-control-passed-twice.md)'s lesson again — *measure the discrimination somewhere cheap* — except this time the thing that needed measuring was my own ranked list.

## the ranking was a guess, and said so

- `notes/drivel-on-vf2-speedup-ideas.md` was written by reading the serving path end to end. it ranks a dozen levers by (win × cheapness), and its §0 opens by arguing that nothing below it should be attempted until someone measures. it even predicts its own failure: *"I expect at least one of those guesses to be wrong by an order of magnitude."*
- so the first thing built was the bench §0 asks for: `bench-serve`, which splits one completion request into `encode / prefill / decode / oracle / sample`. it needs the legality predicate inside a closure it owns, so it restates `handle_request`'s loop rather than calling it — and **a restatement that has drifted does not error, it agrees**. so it refuses to print a number until both loops serve byte-identical completions across every prefix and seed. verified to discriminate: changing the step-seed mixer from `^` to `+`, one operator, is caught on the first prefix and withholds the whole report.

| | cold Tab | warm Tab |
|---|---|---|
| transformer | 92–97% | 72–93% |
| grammar oracle | 2–5% | 4–11% |

- §2 of that document opens *"this is where I'd put money before measuring"*, and it loses by roughly an order of magnitude. the load-bearing error was arithmetic: §2a reasons from "up to ~12,000 parses per Tab", which assumes the sampler burns all 17 refusals every token. measured, **the number of legality verdicts equals the token count in almost every cell** — the model's first proposal is legal essentially every time.
- one item the measurement *promoted*: `vocab.encode` is 1–2% of a cold Tab and **15% of a warm long one**. once prefill is gone it is the third-largest bucket.

## the lever that was already learned, and didn't transfer

- §3b looked like a gift. `rope_one` computes 512 transcendental triples per token for 16 distinct angles — 32× redundant — and the *training* path already fixed exactly this: post 73 records `RotationTable` as the single largest win of the whole throughput sweep, above every matmul.
- hoisted it. bit-identical by construction, same expressions in the same order. one run showed ~10%.
- then the A/B, 3 runs × 4 prefixes: **988 → 974 µs/token. 1.4%.** the ~10% was a single-run artifact and I should not have quoted it before the A/B.
- why the analogy misled: post 73's win came from amortising `powf` across *positions × batch rows*, thousands of them. at generation there is one position, so the redundancy is 32× over a small absolute cost — 32× of ~2% is still ~2%. **the section was true about the code and false about the arithmetic.**
- kept it anyway; it is strictly less work and a small diff. but it is not the lever, and two-for-two meant the remaining §3 ordering had to be measured rather than read.

## the total hid the regression

- so: `bench-forward`, a `Gemm` decorator that delegates to `NaiveGemm` and keeps a stopwatch. `Model::step` already takes its multiply as a parameter and routes >95% of the forward pass through it, so this needed **no production change at all** — the seam was already there for testability.

```
matmul, all of it        94.3% prefill   92.5% decode
  ffn down (tb: false)   27.8%
  ffn up   (tb: false)   25.9%
  proj q/k/v/o (false)   23.7%
  logits   (tb: true)    14.9%
everything else           5.7%
```

- that settled §3 in one run. §3c (head-major KV) and §3d (the ~116 allocations per token) are *both* bounded by the 5.7% residual — everything the decorator cannot see. §3a is the job, and specifically the 77% that passes `transpose_b: false` and strides `b` by `n`, taking one useful float per cache line.
- lifted `cram::blocked_band` into `kvetch-model` as `RowGemm` so the trainer and the server share one kernel. results per bucket, warm-up equalised:

| bucket | `transpose_b` | naive | row | |
|---|---|---|---|---|
| ffn up | false | 36.56 ms | 1.79 | **20×** |
| proj q/k/v/o | false | 35.04 | 1.74 | **20×** |
| ffn down | false | 29.85 | 1.67 | **18×** |
| logits | true | 11.22 | 11.94 | — |
| **whole prefill** | | **120.8** | **21.5** | **5.6×** |

- **the first version made `logits` 1.7× slower** — 10.75 → 18.12 ms — because row-accumulation strides `b` by `k` *and* read-modify-writes the whole 2048-entry output row `k` times, where the naive per-element dot product was already reading contiguously. the kernel now picks whichever traversal makes `b` sequential.
- and the total would never have shown it. prefill still improved 2.9× with that regression inside it. **a net win is a place regressions hide**, and the only reason this one surfaced is that `bench-forward` reports per shape rather than per run.
- second trap, caught by two tools disagreeing 2× about the same prefill: the A/B ran both kernels back to back, so the second found 4.2 MB of weights already in cache. there is now a discarded warm-up per kernel. **the measurement was rigged in favour of the change I wanted.**

## then the board, and four bugs in a stack

- took it to the VF2 and Tab misbehaved. what came back looked like the grammar failing. it wasn't — it was four separate defects, each only visible once the one above it was gone.

**1. the menu was lying.** a full buffer and "I have no opinion" were both `Status::Ok, written: 0`. the completer reads that single shape as "nothing to suggest" and prints a *token menu*, which reads as the text being rejected. now `Status::NoRoom`, appended never renumbered, and the server short-circuits before spending a forward pass to discover it.

**2. the completer livelocked.** a line ending inside a comment made the grammar force the same token forever — a line filling with `(((((((`, one byte per press. the lexer skips the comment, so the appended byte never joins the token stream, the grammar state never advances, and the next press forces it again.

**3. the completion budget was being spent where the grammar is blind.** inside a comment every byte extends the buffer legally, so `viable` returns true for all 2048 candidates and the model's prior decides alone — and drivel's corpus is ~47% comments. one `//` handed the rest of the request to prose. the RED test showed it at its purest: `let x = 1////////`, eight slashes, every one legal.

**4. the buffer was two numbers.** the client offered 256, the server kept scratch for 512, coupled only by the server refusing anything larger. nothing named the coupling, so raising the client alone — the natural move — would have made *every request refused*, silently. one `COMPLETION_BUFFER = 4096` now, sized against real Stitch programs (`examples/stitch/` runs 3.5–15 KB), not against "a REPL line is short".

## the sentence that explains three of them

- **the completer decides what may come next from the token stream, and writes bytes at the end of the buffer. those are the same place only in code.**
- everywhere the lexer skips — a line comment, a nestable block comment, an unterminated string — the two diverge, and each divergence produces a differently-shaped bug from the same root. a forced token becomes a livelock. a model suggestion becomes unconstrained prose. neither looks like the other.
- so the primitive is `lexer::trailing_region`: which region the end of a buffer falls in, and what closes it. built from the lexer's own scan rather than a second copy of the string and nesting rules, with a cross-check test asserting that `Code` *operationally* means an appended token really reaches the lexer.
- `skip_comment` now reports whether it closed, because **running out of input is not the same as closing on the last byte** and only the scan can tell them apart. same for `lex_string`, later, for the same reason.
- one detail worth keeping: the first fix closed the comment and stopped, spending a whole keypress on a newline — about a third of them when the model is writing comments. the closer is a *prefix* to the answer, not the answer.
- and one test I wrote and then deleted. "the rotation angle does not depend on which head it is in" passed, and perturbing `rope_one` showed it failed on changes the behavioural oracle correctly ignored: a uniform or per-head angle offset is RoPE's defining symmetry, invisible in the output because attention depends only on the *relative* rotation. it was pinning an implementation property with no behavioural consequence. what survives is the one that pins a real contract — the two rotation paths must agree bit for bit, because a one-ULP logit difference can cross a sampling boundary and serve a different token.

## the question that made me delete a proposal

- with all that fixed the loop still stalled, and I proposed a grammar fallback: when the model gives up, emit a legal token the grammar can spell. then: *"I thought we were doing grammar constrained decoding? why can't we just take the model's rankings, then filter to only those that are legal?"*
- we were. `draw` strikes each refused candidate and redraws from what remains, which `sample.rs` itself calls *"identical in outcome to masking everything"*. the only difference from a full mask was a give-up threshold of **16**, and its rationale was written down and explicitly a quality judgement: *"the tail is worthless… the honest answer is to stop the completion rather than keep digging for the least-bad legal token."*
- sound for a six-token nudge, where a bad suggestion is worse than none. **backwards for building a program**, where the least-bad legal token beats a line that cannot grow.

| cap | presses before stalling |
|---|---|
| 16 | 29 |
| 64 | never (80/80) |
| 2048 (a full mask) | never (80/80) |

- 64, re-documented as a **latency budget rather than a quality heuristic**, with the table in the doc comment so nobody re-derives it. the bitter detail: at the stall the menu listed 27 legal classes, one of which was almost certainly the `)` that closes the expression the model had opened. the grammar knew the way out; the sampler stopped looking seventeen candidates in.
- my fallback would have papered over a threshold with a mechanism. the question was better than the proposal.

## the hole I had scoped out

- 80 presses, 2160 bytes, no stall. read the log and quality collapses hard around press 46, and the buffer says why:

```
Ok() => Some(" (nct(zle(tail(knFail(stateay(te => None
```

- that `"` never closes. everything after it is inside a string literal, invisible to the grammar, unconstrained — and `trailing_region` returned `Code`, because `lex_string` breaks on `None | Some('"')` and cannot distinguish closing from running out.
- **I had explicitly scoped strings out**, in writing, as "a plausible sibling but not observed". one 80-press run observed it. the tell had been in the log the whole time: press 73 emitted `//` at the end of a completion, which the comment check should have rejected outright, and press 74 still reported no open region. it was never a comment. it was text inside a string.
- the fix is the same shape as the comment one, which is the argument for having named the region rather than special-cased `//`.
- what remains after it is not a hole: unbalanced `[` and `(` piling up. the grammar is *right* that an unclosed list is extendable. that is drivel being a 1M-parameter model with no sense of closing what it opens — §4d's "whole programs are a ballad-class ask", honestly.

## the three round-trips I cost

- between fixes, three separate cycles went to "is the board actually running this?" — and each time I reached for the interesting explanation before the boring one.
- the worst: a stall I attributed to the refusal cap, confidently, having measured that cap on the host. counting the pasted buffer took one command and said **268 bytes** — just past a 256-byte client buffer that bails *before sending a request at all*, so there is no status to report and "line full" and "no suggestion" arrive as the same `None`.
- then I advised reflashing, which was wrong for this setup — the board TFTPs the image from the Mac on every boot, and I should have read `notes/uboot.md` before giving process advice about a process I hadn't checked. the actual suspect is `--tftp-root="$(pwd)"`, fixed at whatever directory dnsmasq started in: a stale *server* rather than a stale image, same symptom, different fix.
- that link is still unverified. `tftpboot` prints `Bytes transferred =`, and comparing it to `wc -c snitchos.img` settles it in one second — a check the note does not contain and should.

## what the session actually produced

- three instruments, and each found something reasoning had backwards. `bench-serve` (request-level split), `bench-forward` (per-matmul-shape, zero production change), `repl-tabs` (the Tab-on-a-growing-line loop nothing else exercised). every one of them refuses to report until it has shown it walks the same path production does — and that guard fired twice for real, not decoratively.
- the forward pass is **~5.6× cheaper on prefill and 2–4× on decode**, byte-identical: `stitch-drivel-completes` passes throughout, because it recomputes the expected completion from the same code rather than pinning stale bytes.
- Tab at the prompt goes from stalling at press 29 to **13 bytes → 2160 bytes over 80 presses**, producing nested matches, `Option` handling, typed parameters. incoherent, as commissioned.
- still open, and worth writing down rather than remembering: `Platform::complete` returns `Option<String>`, so the client *still* cannot say "line full" — the ambiguity that cost two of those three round-trips. the uboot note still has no way to verify what was served. and clippy and the gate never ran across the five changed crates.

## what I would tell myself at the start

- **a net total is where a regression hides.** the granularity of the instrument decides which bugs are expressible, and "the forward pass got 2.9× faster" was true while one bucket inside it got 1.7× slower.
- **an A/B can be rigged by accident.** ordering, cache state, a single run quoted before the repeat.
- **when a document ranks work by guesswork and says so, believe the disclaimer, not the ranking.** §2 was wrong, §3b was oversold, §3e was worth 1%, §3a was sixth and was the whole thing.
- and the one I keep relearning: when the output does not match the source, count something before theorising. 268 is a number. "the refusal cap" was a story.
