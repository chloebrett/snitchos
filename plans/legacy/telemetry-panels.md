# Plan: the telemetry view (milestone 3)

**Branch**: main (this project works directly on main; the human commits)
**Status**: Active

Follows [snemu-wasm-interactive.md](snemu-wasm-interactive.md). The tab
runs a machine you can type at; this is the machine explaining itself.

## Goal

Live panels in the browser for the things Grafana is bad at — the capability
derivation tree, the span tree across a context switch, and the switch timeline —
folded from the same frame stream the terminal already consumes.

## The decision this inherits, and keeps

**Prometheus stays the store; only the UI is replaced.** Owning retention,
downsampling, range queries and alerting is a far bigger commitment than the UI, and
custom React wins only for what Grafana is *bad* at: cap-derivation trees, span trees,
and the terminal. It does not win at line charts, and this plan does not attempt any.
(**Superseded in part** — see the metrics note under Open questions: numbers shown
*in the context that explains them* are also something Grafana cannot do. Still not
line charts, and still not a store.)
(Nor at the physics desktop, which is not a panel at all — the OS renders that itself,
to its own framebuffer. React's job stops at the frame stream.) (`docs/uart-telemetry-design.md` Decision 4.)

So the scope is **structure, not series**.

## What the investigation found

**1. `diagram` compiles to wasm32 unmodified.** Verified, not assumed
(`cargo build -p diagram --target wasm32-unknown-unknown`). Its only dependencies are
`protocol`, `serde_json` and `snitchos-abi` — nothing native.

That matters more than it sounds. The crate already folds `OwnedFrame`s into exactly
the three views this milestone wants:

| fold | what it reconstructs |
|---|---|
| `caps::derivation_tree(&[OwnedFrame]) -> Graph` | who granted which capability to whom |
| `trace::span_call_graph(&[OwnedFrame]) -> Graph` | the span tree |
| `switches::transition_graph(&[OwnedFrame]) -> Graph` | context-switch transitions |

"A diagram is a collector" becomes literally true: **the same fold that produces the
committed `docs/generated/*.md` produces the live panel.** One implementation, no
drift, and the committed diagrams become a regression test for the panels.

**2. `Graph` is write-only from outside.** `Node` and `Edge` are private, there are no
accessors, and the only exits are `to_mermaid()` and `to_dot()`. A React panel cannot
read the structure it is meant to render. That is the one real obstacle, and step 1 is
choosing how to get past it.

**3. `FrameView` is deliberately lossy** — kind, name, timestamp, metric value. It was
right for a boot log and is not enough here: a cap tree needs `cap_id`,
`parent_cap_id`, `holder`, `rights`; a span tree needs `id` and `parent`. The
`Decoder` currently projects and discards the `OwnedFrame`, so the frames the folds
need are being thrown away.

## Acceptance criteria

- [x] A panel shows the capability derivation tree of the running guest, updating as
      caps are granted, and naming objects rather than showing bare ids.
- [x] A panel shows the span tree, and a span that survives a context switch is
      visibly one span rather than two.
- [x] A panel shows context-switch transitions between named tasks.
- [x] The panels update live without the tab dropping below the responsiveness bar
      the acceptance suite already enforces.
- [x] The folds are the *same code* as the committed diagrams — no second
      implementation of any projection.
- [x] Switching workload resets the panels, as it already resets the terminal.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test. Rust unit tests for the folds and projections, Vitest for the panels'
own logic, Playwright only for what needs a browser.

### Step 1: Decide and build the `Graph` → JS path

**Acceptance criteria**: A React component can render a `Graph`'s nodes and edges.
**Decide first, with the human**, because the options differ in kind rather than
degree:

- **(a) Render mermaid in the browser.** `to_mermaid()` already exists, and mermaid.js
  would draw it. Cheapest, and produces a picture identical to the committed docs.
  But it is a *picture*: no hit-testing, no hover, no selecting a cap to see its
  rights — and re-rendering a mermaid graph on every update is not free.
- **(b) Accessors or a serialization on `Graph`.** `nodes()` / `edges()`, or a
  `to_json()` beside `to_mermaid()`. The panel renders its own SVG/DOM and can be
  interactive. More work, and it widens `diagram`'s API for a second consumer.

Recommendation: **(b)**. The whole argument for replacing Grafana is the things it
cannot do — a cap tree you can interrogate is exactly that, and a static image is
Grafana's weakness reproduced. `to_json` sits naturally beside `to_mermaid`/`to_dot`
as a third renderer, which is a shape the crate already has.
**RED**: A `diagram` test that a folded graph serializes to the nodes and edges it
contains, including groups and classes.
**GREEN**: The renderer.
**MUTATE**: `cargo mutants -p diagram`.
**Done when**: Criteria met, report reviewed, human approves commit.

### Step 2: Retain decoded frames so the folds have something to fold

**The naive version of this step is wrong, and wrong quietly.** Read this before
writing a ring buffer.

#### Why "keep the last N frames" produces a lying cap tree

The folds are **cumulative reconstructions, not windowed views**. `derivation_tree`
walks the whole slice three times, and each pass depends on frames that arrived at
startup:

1. `thread_names(frames)` scans for `ThreadRegister` to label holders. Those all
   arrive during boot. Drop them and every holder renders as `h3` instead of
   `stitch_repl` — degraded, still plausible-looking.
2. A `revoked` set is built by scanning for `CapEvent::Revoked`. Drop an old revocation
   and a **revoked capability renders as live**. That is not degraded, it is *wrong*,
   and it is wrong in the direction that matters for a security-shaped view.
3. Roots are `parent_cap_id == 0`. Drop a parent's `CapEvent` while a child's survives
   and the child points at a node that no longer exists — a dangling edge, or a
   phantom node, depending on the renderer.

None of those raise an error. The panel renders confidently and is untrue, which is
precisely the failure class this project keeps finding in its own controls.

#### The shape that works: durable vs windowed

The wire already draws the line for us. Some frames are **cumulative facts** —
registrations and lifecycle events, low-volume by nature, meaningless to drop. The
rest are a **stream** — high-volume, and a recent window is not merely acceptable but
what you actually want to look at.

| bucket | frames | why | bound |
|---|---|---|---|
| durable | `StringRegister`, `ThreadRegister`, `HartRegister`, `MetricRegister`, `BuildInfo`, `Hello` | registrations; a name that stops resolving makes every later view unreadable | none — they are emitted once each |
| durable | `CapEvent` where kind is `Granted` / `Transferred` / `Revoked` | the derivation tree is cumulative state, and a dropped revocation inverts its meaning | none — bounded in practice by how many caps exist |
| windowed | `SpanStart`, `SpanEnd`, `Event`, `ContextSwitch`, `Message`, `Notify*`, `Log`, `Metric` | a stream; the span tree and switch timeline are *about* the recent past | ring, oldest-first |
| ~~windowed~~ | ~~`CapEvent` kind `Invoked` / `Denied`~~ | **those variants do not exist** — see below | — |

**Correction (2026-08-26): `CapEventKind` has four variants — `Granted`,
`Transferred`, `Revoked`, `Minted` — and all four are derivation lifecycle.**
`Invoked`/`Denied` appear only in a doc comment in `protocol/src/lib.rs`, describing
*reserved future* wire slots. This plan asserted a kind-based split as "the detail
most likely to be missed"; there is no split to miss today, and `CapEvent` is entirely
durable.

The correction improves the design rather than simplifying it away. `retention_of`
matches `CapEventKind` **exhaustively, with no catch-all**, so if an audit variant is
ever added it is a *build failure* rather than a silent default into the unbounded
bucket — which is exactly the leak a catch-all would have created, in the one bucket
that has no ceiling.

#### What still has to be decided

- **Does `Decoder` own this, or something above it?** A replay source wants the whole
  stream and can afford it; a live tab cannot. Leaning: a separate `FrameStore` that
  the `Decoder` feeds, so the policy is not welded to the decoder and a replay source
  can choose a different one.
- **Is the durable bucket really unbounded?** "Bounded in practice" is an assumption
  about guest behaviour, and this plan has already been wrong once about guest
  behaviour (the idle task that never idles). It should be *measured* — count
  `CapEvent` and `*Register` frames over a long boot — and given a ceiling with a
  visible warning rather than a silent drop if the measurement says otherwise.
- **The alternative considered: fold incrementally and never retain.** A real
  collector maintains the graph as frames arrive. Rejected for now because
  `diagram`'s API is `fold(&[OwnedFrame]) -> Graph` — batch, by design — and rewriting
  those folds incrementally would fork the very code this milestone exists to share.
  Worth revisiting only if retention proves too expensive, which is a measurement, not
  a guess.

**Acceptance criteria**: a `FrameStore` retains frames under the durable/windowed
policy above; a fold over its contents equals a fold over the same frames passed
directly, **including after the window has overflowed**; the durable bucket survives
an overflow that discards thousands of windowed frames.
**RED**: Tests that (a) retained order matches arrival order; (b) the window drops
oldest-first; (c) a `ThreadRegister` from before an overflow still names a holder in
the folded tree; (d) a `CapEvent::Revoked` from before an overflow still marks its cap
revoked — the failure that would otherwise show a revoked cap as live; (e) `Invoked`
events *are* dropped, so the split is by kind and not by frame type.
**GREEN**: The store.
**MUTATE**: `cargo mutants -p snemu-wasm`.
**KILL MUTANTS**: Address survivors — the kind-based split and the window bound are
the two that matter.
**Done when**: Criteria met, report reviewed, human approves commit.

**Outcome (2026-08-26): `snemu-wasm/src/store.rs` — `FrameStore`, 13 tests, 13
mutants, 0 survivors.**

Durable and windowed frames each carry an arrival sequence and `frames()` merges on
it, rather than concatenating the buckets: the span and switch folds read *sequences*,
and a registration emitted mid-run would otherwise appear to precede everything before
it.

*The test that matters uses the real fold as its oracle.* Asserting a frame is
*present* after an overflow is one inference short of the claim; the claim is that
`derivation_tree` still produces a **true tree**. So a store that discarded 500 stream
frames is folded and compared against one that discarded nothing — plus non-vacuity
assertions, without which two *empty* trees would compare equal and prove nothing.

Those guards earned their place twice over. They caught, in order: a fixture whose
`holder` id did not match its `ThreadRegister`, so no name resolved in either tree;
and then that `derivation_tree` **drops any node that ends up in no edge** ("isolated
bootstrap grant"), so a fixture where every capability had `parent_cap_id: 0` folded
to nothing at all. Neither would have failed a plain equality assertion.

*Mutation testing found the recurring shape again, twice.* `while window.len() >
cap` mutated to `<` or `>=` spins forever — the **third** time in this work that a
loop whose termination depends on arithmetic has produced a timeout, and this plan had
already said to state such bounds structurally. A push adds exactly one frame and the
cap is fixed, so the loop became an `if` and cannot spin. And `durable_len -> 1`
survived because the test asserted it *equalled* 1; the counts now differ from each
other and from one.

### Step 3: The capability derivation tree panel

**Acceptance criteria**: The panel shows the guest's cap tree, edges labelled with the
rights transferred, nodes named from `CapEvent`'s name field rather than by id. This
is the panel that justifies the milestone: it is the project's own subject matter, and
nothing off-the-shelf draws it.
**RED**: Vitest over the panel's own logic (layout/grouping decisions, empty state),
against a fixture graph — not against a live guest.
**GREEN**: The component.
**Done when**: Criteria met, human approves commit.

### Step 4: The span tree and switch panels

**Acceptance criteria**: Both render from their folds. The span tree makes the
survives-a-context-switch case legible — that is the v0.5 devlog's whole angle, and
the thing a flat log cannot show.
**RED**: Vitest over each panel's logic against fixture graphs.
**GREEN**: The components.
**Done when**: Criteria met, human approves commit.

### Step 5: Live, and proven live

**Acceptance criteria**: A Playwright spec boots a workload that grants capabilities
and asserts the tree appears with named nodes; a second asserts the panels clear on
workload switch. Responsiveness stays above the existing bar with panels rendering.
**RED**: The specs.
**GREEN**: Whatever they turn up.
**Done when**: All acceptance criteria at the top are met; human approves commit.

**Outcome (2026-08-26): steps 3–5 done together. 14 browser tests, 79 unit tests.**

The chain is decoder retention → `diagram` folds behind the shell → `Views` on
`FrameSource` → `Panels` with tabs for capabilities, spans, switches and the raw
frame tail. Re-folded four times a second rather than per frame: these are batch
folds over the whole retention window, and the structures change on the timescale a
person reads them, not sixty times a second.

Paced CPU *fell* from 40.9% to 32.3% with the panels in — the default view is a small
tree rather than 400 DOM rows of frame tail.

#### The e2e suite found a race that had been passing on luck

Adding the panels broke two existing tests, and the reason was not the change:

`frame-list` is a bounded 400-row live tail, and this guest emits **thousands of
`ContextSwitch` frames a second** — one heartbeat's metric dump plus its switches
fills the entire window. Any assertion naming a *once-only* frame (`kernel.boot`, a
single `kvetch.complete` span) depends on a poll landing in the fraction of a second
before eviction. Those assertions were races that had been winning.

Fixed by asserting on what the guest emits *continuously* (`snitchos.`, which still
proves name resolution through the intern table), and by dropping the frame-tail
assertion from the drivel test entirely — the terminal is the durable observable and
the actual claim there. **The suite went from 2.2 minutes to 22.6 seconds**, because
the flaky assertions were also the slow ones: retrying for two minutes against a
window that would never contain what they wanted.

Worth keeping as a rule: *do not assert a transient frame in a bounded tail.* Assert
something continuous, or assert the durable projection.

#### The panel assertions are about truth, not presence

- The cap tree must contain **names** (`init`/`fs`/`stitch`), not `h1 → h2`. A tree of
  bare holder ids is what a dropped `ThreadRegister` produces — plausible-looking and
  wrong, which is the whole reason the retention split exists.
- "No source yet" and "a source that produced nothing" are distinguished, because
  conflating them shows an empty capability tree during boot that reads as *this guest
  granted nothing*.
- The durable count is displayed and asserted to stay bounded on a long run — that
  bucket has no ceiling by design, and this is the assumption under measurement rather
  than under trust.

## Open questions

- **How often should a panel re-fold?** Folding every animation frame is certainly
  wasteful and possibly slow; the frame stream is bursty and mostly idle. A dirty flag
  on new frames, or a fixed cadence, or `CapQuiescence` (which `caps.rs` already has,
  for deciding when a cap graph has settled) — that last one exists precisely because
  this question was answered once already for the static diagrams.
- **Does the retention bound belong in the `Decoder` or above it?** A replay source
  would want the whole stream; a live tab cannot afford it.
- ~~**Where do metrics go?** Explicitly not here.~~ **Reversed 2026-08-26: metrics
  belong here too, and custom.**

  This plan scoped itself to "structure, not series" on the inherited reasoning that
  custom React wins only where Grafana is *bad*, and loses at line charts. That is
  still true of line charts and is now the wrong conclusion: the interesting metrics
  this guest emits are not series to plot, they are numbers that mean something
  alongside the structure — a capability's use count next to the capability, a task's
  CPU time next to the task in the switch graph, heap occupancy next to the frames
  that allocated it. Grafana cannot put a number *next to* a derivation-tree node,
  because it has never heard of one.

  So the split is not "structure here, numbers there". It is **numbers in the context
  that explains them here; numbers over time in Grafana**. Prometheus remains the
  store — that decision is untouched, and this needs no storage of its own.

  Its own milestone; this one does not attempt it. Worth noting the frames are already
  arriving and already retained: `Metric` frames are classified `Windowed` in
  `snemu-wasm/src/store.rs`, which is right for a live reading and would need
  revisiting if any view wanted history.

## Pre-PR quality gate

1. Mutation testing — `mutation-testing` skill on `diagram` and `snemu-wasm`.
2. Refactoring assessment — `refactoring` skill.
3. `cargo xtask clippy`, `cargo xtask test`, `cargo xtask links`.
4. `yarn check`, `yarn test`, `yarn e2e`, `yarn measure`.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md this project keeps
the historical record) and follow the archiving checklist in
[README.md](../README.md) — including the `.rs` doc-path citations `cargo xtask links`
cannot see.*
