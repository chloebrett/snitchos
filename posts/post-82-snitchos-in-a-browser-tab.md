# Post 82 — SnitchOS in a browser tab, and an inherited diagnosis

- SnitchOS runs in a browser tab now. it boots, you pick a workload, you type at it, and the Stitch REPL answers Tab with a completion from the trained model — the same rung [post 80](post-80-checkpoint-vocab-pairing.md) was arguing about, now inside a page. four panels show the machine explaining itself.
- that is the setting. the story is a prediction that was **right about the symptom and wrong about the cause**, and how long the wrong cause survived because it arrived attached to a correct observation.
- [post 80](post-80-checkpoint-vocab-pairing.md)'s theme was guards that pass while checking nothing. this is the next one along: a *diagnosis* that fits the evidence, is inherited rather than derived, and steers three fixes before anything contradicts it.
- everything else here is the arc's other findings, recorded because this is a notebook and I will otherwise forget them.

## what's in the tab

four milestones, each its own plan and gate.

- **boot** — `snemu-wasm`, a `cdylib` over the emulator, and a React page that streams its UART into xterm.js with a progress indicator while the kernel loads.
- **interactive** — keystrokes go the other way, and the workload picker is populated by the *kernel's own* `workload=` registry handed across as JSON, so the page cannot offer a workload the kernel does not have. the drivel weights ride along behind a `kvetch-drivel` web feature — its own feature, because the itest kernel image is a shared budget, and 4.5 MB of weights in the shared build broke unrelated scenarios with `OutOfFrames` once already. kernel goes 2.09 → 6.37 MB for the page that wants it, and nothing else pays.
- **telemetry panels** — the virtio-console frames decode *in the browser*, and three structural folds render live: capability derivation, span call graph, switch transitions. plus a frame tail.
- **metric panels** — ~60 guest metrics, grouped, charted over guest time.

about 3.2k lines of Rust and 5.9k of TypeScript. the gate: **2899 Rust tests, 149 TS unit tests, 18 browser tests**, and `cargo mutants -p snemu-wasm` at **70 mutants, 0 survivors**.

## the prediction

- `docs/scaling-down-snitchos.md` had said, long before any of this: snemu fast-forwards emulated time over an idle `wfi` rather than sleeping, so **an idle tab would pin a core**.
- it was right. measured through Chrome DevTools' `TaskDuration` — real main-thread busy seconds, not a proxy — an open tab after boot sat at **100.0% of one core, indefinitely**, at 11.0 MIPS of guest throughput.
- so the note was correct, and I took the whole sentence. *idle* tab. the guest is idling; the emulator is spinning through the idle; fix the spinning.
- the first two words were an observation. the rest was a mechanism nobody had checked.

## the fix that changed nothing, and said so

- snemu leaves its accelerators off by default — the plain interpreter is the differential oracle — and the browser had inherited that. `fetch_cache: None`, `block_cache: None`. turning them on took the page from **11.0 to 38.9 MIPS**.
- CPU: still 100.0%.
- which is exactly what it should have been, and I had written down the reason in advance: raising the ceiling does not lower the floor. the loop was running flat out; a faster loop runs flat out faster. the guest bought guest-time quicker and the core stayed hot.
- that is the first thing that should have made the diagnosis suspicious. a genuinely *idle* guest given 3.5× the speed ought to have gone quiet. it did not, and I filed it as "expected" rather than "informative".
- one thing that pass did buy: enabling accelerators made snemu's "a pure speedup, proven by the on↔off A/B" claim load-bearing for a page whose determinism is advertised. so it got an A/B of its own — a hand-assembled RV64 probe run with accelerators off and on, asserting identical registers, UART bytes, instret and state hash. and then mutation testing pointed out that **a no-op `apply` satisfies every one of those assertions**, because they all assert the two configurations *agree*. a browser silently dropping 38.9 back to 11 MIPS would keep the suite green. the test that kills it is the one deterministic difference: with the block JIT on, a `step()` retires a whole compiled block instead of one instruction.

## pacing, which worked

- the real fix for a flat-out loop is to stop running it flat out. snemu's clock **is** its instruction counter, and the guest's DTB declares a 10 MHz timebase — so one second of guest time costs exactly 10M instructions, and "run at real time" is a number rather than a feeling.
- each animation frame now buys only what the wall clock says is owed. **32.5% of a core, at 10.0 MIPS** — the timebase to three significant figures.
- two guards, both of which mutation testing insisted on. a **debt ceiling**, so a host that cannot keep up does not accumulate a backlog it chases forever — the classic game-loop spiral, and also what makes a hidden tab safe, since the multi-second gap on return is forgiven rather than pursued. and a **credit ceiling**, which I did not write until a test failed: running flat out puts the guest tens of guest-seconds *ahead*, and a pacer that treats that as credit freezes the guest until real time catches up. worse than the bug.
- the credit clamp then broke twice more in one sitting — once for not existing, once for being applied *after* the frame's time was added, which cancelled the time it had just bought. each was a failing test, not a browser session.
- one bug here was pure arithmetic and worth remembering: `BigInt(166666.67)` **throws**. the budget must be floored at the boundary while the *target* stays fractional, or the leftover fraction is discarded every frame and the guest runs permanently slow. the symptom was a guest that never booted at all.

## the counter that ended it

- pacing at real time is honest and it is also **two orders of magnitude slower than the hardware being modelled**. snemu ticks its clock once per instruction, so a paced guest computes at 10 MIPS where a U74 runs ~1.5 GHz.
- that only matters when the guest is doing real work, so: run flat out when it is busy, pace when it is idle. exactly what the original diagnosis implies.
- first attempt sampled `all_harts_idle()` at each slice boundary. **100% of a core.** idle-skip jumps *through* the wait and resumes inside the same slice, so the parked state barely exists when anything looks for it. I had built a detector for a state that is transient by construction.
- replacing it with a cumulative counter — rounds where no hart retired and the clock jumped to the next deadline — asked the question properly. against the real kernel, past 60M instructions:

```
after boot: instret=60000002 fast_forwards=0
over next 2M instret: fast_forwards +0
```

- **zero. the guest never idles at all.** SnitchOS's idle task is `loop { wfi; yield_now(); }` — it *retires instructions* between waits rather than parking. what the pacer had been throttling was never an idle guest. it was a genuinely busy one.
- the prediction had been right about the tab and wrong about the machine inside it, and the wrong half steered a speedup pass, a pacer, and an entire idle-detection mechanism before a counter aimed squarely at it said no.
- what shipped instead is a **paced/turbo control**: the user's choice, honestly, because there is no idle state to be clever about. the mechanism the experiment produced survives — including the credit clamp, which turns out to be *required* for turbo→paced transitions.

## the same shape, one layer up

- the frame tail had the identical problem and I had already worked around it without noticing what it was.
- the tail is a bounded 400-row window, and this guest emits thousands of `ContextSwitch` frames a second. one heartbeat's traffic fills the entire window. so any assertion naming a *once-only* frame — `kernel.boot`, a single `kvetch.complete` span — depends on a poll landing in the fraction of a second before eviction.
- I found this by breaking two e2e tests, diagnosed it as "these assertions are races", **deleted one**, and rewrote the others to name something the guest emits continuously. all correct, and all treating the symptom. (it did make the suite honest: 2.2 minutes down to 22.6 seconds, because most of that time was retries.)
- the cause is that the window counts *frames*. collapsing runs — `ContextSwitch ×500` as one row — costs no information (the count **is** the signal) and makes the same 400 rows cover seconds instead of a fraction of one. it was also the feature I'd been asked for, arriving as a bug fix.
- the test of whether that mattered was already written: put the deleted assertion back. it passed **3/3 on repeat runs**, and it is a permanent test again, with its history in the comment so nobody deletes it a second time.
- that is the difference between claiming a fix and having evidence of one. the assertion I removed *because* of the bug is now the thing that proves the bug is gone.

## a package manager that ignored its own configuration

- the toolchain got chosen deliberately rather than inherited: Vite 8 (rolldown), React 19, TypeScript 7 (the native port), Tailwind 4 (CSS-first `@theme`, no JS config), Vitest 4, Playwright, Biome instead of eslint+prettier, Yarn 4 via corepack. the one substitution I argued *against* was Bun — for a project whose entire claim is determinism, the browser-test and bundler ecosystems being boringly well-trodden is worth more than install speed.
- then this happened **twice**: `/usr/local/bin/yarn` is **Yarn Classic 1.22.18**, and it shadows corepack. Classic ignores `packageManager`, ignores `.yarnrc.yml` (so `nodeLinker` never applies), rewrites `yarn.lock` into the v1 format — **and then builds successfully**.
- that is the worst possible failure shape. a tool that errors teaches you something in one line. a tool that silently does a different correct-looking thing costs an hour, and costs it again later because nothing recorded it.
- so `cargo xtask web` now refuses to run on an unsupported yarn, with the reason spelled out. the fix is a version guard, not a note in a README, because the note is what failed the first time.
- adjacent, same species: `wasm-pack build` **ignores a positional crate path** where `wasm-pack test` accepts one. and `Command`'s "os error 2" is ambiguous between *program missing* and *working directory missing* — improving that error message immediately diagnosed a real cwd-dependence in my own code that I hadn't been looking for.

## what only broke because it crossed to wasm32

- `Machine::state_hash` **differs between the host and `wasm32` for byte-identical state.** not SipHash, not the emulator's arithmetic: `<[u8]>::hash` length-prefixes with a **`usize`**, which is 8 bytes on the host and 4 in a browser, and the fold is full of slices — registers, UART and virtio buffers, `ram[..written]`. hashing a bare `u64` agrees across both; so does pushing raw bytes through `Hasher::write`. the length prefix is the whole of it.
- this made me **correct an acceptance criterion I had already written**, which demanded host/wasm hash equality. the criterion was wrong, not the code. what the cross-target differential should assert is the *architectural* result — registers, UART bytes, retired instret, all of which do match — and it now does.
- making the hash width-independent is small and safe (every consumer compares two hashes from the same process). it is deliberately **not** done, and the reasoning lives in a doc comment on `state_hash` itself rather than in a plan, because plans get archived. there is a wasm test asserting the hashes *differ*, so whoever eventually fixes it gets told.
- the other wasm-specific hazard is that `#[wasm_bindgen]` code can only be exercised through a browser or a Node harness — so anything living in that layer is invisible to the fast suite. the precedent was sitting in `~/c/slay`, whose plan said "push the logic down" and whose shipped `WasmSession::send` had nonetheless grown branching, rendering and save-prompt state, all reachable only across the boundary.
- so `shell.rs` **enforces its own thinness by test**: the guard scans the file's own source and fails on `if`/`match`/`while`/`for`/`loop`/`else` or arithmetic in the bindgen layer. with a negative control proving the guard can go red, because [a control that cannot discriminate](post-80-checkpoint-vocab-pairing.md) is the running theme of this year.

## retention is three policies, not one

- the obvious design is one ring buffer. it is wrong, because the three consumers need different histories, and a shared window serves whichever one is loudest.
- **frames** are kept by *meaning*: `Retention::Durable` for things that happen once and matter forever (`StringRegister`, `ThreadRegister`, `CapEvent`), `Windowed` for the flood. `retention_of` is an exhaustive match over `OwnedFrame` **and** over `CapEventKind`, with no catch-all — so a new wire variant does not silently inherit a retention policy, it fails to compile until someone decides.
- **tail rows** are kept by *display*: 400 rows, runs collapsed.
- **metric series** are kept per name: 600 points each, so a rarely-updated gauge is not evicted by a busy counter.
- three numbers, three justifications. the one I'm least sure of is 600 — chosen from the observed heartbeat rate, not measured under load, and recorded as such in the plan's open questions rather than presented as a result.

## the panels, and the questions a chart forces

the structural panels were nearly free, and for a good reason: the `diagram` crate **compiles to wasm32 unmodified**, so the exact fold that generates the committed `docs/generated/*.md` also generates the live panel. "a diagram is a collector" turning out to be literally true. a live panel and its committed diagram cannot drift, because they are one function.

the metric panels forced choices that were about honesty rather than rendering.

- **counters render as rates, labelled `per second (derived)`.** the guest never emitted that number. two cases would otherwise draw fiction: a repeated guest timestamp divides by zero, and a counter reset becomes an enormous downward spike that reads as a real event. skipped and reported-as-zero respectively.
- **histograms are excluded, and say so.** a line through a histogram's sum is a lie about what it is. a group containing *only* histograms doesn't render a button at all — an empty group promises a view that cannot exist.
- **small multiples, not one chart per group.** a group mixes units (bytes beside block counts in `heap`) and a shared axis is the dual-axis mistake wearing a different hat. a grid compares shapes without claiming the scales are comparable.
- **groups are derived from names**, not curated: `snitchos.heap.bytes_used` → `heap`. a metric added to the guest appears without anyone editing a list here — same reasoning as the workload picker deferring to the kernel's registry.
- **the palette was computed, not chosen.** the validator ran against this page's *actual* surface `#0d0f12`, not the reference surface: all five checks pass, worst adjacent CVD ΔE 8.4 (protan), normal-vision floor 19.3.
- and a test caught a **design** fault rather than a code fault: the metric name rendered twice, as figure caption and as chart legend. a single series needs no legend box, because the title already names it. there's a test each way now.

## what mutation testing found this arc

it earned its runtime, and mostly not by finding weak assertions.

- **three timeouts, which were real defects.** `budget::run`, `Decoder::push`, `FrameStore::push` — each a `while` whose termination depended on a value a mutant could pin. rewriting each so termination is **structural** (`for _ in 0..limit`, or an `if` instead of a `while`) fixed the mutant and the latent hang together. a loop that cannot be proven to terminate by reading it is a loop that can hang a tab.
- **the no-op `Speedups::apply`**, above.
- **a delegating accessor, three times** — `Status::instret`, `Decoder::durable_len`, `Decoder::series`. each returns exactly what its delegate returns, each was covered thoroughly *at the delegate*, and none was exercised *through the wrapper*. `series` returning nothing leaves every chart empty while the store beneath it is perfectly correct. stated plainly, because three is a pattern: **a delegating method is not covered by its delegate's tests.**
- and one near-miss of my own making. I wrote a mutation *exclusion* for an equivalent mutant in `derivation_tree`, and the pattern I wrote matched **both** `&&` mutants at that site — permanently silencing a live dangling-edge detector alongside the equivalent one. the fix was to extract `both_are_nodes` so the exclusion could name the harmless one alone. an exclusion is a test you are deleting; it deserves the scrutiny of one.
- also, separately, I caught myself writing an actual tautology — `expect(ticks(...)).toEqual(ticks(...))` — in `scale.test.ts`. no tool found that one; re-reading did.

## the belief that hid two real bugs

- for a stretch of this session I reported, more than once, that `stitch` did not compile.
- it compiled. I was running bare `cargo nextest run -p stitch` without `--features testing`, which the gate supplies via `EXTRA_TEST_ARGS`. stitch was at **789/789** the entire time.
- the cost was not the wasted minutes. it was that "the tree is broken over there" is an explanation, and while I held it, two *genuine* failures in the same gate output had somewhere to hide: a stale committed deps diagram, and three broken rustdoc intra-doc links. both real, both mine, both fixed only once the false belief went.
- the repo's own note says it: when the gate is red, run `--no-fail-fast`, because one broken crate reads identically to a broken tree.

## what the numbers cost

- measuring first settled the demo's shape. the drivel completion is **416.7M guest instructions** — 150× a babble completion, 17× the entire REPL boot. paced that is 42 seconds; flat out, ~11.
- so the page defaults to babble (0.28s paced, genuinely interactive) and offers the trained model as the showcase, honestly labelled. the wait is *observable* — the telemetry panel shows `kvetch.complete` open and the bytes climb while it works — which is on-thesis rather than an apology.
- the same discipline killed a cheap-looking speedup. the itest harness runs with `native-ops`, and the browser does not, and should not: unlike the other accelerators it is **not instret-transparent**. it charges the clock an *estimate* of what the interpreter would have retired — `real/charged = 1.011`, agreeing "within ~1%" on 94 of 110 scenarios. fine for a throughput budget across the suite; wrong for a page whose whole claim is that it is the real machine, deterministically. worth noting my own A/B guard would not have caught it — the probe program has no memops.

## what I'd tell myself

- **a symptom does not come with a cause, even when it arrives in the same sentence.** "an idle tab would pin a core" is one observation and one hypothesis, and only the first was tested. the wrong half survived three fixes.
- **a fix that changes nothing is a result.** the speedups moved throughput 3.5× and CPU not at all. I recorded that as expected, when it was also evidence that the thing I was speeding up was not the thing costing the core.
- **when the detector reads wrong, ask whether it can read at all.** sampling for a state that idle-skip erases inside the same slice is not a threshold problem.
- **fix the cause and the workaround becomes a test.** deleting a racy assertion is legitimate; the better move is to make it restorable, and then restore it as proof.
- **a delegating method is not covered by its delegate's tests.** three times in one arc.
- **a tool that quietly does the wrong thing costs more than one that fails.** guard against it in code, not in a README — the README is what failed first.
- **an explanation is a place for other bugs to hide.** "stitch is broken" was wrong, and it sheltered two real failures for as long as I believed it.

---

- the four milestones, archived with their findings: [snemu-wasm](../plans/legacy/snemu-wasm.md) (boot), [interactive](../plans/legacy/snemu-wasm-interactive.md) (workloads and keystrokes), [telemetry-panels](../plans/legacy/telemetry-panels.md) (structure), [metric-panels](../plans/legacy/metric-panels.md) (numbers).
- three directions were on the table and this arc took the first: **the telemetry view** (custom panels rather than Grafana), snemu-as-a-host-server, and running the model outside the OS. the other two are still there. one framing got corrected along the way and is worth keeping straight — the [physics desktop](../docs/physics-desktop-design.md) would be rendered *by the OS*, not by React; it is not on this road.
- left open by decision, not oversight: light mode (the page is dark-only, and a light palette needs its own validator run against its own surface, not an inversion of this one); histogram rendering; pinning a metric from the frame tail; wiring `wasm-pack test --node` and Playwright into `cargo xtask test`; a replay/host-socket frame source; the canvas for ramfb.
- **still undeployed.** nobody but me has seen any of it, which is the next thing to fix and by far the cheapest.
