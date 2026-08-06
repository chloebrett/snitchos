# Plan: `Map` you can build

**Branch**: none — all work lands directly on `main` (project rule).
**Status**: Active
**Source**: proposal 1 of
[stitch-language-improvements.md](stitch-language-improvements.md), evidenced by
[stitch-examples-findings.md](stitch-examples-findings.md).

## Goal

Give Stitch a `Map` module so a dictionary can be **built from runtime data**,
retiring the association-list workaround that 12 of the 30 corpus programs
hand-rolled.

## Context a future reader needs

- `Value::Map` already exists: `Rc<Vec<(Value, Value)>>` (`value.rs:37`). It has
  exactly two operations today — the literal (`interp.rs:591-604`, entry count
  fixed in source text) and indexed read (`eval_index`, `interp.rs:1205-1210`).
  There is no way to add, remove, or derive an entry.
- **This work adds no new value representation and no new syntax.** It is
  natives only. `NativeFn` carries its own `module` field and everything
  (`is_builtin_module`, `builtin_modules`, module-value construction, Tab
  completion) derives from the `NATIVES` table — asserted by
  `interp.rs:3523`'s own test. So `module: Some("Map")` is the whole
  registration story; there is no second table to update.
- The `List` module (`natives.rs:62-65`: `listAt`/`listSet`/`listInsert`/
  `listRemoveAt`, exported as `at`/`set`/`insert`/`removeAt`) is the template
  to copy — internal name prefixed to dodge the flat namespace, `export_as` for
  the module-qualified name.
- **Representation stays an association vector.** Lookup stays O(n). What the
  corpus was blocked on was expressibility, not speed; swapping in a hash or
  B-tree is interior work the bytecode-VM stage owns. Shipping the API first
  makes that later change invisible.

## Semantics to pin (the decisions this plan commits to)

These are the contract. Each is pinned by a test in the step that introduces it.

1. **Insertion order is preserved, and `insert` on an existing key keeps that
   key's original position.** This is not a new invention — it is exactly what
   the literal already does (`interp.rs:596`: *"Last duplicate key wins; keep
   the first occurrence's position"*). `Map.insert` must agree, or a literal
   and a built map with the same entries would render and compare differently.
2. **Every operation is persistent** — returns a new `Map`, mutates nothing.
   Consistent with `List.set`/`List.insert`/`List.removeAt`, and with the
   language's immutable-by-default stance.
3. **Keys compare by `Value`'s structural equality** — the same rule the
   literal's dedup and `eval_index` already use. No `Key`/`Hashable` contract
   is introduced here (see "Explicitly out of scope").
4. **Absence is `Maybe`, never a fault.** `Map.get` on a missing key is `None`,
   matching `m[k]`. `Map.remove` of an absent key is a no-op returning an equal
   map, not an error.
5. **A non-`Map` receiver is a runtime fault** with the established message
   shape: `"insert expects a Map, got List"` (mirroring `expect_list`,
   `natives.rs:951-959`).

## Acceptance Criteria

- [ ] A Stitch program can build a dictionary from a runtime-length sequence of
      pairs, with no association-list `prod` in sight.
- [ ] The per-key tally that `logstats.st` and `inventory.st` each hand-wrote as
      a five-line O(n²) `fold`/`find`/`map` is expressible as one `Map.update`
      call, and both files use it.
- [ ] A map built by `Map.insert` and an equal map written as a literal are
      `==`, and render identically through string interpolation.
- [ ] `Map.entries` and `Map.fromList` round-trip: `Map.fromList(Map.entries(m)) == m`.
- [ ] `m[k]` and `Map.get(m, k)` can never disagree — they are one code path.
- [ ] Every existing gate stays green: `cargo xtask test && cargo xtask itest
      && cargo xtask itest --scramble`, plus the `no_std` riscv64 lib build
      (the interpreter runs on target; these natives ship with it).

## Explicitly out of scope

Named so they are deferrals, not oversights:

- **`Set<T>`.** The improvements doc recommends *cutting* it from the design
  doc rather than building it. That is a separate decision and a separate
  (documentation-only) change.
- **The `Key`/`Hashable` contract.** `docs/language-design.md:209` claims a
  `mut`-field type as a map key is "a compile error"; there is no such check.
  Real, but it is checker work, not stdlib work, and it does not block anything
  here. It needs its own increment — or a doc correction.
- **A faster representation.** See above.
- **Checker support (`Ty::Map`).** `Ty` (`check.rs:32-48`) has no `Map`; a map
  value is `Ty::Dyn` and `Map<K, V>` in type position parses as
  `Named { name: "Map", args }` and is ignored, exactly like `List<T>` largely
  is today. Typing the module's signatures is generics work, tracked separately.
- **Rewriting all 12 workaround files.** Step 7 does two, deliberately — the
  two that prove the payoff. The rest are follow-up, and are worth doing only
  once this API has settled.

## Steps

Every step is RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test first, in its own edit.

**Test placement:** behaviour tests go in `natives.rs`'s inline `mod tests`
(`natives.rs:1107`), driven through `crate::testing::run_program` /
`run_program_err` — i.e. the RED case is a *Stitch snippet*, asserting through
the public surface, not a Rust call into a native. That is the existing idiom
in that module and it keeps these tests honest about being language behaviour.

**Run tests with** `cargo nextest run -p stitch` (never plain `cargo test` —
project rule).

**MUTATE step for every increment:**
`cargo xtask mutants stitch -- -f stitch/src/natives.rs`.

---

### Step 1: One lookup path shared by `m[k]` and `Map.get`

**Acceptance criteria**: `Map.get(m, k)` returns `Some(v)` for a present key and
`None` for an absent one, agreeing with `m[k]` on both; `Map.get` on a `List`
faults with `"get expects a Map, got List"`. `eval_index`'s map arm and
`Map.get` are demonstrably the same code — one helper, two callers.

**RED**: In `natives.rs`'s test module — a program that binds a literal map and
asserts `Map.get(m, "a") == m["a"]` for a present key and an absent one; plus a
`run_program_err` case for the non-map receiver.
**GREEN**: Extract the lookup body of `eval_index`'s `Value::Map` arm
(`interp.rs:1207-1210`) into a shared function; add `expect_map` next to
`expect_list`; register `mapGet` with `module: Some("Map"), export_as:
Some("get")`.
**MUTATE / KILL MUTANTS**: as above. A surviving mutant that swaps `Some`/`None`
or drops the key comparison means the present/absent pair isn't discriminating —
strengthen rather than accept.
**REFACTOR**: assess only.
**Done when**: criteria met, gate green, human approves the commit.

*Why this step first:* it is the smallest possible slice that exercises the
whole registration path (native → module → export name → completion), so every
later step is pure content. It also removes a real divergence risk before
adding callers — two lookups that "obviously agree" is exactly the shape of the
`native_cap`/`natives.rs` drift the improvements doc flags elsewhere.

### Step 2: `Map.has` and `Map.size`

**Acceptance criteria**: `Map.has(m, k)` is `true` for a present key, `false`
for an absent one, and `false` for `[:]`. `Map.size([:]) == 0`, and a
three-entry literal with a duplicate key has size 2 (the literal already
dedups — this asserts `size` counts *entries*, not source pairs).

**RED**: the assertions above as one Stitch test program.
**GREEN**: two natives over the shared helper from step 1.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 3: `Map.insert` and `Map.remove` — the ordering contract

**Acceptance criteria**:
- `Map.insert` on a **new** key appends; on an **existing** key it replaces the
  value and **keeps the original position** (assert via `Map.keys` order, or via
  interpolated rendering — pick whichever the step-4 ordering makes available;
  if `keys` isn't in yet, assert on the rendered string, which already shows
  order).
- `Map.remove` of a present key drops it and preserves the order of the rest;
  of an absent key returns an equal map.
- Neither mutates the receiver: after `Map.insert(m, "b", 2)`, `m` is unchanged.
- A built map equals the literal with the same entries:
  `Map.insert(Map.insert([:], "a", 1), "b", 2) == ["a": 1, "b": 2]`.

**RED**: the four assertions above. The last one is the load-bearing test —
it is what makes "built" and "written" maps interchangeable.
**GREEN**: two natives cloning the entry vector and editing the clone.
**MUTATE / KILL MUTANTS**: the replace-in-place branch is the mutation target
most likely to survive weak tests (a mutant that appends instead of replacing
still passes any test that only checks `get`). The order assertion is what
kills it — verify it does.
**REFACTOR**: assess.
**Done when**: criteria met, gate green, approved.

### Step 4: `Map.keys`, `Map.values`, `Map.entries`

**Acceptance criteria**: each returns a `List` in insertion order; on `[:]` each
returns `[]`. `Map.entries` yields `(K, V)` tuples — assertable by destructuring
one in a `match`.

**RED**: order-sensitive assertions (a two-entry map where the wrong order is
observable — not a single-entry map, which agrees with any implementation).
**GREEN**: three natives mapping over the entry vector; `entries` builds
`Value::Tuple`.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 5: `Map.fromList` — round-trip with `entries`

**Acceptance criteria**: `Map.fromList(pairs)` builds a map from a
**runtime-length** list of `(K, V)` tuples — the criterion the whole plan exists
for, so the test must build the list with `map`/`fold` rather than write a
literal. Duplicate keys follow the literal's rule (last wins, first position).
`Map.fromList(Map.entries(m)) == m`. A list element that is not a 2-tuple
faults.

**RED**: a program that does `[1, 2, 3] |> map(n -> (toStr(n), n)) |>
Map.fromList` and asserts size and a lookup; plus the round-trip; plus the
duplicate-key rule; plus the malformed-element fault.
**GREEN**: one native reusing the same insert logic as step 3, so the
duplicate rule cannot diverge from the literal's.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 6: `Map.update` — the operation the corpus was actually reaching for

**Acceptance criteria**: `Map.update(m, k, default, f)` inserts `f(default)`
when `k` is absent and `f(existing)` when present, keeping position.
Concretely, the tally is one expression:

```
fold(["a", "b", "a"], [:], (acc, w) -> Map.update(acc, w, 0, $ + 1))
```

...has size 2, `"a"` → 2, `"b"` → 1, and `"a"` first.

**RED**: exactly that program, asserting all four properties.
**GREEN**: one native over the step-3 insert path.
**MUTATE / KILL MUTANTS**: the absent-key branch (does it apply `f` to
`default`, or insert `default` raw?) is the subtle one — `0` vs `$ + 1` applied
to `0` differ, so the test above discriminates. Confirm the mutant dies.
**REFACTOR**: assess.
**Done when**: criteria met, gate green, approved.

*This is the payoff step.* Six of the twelve workaround files wanted precisely
this.

### Step 7: Retire the workaround in two corpus programs

**Acceptance criteria**: `logstats.st` and `inventory.st` no longer declare
`LevelCount`/`WordCount`/`CategoryTotal` as `Map` stand-ins, use `Map` directly,
**and every one of their existing `test` items still passes unchanged in
meaning**. Net line count drops in both. Their header comments — which currently
explain the association-list workaround and cite the findings doc — are updated
to stop describing a limitation that no longer exists.

**RED**: not applicable in the usual sense — the existing tests in both files
*are* the RED/regression net, and `stitch/tests/examples.rs` already gates them.
Where an assertion is stated in terms of the removed `prod` (e.g. comparing
against a `List<LevelCount>`), rewrite the assertion first, watch it fail
against the old implementation, then change the implementation.
**GREEN**: the rewrite.
**MUTATE**: not meaningful for example programs — skip, and say so in the
commit rather than silently omitting it.
**REFACTOR**: this step *is* the refactor.
**Done when**: `cargo nextest run -p stitch --test examples` green, both files
shorter, comments honest, approved.

*Chosen deliberately:* these two files contain the same five-line tally written
twice, so they are the clearest before/after evidence that the API solves the
problem it was designed for. Ten more files remain as follow-up work.

### Step 8: Documentation

**Acceptance criteria**:
- `docs/language-design.md` describes `Map` as buildable and states the
  ordering + persistence contract. Its `Key`/`Hashable` claim (line 209) is
  either corrected to say the constraint is not yet enforced, or left with an
  explicit "not yet implemented" marker — it must not keep asserting a compile
  error that does not exist.
- `plans/stitch-examples-findings.md` gets an update note on the `Map` finding
  (the doc's own convention — it did exactly this for `Str.parseInt`), saying
  the gap is closed and that future programs should use `Map` directly.
- `cargo xtask links` passes.

**Done when**: the above, approved.

## Pre-PR / pre-commit quality gate

Per step, before presenting for commit approval:

1. `cargo nextest run -p stitch` — green.
2. `cargo xtask clippy` — clean (workspace-correct lint, not `cargo clippy
   --workspace`).
3. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble` —
   the standing gate.
4. **The `no_std` riscv64 lib build** — `cargo build -p stitch --lib --target
   riscv64gc-unknown-none-elf`. The interpreter ships on target
   (`workload=stitch-repl`), so a native that reaches for `std` breaks the
   board, not the host suite. This bit before; check it every step.
5. Mutation report reviewed (steps 1–6).

## Risks

- **Ordering is a real contract, not an implementation detail.** Once `Map.keys`
  is observable, insertion order is user-visible and cannot be changed later
  without breaking programs. That is a deliberate commitment — it is what makes
  a built map interchangeable with a literal, and it is what a hash
  representation would have to preserve. Named here so a future representation
  swap knows it is bound by it.
- **O(n) lookup silently invites O(n²) programs.** The API makes the *shape*
  idiomatic while the cost stays linear. Acceptable at corpus scale; worth a
  sentence in the design doc so nobody is surprised.
- **`Map.get` vs `m[k]` divergence** — designed out in step 1 rather than left
  to discipline.

---
*On completion, `git mv` this file to `plans/legacy/` (project override of the
planning skill's "delete when complete" step) and run `cargo xtask links`.*
