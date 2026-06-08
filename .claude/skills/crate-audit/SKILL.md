---
name: crate-audit
description: Audit a Rust crate or module for bloat, dead code, unused features, repetition, over-broad API, and architectural debt — evidence-first, with a prioritized findings report that separates safe deletions from judgement calls. Use when asked to "crate-audit", "audit this crate", "debloat", "find dead code / unused features", "look for repetition / tech debt", or "what can we delete / simplify" in a given crate.
---

# crate-audit — crate/module debt audit

Systematically find what a crate no longer needs, what's duplicated, and what's
structurally rotting — backed by evidence, not vibes. Produce a report the human
acts on; **do not delete or refactor without explicit approval.**

## Operating rules

1. **Evidence before every claim.** "Unused" requires a grep that finds zero
   callers. "Duplicated" requires showing both sites. "Dead feature" requires
   showing the flag gates nothing. No finding without a command output behind it.
2. **Audit, then report. Don't edit.** This skill produces a findings report.
   Offer to apply the *obvious wins* only after the human picks them. Never
   bundle a refactor into the audit.
3. **Separate "delete now" from "needs your call."** A `pub` item with no
   in-repo caller might be deliberate external/library API. Flag the ambiguity;
   let the human decide. Default to *flag*, not *delete*.
4. **Justified duplication is not debt.** Two code paths that look similar but
   serve genuinely different contracts (e.g. a text exporter and a protobuf
   exporter) are not automatically collapsible. Say why it might be intentional.
5. **Respect the project's conventions** (CLAUDE.md, existing patterns). Debt is
   *drift from the codebase's own norms*, not drift from your preferences.

## Step 1 — Scope

Establish before analysing:

- **The target**: which crate / module(s).
- **Its consumers**: who calls it? Another crate in the workspace? A binary?
  External (published) API? This determines whether unused-in-repo == dead.
  Find them: `grep -rl "use <crate>::" --include=*.rs .` and check `Cargo.toml`
  `[dependencies]` reverse-edges.
- **Its mandate**: what is this crate *supposed* to be? (Read its lib.rs / module
  docs.) Bloat is code that doesn't serve the mandate.

## Step 2 — Gather evidence (run the battery)

Adapt paths to the target. Capture outputs; they're the backing for findings.

```bash
# Size + shape — where is the mass?
wc -l <crate>/src/*.rs | sort -n
cargo xtask loc 2>/dev/null || tokei <crate>   # prod vs test vs comment split

# Compiler/linter dead-code signal (strongest evidence)
RUSTFLAGS="-W dead_code -W unused" cargo build -p <crate> 2>&1 | grep -iE 'never used|unused'
cargo clippy -p <crate> --all-targets 2>&1 | grep -iE 'warning|never used'
# Note: pub items are NOT flagged by dead_code — cross-reference them manually (below).

# Existing debt markers
grep -rn 'dead_code\|#\[allow\|#\[expect\|TODO\|FIXME\|XXX\|HACK\|deprecated\|legacy\|for now\|temporary' <crate>/src/

# Public API inventory
grep -rnE 'pub (fn|struct|enum|trait|const|type|mod) ' <crate>/src/

# Cross-reference each PUBLIC symbol against real usage (the key move for libs):
#   for each `pub` name, count callers OUTSIDE its own module + in consumer crates.
for sym in <Sym1> <Sym2> ...; do
  n=$(grep -rw "$sym" <consumer-dirs> <crate>/src 2>/dev/null | grep -v "pub .* $sym" | wc -l)
  printf "%-30s %s\n" "$sym" "$n"
done

# Features: declared vs gating vs enabled
grep -A30 '\[features\]' <crate>/Cargo.toml
grep -rn 'cfg(feature' <crate>/src/         # what each feature actually gates
grep -rn 'feature = ' . --include=Cargo.toml # who enables them

# Dependency usage — heavy dep for trivial use?
grep -A40 '\[dependencies\]' <crate>/Cargo.toml
# for each dep, grep its import sites; a dep used in one place may be removable.

# Arg-threading / god-functions (architectural smell)
grep -rn 'too_many_arguments\|fn .*(' <crate>/src | grep -cE '\(.*,.*,.*,.*,.*,.*,'  # 6+ args
```

## Step 3 — Analyse across dimensions

For each, ask the questions and record findings with evidence. Go deeper than a
yes/no — name the specific symbol/line/site.

### A. Dead code & unused exports
- Which `pub` items have **zero callers** outside their module *and* zero
  consumer-crate usage? (compiler won't warn on `pub` — you must grep.)
- Should still-public-but-internal-only items be `pub(crate)` / private?
- Any `#[allow(dead_code)]` / `#[expect(dead_code)]` — is the code waiting for a
  caller that never came?
- Unreachable match arms, `if false`, vestigial error variants never constructed?
- Re-exports in `lib.rs` that nothing imports?

### B. Unused / vestigial features
- Cargo features declared but gating nothing, or never enabled by anyone?
- Feature flags effectively always-on (so the flag is noise) or never-on (dead)?
- CLI flags / config options that are parsed but never read, or wired to nothing?
- Config knobs (`Option<T>`, enums) that only ever take one value in practice?

### C. Speculative / YAGNI code
- Abstractions (traits, generics, hooks) with exactly **one** implementation and
  no second on the horizon — is the indirection paying rent?
- "For the future" code with no current caller (the thing you almost shipped but
  parked) — delete it; git remembers.
- Extensibility points (plugin registries, strategy enums) exercised by one case.
- Over-parameterised functions whose extra params are always passed the same value.

### D. Repetition (collapsible)
- Near-duplicate logic across modules/functions — show both, propose the shared
  helper *or* explain why they must stay separate (different contracts).
- Repeated construction boilerplate (the same struct built field-by-field in N
  places) — a constructor / `From` / builder.
- Copy-pasted test fixtures — a factory function.
- Parallel `match` arms or per-variant code that a macro / trait method collapses.
- The same grep/transform pipeline inlined repeatedly.

### E. Architectural debt
- `too_many_arguments` (6+) — threading state that wants a context struct.
- God-functions / god-modules doing several unrelated jobs; or the inverse —
  a "module" that's one tiny function nobody else needs.
- Two different ways to do the same thing in the codebase (inconsistent patterns).
- Primitive obsession — bare `(u32, u32)` / stringly-typed where a named type
  would prevent bugs.
- Leaky abstractions: a "pure" module that secretly does I/O; a layer that just
  forwards.
- Coupling smells: module A reaches into B's internals; cyclic-ish dependencies.
- Mismatch with stated architecture (CLAUDE.md says X, code does Y).

### F. API surface & types
- Over-broad `pub` (publishing internals as API).
- Structs with all-public fields that should have invariants / constructors.
- Types that exist only to be converted to another type immediately.

### G. Test & doc debt
- Test-to-prod ratio anomalies (a tiny module with a huge test file, or logic
  with none).
- Weak tests: `assert!(true)`, round-trip-only tests, tests that restate the impl.
- Duplicated fixtures (link to D).
- Comments that describe *what* not *why*, stale comments, and **SAFETY/invariant
  comments that lie** (assert a guarantee the code doesn't keep — high severity,
  these cause real bugs).

### H. Dependencies
- Deps pulled in for one trivial call (replaceable with std / a few lines).
- Heavy transitive cost for light use.
- Multiple deps doing the same job.

## Step 4 — Report

Produce a prioritized findings table. Each finding:

| # | Dimension | Severity | Finding | Evidence | Recommendation | Effort | Risk |

- **Severity**: high (active hazard / real confusion) · med (clear debt) · low (nit).
- **Evidence**: the file:line + the grep/command output that proves it.
- **Risk**: of *acting* on it — e.g. "may be external API," "changes wire format."

Then split into two lists the human chooses from:

- **Obvious wins (safe to delete/collapse now)** — zero-caller private items,
  dead branches, parked code, exact-duplicate fixtures. Low risk, high confidence.
- **Needs your call** — anything touching public API, wire/serialization formats,
  load-bearing-but-subtle code, or "duplication" that might be intentional.
  Present the trade-off; don't decide for them.

Close with a one-line **mass estimate**: roughly how many lines each bucket
removes, so the human can judge whether it's worth a session.

## Step 5 — Apply (only on request)

If — and only if — the human picks items, apply them following the project's
workflow (TDD where logic changes, run the relevant tests + clippy after each
deletion, keep each change small and independently revertable). Re-run the
build/clippy after every removal to confirm nothing depended on it. Never batch a
big delete without checking the suite is still green.

## Anti-patterns (don't do these)

- Declaring code dead from a single narrow grep. Cross-check consumer crates and
  trait-dispatch / macro-generated call sites (grep misses these).
- Treating all duplication as collapsible. Some is honest divergence.
- Deleting `pub` API because the *repo* doesn't use it — it may be the point.
- "Simplifying" by adding an abstraction (that's usually *more* code, not less).
- Bundling the refactor into the audit. Report first.
