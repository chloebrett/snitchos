# Open-sourcing SnitchOS extractables

Status: proposal / planning. No code changes yet.

SnitchOS is a learning project, but several pieces inside it have grown
into things with standalone value. This plan inventories every credible
extraction candidate, ranks them by (reuse value × extraction cost),
and lays out a concrete sequencing.

## The two the user already named

1. **The itest harness** — flaky-test runner mechanics.
2. **The `.claude` Rust configuration** — TDD/testing skills + agents,
   adapted from citypaul's TypeScript skill set to Rust.

Both are strong candidates. The rest of this doc is "what else", plus a
real plan.

---

## Full candidate inventory

Ranked. Tier 1 = ship first, genuinely reusable, low extraction cost.

### Tier 1 — ship these

#### A. `itest-harness` — flaky-integration-test runner *(user named)*
Already a clean, host-pure workspace member (`itest-harness/`, ~600 LOC).
Owns everything that has nothing to do with QEMU or RISC-V:
- per-scenario flake-rate aggregation + p95 durations
- **Wilson-score 95% CIs** and a **two-proportion pooled z-test** for
  regression detection (`Consistent` / `Different{Worse,Better}`)
- baseline-file persistence (`.itest-baseline.toml`) + pending-sidecar
  recovery for interrupted runs
- per-iteration NDJSON history under `.itest-runs/<ts>/`
- **failure-signature classifier** (`signature.rs`) — taxonomy of
  *why* a run failed (Wedge / Stalled / HartStalled / BudgetExhausted /
  Harness / Unknown), which is what actually cracked the cross-hart
  TX_STAGING flake
- Prometheus textfile + OTLP metric export

Deps are only `serde`/`toml`/`time`/`prost`/`ureq`. The QEMU glue lives
in `xtask`; the harness talks to subjects through a `RunnerConfig` hook.
This is the most "product-shaped" thing in the repo — there is no good
off-the-shelf "run my flaky suite N times, decide statistically whether
it regressed, and tell me *how* it failed" crate. **Highest reuse value.**

Extraction cost: low. It's already split. Work is packaging, not surgery
(see "Genericisation work" below — the `RunnerConfig` needs to be proven
against a non-QEMU subject, and Prom/OTLP export made optional).

#### B. The `.claude` Rust dev-methodology pack *(user named)*
23 skills + 11 agents + 5 commands. The TDD / testing /
mutation-testing / refactoring / functional / planning skills are a
Rust port of citypaul's TypeScript skills; `crate-audit`, `cli-design`,
`finding-seams`, `characterisation-tests`, `test-design-reviewer` round
it into a coherent "how to do disciplined Rust" bundle. Plus the agents
(`rust-enforcer`, `tdd-guardian`, `pr-reviewer`, `refactor-scan`,
`twelve-factor-audit`) and the `setup` / `generate-pr-review` commands.

Reuse value: high — this is directly installable into any Rust repo.
Extraction cost: low-medium, but **attribution + licensing is the real
work** (citypaul lineage must be credited; pick a license; scrub any
snitchos-specific references baked into examples).

#### C. `protocol` — structured-telemetry wire format
`no_std` postcard-encoded `Frame` enum: spans, metrics, thread/hart
registration, context switches, an intern/`StringRegister` table.
Deps: `serde` + `postcard` only. Versioned wire contract (currently v3).

This is a "tracing-over-a-byte-pipe for `no_std`/embedded" format. There
is a real gap here: `tracing` assumes an allocator and a rich runtime;
this is spans+metrics for a kernel emitting down a virtio-console. **High
reuse value for the embedded/OS-dev niche.** Extraction cost: low — it
barely touches anything.

Caveat: tightly co-designed with the collector. Ship B+C+collector as a
trio or not at all (see D).

### Tier 2 — real value, more decoupling needed

#### D. `collector` — `Frame`-stream → OTLP/Prometheus exporter
Reads a socket, decodes `protocol::Frame`s, exports OTLP traces (Tempo)
+ Prometheus. ~1600 LOC. The OTLP/Prom encoding is generic and well
tested; the span state machine hardcodes snitchos's ContextSwitch
semantics and service naming. Reusable *with* `protocol` as a pair:
"here's a wire format for embedded telemetry, and here's the host
collector for it." Extraction cost: medium (parameterise service name,
tag mappings, transport).

#### E. `xtask` dev-tooling utilities
Several `xtask` modules are subject-agnostic and individually useful:
- `audit.rs` (~790 LOC) — symbol-scan dead-code/unused-API finder
  (already surfaced as the `crate-audit` skill's engine; the
  `cargo xtask audit` tool the memory note describes).
- `loc.rs` (~280 LOC) — workspace LOC aggregator with test-line masking.
- `measure.rs` (~390 LOC) — percentile/histogram stats helpers.
- `source.rs` (~130 LOC) — comment-strip + test-region masking.

These are small, no-dependency, copy-paste-grade utilities. Best shipped
as one small `cargo-*` style tool or a `dev-xtasks` crate rather than
four micro-crates. Reuse value: medium. Cost: low but fiddly (they
currently assume the snitchos workspace layout).

### Tier 3 — niche / template-only / keep in-repo

- **`abi`** (~93 LOC, zero deps) — kernel↔userspace syscall ABI. Pure
  contract, very clean, but only meaningful to *this* kernel's
  capability model. Ship only as part of an eventual "fork-this-OS"
  template, not standalone.
- **`kernel-core` Sv39/frame-allocator primitives** — the page-table
  walk + frame `Bitmap` are host-tested and microkernel-general, but
  they're interwoven with the observability logic. A `riscv-sv39-core`
  crate is *possible* but is genuine surgery; defer unless someone asks.
- **`user/runtime` + `runtime-macros`** — snitchos-ABI-specific; only a
  template.
- **`vendor/` linked_list_allocator fork** (`free_block_stats()` patch)
  — **not** a new project; this should become an **upstream PR** to
  `phil-opp/linked_list_allocator` proposing a free-block stats hook.
  Tracked here so it isn't forgotten.

---

## Recommended sequencing

Ship in waves; each wave is independently valuable and doesn't block the
next.

**Wave 0 — repo hygiene (prerequisite for *any* publish).**
- Choose a license (likely MIT/Apache-2.0 dual, the Rust-ecosystem
  default). Add `LICENSE-*` to the snitchos repo first.
- Decide org/home: separate repos per project vs. a `tools/` umbrella.
  Recommendation: separate repos for A, B, C+D; they have different
  audiences.

**Wave 1 — `itest-harness` (candidate A).** Highest value, lowest cost,
already split. This is the flagship extraction.

**Wave 2 — `.claude` Rust pack (candidate B).** Parallelisable with
Wave 1; the work is attribution/licensing/scrubbing, not engineering.

**Wave 3 — `protocol` + `collector` (C+D) as a pair.** "Embedded
structured telemetry + host collector." More decoupling work; do it once
Waves 1-2 prove the publishing pipeline.

**Wave 4 (opportunistic) — xtask utilities (E)** and the
**linked_list_allocator upstream PR**. Low urgency.

---

## Genericisation work, per candidate

What "extract" actually entails — the non-obvious parts.

**A — itest-harness**
- [ ] Prove `RunnerConfig` against a non-QEMU subject (e.g. a flaky
      `cargo test` binary or an HTTP healthcheck) — currently only ever
      driven by snitchos, so the abstraction is untested as *general*.
- [ ] Make Prom/OTLP export a feature flag (not everyone wants `prost`).
- [ ] Extract `.itest-baseline.toml` / `.itest-runs/` path config out of
      hardcoded names into `RunnerConfig`.
- [ ] README rewrite for a general audience (current one assumes xtask).
- [ ] Keep snitchos consuming it from crates.io (or git dep) to dogfood.

**B — .claude pack**
- [ ] Credit citypaul's TypeScript skills prominently (origin lineage).
- [ ] Grep skills/agents/commands for snitchos-specific examples and
      generalise them (e.g. the `setup` command, any RISC-V examples).
- [ ] Decide packaging: a repo you `git clone` into `.claude/`, or an
      `npx skills`-installable bundle (the `find-skills` skill's
      ecosystem).
- [ ] License + CONTRIBUTING.

**C — protocol**
- [ ] Document the wire format + versioning policy as a spec (the
      reorder-is-forbidden postcard constraint is the key gotcha).
- [ ] `no_std`-by-default, `std` feature for the stream decoder — already
      structured this way; just verify it builds clean standalone.

**D — collector**
- [ ] Parameterise service name, hart/task tag mappings, transport
      (socket vs stdin vs file).
- [ ] Split the generic OTLP/Prom encoders from the snitchos-specific
      span state machine.

**E — xtask utils**
- [ ] Decouple from snitchos workspace assumptions (paths, crate names).
- [ ] Bundle as one tool, not four crates.

---

## Open questions for the user

1. Separate repos per project, or one `chloe-rust-tools` umbrella?
2. License preference (MIT/Apache dual is the default — confirm)?
3. For the `.claude` pack: how do you want to credit citypaul, and
   git-clone vs `npx skills` distribution?
4. Is `protocol`+`collector` worth the decoupling now, or park until
   someone outside the project actually wants embedded telemetry?
5. Crates.io naming — `itest-harness` is generic enough it may be taken;
   want a prefixed name (`flake-harness`, `statest`, etc.)?
