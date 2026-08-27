# Why this project

A note to come back to when the motivation dips. Not a roadmap, not a status page —
a record of *why the approach is worth it*, written while it was obvious.

## The premise

SnitchOS is not an attempt to build a better Linux. It is an attempt to ask, one
subsystem at a time: **what would this be if it were designed now, by someone who
owned the whole stack and had no compatibility to preserve?**

Almost every primitive we use daily is a compromise struck under constraints that
no longer apply — 1970s memory budgets, a network that didn't exist yet, hardware
nobody has anymore, or simply the fact that the person who built it couldn't change
the layer below. The wheel is usually round for a reason. It is also, quite often,
round because someone was in a hurry in 1978.

## The recurring move: own the stack, put the seam anywhere

This is the thing that keeps paying, across every subsystem, and it is genuinely
unavailable to most software:

- The kernel needed deterministic tests, so **we wrote the emulator**. Then the
  emulator gave us fast itests, snapshot trees, a browser build, and a
  page-straddle regression guard nobody else could have run.
- The tests needed to prove a bug class was dead, so we made the emulator
  **deliberately hostile** — `--scramble` puts every guest frame on a
  non-contiguous physical frame, so a straddle hazard fires on every crossing
  rather than by luck.
- The model needed a trainer, so we **wrote the trainer** — no framework — and
  checked every backward op against finite differences rather than against
  another implementation that could be wrong the same way.
- The language needed effects, capabilities and telemetry to be first-class, so
  **we wrote the language**.

When you own the layer below, a hard problem upstairs is often a five-line change
downstairs. That is the whole advantage, and it compounds.

## A gallery of decisions worth remembering

**Authority is a thing you hold, not a list someone checks.** Capabilities aren't a
security feature bolted on; they're the noun the whole system is built from. And
because every holding carries a stable id and records its parent, the *derivation
tree* of any authority is reconstructable from the wire. "How did this process get
the right to do that" is a query, not an investigation.

**Everything explains itself, on the wire, by default.** Not logging — a structured
frame stream that is the system's primary output. Refusals snitch. Panics snitch.
The scheduler snitches. Nothing has to be instrumented later, because nothing was
built uninstrumented.

**One model, many projections.** One `Frame` stream becomes OTLP traces and
Prometheus metrics. One diagram model becomes several targets — a diagram is a
collector. One UI description becomes pixels, cells, and a typed interface. The
same move keeps working because the alternative — maintaining N parallel truths —
is where systems rot.

**Determinism as a discipline, not a feature.** It was adopted so tests would be
a one-run gate. It then paid out as replay, as bisection, as the ability to say
"this is the same run" and mean it bit-exactly. Constraints accepted early for a
small reason keep returning larger.

**Registries over compile-time variants.** Workloads are selected at boot from an
additive registry, not compiled in per-build. Model rungs are a config plus a
checkpoint, never a crate. Adding a thing should not fork the build.

**Measure, don't argue.** Repeatedly, and in both directions, the estimate was
wrong and the 30-line benchmark was right. "The floor is uniform, not babble."
"Volume beats purity." "It spins at -O2" was inferred and false. The measurement
is cheaper than the argument nearly every time.

**Documents are contracts too.** Every relative markdown link must resolve, checked
by the same gate that runs the tests, because a `git mv` breaks a doc silently and
did so on every archiving sweep until it was mechanised.

## The through-lines

1. **Re-derive the primitive.** Don't inherit a design; ask what it's for.
2. **Make the truth observable, and make lying structurally impossible** — liveness
   is externally observed, not self-reported; provenance is stamped where it can't
   be forged.
3. **Put the seam where it helps you**, even if that means writing the layer below.
4. **Elegance is a testability property.** The designs that turned out beautiful
   were, without exception, the ones that were easy to test.
5. **Small increments, always working.** The record is a long chain of green states,
   which is why big changes were ever possible.

## When it stops being fun

It usually stops being fun during plumbing — a driver, a refactor, an archiving
sweep. Worth remembering that the plumbing is what buys the next elegant thing:
the frame allocator bought the heap, the heap bought userspace, userspace bought
capabilities, capabilities bought everything since.

And the parts that felt like detours — the emulator, the language, the trainer —
are now the parts that make the project unlike anything else. The detours were
the project.
