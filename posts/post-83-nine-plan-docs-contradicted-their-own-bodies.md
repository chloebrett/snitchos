# Post 83 — nine plan docs that contradicted their own bodies

- this started as a stock-take: where is the project, what is genuinely in flight, what is dangling. it should have been an afternoon and a note.
- what it turned into was nine documents that were wrong — one of which had **already manufactured a false finding in a different document** — and then me committing the same species of error inside the session that was auditing for it.
- [post 79](post-79-correcting-a-wrong-diagnosis.md) is the ancestor: *a note that contradicts its own source is worse than no note.* this is that one level down. these notes did not contradict some distant source. **they contradicted the rest of their own file.**

## the header and the body, in one file

- `plans/glitch.md` opened with "🚧 IN PROGRESS — kernel spine done (Increments 1–4). **Next: Increment 5**", and named the first move: extract a `synth` crate, because `user/` must not depend on `kernel-devices`.
- forty lines further down, the same file marks increments 1 through 8 as complete, carries a `## v1 COMPLETE` section, and records the in-kernel beep as retired. 5a — the layering fix — had landed months earlier.
- so the file asserted two incompatible things about itself, and the header won, because the header is what people read.
- and it is not inert. `notes/loose-ends-2026-07-29.md` §9 quoted `glitch.md:6-9` and reported glitch stalled, concluding "the layering violation it exists to fix is still standing." a confident finding, correctly cited, about a violation that had been fixed. **the citation is what made it look researched** — quoting `:6-9` and never reading `:70+` is indistinguishable, from the outside, from having read the file.
- three more of the same shape. `plans/drivel.md` said "📐 PLAN — not started" above two sections of its own marked `Status: COMPLETE`. `plans/stitch-examples-corpus.md` said "not started" with all 30 programs shipped and 279 native tests passing. `docs/roadmap-and-milestones.md` had v0.11 in progress and v0.12/v0.13 as future work, two milestones after both shipped — and described v0.13 as a thing v0.13 did not turn out to be.
- every one of them **under**-claimed, which is [post 56](post-56-stale-docs-are-stale-the-same-way.md)'s finding arriving again and is structural rather than lucky: nobody writes "finished" about unfinished work, and everybody forgets to go back and say they finished. that gives a usable prior — when a header and a body disagree, believe the body.

## and then I did it

- two plans, `corpus-mvp.md` and `stage-0-validator-funnel.md`, both said "not started". I looked, found a working funnel in `cram-gen`, could not cheaply tell whether it *superseded* those plans or was a narrower slice of them, and reported them as too ambiguous to judge.
- that reads as caution. it was the wrong test. a plan is a numbered list of increments, and the check is to walk the list — nine greps, about ten minutes.
- walking it: **`corpus-mvp` had passed its own gate by 13×.** it asked for 500k validated tokens and a model beating babble-trained drivel; there are ~6.7M tokens and drivel sits at 2.5309 held-out against the 2.742 uniform floor.
- it had also gone *past* its own scope. the plan explicitly skipped the run/test stage — "execution adds failure modes for little MVP value" — and `stitch/src/gate.rs` runs each candidate's own `test` items anyway, because native tests landed in between. that file cites `plans/corpus-mvp.md` in its module doc, which is how I found it. **the code knew which plan it belonged to; the plan did not know the code existed.**
- my error and §9's are the same species: both swapped the settling check for an easier one. the difference is that mine came with a hedge attached, and a hedge reads as rigour.

## what the tidying found, because a doc pass that finds nothing is suspicious

- **`canon.rs` does not call `gate::run`.** `stitch/src/gate.rs` and `stitch/tests/canon.rs` are two independent spellings of the same parse → lower → check chain, filtered identically to `Severity::Error`. `gate.rs`'s doc comment *asserts* "the chain matches `tests/canon.rs` exactly", with nothing enforcing it. `stage-0-validator-funnel.md`'s increment 2 required the call **specifically so the two could not drift** — the lift happened, the call did not. same shape as `satp_for` sitting open-coded twice.
- **the corpus pipeline has no dedup at all**, exact or near. one plan asked for exact dedup, another for MinHash over an alpha-normalised stream; neither exists. batch11 then found domains collapsing toward ~8 structural archetypes, which is precisely what the per-recipe dedup rate was designed to detect.
- **untested candidates pass the corpus gate.** stage-0 says a candidate with no `test` items dies at the run stage — "an untested candidate is not a validated token". `gate.rs` returns `Ok { tests: 0 }`. still unresolved which is right, and it changes what "ok" means in every manifest written so far.
- and the strategic one the stock-take could only frame rather than answer: "volume beats purity" had hit its knee with nothing replacing it. that has since been settled the other way — quip beats drivel by 0.162 nats for 1.8× training wall-clock, against 0.025 nats for 14.9 hours of generation. scale the rung, not the corpus. naming the hole was the contribution; filling it was a different session's.

## the gate

- `cargo xtask plan-status`, now inside `cargo xtask test`, checking the two things a machine can check without guessing at meaning: **every plan carries a dated `**Status (YYYY-MM-DD)**:` header**, and **every plan is reachable from `plans/README.md`**.
- the index is the real target. every sweep this repo has done found `plans/README.md` stale, always the same way — correct when written, wrong within weeks, and wrong *precisely where work was happening*, because that is the only place anything changes.
- three decisions worth keeping:
  - **it does not fail on age.** a gate that reddens through the passage of time alone teaches people to ignore it, and the only way to green it without doing the work is to lie about the date — which corrupts the single signal the convention exists to carry. it prints a staleness readout instead, oldest first, and leaves the judgement to a human.
  - **the header window is bounded to 15 lines.** `stim-v1.md` and `visionfive2-port.md` both carry per-*step* `**Status**:` notes deep in their bodies. a whole-file scan reads the first of those as the plan's status, which is exactly the wrong answer.
  - **non-plans opt out through a deny-list with written reasons**, never an allow-list. this repo has already been bitten by allow-lists-by-omission — it is how three crates sat un-linted for months.
- 25 plans, all dated, all indexed, 6 reference docs skipped.

## what it does not do, and I want this written down

- **the gate cannot tell whether a status header is true.** `glitch.md`'s header — the one that manufactured the false finding, the reason any of this happened — would pass it. add a date and it becomes a well-formed, indexed, confidently wrong claim.
- so what landed closes the *reachability* half and the *freshness-claim* half. the truth half is still a human reading a file to the bottom, which is the exact step that failed here twice in one session.
- that is not an argument against it. `links` does not check that a link points somewhere *useful* either, and it has caught every archiving sweep this repo has done. a check that eliminates one failure mode completely beats one that gestures at three.
- but the honest status is: **the thing that actually went wrong is not gated, and I do not currently know how to gate it.** saying so is the point — an unstated limitation is how a gate becomes its own confidently-wrong header.

## what I learned

- **a stale header is not a stale header, it is a wrong answer with a citation.** §9 was not sloppy; it quoted a file and a line range. quoting the part that was wrong is what made it persuasive, and there is no amount of "read more carefully" that catches that. reading to the bottom of the file is the entire defence.
- **when a header and a body disagree, believe the body.** the drift is one-directional for structural reasons, so this is a prior rather than a coin flip.
- **a plan is a numbered list, so check the numbers.** I substituted "does a system like this exist?" for "did increment 3 land?" — the first is a judgement call, the second is a grep, and I picked the one that let me remain uncertain.
- **a hedge reads as rigour and is not.** declining to conclude looked like the careful move. it was an unfinished check with a disclaimer attached, and the disclaimer is what stopped anyone — me included — from noticing it was unfinished.
- **gate what a machine can decide, and write down the rest explicitly.** presence of a date and reachability from an index are decidable. whether a sentence is true is not.

## where it leaves things

- one note, about a dozen documents corrected, no production code touched by the audit itself, and one gate that did not exist before.
- `notes/stock-take-2026-08-06.md` now opens with a superseded-in-part banner, because it was outrun within hours of being committed — debt #16's first precondition landed, #19 got a plan, three Stitch gaps got plans of their own.
- that banner is the convention arguing for itself. a snapshot that does not say it is a snapshot is just the next stale header, waiting.
