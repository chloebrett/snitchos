# Stock-take, 2026-08-06

> **Superseded in part, within hours — by design.** Since this was committed
> (`07d1658`) the tree has moved: debt **#16's first precondition is DONE**
> (`OptLevel::Mid => Some("1")`, so the four-rung ladder is declared rather than
> inherited), debt **#19 has a plan** ([../plans/board-image-opt-level.md](../plans/board-image-opt-level.md)),
> and three of the Stitch gaps §2C lists now have plans of their own
> ([stitch-language-improvements.md](../plans/stitch-language-improvements.md),
> [stitch-map-you-can-build.md](../plans/legacy/stitch-map-you-can-build.md)) with work in
> flight in `stitch/src/natives.rs`. Per-item corrections are inline below. A
> stock-take is a snapshot, and this line exists so nobody reads it as a standing claim.

Where the project is, what's genuinely in flight, and what's dangling. Written by
reading `plans/` status headers, `docs/debt-register.md`, `docs/roadmap-and-milestones.md`,
`notes/loose-ends-2026-07-29.md`, `notes/batch11-training-findings.md`, and posts
68–80 + stitch 17–18 — then checking the claims against the tree rather than
trusting them, because posts 69, 78 and 79 are all about what happens when you don't.

Tree state: clean, on `main`, HEAD `adc8802` ("Batch 11 training findings").

---

## 1. Where the OS itself is

**v0.13 shipped.** `init` is the userspace delegation root and the default boot;
cap-id spine, `WaitAny`, `EndpointCreate`, transitive `Revoke`. The itest suite is
**132 scenarios** (the harness reports it; posts 71–80 all quote 130, which was true
when each was written), deterministic under snemu, and green at opt-1, opt-2 and opt-3.

Also shipped since the roadmap was last written: preemption + priorities (v0.8), IPC
(v0.9), RAMfs (v0.10), console input + spawn-with-caps (v0.11), Exit/Wait/reap +
notifications (v0.12), kernel stack guard pages, span/metric name GC,
userspace-defined metrics, cap revocation, the `kernel-core` split into five concept
crates, UART telemetry (M2), UDP telemetry (M2.5), FP context switching, and first
light on the VisionFive 2.

**The roadmap doc is the single most out-of-date artifact in the repo.**
`docs/roadmap-and-milestones.md:32` still shows v0.11 as 🚧 and describes v0.12/v0.13
as future work — and v0.13's description ("the explicit-authority shell") is not what
v0.13 turned out to be (`init` bootstrap). Anyone reading it to orient themselves gets
a picture two milestones stale. This is exactly post 79's "a note that contradicts its
own source is worse than no note".

Nominally next on that roadmap: **v0.14, a text editor** — which in practice is `stim`,
already well advanced as a Stitch program, so the milestone and the side-project have
converged without the roadmap noticing.

---

## 2. Active fronts

Six threads are genuinely live. Roughly in order of recent commit weight:

### A. kvetch / the model ladder — the loudest thread

Where it stands: drivel answers Tab at the on-target Stitch prompt, byte-identical
through the emulator, gated by an opt-in `slow` scenario. Checkpoint provenance is
enforced by a vocab fingerprint. Constrained decoding is lazy-legality (draw, ask,
strike) rather than full masking. `plans/kvetch-drivel-on-target.md` is archived.

The live question is **corpus strategy, and it just changed**. `notes/batch11-training-findings.md`
is the current front edge:

- Volume has **largely stopped paying**. +55.7% more corpus bought 0.025 nats, against
  +47% buying 0.111 last time — a 4.6× collapse in marginal return. Per million tokens:
  0.080 nats → 0.011.
- Finishing batch10 (712→1000 candidates) bought **nothing** (+0.0055, wrong sign, both seeds).
- The 24 hand-polished exemplars that reach training are worth **~20× per token** vs
  generated corpus — but that is a **ceiling, not an estimate**, and the confound could
  account for all of it.
- Best checkpoint the project has produced is `drivel-D-b9b10b11` at **2.5309**, and it
  is deliberately *not* promoted to the embedded checkpoint.

So the four-batch "volume beats purity" doctrine has hit its knee, and there is no
written successor strategy. That's the decision this thread is actually parked on.

### B. glitch / audio — v1 complete, v2 at 5/9

The DAC is a capability held by a userspace server; the async ring, `AudioEnqueue`,
and the `TimerWheel` multiplexing the single hardware timer between scheduler and audio
deadlines are all live end to end (post 71). Behaviour-preserving through the itest
suite, plain and scrambled.

**Remaining (6–9):** mixing, init-delegated `AudioSink`, snemu PCM capture, and the
acceptance itests — which is where the XRun observable actually fires. It is **wired
but dormant**: `AUDIO_ACTIVE` ships hardcoded `false`, so the drain reads an empty ring
as `Idle`, never `Underrun`. The OS's first real-time deadline exists structurally and
has never been made to miss.

### C. Stitch the language — several open seams

Type system: Stage 6 (capabilities-as-effects) complete; **generics (G3–G6) are what's
left**. Native tests: increments 1–8 done, **increment 9 is all that remains** (and it
is also debt #17 — see §4). 30 gold example programs shipped with 279 native tests.

The stitch-18 batch left a specific, well-characterised list open:

- `Set<T>` — designed in `plans/lang/01-*.md`, **never implemented at all**.
- `Either`/`Left`/`Right` — in the design doc, absent from the registry. Three words to close.
- `Map<K,V>` cannot be built from a runtime-computed sequence of pairs (design question, not omission).
- No `Float` math whatsoever — no `pow`/`exp`/`sqrt` (design question; `elo.st` worked around it).
- **The checker's `walk_effects` is stricter than the runtime for plain calls, and blind
  to method calls and higher-order calls entirely.** This is the one that reads as a real
  inconsistency rather than a documented gap.

**Update (same day):** this list is no longer just a list — `Map` construction now has
[stitch-map-you-can-build.md](../plans/legacy/stitch-map-you-can-build.md) and the rest are
scoped in [stitch-language-improvements.md](../plans/stitch-language-improvements.md),
with work in flight in `stitch/src/natives.rs`.

### D. VisionFive 2 — booting, mostly blocked on one driver

M1 first light achieved on hardware. UART telemetry (M2) and UDP telemetry (M2.5) are
both proven under the deterministic gate. **The entire remaining cost of network
telemetry is PR 8, the JH7110 GMAC driver** — MAC init, DMA descriptor rings, PHY
bring-up over MDIO. It is its own project, behind a `NetDevice` trait that is already
proven.

`plans/vf2-display.md` is **proposed, nothing built** — and it argues (persuasively)
that the display driver is what the port is *for*, given the physics-desktop and arcade
directions.

### E. snemu — the emulator keeps earning its keep

Shipped since the last sweep: PLIC model, interactive mode (`boot --interactive`, raw
mode, Ctrl-] escape), ramfb device model, block JIT at parity, snapshot-tree sharing,
rounding-mode work. `plans/snemu-wasm.md` is Active with the premise verified live
(`--lib --target wasm32-unknown-unknown` already builds).

One named performance lead, unpulled: **the block JIT lowers no FP family and no
MULDIV**, so a matmul inner loop compiles to two-instruction blocks and the block
machinery is pure overhead. Pays for audio and on-target Stitch floats, not just drivel.

### F. Corpus generation infrastructure

`cram-gen` produced batch10 and batch11 against a frozen recipe sheet: parse deaths
45% → 15% → 14%, wall clock 87s → 59s per candidate. The `long` cap, `abandoned` field,
and incremental manifest writes all landed.

**`corpus-mvp.md` is delivered and its gate passed by 13×** — it asked for 500k validated
tokens and drivel beating babble-trained drivel; there are ~6.7M tokens and drivel sits
at 2.5309 against the 2.742 uniform floor. Three named gaps survive it: **no dedup of any
kind** (increment 6), **no model-response cache** (increment 5), and **exemplars are
fixed rather than recipe-matched** (increment 4 — the fixed pair buys a fully invariant
prompt prefix instead, which is better caching than the plan's bucketing scheme, but it
forgoes matching just as batch11 measured exemplars at ~20×/token). Increment 7,
constrained decoding, is **demoted from "the biggest lever" to a recorded future
direction** — it collapses the parse-yield term, and parse yield stopped being the
binding constraint the moment the rewind guard took deaths to 14%.

**`stage-0-validator-funnel.md` is partly delivered.** The funnel is built and in daily
use — but it landed inside `cram-gen` + `stitch/src/gate.rs` rather than as the planned
standalone `sift` crate, because the corpus MVP needed a gate before this plan was picked
up. Increments 1–3, 9 are done (and the run stage is *better* than scoped, since native
`test`/`expect` landed in time). Increments 4–8 and 11 — alpha-normalization, MinHash
dedup, the production-coverage curve, the per-recipe yield report, distribution-vs-real
deltas, and the augmentation tier — are unbuilt. Splitting out `sift` remains open.

**That backlog is worth more now than when it was written.** Everything unbuilt in it is
a *diversity* instrument, and diversity is the axis nobody has measured — while volume,
the axis that was carrying everything, has just stopped paying. batch11's own analysis
noticed domains collapsing toward ~8 structural archetypes, which is exactly what
increment 5's per-recipe dedup rate exists to detect. Increment 11's augmentation tier is
also the one lever the volume finding does *not* rule out: a 2–4× multiplier on survivors
that costs no generation wall-clock.

---

## 3. Loose ends — the 2026-07-29 sweep, re-checked

| # | item | status today |
|---|---|---|
| 1 | dead-code warnings on every `x` | ✅ fixed |
| 2 | uncommitted `workload_features` fix, no test | ✅ committed (`e397d1d`); **still no test pinning it** |
| 3 | stale self-contradicting plan headers | ✅ fixed *for those two* — see §5, the class recurred |
| 4 | **a dead server hangs its clients silently** | ❌ **still open** |
| 5 | `kvetch-drivel-on-target` 🚧 with boxes unticked | ✅ closed, archived to `legacy/` |
| 6 | three named performance leads | ❌ **all three still unpulled** |
| 7 | debt #16 (the "opt≥2 UB class") | ✅ root-caused; **the pin decision remains** |
| 8 | batch10's three corpus items | ❌ **still open** |
| 9 | glitch increment 5 | ✅ superseded — v1 complete, v2 at 5/9 |
| 10 | `prelude.st` had never had a test | ✅ fixed (20 tests) |

**#4 is the one I'd promote.** Nothing in `kernel-proc/src/ipc.rs` refuses a call on an
endpoint whose only receiver has exited. In a codebase whose stated rule is "refusals
snitch, never silent," this is the one place a failure is silent — and it surfaces two
processes away from its cause. Post 74 independently rediscovered it from the kvetch
side ("a mispaired server answers rather than dies"), and post 72's FP work hit the same
shape. Three arcs have now bumped into it.

**#6, restated by post 80**, in the order the profile argues for:

1. The long-completion profile — the one measurement that would choose between the other
   two, and it wants an idle machine.
2. Borrowed weights — `Model::decode` copies into an owned `Vec<f32>`, so the process
   holds ~4.2 MB rodata *plus* ~4.2 MB heap. This is what would let the 64 MiB machine
   come back down.
3. FP in snemu's block JIT (see §2E).

---

## 4. The debt register

Correctness gaps and deferred placeholders that are current:

- **#16 — the opt-1 userspace pin.** No longer a mystery; now a *decision* with three
  written preconditions. **Precondition 1 landed 2026-08-06** — `OptLevel::Mid =>
  Some("1")`, so the ladder is monotonic Low(0) → Mid(1) → Hi(2) → Max(3) by declaration
  instead of by inheriting `build.rs`'s default, and the pin is a selectable regime
  rather than an invisible one. Behaviour-preserving, as predicted. **Still open:**
  deciding what exercises whichever level stops being the default (there is no CI; the
  gate is what a human runs), and guarding the latent FS talc-OOM symptom before removing
  the thing that would mask it. Its tell is a flood of 68 KiB `MapAnon`s.
- **#17 — the canon's 89 native tests (plus 279 in `examples/`) have never run on
  target.** The canon stratum's whole justification is "validated by use"; that holds for
  the programs and not for their test suites. Closing it *is* stitch-native-tests
  increment 9.
- **#18 — no `ConsoleMode::Quiet`.** Measured on hardware: typing at the `stitch-drivel`
  prompt renders as `// he` + `hb 7` + `llo`. `console=frames` is wrong advice on the
  board (the collector has no serial source). Decided, unimplemented: roughly a variant,
  a parser arm, and two match sites.
- **#19 — `cargo xtask image` has no `--opt`**, so every board image is a debug build.
  An opt-3 image was produced by hand (7.78 MB → 6.13 MB) and **any later
  `cargo xtask image` silently overwrites it**. Note that landing this makes release
  `vf2` images routine — and that regime is where both the `tp`-truncation and the SBI
  `a1`-clobber bugs lived, the latter hidden *precisely because* board images are debug.
  **Now has a plan** ([../plans/board-image-opt-level.md](../plans/board-image-opt-level.md)),
  which adds the detail that `Low` passes no `--release`, so the embedded userspace is
  **opt-0** rather than the opt-1 I assumed — and that the hand-built opt-3 image has
  never been recorded as booting, so optimized-on-hardware is unproven.
- **#8/#9/#10** — one-flavour `kernel::sync`, the `TX_STAGING` hack, hardcoded QEMU-`virt`
  MMIO + the parked DTB walk. All genuine deferrals.

---

## 5. Stale records — found, and fixed 2026-08-06

Post 79's lesson is that transcription is the least-instrumented hop in the system.
Nine instances were standing; all nine have been corrected. Listed here as the record of
what was wrong, since the corrected files no longer show it.

**Fixed:** `docs/roadmap-and-milestones.md` (shipped list + road section + hardening,
and a standing caveat that the version ladder is a poor index now that most work arrives
as parallel tracks) · `plans/glitch.md` (header) · `plans/stitch-examples-corpus.md`
(header) · `plans/drivel.md` (header) · `plans/network-telemetry.md` (status + all eight
acceptance boxes) · `plans/uart-telemetry.md` (status, per-step) ·
`plans/visionfive2-port.md` ("Next: M2") · `notes/batch11-training-findings.md` (the
retracted gate-failure caveat) · `notes/loose-ends-2026-07-29.md` §9 (a wrong finding,
kept visible with its correction rather than deleted).

The original findings:

1. **`docs/roadmap-and-milestones.md`** — two milestones behind, and v0.13's description
   doesn't match what v0.13 became. (§1)
2. **`plans/glitch.md:3-6`** — header says *"IN PROGRESS — kernel spine done (Increments
   1–4). Next: Increment 5"*. The body of the same file marks increments 1–8 ✅ DONE,
   carries a `## v1 COMPLETE` section, and records the in-kernel beep as retired. The
   plan should archive to `legacy/`. Note this also means **loose-end #9's diagnosis
   ("glitch stalled at the Increment 5 boundary", "the layering violation it exists to
   fix is still standing") was read off that stale header and was wrong** — 5a landed.
3. **`plans/stitch-examples-corpus.md:3`** — *"📐 PLAN — not started."* `examples/stitch/`
   contains 30 programs and stitch 18 is the write-up of finishing them.
4. **`notes/batch11-training-findings.md:263-265`** — claims `cargo xtask test` has a
   pre-existing failure (`mutant_plan_tests::the_derived_plan_matches_the_previously_hardcoded_set`)
   on a clean tree. Verified today: `cargo nextest run -p xtask` is **48/48, green**,
   that test included. The loose-ends note had already retired this claim on 07-29; it
   came back in a note written a week later.
5. **`plans/network-telemetry.md`** — "Status: Active" with all eight acceptance boxes
   unticked, while PRs 1–7 are demonstrably shipped: `kernel-net` exists, the
   `net-telemetry-over-udp` scenario is registered and passes under snemu, the collector
   has `--udp`. Exactly the pattern loose-end #5 flagged for kvetch-drivel-on-target —
   a plan that can neither be archived nor read as a record.
6. **`plans/uart-telemetry.md`** — "Status: Active" with no top-level statement of where
   it got to. Step 9 (`UartFrameSink`) is done (`kernel-obs/src/uart_sink.rs`); Step 10
   (collector `--serial`) is not — `collector/src/source.rs:5` still names it as future.
7. **`plans/visionfive2-port.md:59`** — "Next: **M2 — telemetry over UART**". M2 is
   substantially done and M2.5 shipped past it.
8. **`plans/drivel.md:3`** — "📐 PLAN — not started", on a plan whose model now answers
   Tab on target and whose successor question (the volume knee) is the project's live
   strategic decision. Two `Status: COMPLETE` markers sat below it in the same file.
9. **The itest scenario count.** Posts 71–80 and several plans quote **130**; the
   harness reports **132**. Each quote was true when written. Worth noting because it is
   the benign version of the same drift — a number copied forward past its source.
10. **`plans/corpus-mvp.md` and `plans/stage-0-validator-funnel.md`** — both said
    "📐 PLAN — not started". The first is delivered with its gate passed 13× over; the
    second is partly delivered. Both now carry per-increment status tables.

    **This one is worth recording as a method failure, not just a stale header.** In the
    first pass I declined to touch these two, on the grounds that I could see a funnel in
    `cram-gen` but couldn't cheaply tell whether it *superseded* the plans or was a
    narrower slice. That was the wrong test. The plans are lists of numbered increments,
    and the cheap check was to walk the list — nine greps, about ten minutes — not to
    reason about whether a subsystem "corresponds". I applied the caution correctly and
    to the wrong question, which is a subtler version of the same error post 79 describes:
    the check I ran was the one that came to mind, not the one that would have settled it.

---

## 6. Known-open items with no plan file

Things named only in a post or a code comment, i.e. the shape post 72 says "gets
rediscovered expensively":

- **FP ownership is per-process, the registers are per-task.** `Process::fp_enabled` is
  read by the switch. Identical only while each process has one task — true today,
  verified nowhere. The trigger is the first thing that gives a process two tasks; the
  fix is a flag on `Task`, and the reason it isn't done is that setting it lives in
  `try_enable`, in trap context, which would have to reach the scheduler lock.
- **The cram-gen abandon-path cap leak.** Confirmed still open at
  `cram-gen/src/lib.rs:466`: the abandon path calls `model.complete` unguarded, with no
  `max_bytes` check, so capped-length enforcement doesn't apply to the path that produces
  *finished* programs. 15 files in each of batch10 and batch11 escaped, up to 28 KB.
  Post 78 called it a ten-line fix that should land before the next batch. Nothing
  downstream is wrong (they belong in the corpus anyway) — but the guard's doc comment
  lies.
- **The virtio_mmio DEDUP.** `virtio_net.rs`'s `read_reg`/`write_reg`/`transmit` mirror
  the console driver and can't be shared because each device owns its own `static mut`
  queues. Flagged deliberately during post 70 to avoid editing the console file mid-churn.
- **Heartbeat-driven telemetry batching** — today it's one datagram per frame, correct
  but wasteful; telemetry trickles below the MTU so batch-overflow never flushes.
- **QEMU `--engine qemu` parity for net telemetry** — needs `-device virtio-net-device`
  plus a host UDP capture.
- **The exemplar deconfounding pair (G/H).** Corpora built, leak check done, commands
  written out in the batch11 note's "Open" section, launched but not finished. Until it
  lands, the ~20×-per-token exemplar figure is a ceiling.
- **The `--drop-stage` / comment-stripping findings** are measured-harmful and off by
  default; that's settled, not open — recorded here so nobody re-opens it.
- **`canon.rs` does not call `gate::run`.** `stitch/src/gate.rs:64-73` and
  `stitch/tests/canon.rs:39-44` are two independent spellings of the same
  parse → lower → check chain, and `gate.rs`'s doc comment *asserts* they match with
  nothing enforcing it. `stage-0-validator-funnel.md` increment 2 required the call
  specifically so they could not drift. Small fix; same shape as debt #13's
  double-encoded `satp_for`.
- **Untested candidates pass the corpus gate.** stage-0 increment 3 says a candidate with
  no `test` items must die at the run stage; `gate.rs` allows `Ok { tests: 0 }`.
  Unresolved whether that is a deliberate relaxation or drift — flagged in the plan.
- **No dedup in the corpus pipeline, exact or near.** corpus-mvp increment 6 asked for
  exact dedup; stage-0 increments 4–5 for alpha-normalization + MinHash. None built.
  batch11 found domains collapsing toward ~8 structural archetypes, which is precisely
  what the per-recipe dedup rate was designed to detect.
- **No model-response cache**, so re-running the gate or extractor over a batch costs
  generation time rather than replaying saved `.raw.md` files.

---

## 7. If I had to pick

Cheap and closes a claim:

- Fix the four stale records in §5 (an hour, and one of them already caused a wrong
  loose-end diagnosis).
- The abandon-path cap leak (ten lines).
- Land `Either` in the stdlib (three words) and decide whether `Set<T>` is in or out.

Substantive and unblocks a class:

- **Loose-end #4** — a dead server should refuse its clients, not hang them. Three
  separate arcs have now hit it, and it violates the project's own loudest rule.
- **Glitch increment 9** — arm `AUDIO_ACTIVE` and make the first real-time deadline
  actually miss, asserting on the waveform rather than the counters. The detection
  machinery is built and dormant; this is what makes it real.
- **Decide the corpus strategy after the volume knee.** Finish G/H, then choose: more
  exemplars, a different generator regime, or stop growing the corpus and move up the
  ladder. Right now the doctrine has expired and nothing has replaced it.
