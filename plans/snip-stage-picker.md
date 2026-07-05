# `snip` — Sonnet-assisted staging for parallel-agent workflows

> Status: **DESIGN** (approved plan, not yet built). TDD from here.

## Problem

Running several agents in parallel produces a working tree with **many unrelated
changes at once**. Turning that into clean, single-concern commits means answering
"which of these changed files belong to *this* commit?" by hand, repeatedly. That
triage is the bottleneck, not writing the message.

`snip` moves the triage to Sonnet: **you write the commit message, Sonnet selects
the files that match it**, you eyeball the result, then finalize. The agent in the
loop never runs git — the deterministic `xtask` binary does the staging/committing.
So this respects the "I own commits" rule: no agent git access, just a build tool
running git after you approve.

## Goals

- **Fast.** One model call per proposal; tight payload; sub-few-second turnaround.
- **Accurate.** Correctly separates N concurrent unrelated changes by concern.
- **Safe.** Never invents paths; never commits without an explicit second step;
  detects if the working tree drifted between propose and finalize.

Non-goals: amending, multi-commit planning in one shot.

**Update (shipped):** partial-file / hunk-level staging now works — see "Partial
application" below. And every `snip` subcommand prints its wall-clock duration to
stderr (`(snip propose took 4.2s)`), so the model round-trip cost is visible.

## User-facing flow — three explicit steps

```
cargo xtask snip "kernel: guard-page fault reporting via per-hart exc stack"
    → gathers working-tree changes, asks Sonnet, prints (confidence per file + overall):
        include (4):
          + [high] kernel/src/trap/exc_stack.rs      new file, matches "per-hart exc stack"
          + [high] kernel/src/trap/mod.rs            wires exc stack into trap entry
          + [med ] kernel-core/src/scause.rs         guard-page fault decode
          + [high] docs/trap-and-interrupt-model.md  documents the reporting path
        exclude (3):
          - [high] stitch/src/interp.rs   unrelated: Stitch evaluator change
          - [high] hitch/src/lib.rs       unrelated: hitch manifest work
          - [low ] Cargo.lock             ambiguous: touched by multiple changes; excluded
        overall: medium — scause.rs is plausibly shared with another concurrent change
      writes .git/snip-plan.json  (message, selected paths, confidence, worktree fingerprint)

cargo xtask snip stage
    → git add <selected paths>.  Now inspect: `git diff --cached`.

cargo xtask snip commit
    → git commit -m <message from plan>, then delete the plan file.
```

Rationale for three steps (not two): finalize is split into **stage** then **commit**
so there is a natural checkpoint to review the staged diff before it's sealed. `snip`
proposes and writes the plan but mutates nothing; `stage` mutates the index only;
`commit` seals it.

### Flags

- `snip "<msg>" --fast` — diffstat-only payload (filenames + ins/del counts, no diff
  bodies). Coarser, fastest. Default sends capped diffs.
- `snip "<msg>" --yes` — propose **and** stage in one go (skips the manual look at the
  proposal; still leaves `commit` as a separate step).
- `snip stage --force` / `snip commit --force` — proceed even if the worktree
  fingerprint drifted (default: refuse and tell you to re-run `snip`).
- `snip commit --no-verify` — pass through to `git commit --no-verify`.

## Architecture — new crate `snip` + xtask glue

Split along the hexagonal boundary already used in this workspace: **pure core is
network- and git-free and unit-tested with fixtures; the edges (git, the `claude`
subprocess) live where they can't be unit-tested anyway.**

### Crate `snip` (new workspace member, host-only)

Pure, testable core:

```rust
pub enum Status { Added, Modified, Deleted, Renamed, Untracked, TypeChange }

pub struct Candidate {
    pub path: String,
    pub status: Status,
    pub diff: String,     // capped/rendered by the caller; core just embeds it
}

/// Model-reported confidence, low granularity on purpose (calibrated buckets
/// beat a fake-precise 0.0–1.0 the model can't actually calibrate).
pub enum Confidence { High, Medium, Low }

pub struct Excluded { pub path: String, pub reason: String, pub confidence: Confidence }
pub struct Included { pub path: String, pub reason: String, pub confidence: Confidence }

pub struct Selection {
    pub include: Vec<Included>,
    pub exclude: Vec<Excluded>,
    /// Overall confidence in the whole partition ("did I understand this tree?"),
    /// distinct from per-file confidence. Low here = re-read before staging.
    pub overall: Confidence,
    /// Free-text caveat the model may attach when overall is not High
    /// (e.g. "two changes both touch scause.rs; couldn't cleanly separate").
    pub note: Option<String>,
}

/// Pure: builds the exact prompt string sent to `claude -p`. Testable.
pub fn build_prompt(message: &str, candidates: &[Candidate]) -> String;

/// Pure: parse the model's JSON reply and VALIDATE it against the candidate
/// set — any path the model returned that isn't a real candidate is dropped
/// (with a recorded warning); any candidate the model didn't mention is
/// treated as excluded-by-omission. Testable, incl. hallucination + empty cases.
pub fn parse_reply(raw_json: &str, candidates: &[Candidate])
    -> Result<Selection, ParseError>;
```

One thin impure function (the only network/subprocess surface in the crate):

```rust
pub struct ClaudeCfg { pub model: String, pub timeout: Duration }

/// Spawns `claude -p --model <m> --output-format json --allowedTools ""`,
/// feeds the prompt on stdin, reads the JSON envelope, extracts the result
/// text, then `parse_reply`s it. One retry on parse failure with a
/// "return ONLY the JSON, no prose" nudge appended.
pub fn pick(message: &str, candidates: &[Candidate], cfg: &ClaudeCfg)
    -> Result<Selection, PickError>;
```

`snip`'s deps: `serde` + `serde_json` only. No git, no http. `claude` is invoked
via `std::process::Command`.

### xtask glue (`xtask/src/snip.rs`)

Owns **all** git I/O and the CLI wiring:

- **Gather** — `git status --porcelain=v1 -z` for the candidate set (staged +
  unstaged + untracked). For each candidate, `git diff` (tracked) or read file
  contents (untracked), then **cap to ~200 lines/file** with a `… N lines
  truncated …` marker; binaries → status line only. Build `Vec<Candidate>`.
  - Note: if files are already staged when `snip` runs, include them as candidates
    too (diff against HEAD), so a half-staged tree still gets sorted correctly.
- **Fingerprint** — a cheap hash over `(path, status, blob-oid-or-mtime+len)` of the
  candidate set, stored in the plan; `stage`/`commit` recompute and compare.
- **Propose** — call `snip::pick`, print the include/exclude table, write
  `.git/snip-plan.json`.
- **Stage** — read plan, verify fingerprint, `git add -- <paths>`.
- **Commit** — read plan, verify fingerprint, `git commit -m <message>`, delete plan.

Plan file schema (`.git/snip-plan.json`, git-ignored by living under `.git/`):

```json
{
  "message": "…",
  "include": ["kernel/src/trap/exc_stack.rs", "…"],
  "fingerprint": "blake3-or-sha of candidate set",
  "created_step": "<no wall-clock; use git's own or a counter>"
}
```

## Transport decision — `claude -p`, not the API

The Anthropic **Messages API is separate pay-per-token billing** and is **not**
covered by the Claude Max subscription. The `claude` CLI *is* covered by Max and is
already on PATH. So `snip` shells out to `claude -p` — no API key, no extra billing.

Consequence: we don't get tool-forced structured output. We recover reliability by:

1. A strict prompt: "Respond with ONLY a JSON object of this exact shape … no
   markdown, no prose." Schema embedded in the prompt.
2. `--output-format json` so we get a clean envelope; extract the `result` field.
3. `--allowedTools ""` + single-turn: the model can't read files or wander — all
   context is in the prompt, so it just answers.
4. `parse_reply` validates against the candidate set and one retry on parse failure.

If a raw API key ever gets set up (`ANTHROPIC_API_KEY`), a `pick` variant using
`ureq` (the stack `collector` already uses) with tool-forced output is a drop-in
faster/stricter alternative. Out of scope for v1.

## The prompt (shape)

System/instructions embedded in the single user prompt:

- Role: "You are triaging a git working tree that contains **several unrelated
  changes made concurrently by parallel agents**. Given ONE commit message, select
  exactly the files that belong to that commit and no others."
- The commit message.
- The candidate list: for each, `path`, `status`, and the capped diff.
- Decision rules: when a file plausibly belongs to a *different* concurrent change,
  exclude it; when a shared file (e.g. `Cargo.lock`, a mod.rs) is touched by multiple
  concerns and can't be cleanly attributed, exclude it and say why (whole-file only
  in v1 — can't split it, so don't guess).
- Output contract: the exact JSON shape; include+exclude each carry a one-line
  reason **and a `confidence` of high|medium|low**; a top-level `overall` confidence
  plus an optional `note` explaining any hesitation. Instruct: "be honest — low
  confidence when a file is plausibly part of a *different* concurrent change is the
  useful signal, not a failure."

### Confidence gating

The tool surfaces confidence, and gently gates on it (never silently):

- `snip` prints per-file `[high|med|low]` tags and the `overall` line; low-confidence
  rows are colored/marked so they draw the eye.
- If `overall` is **Low**, `snip stage` refuses without `--force` and points at the
  `note`. Medium/High stage normally. `--yes` (propose+stage) is likewise blocked at
  Low overall unless `--force`.
- Confidence is persisted in the plan so `stage` can enforce this without re-asking.

## Speed budget

- Payload: status always; diffs capped at ~200 lines/file; binaries elided. Keeps the
  prompt small even with a large working tree.
- One `claude -p` call per `snip`. `--model sonnet`. Node startup (~1–2s) dominates;
  model latency on a small payload is low.
- `--fast` drops diff bodies entirely for the cheapest possible call.

## TDD order

1. **`parse_reply`** (pure) — fixtures: clean selection; hallucinated path dropped;
   candidate omitted → excluded-by-omission; malformed JSON → `ParseError`; empty
   include (model says "none match"); confidence parsing (per-file + overall, with an
   unknown/missing confidence value defaulting to Low, not erroring).
2. **`build_prompt`** (pure) — asserts message + every candidate path + the JSON
   contract appear; snapshot via `insta`.
3. **xtask gather** — cap logic (a >200-line diff gets the truncation marker), binary
   elision, untracked-file inclusion. Testable against a temp git repo fixture.
4. **fingerprint drift** — same candidates → same hash; a changed blob → different
   hash; `stage`/`commit` refuse on mismatch.
5. **`pick`** (impure) — behind a test seam: inject a fake "claude" command (a script
   that echoes canned JSON) so the subprocess path is covered without hitting Max.
6. Wire the three subcommands into `xtask` `Cmd`.

## Partial application (shipped)

When a file mixes changes belonging to this commit with unrelated ones, the model
returns it in `include` with a `hunks` array naming only the relevant hunk ids —
otherwise it omits `hunks` and the whole file is staged.

- Each candidate's diff is parsed into positional hunks (`H1`, `H2`, …) by
  `snip::parse_hunks` (byte-preserving: header + every hunk text reconstructs the
  input). `build_prompt` labels each hunk `[H1]` in the prompt and documents the
  contract; `parse_reply` validates requested ids against the file's real hunks and
  **drops** an include whose partial selection names no real hunk (never silently
  stages a whole file the model wanted only part of).
- The plan persists partials as `{path, hunks: [ids]}` (ids, not patch text). At
  `stage`, `snip::build_patch` reconstructs a patch of just those hunks from a
  freshly re-derived `git diff HEAD -- <path>`, applied with
  `git apply --cached --recount`. Re-deriving is safe because the drift guard already
  proved the working tree (and thus the hunk ids) is unchanged since propose.
- Gather stores the **full** diff (no line cap); the per-hunk display cap lives in
  `build_prompt`, so `parse_reply`'s id validation and `build_patch`'s reconstruction
  both see faithful hunk text.
- Proven end-to-end by `snip/tests/apply.rs`: a two-hunk change, stage only H2 via
  `git apply --cached`, assert the index holds H2 and H1 stays unstaged.

## Open questions deferred

- Caching the gather between `snip` and `stage` to skip a second `git diff`.

### Future: suggest a commit message from already-staged changes

The inverse of the core flow. Given whatever is *already staged* (`git diff --cached`),
ask Sonnet to propose a commit message (subject + optional body), so the tool closes
the loop in both directions: "message → files" (today) and "files → message" (this).
Useful when you've hand-staged with `git add -p` or `snip --stage` and just want a
well-formed message. New verb, e.g. `snip --message` (or `snip msg`): gather the
staged diff (reuse `parse_status` on `--cached` + `git diff --cached`), prompt for a
conventional-commit-style subject, print it (and optionally `git commit` it, same
opt-in split as today). Reuses the whole transport + gather spine; only the prompt and
output contract change (a message string instead of a `Selection`). Pairs naturally
with the edit-provenance ledger (message could summarise "everything agent X did").

### Future: whole-tree partition mode (`snip plan`)

The inverse of v1. Instead of "here's a message, pick its files", ask Sonnet to
**partition the entire working tree into a disjoint set of proposed commits** — one
call, N suggested commits, each with a generated message and its file set, every
changed file assigned to exactly one commit (a true partition: no overlap, no
leftovers). Then present them as a **checklist you tick**: approve commit 1, skip 2,
approve 3 → `snip` stages+commits the approved ones in order.

This is the natural end state for the parallel-agent workflow: one command turns a
chaotic multi-change tree into a reviewed sequence of clean commits. v1 (message →
files) is the building block; this reuses the same gather + prompt + validate spine,
swapping the output contract from `Selection` to `Vec<ProposedCommit>` and adding the
disjointness/coverage invariant to `parse_reply`'s validation. Worth building once v1
proves the triage quality.

### Future: edit-provenance ledger (deterministic "pure file" fast path)

A `PostToolUse` hook (`.claude/settings.json`, on Write/Edit) appends to a local
ledger — e.g. `.git/snip-provenance.jsonl` — one record per agent edit:
`{ timestamp, agent_id/session, file, lines_touched }`. Over a parallel-agent run
this builds a map of **who touched what**.

From that ledger, a file is **pure** if only one agent edited it. Pure files can be
staged/committed with zero model inference — the provenance already proves the change
is single-concern. `snip` only needs the model for the *mixed* files (touched by
several agents, or edited by hand outside any agent). So the flow becomes:

1. Hook records provenance as agents work.
2. `snip` partitions the tree: pure-by-agent files → group by agent (trivial, free,
   deterministic); mixed/ambiguous files → fall back to the Sonnet triage above.
3. Report both, staged separately.

This is faster and cheaper than asking the model about everything, and it's *more*
accurate for the pure majority (provenance is ground truth, not inference). It also
opens per-agent commit messages ("everything agent X did") as a natural grouping.
Caveats: the hook only sees agent edits, not manual ones or non-Edit mutations
(codegen, `cargo fmt`), so "pure" means "pure among tracked agent edits" — the model
fallback still owns anything the ledger can't vouch for. Line-level records also let a
future hunk-staging mode attribute individual hunks, not just whole files.
