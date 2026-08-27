# Plan: A Tour of SnitchOS — the tracer chapter

**Status:** 📝 **PLANNED — not started.** Written 2026-08-27.

Implements stage 3 of [../docs/tour-and-user-docs-design.md](../docs/tour-and-user-docs-design.md)
(snapshot-pinned chapters + the CI contract), with two of that design's calls
overturned by measurement and one by architecture. Stages 1 and 2 of its
sequencing — the wasm MVP and the frame-fold store with panels — shipped in the
arc closed by [../posts/post-82-a-symptom-arrives-with-a-diagnosis-attached.md](../posts/post-82-a-symptom-arrives-with-a-diagnosis-attached.md).

## Goal

One chapter of prose, beside a real SnitchOS booted to the exact world-state it
describes, with its claims asserted against the decoded frame stream in
`cargo xtask test` — so that a kernel change which falsifies the prose fails the
gate.

## What was decided, and why

Four decisions were taken before this plan, each reversing or sharpening the
design doc. Recorded here because the reasoning is the expensive part.

### Replay, not snapshots

The design's load-bearing idea was that snapshot IDs become content-addressed doc
assets. That machinery does not exist: snemu's snapshot primitive is
`#[derive(Clone)]` on `Machine` (`snemu/src/machine.rs:44`), an in-process deep
copy with no serialization and no content address. Guest RAM in the browser is
128 MiB (`web/src/snemu.ts:47`), so a naive per-chapter blob is 128 MiB before
compression.

Determinism makes the blob unnecessary. **A chapter declares initial conditions
and the tab executes to its anchor** — byte-identical to a restored snapshot,
because that is the property snemu already sells and the itest suite already
depends on. It costs boot time instead of page weight, and boot is cheap: post 82
measured the drivel completion at 416.7M guest instructions and called it 17× the
entire REPL boot, putting boot near 25M — under a second at the 38.9 MIPS the
accelerated browser build reaches.

Snapshots stay available as a **later optimization**, not a prerequisite. See
*Where to from here* for the condition that would force them.

### Anchor by predicate, not by instret

An anchor defined as an instret count is invalidated by every kernel rebuild. An
anchor defined as a **predicate over the decoded frame stream** — "the third
`kernel.heartbeat` SpanStart", "the `CapEvent::Transferred` that hands the FS
server its endpoint" — re-finds itself across rebuilds, and is the same contract
the itest scenarios already use.

Instret demotes to a cache: run to the remembered count, verify the predicate
holds there, scan forward if it does not. This answers the kernel-compatibility
question directly — compatibility is *validated by re-evaluating the predicate*,
not asserted by a fingerprint. A kernel change that stops a predicate firing
fails the gate, which is the intended behaviour rather than a problem to route
around.

A build fingerprint is still needed for shared **diverged** URLs (below), where
positions are inherent. There it **warns**, and it can warn intelligently: replay
and report whether the originating chapter's predicate still holds.

### An SPA, not a docs framework

The tour is an app shell with documents inside it, not a document site with
widgets on some pages. The deciding requirement is that telemetry panels are
**global overlays openable on any page**, which means the emulator must stay
alive and continuous across navigation.

A statically-rendered multi-page site reloads per navigation, discarding the wasm
instance and 128 MiB of guest RAM. Astro's `<ClientRouter />` with
`transition:persist` does preserve island DOM and JS state across navigations, so
it is possible — but it is a feature designed around counters and audio players,
and betting a multi-MB wasm VM driving a `requestAnimationFrame` pump on it is
unproven territory. With client-side routing the problem is absent rather than
solved.

Astro+Starlight was evaluated seriously and rejected on that basis alone; its
content-collection schema validation and islands model are genuinely good, and
its Cloudflare/Netlify/Biome-scale usage says it does not run out of road. It
remains the right answer for a **reference** section later (syscall tables, frame
variants, cap semantics) where pages really are documents and no live emulator is
needed. That is additive whenever wanted.

What the SPA must replicate is catalogued in *Step 7* and *Where to from here*.

### The tracer needs no navigation

With one chapter there is no sidebar, no search, no table of contents, and no
prev/next. All of it is deferred to chapter two. The tracer proves exactly one
path: **manifest → replay → anchor → assert → render.**

## Architecture

Four pieces, with the seam between Rust and TypeScript at the manifest.

| Piece | Where | Owns |
|---|---|---|
| `tour` crate (new) | `tour/` | Chapter manifest schema, anchor predicates, the predicate evaluator over decoded frames. Pure logic, no snemu, no MMIO. Host-tested; also linked into `snemu-wasm` so the browser evaluates the same predicates the gate does. |
| `snemu-wasm` | existing | `run_to_anchor(predicate)`; the stamped input log. |
| `xtask-itest` | existing | The drift check: boot each chapter under snemu, replay to its anchor, assert its claims. Lives here because snemu is already linked here — an edit to snemu must not recompile lean `xtask`. |
| `web/` | existing | SPA shell, routing, MDX chapter rendering, the emulator above the router, panels as overlays. |

One predicate evaluator, compiled twice: the browser stops at the same frame the
gate asserts at. A second implementation in TypeScript would be two definitions of
"the anchor" free to drift.

## Acceptance criteria

- [ ] A chapter is declared by data — workload, anchor predicate, claims — parsed by `tour` and rejected at parse time if its workload is not in the kernel's own registry.
- [ ] Given a decoded frame sequence, the predicate evaluator identifies the anchor frame, and does not fire early on a near-miss.
- [ ] `snemu-wasm` records every pushed input as `(instret_at_delivery, bytes)`, retrievable in order.
- [ ] Booting `init` and running to the cap-delegation anchor stops at the same instret on two consecutive runs in the same process.
- [ ] The cap-delegation chapter exists as prose plus a manifest entry.
- [ ] `cargo xtask test` fails when a chapter's claim is falsified, and the failure names the chapter and the claim.
- [ ] Navigating to the chapter URL, then browser-back and browser-forward, restores the right chapter and its scroll position; focus moves to the new page heading on each route change.
- [ ] Opening the chapter boots a guest, replays to the anchor, and renders the cap derivation tree at that state.
- [ ] After the reader types at the guest, the UI shows the state has diverged from the anchor, and offers a reset that returns to it.

## Steps

Every step is RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test. Present acceptance criteria and get confirmation before writing
code for each step.

### Step 1: A chapter manifest that rejects an unknown workload

**Acceptance criteria**: `tour::Chapter::parse` accepts a manifest naming a real workload and returns its fields; it returns an error naming the offending value for a workload string the kernel's `workload=` parser does not know. The check is against `kernel_boot::bootargs`, not a hand-copied list — the existing web workload-picker test is the precedent.
**RED**: A test parsing a manifest with `workload = "no-such-workload"` and asserting the error names it.
**GREEN**: The `Chapter` struct, serde derive, and the registry check.
**MUTATE / KILL MUTANTS / REFACTOR**: per the standard loop.
**Done when**: Criteria met, mutation report reviewed, commit approved.

### Step 2: An anchor predicate that fires on the right frame

**Acceptance criteria**: Given a `Vec<OwnedFrame>`, the evaluator returns the index of the frame satisfying the predicate. For an "nth occurrence" predicate it does not return the (n−1)th. For a predicate never satisfied it returns `None` rather than the last frame.
**RED**: A test over a hand-built frame sequence containing a near-miss before the true anchor.
**GREEN**: The `Anchor` enum and its evaluator.
**Note**: Predicate vocabulary starts minimal — enough for the cap-delegation chapter and no more. Additive later, same as `WorkloadKind`.
**Done when**: As above.

### Step 3: `snemu-wasm` records a stamped input log

**Acceptance criteria**: Pushing input at two different instrets records two entries, each carrying the instret at which it was delivered, retrievable in delivery order. Independent of everything else here — it can land at any time.
**RED**: A test pushing input, stepping, pushing again, and asserting both stamps and their order.
**GREEN**: The log and its accessor.
**Why now**: this is the foundation of both shareable diverged URLs and the stage-4 scrubber, and it is a handful of lines. Today `push_input` delivers at whatever instret the rAF pump happens to have reached, so no session is reproducible without it.
**Done when**: As above.

### Step 4: `snemu-wasm` runs to an anchor, deterministically

**Acceptance criteria**: `run_to_anchor` on the cap-delegation predicate returns the instret at which it fired; two calls on two fresh guests in the same process return the same instret. A predicate that never fires returns a bounded-search error rather than running forever.
**RED**: The two-runs-agree test, and a never-fires test asserting the bound.
**GREEN**: The `tour` evaluator driven over the frame stream, with a structural instret bound.
**Note**: The bound must be structural — post 82 found three `while` loops whose termination a mutant could pin, each a latent tab hang. `for _ in 0..limit`, not `while !found`.
**Done when**: As above.

### Step 5: The cap-delegation chapter, as content

**Acceptance criteria**: An MDX chapter and a manifest entry exist declaring workload `init`, the anchor predicate, and its claims. Claims are specific enough to be falsifiable — "the FS server holds an endpoint cap whose `parent_cap_id` is init's holding", not "capabilities are delegated".
**RED**: n/a — content. The assertion arrives in step 6, which is why these are adjacent.
**Done when**: The prose reads correctly, its claims are individually checkable, and the manifest parses.

### Step 6: The drift check runs in the gate

**Acceptance criteria**: A Rust test in `xtask-itest` boots each manifest chapter under snemu, replays to its anchor, and asserts its claims against the decoded frames. Deliberately falsifying one claim turns `cargo xtask test` red with a message naming the chapter and the claim.
**RED**: The test against the real chapter, plus a case proving a falsified claim fails.
**GREEN**: The harness loop over chapters.
**Note**: `cargo xtask test` already carries more than its name suggests — the loom checks, the generated-diagram drift, the doc links. This is the same kind of contract artifact; it belongs in the same gate.
**Done when**: As above.

### Step 7: The SPA shell routes, and back/forward works

**Acceptance criteria**: History API routing, not hash routing — a chapter has a real linkable URL. Browser back and forward restore the right chapter *and* its scroll position. On each route change focus moves to the new `<h1>` and the change is announced via an aria-live region.
**RED**: Unit tests for the route resolver; Playwright for back/forward and scroll restoration, since those are browser behaviour and cannot be faked in jsdom.
**GREEN**: A minimal router and the shell.
**Note**: These four behaviours are the ones hand-rolled SPAs reliably break, and their failure is invisible to the author. Copy the known-correct semantics; own the code.
**Done when**: As above.

### Step 8: The chapter renders, with the guest at its anchor

**Acceptance criteria**: Opening the chapter URL boots a guest, replays to the anchor, and renders the cap derivation tree at that state beside the prose. The emulator instance lives **above** the router — one guest, not one per chapter component.
**RED**: A browser test asserting the derivation tree contains the FS server's endpoint holding once the anchor is reached.
**GREEN**: MDX pipeline (`@mdx-js/rollup`, remark-gfm, rehype-slug, build-time Shiki), the chapter route, and the embed wired to the existing `FrameSource`.
**Note**: `web/src/snemu.ts` already declares itself the only file that knows `snemu-wasm` exists, with everything above written against `FrameSource` — a replay-backed source is a sibling of that file, not a change to its consumers.
**Done when**: As above.

### Step 9: Divergence is visible, and reset returns to the anchor

**Acceptance criteria**: Once the reader pushes input, the UI indicates the guest has left the anchor. A reset re-boots and replays, and the indicator clears. The chapter's asserted claims are only advertised as holding at the anchor.
**RED**: A browser test: type, assert the indicator appears; reset, assert it clears and the derivation tree matches the anchor state again.
**GREEN**: Divergence tracking off the input log from step 3, plus the reset path.
**Why it matters**: without this a reader wanders off the anchor, sees something contradicting the prose, and the docs look wrong when they are not.
**Done when**: As above.

## Pre-PR quality gate

Per CLAUDE.md: `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
`cargo xtask clippy` for lints, `cargo xtask links` after any doc move. Mutation
testing on the new `tour` crate and the `snemu-wasm` additions — post 82's report
is the standard to meet, and its three findings are the ones to expect again:
unbounded loops, no-op methods, and **delegating accessors that are covered at
their delegate but never through the wrapper**.

## Where to from here

Deliberately out of scope, roughly in the order the work would want them.

**Chapter two, and everything navigation implies.** Sidebar with explicit
ordering, prev/next, per-page TOC derived from the MDX heading AST, and search.
Search wants a client-side index built from chapter *source* at build time —
Pagefind indexes rendered HTML, so it needs a pre-render step and would index
markup rather than prose. Tens of chapters make this trivial either way.

**URL-encoded world-state.** The design is settled even though only the anchor
half ships here. A state is `f(workload, inputs[], target)` where each input
carries its delivery instret — so the URL is a replay script, never a state blob.
The canonical chapter link stays clean (`/tour/capabilities`, anchor implied by
the manifest); divergence adds a versioned query param, capped at N events with
an honest refusal past it rather than a truncated URL. Version the encoding from
the first byte; that cannot be retrofitted. Payoff: every Playwright test becomes
a URL, including diverged states, so the drift check can assert claims at states
a reader reached by typing rather than only at boot-and-run anchors.

**Multiple anchors per chapter.** "Now watch the grow fire" is a second anchor
mid-prose. Keep the manifest schema shaped so a list is additive; ship one.

**The scrubber** (design stage 4). Step 3's input log is its foundation — a scrub
is "replay to instret N with the log applied", the same seek. Worth reading how
TutorialKit models lesson state and reset first; it solved the same problem
against a different runtime (WebContainers runs Node, not machines — it fills the
slot `snemu-wasm` already fills here, so there is nothing to adopt from it but
the state model).

**Pre-buffering: the fold store is the tape.** Replay is forward-only and
deterministic, so if chapters are ordered by increasing anchor the tour is *one
guest running forward*, and each anchor is a checkpoint it passes through. The
cycles spent while a reader reads prose go into pre-reaching the next anchor
rather than animating an unopened chart — the YouTube-buffer model. Better still,
the panels are folds over the frame stream, so the store *is* the buffer: the
guest may be at chapter 3's anchor while the panels render the fold up to chapter
2's. Tape, write head, read cursor. Two limits to state plainly when building it:
the guest's live state is not rewound, only its telemetry projection — so history
is readable anywhere but the guest is only *typeable* at the write head — and
retention is bounded by what the frame store keeps (`Decoder::durable_len`), so
the tape is finite on a long tour.

**Backwards navigation, and when snapshots become necessary.** Replay is
forward-only. Navigating from a chapter anchored late to one anchored early
cannot rewind — it costs a fresh boot and replay. Sub-second today; it scales
with anchor depth. **This is the condition that forces snapshot serialization**,
and naming it now makes that a known deferral rather than a surprise. Until then,
order chapters so the common path runs forward.

**What runs while you read.** The pump keeps going as the reader scrolls prose.
Post 82's debt ceiling already makes a hidden tab safe; a *visible* tab burning a
third of a core to animate a chart nobody opened is a separate choice, and should
be made on purpose.

**Generated per-chapter smoke tests.** Every chapter needs a test that it routes,
renders, and replays. Generate them from the manifest — hand-writing one per
chapter guarantees drift by chapter twelve.

**A Starlight reference section.** Syscall tables, frame variants, cap semantics:
real documents, no live emulator, and exactly where its chrome and schema
validation earn their keep. Additive, whenever wanted.

**The user-docs corpus itself.** Independent of all of the above and startable at
any time — the design notes it is wanted by the help system regardless, and that
chapters written as prose become tour chapters later by adding a manifest entry.

---
*On completion, `git mv` to `plans/legacy/` (CLAUDE.md overrides the planning
skill's delete step), fix outbound `../` links in the moved file, fix inbound
links repo-wide, then `cargo xtask links`.*
