# Post 74 — trained weights answer Tab on target

- the goal was one line at a prompt. press Tab in the on-target Stitch REPL and have the answer come from **trained weights** rather than from the grammar sampler:

```
greet(name) {
    let padded =
```

- babble, the zero-parameter version from post 64, gives `.. and ..= < "score" +` for the same prompt. both are syntactically legal Stitch. only one of them looks like a program.
- getting there took four steps of plan and turned up two diagnostic holes that were each worth more than the feature. one of them had been swallowing the single most useful error message the emulator produces.

## the estimate was wrong by an order of magnitude

- the plan wrote down its guesses so they could be checked, which is the only reason this table exists:

| | predicted | measured |
|---|---|---|
| one completion, guest instructions | 0.2–0.5B | **4–8B** (8 tokens) |
| machine RAM | *unconsidered* | 16 MiB default **too small**, needs 64 |
| snemu support | assumed complete | **`fclass.s` was missing** |

- an order of magnitude on the headline number, and the two things I hadn't thought about at all were the ones that actually broke.

### a gate that takes minutes is a gate people stop running

- 4–8 billion guest instructions is ~46 seconds per completion under snemu. that cannot live in a suite whose whole selling point is that it finishes in seconds.
- so `itest-harness` grew a third scenario profile: `slow` — cpu-bound **and opt-in**. excluded from an unfiltered run, still runnable by name or `--tag kvetch`. the value of the scenario is undiminished; only its cost makes it unfit for the every-commit gate.
- the snemu audit path needed the same filter and didn't get it for free, because it selects from `SCENARIOS` directly rather than through `select_by_tags`. two selection paths, one of which didn't know about the new concept — worth remembering as a shape.

### 4.5 MB of weights rode into 130 kernels that didn't want them

- this is the good one. the design call was "a separate binary, so nothing else pays" — and that's true about *binaries* and false about **images**. `itest-workloads` embeds every program into one kernel.
- so the checkpoint rode into all 130 other scenarios' kernels, pushed a 16 MiB machine out of frames at userspace load, and half the userspace suite started failing `OutOfFrames`. each failure then burned its **full step budget** before giving up. a 7-second gate became fifteen minutes.
- fixed with a `kvetch-drivel` kernel feature: the weights embed only when it's on, and the itest build turns it on only when a selected scenario needs it — so selection has to happen *before* the build, which is a small inversion that took a moment to see.
- the workload registry stays additive, which is the property I keep paying to preserve: without the feature the workload still exists and fails honestly at ELF load, rather than vanishing from the registry.

### the KV cache, pulled forward because the numbers demanded it

- 46.5s → **11.8s** for the six-token completion. about 10× on the marginal per-token cost once the fixed ~8s boot is discounted.
- it's asserted bit-identical to re-running the whole prefix, not hoped to be. a cache that is *nearly* the same as recomputation is a cache that produces a different program on Tuesday.

## two diagnostic holes

### the harness threw away *why* the emulator stopped

- this cost me an evening and it is one line:

```rust
if self.machine.step().is_err() { … }
```

- snemu's entire design contract is that an unmodelled instruction **halts the host and names itself** — pc and encoding — rather than becoming silence the guest experiences. that's the rule post 65 was built around.
- and the itest harness discarded the reason. so the emulator shouting "I don't implement instruction `0xe00516d3` at `0x1000409a`" arrived as *"no frame arrived"*, and the scenario dutifully blamed whatever it happened to be waiting for. the loudest error in the system was being converted into the vaguest one at the last hop.
- carrying the reason through took minutes. the first run after the fix printed:

```
Unimplemented { pc: 0x1000409a, instr: 0xe00516d3 }
```

- which decodes to `fclass.s` — a single-precision classify I'd never implemented because nothing had asked for it. minutes of guessing became one run.
- the lesson isn't "check your Results". it's that **a diagnostic is only as good as its worst hop**, and the hops are usually owned by different code than the diagnostic.

### a mispaired server answers rather than dies

- `serve_model` correctly refuses to serve when the checkpoint and vocab don't match. it then keeps answering `Malformed` **forever** instead of exiting.
- which would be a fine local choice except that a client blocked in `call` on a dead endpoint has no refusal and no timeout — so the symptom surfaces two processes away from the cause, as a REPL that completes to nothing.
- same shape as the FP guard in post 72: the component that detects the problem is not the component that experiences it, and without a timeout or a death there is nothing connecting them.

## where it landed

- Tab at the on-target prompt is answered by the trained checkpoint, on both engines, gated by an opt-in `slow` scenario. `stitch-kvetch-completes` — written months ago, unregistered, and blamed on three different things over its life — is registered and green.
- two defects found while getting there and worth naming because they're both *observability* bugs in the thing whose job is observability: `RuntimePlatform::complete` re-registers its counter on every Tab (per-process quota is 16, no dedup, so it silently stops counting after ~13) and emits a constant `1` rather than a running total.
- a metric that stops counting without saying so is worse than no metric. that's the third time this project has learned that, and the first time it was in code I'd written to *report* on something else.
