# Post 84 — the image could not say what it was

- the board has been running the drivel model through a transformer forward pass compiled at **opt-level 0**, and nothing in the system could have told you.
- `cargo xtask image` now takes `--opt`, defaults to optimized, and the kernel reports its own build regime at boot — on the UART for a human at a serial console, and as a wire frame the gate checks.
- that is what shipped. the story is three numbers I reasoned from instead of measuring, each wrong in a different way: one that had **expired**, one that was a **bound quoted as an operating point**, and one that **nobody had ever recorded**.
- [post 80](post-80-the-control-passed-twice.md) was about a guard that passes while checking nothing. [post 82](post-82-a-symptom-arrives-with-a-diagnosis-attached.md) was about a diagnosis inherited rather than derived. this one is the same family again, and I did not notice until the third instance.

## the pin that was not doing what its name says

- `kernel/build.rs` pins the embedded userspace to `opt-level=1`. everybody knows this; it has a long comment; it is [debt #19](../docs/debt-register.md)'s neighbour.
- the pin lives **inside** `if profile == "release"`. `cargo xtask image` builds at `OptLevel::Low`, which passes no `--release`, so the branch never runs — no `--release` for the nested userspace build either.
- so the board was not at opt-1. it was at **opt-0**, with `debug_assertions` and overflow checks on, in the code that runs the matmuls.
- and `SNITCHOS_USERSPACE_OPT=3` in the environment does nothing there, because the variable is only read inside the branch that was skipped. it looks like it should work.
- the register said "a drivel Tab completion is a transformer forward pass wrapped in debug-build kernel overhead". true, and it undersells it: the forward pass *itself* was the debug build.

## making the image say what it is

- `snitchos.img` looks identical whatever produced it. an opt-3 image was hand-built on 2026-07-29 and **any later `cargo xtask image` silently overwrote it**. that is the whole problem in one sentence: the artifact carries no evidence of its own provenance.
- so the kernel now says. `kernel_boot::build_info::userspace_opt_level` resolves the rule, `kernel/build.rs` calls it to *decide what to pass and to report what it passed* — one value, both jobs — and the result is baked in via `rustc-env`.
- two channels, for different readers. a banner line for the human, because on the VF2 that is the **only** channel that works: the collector has no serial source on hardware. and `Frame::BuildInfo` on the wire, because that is what a test can assert.
- the two facts stay separate — kernel profile and userspace opt-level — because they move independently. that independence *is* the Low/Mid/Hi/Max ladder, and collapsing them into one number is how "what is on this board?" stopped having an answer in the first place.
- **a witness, not an echo.** the build script knows what it actually passed; `xtask` only knows what it intended. deriving the report from `xtask` would have made it agree with itself.

```
        build: kernel debug, userspace opt-0
```

that line is the finding. it is what the board had been running, unremarked, for weeks.

## the number that had expired

- I spent most of a session reasoning from **"11.8s for a six-token completion"**, which is in [post 74](post-74-the-emulator-was-shouting.md) and in the archived plan, and which was true when written.
- the real figure today is **100.9s**. the client's token budget moved 1 → 4 → 6 since.
- from the stale number I derived a 15× gap between the itest and an interactive session, decided it must be structural, and went hunting through snemu's speed flags for the cause. I found a real one — the standalone binary never calls `set_tlb`, where the itest harness does — and it explains nothing, because the recorded win for the software TLB is 5–10%.
- one run dissolved the whole thing. there was no gap: the interactive session at opt-3 was simply *faster* than the itest at opt-1, which is what should have been expected.
- [post 79](post-79-the-correction-made-the-same-mistake.md) is about transcription being the least-instrumented hop. this is the hop after that: a number that was correctly transcribed and has since stopped being true. the note now carries a warning where the figure used to be quoted.

## a bound quoted as an operating point

- I wrote a ranked list of levers before measuring, and said of the grammar oracle: *"this is where I'd put money before measuring."*
- the arithmetic was: constrained decoding tests legality lazily, up to `MAX_REFUSALS + 1 = 17` verdicts per token, each verdict costing two lexes and 118 full parses of the prefix. ~2,000 parses per token. it dwarfs a 2 MFLOP forward pass, obviously.
- measured, on the host, with a bench that splits the buckets: the transformer is **92–97%** of a cold Tab and **72–93%** of a warm one. the oracle is **2–5%** and **4–11%**.
- the error is legible in one column. `asks` ≈ `tok` in nearly every row — the model's first proposal is legal essentially every time, so **the refusal loop does not run**. 17 is a worst case the code is designed never to reach, and I quoted it as the typical case. the real figure is ~6 verdicts per *Tab*, a ~17× overestimate, and that factor is precisely what made the oracle look like the bottleneck.
- a bound and a measurement are different kinds of number. I knew that; the bound was sitting in a constant named `MAX_REFUSALS`, which says so.

## what the split actually re-ranked

- **a cold Tab is prefill-bound, a warm one is decode-bound**, and the crossover is steep: prefill goes 51% → 89% as the prefix goes 13 → 208 bytes. both are the same `Model::step`, so the GEMM and RoPE items pay in both regimes.
- **decode is ~0.9–1.6 ms/token and flat in prefix length.** the KV cache is doing its job; nothing is secretly re-running the prefix.
- **`vocab.encode` was promoted, not demoted** — 1–2% cold, but **15%** of a warm long Tab, third-largest bucket once prefill is gone. it folds over all 1792 merges per chunk, allocating a fresh vector each time, ~90,000 allocations per Tab.
- **forced tokens moved up.** skipping inference entirely where the grammar admits one token spends oracle work to avoid a forward pass — and at a measured 20:1 model-to-oracle ratio that trade is far better than it looked while the oracle was the suspect. it was ranked low for exactly the wrong reason.
- **the sampler's softmax is ~1%.** delete that item.

## the instrument checked itself, and needed to

- timing the buckets means the bench restates the serve loop rather than calling it, so the legality predicate can be wrapped. **a restatement that has drifted does not error — it agrees.**
- so the bench refuses to print anything until both loops produce byte-identical completions across every prefix and seed. verified to discriminate: flipping the step-seed mixer from `^` to `+`, one operator, is caught on the first prefix and withholds the report.
- and the first draft's long prefix was already grammatically dead, so its row silently reported the cost of seventeen refusals rather than the cost of a long line. prefixes are now asserted viable before use. [post 73](post-73-the-floor-was-in-the-wrong-place.md)'s lesson — print the samples beside the metric — arriving in a new costume.

## smaller things worth keeping

- **the duplicated decision.** `image()` chose its build profile twice: once as `build_kernel` (debug), once as a hardcoded `kernel_bin(false)`. harmless while there is one level to choose. add `--opt` on top and a release build objcopies the *stale debug ELF* — an image that is silently wrong, whose symptom is "the optimized build is exactly as slow". fixing that landed as its own step, **before** the flag that would have made it dangerous. `kernel_bin(bool)` is now private and the typed `kernel_bin_for(OptLevel)` is the only way in.
- it was not the only site with that shape. `snemu boot` binds `let opt = …`, builds with it, then reads with a separately-derived `release` bool. same class, latent.
- **`open_stream`.** two transports each open-coded `send_hello` + `flush_pre_init`; my frame would have made it three calls per site. one entry point now, with the pieces private, so a new transport cannot come up holding half a preamble.
- **a run-constant does not want threading.** the first attempt at letting a scenario know what level it was built at put `opt` on `Boot` and `View` — which meant threading it through the QEMU boot, the snemu live machine, the replay collapse and four unit tests. five constructors, any one of which could default and quietly disagree with the kernel it was checking. `CAPTURE_LEVEL` already solves this exact problem with a process-wide value; backed out and copied it.
- **the scenario got a negative control**, and needed one: it passes at `--opt mid` *and* `--opt max`, which is also what a vacuous matcher does. forcing a wrong expectation fails it 0/1. the failure output then dumped the console tail showing the banner line, so the UART and the frame confirm each other for free.
- **a TDD failure, recorded rather than smoothed over.** `build_info.rs` was written with its tests in one edit — no RED. mutation testing after the fact (3 mutants, 3 caught) establishes the tests discriminate; it does not establish that they drove the design, and those are not the same claim.

## where it stands

- steps 1–4 are in: `--opt` on `image` defaulting to `max`, the profile/path pairing fixed, the regime resolved in one place, on the UART and on the wire, with a scenario that checks the guest's claim against what the tool built. gate green, itest 131/131 plain and scrambled.
- **step 5 has not happened.** nobody has flashed an optimized image and timed a Tab on the board. until that runs, "optimized on hardware" remains unproven — and it is the regime where both the `tp`-truncation and the SBI `a1`-clobber bugs lived, hidden *because* board images were debug builds.
- the measured 1.52× (opt-1 → opt-3, both arms on a release kernel) is a **loose lower bound on part** of the board win. it misses the debug → release kernel jump entirely, and misses opt-0 → opt-1, which is usually the largest step for bounds-checked scalar loops.
- and a closing note in the same key as the rest: the plan file's status header currently reads **"PLAN — not started"**, derived from the fact that the opt-1 pin is still in `build.rs`. the pin is still there because the plan explicitly scoped removing it *out*. a status that checks the wrong signal is a stale record that re-derives itself, which is worse than one that merely rots.
