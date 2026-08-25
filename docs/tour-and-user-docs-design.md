# A Tour of SnitchOS: user-facing docs with a real OS inside

**Status:** 📐 **DESIGN — exploration, not started.** Captures the 2026-07-25
vision: a documentation website (think the Rust docs) whose pages **embed a real
running OS** — snemu compiled to wasm, pre-booted via the snapshot tree to the
exact world-state each concept needs. Small inline embeds; full-screen mode with
terminal, display, audio, telemetry (logs, frames, spans, metrics), a bespoke
React visualization set, and a step/pause/rewind kernel debugger. And, prior to
all of that: the recognition that **user-facing documentation is a separate
corpus from the dev docs**, with different truth conditions — and that this new
corpus is what the help system retrieves over and the models train on.

Related: [snemu-wasm-design.md](snemu-wasm-design.md) (the embed substrate),
[snemu-itest-snapshot-tree-design.md](snemu-itest-snapshot-tree-design.md) (the
snapshot machinery), [llm-design.md](llm-design.md) (help layers, retrieval,
provenance), [diagrams-design.md](diagrams-design.md) (telemetry folds — the
metric store's ancestor), [snemu-design.md](snemu-design.md),
[observability-design.md](observability-design.md).

---

## User docs are a separate corpus, not a rewrite of the dev docs

Everything in `docs/` today is dev-side: design rationale, findings, trade-off
records. The two corpora have **different truth conditions**:

- **Dev docs** explain *why we built it this way*. They may legitimately contain
  history, dead ends, superseded designs. Their audience already has the map.
- **User docs** assert *what the system does right now*. Every claim should be
  checkable against the running OS. "How do I", never "why did we".

Consequences:

- **User docs are the retrieval target** for the help system's layers 2–3
  ([llm-design.md](llm-design.md)) — help must never surface "we considered
  three heap designs…" at a confused user. The help cards (layer 1) are
  effectively the event-indexed subset of this corpus.
- **User docs are training text.** Tier-1-quality prose about Stitch and the OS,
  written once, consumed three ways: humans, retrieval, fine-tuning.
- **Every code example is executed.** User-doc snippets go through the same
  parse → type-check → run validators as the synthetic corpus. Doctests for an
  OS: documentation that executes can't rot.

## The snapshot as a first-class documentation primitive

The load-bearing idea — what separates this from "docs site with an emulator
iframe": Rust docs have runnable examples; the tour has **runnable
world-states**. A page about heap pressure doesn't describe the watermark
policy — it pins a snapshot taken three heartbeats before the grow fires, and
the reader watches it happen. Deterministic emulation means it happens the same
way every time; the tour never flakes.

Mechanics:

- Snapshot IDs are **content-addressed doc assets**, produced by the docs build
  the way `cargo xtask diagram --check` produces diagrams today. Each chapter
  declares a setup script (boot + replay to its anchor point); the build boots
  the OS, runs it, snapshots.
- The build then **asserts the chapter's claims against the frame stream** — a
  tour chapter is an itest scenario wearing prose. If a kernel change moves when
  the heap grow fires, the heap chapter's build fails.
- **Docs drift fails CI.** This extends the repo's existing drift-check contract
  (generated diagrams, doc links) to the user docs' semantic claims. No
  interactive-docs project we know of has this property.

## One React app, two backends

This project is a **convergence point for four parked directions**, not a new
one: snemu-wasm (already scoped text/telemetry-first), collector-as-server
(React dashboards + terminal replacing the Grafana UI), the diagram system's
telemetry folds, and the physics-desktop replay instinct
([physics-desktop-design.md](physics-desktop-design.md)).

The key architectural call: **the tour's full-screen embed and the collector's
dashboard UI are one React app with two backends.**

- **In-tab backend**: snemu compiled to wasm, frames folded live in the client.
- **Host backend**: websocket to a host-side collector/snemu.

Same components, same frame-stream contract — this is the user's "running in
browser or talking to a snemu on a host" toggle, and it means every tab built
for the tour is also the daily-driver observability UI. Build the Memory tab
once, use it in both worlds.

### The frame-fold metric store (not Prometheus-in-a-tab)

In the browser there is no scrape loop — there is the frame stream, and metrics
are a **fold over it** (the diagram telemetry targets already implement exactly
this fold in Rust). The app's data layer is a frame-fold store:

- in-tab: folds the live stream (a small TS fold, or the Rust fold via wasm);
- on-host: same store, optionally proxying real PromQL for long-range queries.

Do **not** ship Grafana into the tab. Reimplement the handful of charts the
tour actually needs over the fold — bespoke was the right instinct anyway;
Grafana's genericity is exactly wrong for a guided tour.

### Visualization set (bespoke, Grafana-inspired)

Tabs are folds over frames the kernel already emits:

- **Memory** — heap bytes/grow events/watermark line, frame-allocator
  pressure, OOM events; the heap-oom chapter pins its snapshot here.
- **Process** — context switches, per-task CPU ticks/runs, spawn/exit/reap
  timeline, the cap delegation graph (the `caps` diagram fold, live).
- **Traces** — span tree from Span* frames (the collector's state machine,
  client-side).
- **Terminal / Display / Audio** — virtio-console, ramfb canvas, PWMDAC →
  WebAudio.
- **Wire** — the raw decoded frame stream, filterable; the "no magic" tab that
  shows everything else is derived.

## The debugger: time travel is a seek

Pause/step/inspect needs a snemu control surface + UI, but no new theory —
determinism does the heavy lifting. **Rewind = restore the nearest snapshot
ancestor + execute forward to instret N**; the snapshot tree turns time travel
into a seek operation. Periodic auto-snapshots while full-screened bound the
re-execution distance. Stepping the kernel, in a browser, in a docs page — with
the telemetry panes updating in lockstep — is the tour's showpiece interaction.

## Audio

snemu models virtio-console and ramfb; the PWMDAC device model is **in flight**
(companion to the VF2 audio work,
[../plans/legacy/vf2-audio-tier0.md](../plans/legacy/vf2-audio-tier0.md)). In-tab it sinks to
WebAudio. Until it lands, the embed ships without the audio pane — it's the one
full-screen item that is emulation work rather than UI work.

## Sequencing

Each stage independently shippable; stage 2's artifact is wanted by
collector-as-server regardless of whether the tour ships.

1. **snemu-wasm MVP** as already scoped: terminal + raw frame view in a tab.
2. **Frame-fold store + first two tabs** (Memory, Process — pure folds over
   existing frames).
3. **Snapshot-pinned chapters + the CI contract** (chapter = setup script +
   snapshot + frame-stream assertions + prose).
4. **Pause / step / rewind.**
5. **Host-backend toggle** — which retroactively upgrades the collector into
   collector-as-server.
6. **Audio pane** (once the snemu PWMDAC model lands).

The user-docs corpus itself starts before any of this — it's prose + validated
examples, needed by the help system independently, and chapters written now
become tour chapters later by adding a setup script.

## Tie-ins

- **Help**: the tour page where the reader types `help` and an in-tab model
  answers — grounded in the same user docs they're reading, trace visible in
  the telemetry pane — is the whole [llm-design.md](llm-design.md) thesis in one
  screenshot.
- **Provenance**: the tour is the demo vehicle for the provenance story; the
  Wire tab makes "the system can prove which one happened" visible to a reader
  who's never heard of OTLP.
- **Corpus**: validated user-doc examples feed the Stitch training corpus; the
  flywheel ("was this helpful" events) grows the card set where readers
  actually get confused.

## Open questions

- Snapshot asset size & hosting: content-addressed blobs per chapter — how big
  is a post-boot snapshot compressed, and does the snapshot tree let chapters
  share a common boot prefix (it should — that's its whole design)?
- Setup scripts: bespoke per chapter, or reuse the itest scenario harness
  wholesale (a chapter *is* a scenario — can `SCENARIOS` entries be tagged as
  tour anchors)?
- Where the user-docs corpus lives: `docs/user/` vs a separate top-level
  (`tour/`? `book/`?) — it has a different audience, build, and truth contract
  than `docs/`.
- How much of the fold layer is Rust-via-wasm (shared with the diagram folds)
  vs TS (fast to iterate on)?
