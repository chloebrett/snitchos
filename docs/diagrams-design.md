# Diagrams: what we draw by hand, what the code draws for us

> **Status:** design, unbuilt. First slice = one hand-drawn diagram + one
> generated one, to prove the split and the `xtask diagram` scaffolding.
>
> **Thesis:** A diagram is either an *editorial claim* about the system (needs
> judgement, drifts slowly, hand-drawn) or a *projection* of a source of truth
> (mechanically derivable, drifts silently, must be generated). Treat those two
> differently. The interesting SnitchOS-specific move is that our source of
> truth is sometimes the running system's own telemetry — the OS snitches its
> structure onto the wire, so a diagram generator is just another collector.

## The three buckets

| Bucket | Source of truth | Drift model | Mechanism |
|---|---|---|---|
| **A — editorial** | a person's mental model | slow, semantic | hand-authored `.md` with a mermaid block, updated ad-hoc |
| **B1 — static projection** | workspace source (`cargo metadata`, enum defs) | silent, structural | `cargo xtask diagram <target>` parses source → mermaid |
| **B2 — runtime projection** | a captured telemetry stream (`OwnedFrame`s) | none — it *is* the run | `cargo xtask diagram <target> <capture>` folds frames → mermaid |

The rule of thumb: **if a diagram would be wrong after a mechanical code change
that nobody thought to re-draw for, it belongs in B.** If it would only be wrong
after a *decision* changed, it belongs in A.

---

## Bucket A — editorial diagrams (hand-drawn, drift-tolerant)

These encode decisions and mental models. Code cannot derive them; they live as
mermaid blocks inside the design doc they explain, and we update them when the
design moves — not on a schedule, not in CI.

Priority order (highest "I keep re-explaining this" load first):

1. **The four address spaces** (`flowchart`) — higher-half / linear-map /
   heap-VA / physical, with root-PTE slots (256 heap, 257 guard, 322 linear)
   and which consumer uses which lens. Lands in `docs/observability-design.md`?
   No — memory. Put it at the top of a new short `docs/memory-map.md` or inline
   in the v0.4 findings. This is the single most-explained artifact in the repo.
2. **Context switch across a span** (`sequenceDiagram`) — `yield_now` saving
   callee-saved regs → runqueue pick → restore → `ret`, with the
   `CURRENT_SPAN_CURSOR` swap called out. This is the v0.5 "follow a trace
   across a switch" story; belongs beside `posts/post-12`.
3. **Boot → higher-half handoff** (`flowchart` or `sequenceDiagram`) —
   `entry.S` → `kmain` → `mmu::enable` → trampoline → `unmap_identity`, with the
   "no formatted `println!` before enable" cliff marked as a hazard node.
4. **IPC Call/Reply/ReplyRecv** (`sequenceDiagram`) — client `Call` → endpoint →
   server `ReplyRecv`, the one-shot reply cap's mint-and-consume lifecycle.
   Lives in `docs/ipc-design.md`.
5. **Supervision tree** (`graph`) — `init` as root, cap ownership viewed as
   supervision. A hand-drawn one already exists in `docs/supervision-design.md`;
   note it as the reference exemplar.
6. **Stitch pipeline** (`flowchart`) — source → lexer → parser → AST →
   tree-walk interp → (future bytecode VM). Lives in `docs/language-design.md`.

**Convention for A:** the mermaid block lives inline in the owning design doc,
fenced as ` ```mermaid `. No generation, no `--check`. A one-line HTML comment
above the fence records the commit/last-reviewed date so staleness is at least
visible: `<!-- diagram: reviewed 2026-07-05, owner=memory-map -->`.

---

## Bucket B1 — static projections (`xtask diagram`, from source)

Correct-by-construction from the workspace itself. Each has a machine-readable
origin, so the generator is short and the output is a guard, not a guess.

| Target | Source of truth | Output | Notes |
|---|---|---|---|
| `deps` | `cargo metadata` | `graph` of workspace crates | **first slice.** Highest drift-risk, lowest effort, zero editorial cost. Filter to workspace members; drop external deps. |
| `frame` | `protocol::Frame` variants (parse via `syn`) | `classDiagram` / table of wire variants + fields | This is *the contract*. A `--check` failure = the wire format changed without the diagram noticing. |
| `syscall` | `abi::Syscall` (0–25) | table/`graph`, grouped cap-mediated vs ambient | Same contract argument as `frame`. |
| `caps-model` | `CapObject` + `CapEventKind` + rights | `classDiagram` of the object/rights model | Static shape of authority, distinct from the runtime tree (B2). |
| `itest-matrix` | `SCENARIOS` registry + `bootargs::WorkloadKind` | table: scenario × workload | Which scenario boots which workload; catches orphaned workloads. |

**Parsing strategy:** `deps` and `itest-matrix` read structured data
(`cargo metadata` JSON; the `SCENARIOS` slice, ideally exposed as data). The
enum-derived targets (`frame`, `syscall`, `caps-model`) parse the source with
`syn` — xtask is a host crate, `syn` is cheap, and this avoids a proc-macro or
a runtime-reflection dependency in `protocol`/`abi`. If `syn`-parsing the enums
proves brittle, the fallback is a tiny `build.rs`-emitted manifest, but start
with `syn` — no new build-time machinery.

---

## Bucket B2 — runtime projections (`xtask diagram`, from a capture)

**The on-brand bucket.** SnitchOS already emits its own structure on the wire;
these diagrams *replay a capture* and are not merely correct but are the actual
observed run. The decoder already exists — `protocol::stream::OwnedFrame` is
serde-serializable and the itest harness already persists frame transcripts as
`.itest-runs/<ts>/…capture.json`. The generator is a fold over a `Vec<OwnedFrame>`.

| Target | Frames folded | Output | Why it's compelling |
|---|---|---|---|
| `caps` | `CapEvent{Granted,Transferred,Revoked}` via `cap_id`/`parent_cap_id` | `graph` — the capability derivation tree | **showcase slice.** The v0.13 cap-id spine exists *precisely* to make this reconstructable. "The OS draws its own authority graph." |
| `trace` | `SpanStart`/`SpanEnd` + parent, `ThreadRegister` for names | `gantt` or tree | What Tempo shows, but local and diffable in a PR. |
| `switches` | `ContextSwitch{from,to,reason,hart_id}` | swimlane per hart | Who-ran-when; makes the scheduler legible. |
| `ipc` | `Message{endpoint,from,to}` + `CapEvent` reply mints | `sequenceDiagram` | The *observed* Call/Reply, the runtime twin of A#4. |

**Input contract:** B2 targets take a capture path and reuse the collector's
decode path — no re-parsing, no bespoke reader. Two input modes:

- `--capture .itest-runs/<ts>/foo.capture.json` — an existing transcript.
- `--workload <name>` — boot under snemu (fast, deterministic, no QEMU/socket
  dance; snemu already decodes frames with `--frames`) into a fresh capture,
  then fold it. This makes `xtask diagram caps --workload demo` a one-shot.

B2 has **no `--check` mode** — it's a snapshot of a run, not a contract.

**`caps` shipped (first B2 target).** `diagram::caps::derivation_tree(&[OwnedFrame])`
folds `CapEvent` frames by `parent_cap_id → cap_id` into a top-down `Graph`
(TDD, pure). xtask's `diagram caps` sources the frames by booting under snemu
(`snemu_diff::collect_frames`, default `init`, `--workload`/`--steps` overrides),
writes `docs/generated/caps.md` (committed illustrative snapshot, **not** gated)
+ a gitignored `caps.svg`. A real `init` boot folds ~35 CapEvents into init → fs-server
→ per-connection badged-endpoint branches. **Known refinement:** one-shot `Reply`
caps are minted `parent_cap_id: 0`, so they render as isolated nodes — faithful,
but noise in a derivation view. Candidate follow-ups: drop/collapse `Reply` caps,
style roots distinctly, or annotate `Revoked`.

---

## The `xtask diagram` surface

```
cargo xtask diagram <target> [--check] [--out PATH] [--capture PATH | --workload NAME]
```

- Emits mermaid to **stdout** by default (composable — pipe into a `.md`, into
  a mermaid CLI, into `pbcopy`). `--out` writes a file.
- `--check` (B1 only): regenerate, diff against the committed artifact, exit
  non-zero on drift. This is the whole point of B1 — the contract diagrams
  (`frame`, `syscall`) become snapshot-tested like `insta`. Wire it into the
  same gate as clippy/tests.
- `--capture` / `--workload` (B2 only): the input source.
- Targets are a flat `ValueEnum` (`deps`, `frame`, `syscall`, `caps-model`,
  `itest-matrix`, `caps`, `trace`, `switches`, `ipc`); `--check`/`--capture`
  are rejected for the buckets they don't apply to (clap can't express that
  cross-field rule, so validate in the handler and error clearly).

**Crate boundary:** diagram logic lives in its **own `diagram` library crate**,
not inside xtask. This keeps the projections pure, host-tested, and
independently mutation-testable (add it to the `xtask mutants` set), and matches
the workspace grain — xtask orchestrates libs (`kernel-core`, `protocol`,
`collector`), it doesn't house them. xtask stays a thin I/O shell: it shells out
`cargo metadata`, drives snemu for a capture, does `--check` diffs and file
writes, then **delegates every projection to `diagram`**.

- `diagram` crate (host-only lib; depends on `protocol` w/ `std` + `serde_json`):
  - `model` — **typed diagram values** (`Graph { nodes, edges }`,
    `Sequence { participants, messages }`, a class/table model), each with a
    `to_mermaid()` emitter, so no target hand-concatenates syntax. The typed
    model is the testable seam: a target builds a *value*, the emitter turns it
    into syntax, tests assert on the emitted string.
  - `deps` — `parse_cargo_metadata(json) -> Vec<CrateNode>` and
    `workspace_graph(&[CrateNode]) -> Graph`, both **pure** (tested against a
    JSON fixture, no `cargo` invocation).
  - `caps` — `derivation_tree(&[OwnedFrame]) -> Graph`, **pure** (tested against
    a hand-built frame fixture, no boot).
- `xtask` — a `Diagram` subcommand that provides the I/O and hands data to the
  lib.

Mermaid is the primary backend (GitHub renders it in-diff for free); DOT stays
reachable as an optional second `Graph` backend for local graphviz layout, but
is not built in the first slice. `petgraph` is deliberately avoided — dedup is a
`HashSet`, and the graph targets need no algorithms yet.

---

## First slice — one from each bucket

Deliberately minimal, to prove the pattern and the scaffolding end-to-end.

1. **B1: `cargo xtask diagram deps`** — crate graph from `cargo metadata`.
   Fastest to green, no `syn`, establishes `diagram/mod.rs` + `mermaid.rs` +
   the `ValueEnum`. Add `--check` and commit `docs/generated/deps.md`; wire the
   check into the test gate. This proves the "generated diagram as guard" loop.

2. **B2: `cargo xtask diagram caps --workload demo`** — capability derivation
   tree from `CapEvent` frames. The showcase piece: reuses the `OwnedFrame`
   decoder, folds `cap_id`/`parent_cap_id` into a `graph`, proves "the system
   draws its own picture." Test the fold against a hand-built `Vec<OwnedFrame>`
   fixture (no boot needed in the unit test); the `--workload` path is the
   integration smoke.

Bucket A ships alongside as a **hand-authored** commit — the four-address-spaces
diagram — with no tooling, just to seat the convention (inline mermaid + a
`reviewed` comment) so A and B are visibly different-in-kind from day one.

### TDD order for the first slice

Per house rules, test-first, one increment at a time:

1. RED `mermaid.rs`: a graph-builder emits expected mermaid for a 2-node graph.
2. GREEN the builder.
3. RED `diagram deps`: `cargo metadata` fixture → expected workspace graph.
4. GREEN via a `cargo metadata` shell-out at the edge + pure projection.
5. RED `--check`: committed-vs-regenerated mismatch exits non-zero.
6. GREEN, commit `docs/generated/deps.md`, wire into the gate.
7. RED `diagram caps`: `Vec<OwnedFrame>` fixture with a small
   Granted→Transferred chain → expected derivation `graph`.
8. GREEN the fold; then add the `--workload` boot path as integration glue.

## Decisions

- **B1/B2 outputs live in `docs/generated/`** — a machine-owned dir,
  `--check`-guarded, never hand-edited. Each file carries a header comment
  (`<!-- generated by: cargo xtask diagram <target> — do not edit -->`) so its
  provenance is unmistakable. Editorial (bucket A) mermaid stays inline in its
  owning design doc; the two never mix.
- **`itest-matrix` shipped** — the `SCENARIOS`-as-data prereq turned out
  trivial: `itest_harness::Scenario` already exposes `name`, `workload`,
  `tags`, and `cpu_profile` as public fields, so the only change was making the
  macro-generated `const SCENARIOS` `pub(crate)`. xtask maps each `Scenario`
  into `diagram::itest_matrix::ScenarioMeta`; the pure `matrix_table` projects
  them into a `model::Table` sorted by (workload, name). Rendered as a markdown
  **table** (not a mermaid graph — a 60×25 scenario×workload grid would be
  sparse and unreadable); the `Table` model gained a `to_markdown()` emitter
  beside `Graph`'s `to_mermaid()`/`to_dot()`. Committed to
  `docs/generated/itest-matrix.md`, `--check`-gated alongside `deps`.
- **Mermaid rendering in CI/PRs is free** — GitHub renders mermaid in `.md`
  natively, so committed generated `.md` files render in-diff. No renderer
  dependency, no HTML step.
