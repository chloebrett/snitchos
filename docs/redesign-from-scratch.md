# Redesign-from-scratch — the method

A recurring design-review exercise. Pick a subarea and ask:

> **If you were to redesign this entire \<subarea\> from scratch, without regard
> for development cost, what would you change?**

The *outcomes* live one per file in [`redesign-reviews/`](redesign-reviews/) (see
the index below). This doc is just the guidance on how to run one.

## Why this question works

Dropping the development-cost constraint is the whole trick. Most design
discussion is implicitly anchored to "what's a reasonable diff from here," which
protects every accreted decision. Removing that anchor surfaces the choices that
are load-bearing only because they were cheap at the time — the ones you'd never
pick on a second pass but can't see while you're defending the diff.

It also separates **bones from scaffolding**: a working, honestly-tested bring-up
accumulates sprawl (per-feature demos, hand-maintained registries, positional
ABIs) that looks like architecture but was really the cost of getting each thing
working end-to-end first. This question is the second pass.

## How to run it well

- Answer with a *ranked* list, biggest structural lever first — not a flat pile.
- For each item: name the current **smell**, the **redesign**, what it **buys**,
  and the **tension it resolves**. Ground every point in something concrete from
  the code — cite `file:line`.
- Always include a **"what I'd keep"** — the good bones. A redesign that trashes
  everything is a tell that the reviewer didn't understand why it's the way it is.
- End with a **one-thing pick** (the single highest-value first move — which may
  differ from #1 if something lower is *blocking now*) and the **caveat**: "this is
  only visible because the first pass worked."
- For a large subarea, run it as **parallel forks that inherit full context** (one
  per sub-component), then synthesize into one ranked entry — cross-cutting themes
  hoisted to the top, and independent triangulation on the same gap treated as
  signal.

## Where the outcomes go

Each review is its own file in [`redesign-reviews/`](redesign-reviews/), named for
its subarea, dated, with a one-line pointer added to the index here.

## Index of reviews

- [Userspace (`user/`)](redesign-reviews/userspace.md) — 2026-07-04. Museum of
  kernel-feature demos; top lever = programs-as-files, not statics baked into the
  kernel.
- [Stitch tokenizer, parser & interpreter](redesign-reviews/stitch-tokenizer-parser-interpreter.md)
  — 2026-07-05. Private tree-walker pipeline becoming a shared IR; top levers =
  source spans everywhere and a reified evaluator (fuel + trampoline + stack).
