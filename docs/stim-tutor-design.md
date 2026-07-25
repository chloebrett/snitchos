# The stim tutor: passive spaced repetition over real editing

**Status:** 📐 **DESIGN — far-future exploration.** Captures the 2026-07-25
idea thread: the golf-solver coach ([generative-ladder.md](generative-ladder.md),
whose v1 remains the peephole digest) grown into an adaptive editing tutor —
competence estimation, **spaced repetition scheduled over organic
occurrences in real work**, FSRS-class memory models trained on the user's
own edit log, and competence-gated crutch-masking. Explicitly not v1 of
anything; it's quarantined here because it's a product wearing a feature's
clothes — and plausibly a paper. SRS × LLM is a cavern nobody has scratched.

Related: [stim-design.md](stim-design.md) (the editor; modes as membranes),
[generative-ladder.md](generative-ladder.md) (the solver: coach → labeler →
compiler), [llm-design.md](llm-design.md) (the model arc this rides beside).

---

## The substrate advantage

Every piece of this exists elsewhere as a gimmick: hardtime.nvim blocks
arrow-spam (static config), vim-be-good gamifies drills, vimtutor is a
static walkthrough, Anki schedules flashcards. Nobody has built the
integrated tutor, for a structural reason: **a real tutor needs a complete,
structured practice history, and no other editor has one.** stim's edit log
is exactly that — every command, mode transition, burst, and pause, already
frames on the wire. Everyone else would bolt on keylogging; we have
telemetry.

## Competence: estimated, never configured

The user's trace history reveals their command vocabulary and its
reliability directly. From it:

- **Suggestions aim one notch above the revealed vocabulary** (the zone of
  proximal development): `daw` to an arrow-spammer is teaching; a recursive
  macro to them is noise; the same macro to an expert is correct.
- **Skill is stratified, not scalar** — novices arrow-spam, intermediates
  know text objects, experts write macros. The VimGolf solution spectra
  (ranked human solutions per challenge) give the strata boundaries
  empirically, and calibrate the idiomatic cost model's learnability
  weights (if 80% of players found the 12-key idiom and 2% the 6-key trick,
  the crowd just measured learnability).

## Passive SRS: the deck reviews itself

The core mechanism, and the genuinely novel part. Classic SRS schedules
flashcards; here:

- **Item** = an idiom/command pattern (`daw`, `ci(`, count-prefixing, a
  peephole rule's "better" side).
- **Review** = an *organic occurrence*: a real editing moment where the item
  applied. Used it → recall success, interval extends. The solver detected
  the missed opportunity → lapse, item comes due sooner. No flashcards; the
  deck reviews itself against actual work.
- **Teachable-moment surfacing is the scheduler's decision.** The coach
  shows the `daw` hint when `daw` is *due*, not every time it applies — the
  memory schedule replaces the arbitrary nag-throttle. This solves the
  don't-nag problem optimally rather than heuristically.
- **Drills are the fallback, not the mechanism**: only when an item is due
  and no organic occurrence has arrived does the tutor synthesize an
  exercise (from the golf corpus / task-recipe machinery). Flashcard-mode
  is the exception path.

## The scheduler ladder

Same philosophy as the model ladder — start dumb, measure, escalate:

1. **Heuristic half-life** (SM-2-ish, or a fixed decay per item) — days of
   work, good enough to prove the loop.
2. **FSRS** — the modern memory model; open, well-understood, and its
   optimizer **backprop-trains on the user's own review log**. Our review
   log is *richer* than Anki's (context, latency, burst structure, whether
   the occurrence was organic or drilled), which FSRS was never fed before.
3. **A personal model** — FSRS-class architecture retrained per-user on the
   edit log, possibly with features no flashcard system has (time-of-day,
   file type, edit-burst tempo). The fleet-retraining machinery makes this
   push-button like everything else.

Prior art to steal from honestly: FSRS itself; Duolingo's half-life
regression (2016 — the closest thing to SRS-on-organic-usage in
production); Anki's ecosystem for item design. The gap this fills: SRS over
*incidental occurrences in professional work*, not app-structured practice.

## Crutch-masking, competence-gated

hardtime.nvim's idea with the config replaced by the model: a crutch
(arrows, repeated `x`) acquires a soft cost — delay, then warning, then
off — only once the tutor estimates mastery of its replacement. Scaffold
removal as a function of measured competence, reversible if the lapse rate
says the estimate was wrong.

## The SRS × LLM cavern

Directions visible from the entrance, none explored anywhere:

- **LLM-synthesized reviews**: when an item is due with no organic
  occurrence, generate a drill *in the user's actual codebase style*
  (task-recipe machinery + Stitch corpus), not a canned exercise.
- **Interference-aware scheduling**: FSRS treats items as independent;
  embedding similarity between idioms (from the code models we're training
  anyway) could model interference and transfer — reviewing `ci(`
  partially refreshes `ca(`. Memory models with a semantic geometry over
  items.
- **Item discovery**: the peephole library is hand-written; clustering the
  solver's (actual, better) pairs finds the *user-specific* recurring
  inefficiencies — items nobody thought to author.
- **The atrophy/reliance paper.** The concern people actually hold is
  coding-skill and cognitive atrophy under LLM assistance, not keystroke
  muscle memory — and the existing literature is surveys and lab tasks,
  because no real tool can see *what the human still does unaided*.
  Provenance is exactly that instrument: a per-byte ledger of
  wrote-it-yourself vs accepted-from-model, longitudinally, in real work —
  **reliance measurement alone is a contribution**, and it requires the
  substrate property from the provenance paper (the papers connect:
  provenance = instrument, this = study design). The edit log adds
  cognitive-fluency proxies (hesitation structure, undo rates,
  time-to-green with assistance off); the verification stack scores
  outcomes. Vim vocabulary is the *model organism* — the clean,
  high-frequency, low-confound pilot domain where the methodology validates
  before aiming at the noisy target. The provocative endpoint:
  **crutch-masking generalizes to the model itself** — for skills the
  scheduler says are due, the tutor withholds or delays completion,
  producing scheduled unaided practice inside real work, measured by
  provenance, calibrated by the memory model. Deliberate practice as a
  substrate feature, aimed at the concern people actually have.

## Open questions

- Item granularity: commands, idiom patterns, or peephole rules? (Probably
  rules — they carry their own explanation and detection logic.)
- Lapse semantics: is *every* solver-detected miss a lapse, or only when
  the item was surfaced before? (Unsurfaced misses are discovery, not
  forgetting.)
- Does crutch-masking belong in the tutor or in stim's mode config with the
  tutor as advisor? (Enforcement vs recommendation — caps philosophy says
  the membrane enforces, the tutor recommends.)
- What's the minimal loop that proves passive-SRS works — one item
  (`daw`), heuristic half-life, digest-only surfacing?
