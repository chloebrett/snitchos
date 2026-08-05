# Post 77 — the control passed twice

- this is the tail of the drivel-on-target arc from [post 74](post-74-the-emulator-was-shouting.md): the last correctness piece, two defects that post confessed to in print, and a bug I introduced by fixing something correctly in one place.
- they turned out to be one story. **every guard here was already passing while checking nothing**, and each needed a different kind of evidence to admit it. not "the test was missing" — post 69's theme, and stitch 17's. these tests existed, ran, and went green against exactly the failure they were built to catch.

## the check that cannot work, and looks like it does

- a checkpoint's weights are meaningless without the vocab they were trained against. pair `drivel-b9b10-30k.kvetch` with some other vocab and every array is the right size, every token id is in range, and the model emits confident nonsense — no error, anywhere, ever.
- so the server checks. the obvious check is that the vocab's token count matches the model's, and it is **worthless**: vocab size is a frozen hyper-parameter, 2048 across the whole ladder, so *every mispairing anyone could actually make is size-compatible.* the check passes on precisely the inputs it exists to reject.
- what distinguishes two vocabs is the merge list and its order, because that is what decides what a token id *means*. so the checkpoint header carries a 64-bit FNV-1a of the serialized vocab, written by `cram` at save time, and the server recomputes it over the vocab it embedded and refuses to serve on mismatch. the checkpoint asserts its own provenance rather than something downstream asserting a coincidence.
- two details that stopped it being annoying. the header gained a field, so the format version bumped — and `decode` still *reads* version 1, as `UNSTAMPED`, because refusing it would have stranded every checkpoint trained before the field existed and forced a retrain to recover numbers that were still valid. writing is always version 2, so a stamp is one decode-and-re-encode away (`cram --stamp`), and it refuses to overwrite a *different* fingerprint — that's not a checkpoint awaiting migration, it's the mispairing the field exists to catch, and re-stamping would launder the evidence.
- `UNSTAMPED` is itself refused at serve time. unverifiable is not verified.
- the test that made me trust it: `two_vocabs_of_the_same_size_but_different_merges_fingerprint_differently`. it fails against any size-based check, which is the only reason to write it.

## the mask you cannot afford, and the one you can

- the guarantee that survives the switch from a zero-parameter sampler to a trained one is that **a completion is always legal Stitch**. the model chooses among continuations the oracle permits; it never decides what is permitted.
- textbook constrained decoding zeroes every illegal logit and renormalises. that needs a legality verdict for all 2048 tokens at every step, and here a verdict costs a lex *and* a parse of the extended prefix — thousands of parses per token, on a machine whose entire completion budget is a few million instructions.
- so legality is tested **lazily, in descending probability**: draw, ask, and on a refusal strike that candidate and draw again from what remains. that's sampling without replacement from the masked distribution — identical in outcome to masking everything, but it pays for verdicts only on the tokens it actually considers, and the model proposes a legal token first the overwhelming majority of the time.
- striking rather than re-drawing is what makes it terminate. rejection sampling against a mostly-illegal distribution can spin forever; removing the candidate cannot.
- bounded at 16 refusals, and the bound is a judgement rather than a safety valve: if the top sixteen candidates are all illegal the model has no useful opinion here, and the honest answer is to stop. a completion is a *fragment*, so a shorter one is still valid.
- mutation testing paid for itself twice on this file. it found that the softmax's normalisation was dead code — `weighted_pick` divides by its own sum, so scale cancels, and swapping the division for a multiplication changed nothing observable. and it found that a **constant RNG passed every test I had written**: same seed, same token ✓; likelier token dominates ✓. a sampler where every seed returns the same completion satisfies both. the test that kills it is `different_seeds_draw_different_tokens`, which I would not have thought to write.

## the control that passed twice

- the last piece was byte-identity: the host recomputes the completion from the committed checkpoint with the same seed, and the scenario asserts the guest served exactly those bytes. babble has had this since [post 64](post-64-a-model-with-no-weights.md); the trained rung had only "a completion appeared", which would pass with every logit subtly wrong.
- it went green first run. **a transformer's output — a long chain of floating-point arithmetic — comes back byte-identical through the emulator.** that is the claim "deterministic given its own trace" actually requires.
- then the negative control, because a green oracle is only reassuring if it can go red. recompute with a **different seed**: still passed. recompute with an entirely **different prefix**: still passed.
- not a broken assertion. at the client's then-current one-token budget, the model answers `"\n   "` — a newline and an indent — for any prefix and any seed, because a code model's first move is always to start a line. byte-identity against that is nearly a tautology.
- the fix is the token budget, and I found it in the right place: on the **host**, where the same question costs milliseconds instead of a two-minute emulator run.

```
1 token   "\n   "              "\n   "              same
2 tokens  "\n    let"          "\n    //"           differ
4 tokens  "\n    let water ="  "\n    // Sort by"   differ
```

- two is the minimum that discriminates; the client asks for four, which is that with margin and covers more arithmetic per run. the budget had been one because [post 74](post-74-the-emulator-was-shouting.md) cut it for speed before the oracle existed to have an opinion — a number chosen for one constraint, still sitting there when the constraint changed.
- **a byte-identity oracle is only as strong as the variety of the bytes it compares**, and that strength is measurable rather than arguable. the question "how many tokens before the answer depends on the question" took thirty seconds to answer somewhere fast.

## a metric that stops counting without saying so

- post 74 ended by naming two defects in `RuntimePlatform::complete` and not fixing them. both are fixed now, and both are the same failure as everything above: a signal that reports success while measuring nothing.
- it re-registered its counter **on every Tab**. a process may name at most 16 metrics, there is no dedup, and a refused registration hands back a handle whose `emit` is a silent no-op — so the counter worked for about thirteen Tabs and then stopped, with nothing on the wire to say it had. it now registers once, lazily (a metric name is a quota'd, permanently-interned resource, and most programs that build a `RuntimePlatform` never press Tab).
- and it emitted a constant `1` rather than a running total, so the wire could not distinguish one completion from fifty.
- the gate is one line in an existing scenario: press Tab twice, assert the counter reads **2**. either bug leaves it at 1 forever.
- writing that assertion cost me a run, for a reason worth keeping: the client counts *before* it calls, so the counter frame precedes the server's span on the wire. I waited for the span first, which walked the cursor straight past the value I was looking for. **a forward-cursor stream makes assertion order a correctness property**, and this is the third time this project has paid for that — the same shape as the seeds in post 74 and the interleaved rounds before them.

## the mapping that was right in one place

- and one bug that was entirely mine, from fixing something correctly. post 74's 4.5 MB feature gate needs a workload→feature mapping: boot `stitch-drivel`, get the checkpoint compiled in. I put that mapping in the itest audit, which is where I needed it.
- `cargo xtask snemu boot --workload stitch-drivel` then booted a kernel with an empty ELF stub and panicked `Parse(BadMagic)`, because `snemu boot` builds its own kernel and had never heard of the feature.
- the fix isn't the second call site, it's the *single home*: `qemu::workload_features(workload)`, beside `build_kernel`, used by all three paths. **a mapping duplicated per call site is a mapping that is wrong at all but one of them** — which is post 70's sink-bypass lesson and the VF2 codegen family's, arriving in a third costume.
- the tell I should have read: I was writing the mapping *inside* the consumer that needed it, rather than beside the thing it describes.

## what I learned

- **a check that passes on the inputs it exists to reject is not a weak check, it is a decoration.** the vocab token count would have run forever, gone green forever, and caught nothing, because the one variable it reads is frozen across the entire space of realistic failures.
- **a negative control is worth running even when — especially when — you are confident.** two of mine passed. the first told me nothing was wired; the second told me the *question* was too easy. neither is something I would have discovered from a green run.
- **when a control fails to discriminate, measure the discrimination somewhere cheap.** "how many tokens until the answer depends on the prefix" is a question the host answers in milliseconds and the emulator answers in minutes, and it is the same question.
- **mutation testing finds the tests you did not think of, not just the ones you skipped.** a constant RNG satisfying every property I had written is a better argument for the technique than any coverage number.
- **a number chosen for one constraint outlives the constraint.** the one-token budget was correct when it was set, for speed, before there was an oracle to weaken. nothing announces that a tuning decision has become a correctness decision.

## where the plan went

- `plans/kvetch-drivel-on-target.md` is archived to `legacy/`. steps 1–5 done, the KV cache landed early, and step 6's fork — *is drivel-at-the-prompt a board feature snemu can only gate, or something you can use inside the emulator?* — resolved in writing: ~12s for six tokens on an opt-in scenario is bearable, so the remaining performance levers are worth doing on their own merits rather than to rescue this feature.
- three of those carry on without the plan: the long-completion profile (the one measurement that would choose between them, and it wants an idle machine — [post 67](post-67-the-build-watched-what-it-wrote.md) is emphatic about why), borrowed weights instead of `Model::decode`'s owned `Vec<f32>` (~8.4 MB resident for a 4.2 MB model, and what would let the 64 MiB machine come back down), and FP in snemu's block JIT, which lowers no FP family at all today so a matmul inner loop compiles to two-instruction blocks.
- and one deliberately not carried: FP ownership reads `Process::fp_enabled`, which is per-process while the registers belong to a task. identical while a process has one task — true today, unverified — and the trigger to fix it is the first thing that gives a process two. it is written down in [post 72](post-72-the-unit-was-already-on.md) rather than left in a plan file, because that is the shape of thing that gets rediscovered expensively.
