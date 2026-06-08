# Plan: `cargo xtask audit [crate]` — mechanical evidence-gatherer for crate-audit

**Status:** proposed (plan only — no code yet).

**What this is:** a small, robust Rust tool that replaces the **painful, fragile
bash** the `crate-audit` skill currently drives by hand, and emits its evidence
in a stable form the skill consumes. It is deliberately **not** a static
analyzer — it gathers facts; the skill (plus the human) keeps every judgement.

**What this is not:** a name-resolution engine. It does not decide what is dead.
It produces *candidates with evidence*; the skill applies rule 6 (reserved
surface), spots intentional divergence, and proposes abstractions.

## Why — the bash hurts, repeatably

Running the skill on `kernel-core` (`plans/kernel-core-audit.md`), the mechanical
steps fought back in ways that are inherent to shell, not one-off mistakes:

| Painful bash step | How it broke | Tool replacement |
|---|---|---|
| per-symbol `for sym; do grep ... done` table | a stray `\r` in the symbol list truncated every `echo`; output rendered as bare names | structured table built in Rust, no shell quoting/`\r` hazards |
| `grep -rw "$sym"` caller counts | single-letter `PtePerms` flags `R`/`W`/`X` matched everywhere → garbage counts | word-boundary match + skip 1–2 char idents by default |
| "is it test-only?" | conflated test and prod callers; had to eyeball | counts split into **ext / int / test** columns |
| `RUSTFLAGS=-W dead_code` build | **silent on `pub` items in `pub` modules** — the dominant shape here; missed `alloc_contiguous`/`count_in_use` entirely | cross-crate symbol scan, which is the only thing that surfaces those |
| marker sweep | several greps, easy to forget one | one pass for `TODO/FIXME/HACK/stub/#[allow]/#[expect]/dead_code` |

The tool's job is to make those five rows **one command, deterministic, copy-paste
free** — so the skill spends its tokens on judgement, not on fighting `grep`.

## Decision: cheap mechanical tool, not semantic analysis

We explicitly choose a **line/regex/word-boundary** tool (the `loc.rs` altitude),
**not** a Rust-parsing analyzer. Rationale, from the syn-vs-semantics analysis:

- The annoying-to-regex part (symbol *extraction*) is the part `syn` would fix
  cheaply — but extraction was never the source of the *wrong* numbers.
- The part that caused the false positives (caller *resolution*: shadowing,
  trait-method name collisions, single-letter idents) is a **name-resolution**
  problem. `syn` does **not** resolve names — it hands back an AST of unresolved
  identifier tokens. So `syn` would make the tool prettier without making the
  counts more correct. Worst-of-both; rejected.
- Correct caller resolution needs rustc-grade analysis (rustdoc JSON /
  rust-analyzer crates / `rustc_private`). That's heavy, nightly-coupled, and is
  **already shipped** by mature tools — not worth hand-rolling for a candidate
  flagger.

Therefore the tool **embraces being a lower bound on deadness**: its counts can
over-report callers (a name collision looks like a use), never the reverse — so a
`pub` it flags as zero-caller is a *high-confidence* candidate, and the skill
verifies against design docs before anyone deletes. This is stated in the report
header and `--help`, so nobody mistakes a candidate for a verdict.

### Considered & rejected (record so we don't relitigate)

- **`syn` for full parsing** — fixes extraction, not resolution; the FPs survive.
  Adds a dep tree for no correctness gain on our actual pain. Rejected.
- **rust-analyzer crates / `rustc_private` / `dylint`** — real name resolution,
  but heavy, version-churny, toolchain-pinned. Massive overkill for replacing
  five greps. Rejected.

### Wrapped, not reimplemented (assumed installed)

This is workspace-specific tooling, so the tool **assumes** `cargo-machete`,
`cargo-udeps`, and `cargo-public-api` are on `PATH` and shells out to them for
the jobs they already do correctly — **no graceful-degradation path**; if one is
missing, the tool errors with an install hint (a one-line `cargo install …`),
the same way `xtask itest` simply requires `qemu-system-riscv64`.

- **`cargo-machete`** — unused-dependency detection (syntactic, fast). Replaces
  our manual `Cargo.toml` dep-by-dep grep entirely.
- **`cargo-public-api`** — public API surface listing/diff. Correct *surface*
  (nightly-backed). Complements our ext/int/test *caller* split — surface says
  *what's public*, our table says *who uses it*.
- **`cargo-udeps`** — compile-based unused-dep cross-check of `cargo-machete`
  (catches what the syntactic pass misses). Run on request (`--udeps`) since it
  triggers a full build.

The bespoke part — the **ext/int/test caller table** — is the one thing none of
these provide, and is the tool's actual spine.

## Design (mirrors `xtask/src/loc.rs`)

`loc.rs` is the template: pure, host-unit-tested classifier functions + a thin
`run()` that walks the workspace and prints a table. Reuse it directly.

- **File walk:** lift `collect_rs_files` (skips `target/`, `vendor/`).
- **Test-block detection:** `loc::count_file_lines` already tracks
  `#[cfg(test)]`/`#[test]` blocks via brace depth. Extract that into a shared
  `fn test_line_mask(content: &str) -> Vec<bool>`; `loc` consumes it too (refactor
  behind loc's existing tests — a safe seam). This is what lets caller counts
  split prod vs test, and lets symbol extraction skip test-only `pub` helpers
  (the reason `CapturingSink` must NOT be flagged).

Core pure functions (host-tested, no I/O):

1. `extract_pub_symbols(content, test_mask) -> Vec<PubSymbol{name,kind,line}>` —
   match `pub (fn|struct|enum|trait|const|type|mod) <name>`; skip `pub(crate)`/
   `pub(super)`, skip lines inside a test block, skip comments/strings.
2. `count_callers(symbol, content, test_mask) -> (prod, test)` — hand-rolled
   word-boundary match (no `regex` dep), minus the definition line, split by the
   mask. Idents ≤2 chars are skipped unless `--include-short` (kills the `R`/`W`/
   `X` noise by default).
3. `classify(ext, int, test) -> Verdict` — `KeepPublic` (ext>0) ·
   `DemotePubCrate` (ext=0,int>0) · `ProdUnusedTestOnly` (ext=0,int=0,test>0) ·
   `DeadCandidate` (all 0). Names say "candidate"; the report never says "dead".

`run(crate_name)`:
- Resolve target + sibling crate dirs from `CARGO_MANIFEST_DIR.parent()` (as
  `loc.rs`). Siblings = top-level crate dirs minus target/`vendor`/`target`/
  `stack`/`docs`/`plans`/`posts`.
- Count ext across sibling `src/`, int+test across target `src/`.
- Print: (a) the ext/int/test table sorted (ext asc, name); (b) a **candidates**
  section (ext=0 rows) headed "verify against design docs before acting — rule
  6"; (c) a **markers** section (`TODO/FIXME/HACK/stub/#[allow]/#[expect]/
  dead_code` with file:line); (d) an **unused-deps** section = parsed output of
  `cargo machete <crate>` (+ `cargo udeps` under `--udeps`); (e) a **surface**
  section = parsed `cargo public-api -p <crate>` listing.
- **`--json`** mirrors all of the above as one object so the skill ingests it
  without scraping the table.

CLI: add `Cmd::Audit { crate_name: String, json: bool, include_short: bool,
udeps: bool }` to `xtask/src/main.rs`, dispatch `audit::run(...)`, new
`xtask/src/audit.rs`, README subcommand row. Missing external tool → error with a
`cargo install <tool>` hint (no degradation).

## Sanity gate (bake the anti-pattern check into the tool)

If every symbol reports ext=0 AND int=0, that is almost always a broken scan
(wrong dir, zero siblings resolved), not a dead crate. Emit a loud warning and a
non-zero hint rather than a clean-looking empty table — the exact all-zero-table
anti-pattern the skill warns about, enforced in code.

## Increments (TDD, each leaves the tree green)

1. Extract `test_line_mask` from `loc::count_file_lines`; `loc` reuses it, its
   tests stay green. (Pure refactor behind existing coverage.)
2. `extract_pub_symbols` — tests: each kind; skips `pub` in a `#[cfg(test)]`
   block; skips `pub(crate)`/`pub(super)`; ignores `pub` in comment/string.
3. `count_callers` — tests: ≤2-char ident skipped by default; definition line
   excluded; test-block callers land in `test`; trait-method collision documented
   as a known over-count.
4. `classify` — tests: each of the four verdicts.
5. External-tool wrappers — `parse_machete(stdout)`, `parse_public_api(stdout)`
   as pure functions tested against captured sample output; a `which`-style
   presence check that errors with an install hint.
6. `run` wiring + table/candidates/markers/unused-deps/surface print + `--json` +
   the all-zero sanity gate.
7. README row + `cargo xtask clippy` clean.

## Acceptance (regression oracle = today's hand audit)

`cargo xtask audit kernel-core` must reproduce `plans/kernel-core-audit.md`:
- `alloc_contiguous`, `count_in_use` → candidates (ext=0).
- `is_empty`, `PRE_INIT_BYTES` → DemotePubCrate (ext=0, int>0).
- `CapturingSink` **absent** from candidates (it's in a `#[cfg(test)]` block).
- markers section lists the three `#[allow]`/comment sites and zero TODOs.
- unused-deps section reports none for `kernel-core` (spin/protocol/postcard all
  used) — confirms the `cargo machete` wrapper parses correctly.
- `cargo test -p xtask` green; `cargo xtask clippy` clean; **no new linked deps**
  (no `regex`, no `syn`) — the external tools are subprocesses, not crate deps.

## Required external tools

`cargo install cargo-machete cargo-public-api cargo-udeps`. Assumed present (this
is workspace tooling); the tool errors with this exact line if one is missing.
Document it in the README subcommand note.

## Follow-ups (separate plans if wanted)

- Workspace-wide mode (`xtask audit` no-arg → every crate's candidate list).
- Teach the `crate-audit` skill to invoke `xtask audit --json` as its Step 2
  evidence-gathering default (it replaces the bash entirely, not as a fallback).
