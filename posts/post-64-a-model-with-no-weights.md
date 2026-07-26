# Post 64 — a model with no weights

- I set out to make the Stitch REPL do tab-completion. by the end of the day I had a completion stack, a model with zero parameters serving suggestions over a capability-mediated IPC endpoint, byte-identical output on the host and inside the emulator — and a one-line proof that any userspace program can panic the kernel.
- the last one wasn't on the plan. it also wasn't a bug I introduced. this post is the whole arc, including the hour I spent confidently chasing the wrong cause.

## the idea: the grammar knows more than the model does

- the frame for all of this: **the grammar says what is *legal*, a model says what is *likely*.** those are different questions and only one of them needs weights.
- so before any model, a pure function: `valid_next(source, pos) -> TokenSet`. given a prefix, which token classes may come next?
- the obvious implementation is to instrument the parser — sentinel token, record what each `expect` would have accepted, unwind. I started there and then found something better.

### trial by append

- ask the parser instead of asking *about* the parser. append a candidate token to the prefix and read **where** parsing fails:
  - error *at* the appended token → the parser rejected it. dead.
  - error *beyond* it, or none at all → the parser consumed it and wanted more. viable.
- that's it. 58 token classes, 58 probes, and the answer comes from the real parser every time.
- the property that makes this worth it: **the oracle cannot drift from the grammar.** there is no second copy of grammar knowledge to maintain. add a keyword to Stitch tomorrow and the oracle knows about it, because the oracle *is* the parser being interrogated.

### it immediately found a parser bug

- the viability rule depends on error spans being accurate, so I checked them. three sites in `parse_atom` / `parse_pattern_atom` do `match self.bump()` and then build the error from `self.err()` — which reads the *current* token. by then `bump` has already moved past the offending one.
- so "unexpected token" errors pointed one token too far right. every caret in the language was off by one, in the cases that mattered.
- it was invisible because the most common instance is correct **by accident**: `bump` doesn't advance past `Eof`, so `unexpected token: Eof` — the case every existing snapshot pinned — reports the right span. the wrong ones were the ones nobody had written a test for.
- fixed by capturing the span before the bump. one snapshot updated, because it had been pinning the bug.

### four consumers, one function

- the payoff for building this as a pure function rather than a completion feature: parser diagnostics are now a *view* of the oracle. errors end with the legal continuations, and they aren't a second hand-maintained list — they're `valid_next` at the failure position, so the completion menu and the error message cannot disagree.

```
1:9: expected ')' after parameters
greet(a b)
        ^
expected one of: `)`, `,`, `:`
```

- one subtlety worth recording. a REPL line may be a *declaration* or an *expression* — `eval_line` tries `parse_program` first, then `parse`. neither entry alone is right (`let x = 1` is one, `1 +` is the other), so the completer takes the **union of both**. consequence: a token is only ever "forced" if *both* readings force it, which makes forcing rare and every instance of it trustworthy.
- while wiring that up I found the diagnostics path had *three* callers, and only one of them called `render`. a parse error in a **file** had always been strictly less helpful than the same error typed at a prompt — no caret, no location. that predates all of this by months.

## babble: rung zero

- with the oracle in hand, the cheapest possible model is: sample uniformly from whatever the oracle says is legal. no weights, no training, no corpus. [babble](../docs/babble-design.md).
- it exists for a real reason, not as a joke. it's the **eval floor** (every trained rung is measured against it), it's the Tier-0 corpus generator, and — the point here — it lets the entire *serving* path be built and tested before any checkpoint exists. quip shouldn't have to debug a model and an IPC stack at the same time.
- first output, before any tuning:

```
contract x < x , x , x > { free x ( ) mut x ( ) -> x -> x x ( ) = true ( ) != true ..= => .. | 0.0 mut x ( ) …
```

- syntactically valid Stitch, meaning nothing. every identifier `x` and every number `0`, because the oracle answers in *classes* and the sampler was appending the class's probe lexeme.

### weights can't make a grammar walk terminate

- the walk ran to its 200-token cap instead of ever choosing `Eof`, so I did the obvious thing: bias tables. damp the constructs that open nesting, boost the ones that close it, and let pressure grow with length and depth.
- it didn't work. so I measured instead of tuning further, and the number was decisive: **`Eof` was legal at only 2–7 of 200 steps.** the pressure never got a turn, because the walk was almost never at a point where stopping was even an option.
- the reason is nicer than the symptom. damping every obligation-creating class *also* damps the token that **pays** an existing debt — the `{` that a pending `match` is waiting for. the walk enters constructs it is then discouraged from finishing.
- the fix inverts the responsibility: **ask the oracle when to stop.** past a wind-down point, end at the first position where the program is complete; if the cap arrives still owing a construct, rewind to the last complete point. a babbled program is now always a *whole* program.
- two smaller findings on the way. a per-class floor of 1 silently flattens the whole policy — with ~25 operator classes pinned at weight 1 they always outvoted `Eof` — so weights need a scale factor and a pressure cap, or every class bottoms out at 1 exactly when the walk most needs to finish.
- after terminal synthesis (wordlists instead of probe lexemes), it reads like Stitch:

```
ext let label = .. true >= 3 |> not - ( @ , false )
sum queue < value > = frame line ( ) -> @ uses name = - - @ or [ ] ? ?. buffer
```

- names are lowercase-only, and that's correctness rather than taste: the oracle probes `Ident` with a lowercase representative, and the parser branches on identifier case (`starts_uppercase` separates a constructor pattern from a binding). emitting `Point` where `x` was approved would step outside what was actually checked.

## serving it

- kvetch is a userspace server holding `RECV` on an endpoint — the same shape as the FS server. a client holds `SEND` and calls. behind the endpoint is babble; later it'll be a checkpoint, and no client will know the difference.
- the request is one buffer in and out (the four-word IPC message has no room for two), and it carries `max_tokens` rather than `max_bytes` — because truncating at a byte boundary can split a token, and half of ` without` is `with`, which is a *keyword*. tokens are the unit that preserves the property.
- **no seed field in the protocol, deliberately.** sampling entropy derives from a per-boot root plus a request counter, and *time never enters the derivation*. clock-seeded sampling would promote engine clock skew into content divergence and poison `snemu diff`. the seed is emitted in the completion's span, so a recorded generation is replayable from its own trace.
- the seed derivation is pinned by golden vectors, and the golden test earned its keep on the first run: `request_seed(0, 0)` was **0**. SplitMix64 maps zero to zero, and boot-seed 0 with request 0 is the plainest run the system has — so the most common path handed the sampler its most degenerate state. fixed with `counter + 1`.

### it works, and it agrees with itself across engines

| | snemu guest | host sampler |
|---|---|---|
| seed | `-2152535657050944081` | `-2152535657050944081` |
| bytes | 34 | 34 |
| checksum | 6643581145736680012 | 6643581145736680012 |

```
greet(name) { "price" ( ) ~> not ..= true delta
```

- same seed derivation, same sampler, same bias tables, same bytes — on the host and inside an emulated RISC-V machine. that equality is now an itest, and I verified the itest *fails* when it should by bumping the host's token budget by one.
- the span nesting worked unprompted too: `kvetch-client` → `kvetch.client` → `kvetch.complete` **on the server's task id**. the trace crosses the process boundary for free, which is the whole point of the telemetry design.

## tab

- three outcomes, and the distinction matters:
  - **Forced** — exactly one legal spelling. type it. *no round trip* — a model cannot improve on a certainty, and waiting on one would make the best case the slowest.
  - **Suggested** — a model's guess. legal, but one of several.
  - **Choices** — show the menu.
- a forced *class* is not a forced *spelling*, which took a test to notice. after `use`, exactly one class is legal — an identifier — but only the user knows the module name. inserting the oracle's probe lexeme (`x`) there would be **inventing code**, not completing it.
- and the menus told me something useful about where a model actually earns its place:

```
"let x = "   an integer, a float, a boolean, a string, a name, a placeholder, `match`, `handle`, … (17 total)
"greet"      `and`, `or`, `+`, `-`, `*`, `/`, `%`, `==`, … (24 total)
```

- the first is genuinely useful. the second is bad — after a bare name what you want is `(` or `.`, and both fall past the cap behind `and`/`or`/arithmetic. **grammar-only ranking is good at expression openers and bad at continuations.** that's a concrete, measurable target for the first trained rung, rather than a vague hope that a model would help.

## and then Tab panicked the kernel

- wired it all up on target. typed Tab. the guest went silent — no echo, no completion, no further telemetry of any kind.
- what followed is the part I'd like to have done better.

### the wrong answer, confidently

- I reasoned: one `complete()` makes 232 probes, each allocating; talc maps a fresh ~68 KiB region per OOM; 232 × 68 KiB ≈ **15.8 MiB** against a per-process cap of exactly **16 MiB**. the arithmetic landed on the cap. I wrote it up as the cause.
- it was wrong. the kernel refuses `MapAnon` with `OutOfMemory` and **refusals snitch** — and no refusal frame is ever emitted. the heap is never exhausted.
- a number that fits a known limit is a hypothesis, not evidence. I'd treated it as evidence because it was satisfying.
- I also misread an instrument: `guest_instret` reports instret at the last *matched* frame, so "flat instret" means "no frames arrived", not "the guest stopped executing". I read it as a progress counter and it pointed me the wrong way for an hour.

### the right answer, in sixty seconds

- the bisect that did work was structural: plain input round-trips, so it isn't the console; `use M.` alone echoes, so it isn't partial input; `use M.\t` echoes *nothing*, which also explains itself (the editor returns its echo at the end of a chunk, so a hang on Tab swallows the characters before it); a client-side counter never fires, so it never reaches IPC; the `Forced` path hangs too, so it isn't the model; and **`kernel.heartbeat` stops**, so this reaches the kernel.
- then I did the thing I should have done at the start and ran it under the other engine.

```
$ cargo xtask itest stitch-kvetch-completes --engine qemu
Kernel panic: unhandled trap: UnknownException(2) (scause=0x2)
```

- `scause=2` is **illegal instruction**. nothing in the kernel ever sets `sstatus.FS`, so it sits at its reset value — Off — and every floating-point instruction traps. the oracle probes all 58 classes including `Float`, whose token carries an `f64`.
- and it has nothing to do with completion:

```
stitch> 1.5 + 1.5
Kernel panic: unhandled trap: UnknownException(2) (scause=0x2)
```

## three bugs, none of them mine

1. **Stitch's floats cannot run on target.** the language supports them fully; nothing on the metal had ever used one. every fixture — `primes.st`, the REPL demo — is integer-only, so it had sat there unexercised.
2. **a user program can panic the kernel with one illegal instruction.** the `UnknownException` arm panics regardless of privilege, though the surrounding code already computes `from_user`. an unhandled *user* trap should kill the process, as every other user fault does. this is a robustness hole entirely independent of floating point, and it's the one I'd fix first.
3. **snemu hid the panic.** under snemu the guest simply stopped emitting frames; QEMU printed it. the `panic-now` scenario exists specifically to assert that kernel panics reach the wire — so this is a real fidelity gap, and it's what turned a one-minute diagnosis into an hour.

- the design that fell out ([floating-point-design](../docs/floating-point-design.md)): FP is **opt-in per program, derived from the ELF** rather than declared — RISC-V binaries carry their float ABI in `e_flags`, so the loader can *read* the claim instead of trusting it. unusually for an authority question, this one is mechanically checkable.
- no capability for it, because FP isn't scarce — what opt-in buys is *cost attribution*, since 32 FP registers is 256 bytes of save/restore on every context switch and it's wrong to tax integer-only programs for it.
- and the mechanism is the fix for bug 2: on an illegal instruction from U-mode, if the process is FP-authorised, enable `FS`, snitch, and retry; otherwise refuse and kill. lazy FP context-switching doubling as the authority check — the trap that currently panics becomes the decision point.
- snemu gets real RV64F/D rather than a soft-float dodge. we own the emulator, and `snemu diff` means divergence is detectable rather than silent, which is exactly what makes incremental FP safe.

## what I'm taking away

- **switch engines earlier.** when the tool you're interrogating is the one giving worse signal, stop interrogating it. one `--engine qemu` run answered what an hour of snemu-side inference didn't. that's what the fidelity escape hatch is *for*, and I treated it as a last resort instead of a first question.
- **a fitting number is not a finding.** 15.8 against 16.0 felt like proof. the falsifying observation — an absent refusal frame — was available the whole time and cost one command.
- **distrust a test that passes first try.** the truncation test I wrote initially only checked "the buffer got shorter and is still viable", which naive byte-truncation passes. rewriting it to assert the served *token stream* is a prefix of the untruncated one, then deliberately breaking the implementation to watch it fail, caught `Ident("t")` where `Ident("task")` was approved.
- **the grammar is worth more than I expected.** a completion menu that is never wrong, forced tokens typed with no model and no round trip, diagnostics that can't contradict the completer, and a generator that cannot emit invalid code — all of it from one pure function, before a single weight exists.
