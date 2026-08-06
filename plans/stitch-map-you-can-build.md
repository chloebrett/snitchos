# Plan: `Map` you can build

**Branch**: none — all work lands directly on `main` (project rule).
**Status**: Active
**Source**: proposal 1 of
[stitch-language-improvements.md](stitch-language-improvements.md), evidenced by
[stitch-examples-findings.md](stitch-examples-findings.md).

## Goal

Give Stitch a `Map` module so a dictionary can be **built from runtime data**,
retiring the association-list workaround that 12 of the 30 corpus programs
hand-rolled — and make `Map` a first-class citizen of the combinator vocabulary
rather than a third collection kind the stdlib does not recognise.

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

### Measured starting state (probes, 2026-08-06)

Run before writing the steps, because the first draft of this plan assumed
otherwise and was wrong:

```
map(["a": 1, "b": 2], $a)      → map expects a List, got Map
fold(["a": 1, "b": 2], 0, f)   → fold expects a List, got Map
count(["a": 1, "b": 2])        → fold expects a List, got Map
                                     in count
map(pairs, (k, v) -> k)        → function expects 2 argument(s), got 1
map(pairs, e -> match e { (k, v) => k })   → works
```

Two consequences that shape the whole plan:

1. `native_map`/`native_filter`/`native_fold` fall through to `expect_list`, so
   a `Map` is invisible to them — and because **the entire prelude is
   fold-derived** (`count`, `any`, `all`, `contains`, `find`, `first`, `last`,
   `min`, `max`, `flatten`), none of it works on a `Map` either. Adding a `Map`
   module without fixing this would ship a third collection kind the language's
   own vocabulary does not know: exactly the non-uniformity proposal 5 of the
   improvements doc exists to complain about.
2. `(k, v) -> body` is **two parameters, not tuple destructuring** — so entry
   iteration needs `e -> match e { (k, v) => … }`. See "Risks".

## Semantics to pin (the decisions this plan commits to)

These are the contract. Each is pinned by a test in the step that introduces it.

1. **Insertion order is preserved, and `insert` on an existing key keeps that
   key's original position.** Not a new invention — it is what the literal
   already does (`interp.rs:596`: *"Last duplicate key wins; keep the first
   occurrence's position"*). `Map.insert` must agree, or a literal and a built
   map with the same entries would render and compare differently.
2. **Every operation is persistent** — returns a new `Map`, mutates nothing.
   Consistent with `List.set`/`List.insert`/`List.removeAt`, and with the
   language's immutable-by-default stance.
3. **Keys compare by `Value`'s structural equality** — the same rule the
   literal's dedup and `eval_index` already use. No `Key`/`Hashable` contract is
   introduced here (see "Explicitly out of scope").
4. **Absence is `Maybe`, never a fault.** `Map.get` on a missing key is `None`,
   matching `m[k]`. `Map.remove` of an absent key is a no-op returning an equal
   map, not an error.
5. **A non-`Map` receiver is a runtime fault** with the established message
   shape: `"insert expects a Map, got List"` (mirroring `expect_list`,
   `natives.rs:951-959`).
6. **The core combinators accept a `Map`, iterating `(K, V)` tuples**, under the
   eager-in/eager-out rule:
   - `fold(m, init, f)` — `f` receives each entry as a `(K, V)` tuple.
   - `map(m, f)` → a **`List`**: `f` projects an entry to an arbitrary value, so
     the result is no longer key-shaped.
   - `filter(m, pred)` → a **`Map`**: a predicate preserves entries, so shape is
     preserved.
   The map/filter asymmetry is deliberate and matches Kotlin. It is what makes
   "transform" and "select" read differently at a glance.
7. **`Map.update(m, k, default, f)` applies `f` to `default` when the key is
   absent**, giving one algebraic law:
   `update(m, k, d, f) ≡ insert(m, k, f(unwrapOr(get(m, k), d)))`.
   *(Elixir's identically-named function inserts `default` raw instead, which
   would make the tally `Map.update(acc, w, 1, $ + 1)` rather than
   `Map.update(acc, w, 0, $ + 1)`. Chosen deliberately for the law; flipping it
   is a one-line change to step 8 if the other reading is preferred.)*

## API surface

```
// combinators — existing natives taught about Map (steps 1–2)
fold(m, init, f)   map(m, f) -> List   filter(m, pred) -> Map
// ...which lights up the whole fold-derived prelude: count, any, all,
//    find, first, last, flatten — for free, no new natives.

Map.get(m, k)                 -> Maybe<V>
Map.has(m, k)                 -> Bool
Map.insert(m, k, v)           -> Map
Map.remove(m, k)              -> Map
Map.update(m, k, default, f)  -> Map
Map.keys(m)                   -> List<K>
Map.values(m)                 -> List<V>
Map.entries(m)                -> List<(K, V)>
Map.fromList(pairs)           -> Map
Map.mapValues(m, f)           -> Map
```

**Two deliberate omissions**, both from applying *"could I recompute it? yes ⇒
delete and derive"*:

- **No `Map.size`.** Once `fold` accepts a `Map`, `count(m)` is it.
- **No `Map.any`/`Map.find`/`Map.each`.** Same reason — the prelude's are
  fold-derived and will just work.

**One survivor worth naming as a judgement call:** `Map.entries(m)` is
technically `map(m, e -> e)`, so by the same test it is derivable. It is kept
because it is the *named inverse* of `fromList` and the round-trip law
(`fromList(entries(m)) == m`) is much easier to read with both names present.
A reviewer who disagrees can cut it without touching anything else — `keys` and
`values` are not similarly derivable (their `map`-based spellings need the
`match` wrapper and read horribly), so they stay regardless.

**`Map.has` survives the same test on a subtlety:** the prelude's
`contains(m, v)` folds over *entries*, so it answers "is `("a", 1)` an entry?",
not "is `"a"` a key?". Key membership genuinely is a different question.

## Acceptance Criteria

- [ ] A Stitch program can build a dictionary from a runtime-length sequence of
      pairs, with no association-list `prod` in sight.
- [ ] The per-key tally that `logstats.st` and `inventory.st` each hand-wrote as
      a five-line O(n²) `fold`/`find`/`map` is expressible as one `Map.update`
      call, and both files use it.
- [ ] `count`, `any`, `all` and `find` work on a `Map` with no `Map.`-prefixed
      equivalents added.
- [ ] `map` over a `Map` yields a `List`; `filter` over a `Map` yields a `Map`.
- [ ] A map built by `Map.insert` and an equal map written as a literal are
      `==`, and render identically through string interpolation.
- [ ] `Map.fromList(Map.entries(m)) == m`.
- [ ] `m[k]` and `Map.get(m, k)` can never disagree — they are one code path.
- [ ] Every existing gate stays green: `cargo xtask test && cargo xtask itest
      && cargo xtask itest --scramble`, plus the `no_std` riscv64 lib build.

## Explicitly out of scope

Named so they are deferrals, not oversights:

- **Lambda tuple auto-spread.** Making `(k, v) -> …` destructure a tuple
  argument would fix entry-iteration ergonomics, but it changes arity resolution
  at *every* lambda application site and contradicts the design doc's explicit
  "parens group multiple params". `Map.mapValues` covers the common case;
  `e -> match e { (k, v) => … }` remains the general escape hatch. Rejected, not
  forgotten.
- **`Set<T>`.** The improvements doc recommends *cutting* it from the design doc
  rather than building it. Separate decision, documentation-only change.
- **The `Key`/`Hashable` contract.** `docs/language-design.md:209` claims a
  `mut`-field type as a map key is "a compile error"; there is no such check.
  Real, but checker work, and it blocks nothing here.
- **A faster representation.** See above.
- **Checker support (`Ty::Map`).** `Ty` (`check.rs:32-48`) has no `Map`; a map
  value is `Ty::Dyn` and `Map<K, V>` in type position parses as
  `Named { name: "Map", args }` and is ignored, exactly like `List<T>` largely
  is today. Typing these signatures is generics work, tracked separately.
- **Rewriting all 12 workaround files.** Step 10 does two, deliberately — the
  two that prove the payoff. The rest are follow-up, worth doing only once this
  API has settled.

## Steps

Every step is RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test first, in its own edit.

**Test placement:** behaviour tests go in `natives.rs`'s inline `mod tests`
(`natives.rs:1107`), driven through `crate::testing::run_program` /
`run_program_err` — the RED case is a *Stitch snippet*, asserting through the
public surface, not a Rust call into a native. That is the existing idiom in
that module and it keeps these tests honest about being language behaviour.

**Run tests with** `cargo nextest run -p stitch` (never plain `cargo test` —
project rule).

**MUTATE step for every increment:**
`cargo xtask mutants stitch -- -f stitch/src/natives.rs`.

**Note on ordering:** steps 1–2 need no new API at all — map *literals* already
exist, so they are testable against today's language. They come first because
they are the smallest possible increments *and* because they decide what the
rest of the API needs to contain (they are what deleted `Map.size`).

---

### Step 1: `fold` accepts a `Map`

**Acceptance criteria**: `fold(["a": 1, "b": 2], 0, (acc, e) -> acc + 1) == 2`,
folding entries as `(K, V)` tuples in insertion order. As a consequence and
asserted in the same step, the fold-derived prelude works with **no new
natives**: `count(m) == 2`, `any(m, …)`, `all(m, …)`, and `find(m, …)` returning
`Some((k, v))`. `fold` over `[:]` returns the seed.

**RED**: a program asserting the above, including at least one where entry
*order* is observable (a two-entry map folded into a concatenated string — a
one-entry map agrees with any implementation).
**GREEN**: a `Value::Map` arm in `native_fold` that iterates entries as
`Value::Tuple`s.
**MUTATE / KILL MUTANTS**: a mutant that reverses entry order or drops the last
entry must die — that is what the order and count assertions are for.
**REFACTOR**: assess only.
**Done when**: criteria met, gate green, human approves the commit.

*Highest value-per-line in the plan: one arm, and ten prelude functions light up.*

### Step 2: `map` and `filter` accept a `Map`

**Acceptance criteria**: `map(m, f)` returns a **`List`** of `f`'s results, in
insertion order. `filter(m, pred)` returns a **`Map`** preserving order, and
`filter(m, _ -> false) == [:]`. The asymmetry is asserted explicitly — a test
that would pass if `filter` returned a `List` is not testing the contract.

**RED**: assertions above, including `filter(…) == [:]` and a filtered result
still answering `m2["a"]` (proving it is genuinely a `Map`, not a `List` that
prints similarly).
**GREEN**: `Value::Map` arms in `native_map` and `native_filter`.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 3: One lookup path shared by `m[k]` and `Map.get`

**Acceptance criteria**: `Map.get(m, k)` returns `Some(v)` for a present key and
`None` for an absent one, agreeing with `m[k]` on both; `Map.get` on a `List`
faults with `"get expects a Map, got List"`. `eval_index`'s map arm and
`Map.get` are demonstrably the same code — one helper, two callers.

**RED**: a program asserting `Map.get(m, "a") == m["a"]` for a present *and* an
absent key, plus a `run_program_err` case for the non-map receiver.
**GREEN**: extract the lookup body of `eval_index`'s `Value::Map` arm
(`interp.rs:1207-1210`) into a shared function; add `expect_map` next to
`expect_list`; register `mapGet` with `module: Some("Map"), export_as:
Some("get")`.
**MUTATE / KILL MUTANTS**: a mutant swapping `Some`/`None` or dropping the key
comparison means the present/absent pair is not discriminating — strengthen
rather than accept.
**REFACTOR**: assess only.
**Done when**: criteria met, gate green, approved.

*First step introducing a native, so it exercises the whole registration path
(native → module → export name → completion); every later step is pure content.
It also removes a divergence risk before adding callers — two lookups that
"obviously agree" is the shape of the `native_cap`/`natives.rs` drift the
improvements doc flags elsewhere.*

### Step 4: `Map.has`

**Acceptance criteria**: `true` for a present key, `false` for an absent one,
`false` for `[:]`. Asserted alongside `contains(m, …)` to document that they
answer different questions — `Map.has(m, "a")` is key membership,
`contains(m, ("a", 1))` is entry membership.

**RED**: the assertions above, including the `contains` contrast.
**GREEN**: one native over the shared helper from step 3.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 5: `Map.insert` and `Map.remove` — the ordering contract

**Acceptance criteria**:
- `Map.insert` on a **new** key appends; on an **existing** key it replaces the
  value and **keeps the original position** (assert via `count` + the rendered
  string, both available by now).
- `Map.remove` of a present key drops it and preserves the order of the rest; of
  an absent key returns an equal map.
- Neither mutates the receiver: after `Map.insert(m, "b", 2)`, `m` is unchanged.
- A built map equals the literal with the same entries:
  `Map.insert(Map.insert([:], "a", 1), "b", 2) == ["a": 1, "b": 2]`.

**RED**: the four assertions above. The last is load-bearing — it is what makes
"built" and "written" maps interchangeable.
**GREEN**: two natives cloning the entry vector and editing the clone.
**MUTATE / KILL MUTANTS**: the replace-in-place branch is the likeliest mutant to
survive weak tests (appending instead of replacing still passes any test that
only checks `get`). The order assertion is what kills it — verify that it does.
**REFACTOR**: assess.
**Done when**: criteria met, gate green, approved.

### Step 6: `Map.keys`, `Map.values`, `Map.entries`

**Acceptance criteria**: each returns a `List` in insertion order; on `[:]` each
returns `[]`. `Map.entries` yields `(K, V)` tuples — assertable by
destructuring one in a `match`.

**RED**: order-sensitive assertions over a two-entry map (see step 1's note on
why one entry proves nothing).
**GREEN**: three natives mapping over the entry vector; `entries` builds
`Value::Tuple`.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 7: `Map.fromList` — round-trip with `entries`

**Acceptance criteria**: builds a map from a **runtime-length** list of `(K, V)`
tuples — the criterion the whole plan exists for, so the test must build the
list with `map`/`fold` rather than write a literal. Duplicate keys follow the
literal's rule (last wins, first position). `Map.fromList(Map.entries(m)) == m`.
A list element that is not a 2-tuple faults.

**RED**: `[1, 2, 3] |> map(n -> (toStr(n), n)) |> Map.fromList`, asserting size
and a lookup; plus the round-trip; plus the duplicate-key rule; plus the
malformed-element fault.
**GREEN**: one native reusing step 5's insert logic, so the duplicate rule cannot
diverge from the literal's.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

### Step 8: `Map.update` — the operation the corpus was reaching for

**Acceptance criteria**: inserts `f(default)` when the key is absent and
`f(existing)` when present, keeping position. Concretely the tally is one
expression:

```
fold(["a", "b", "a"], [:], (acc, w) -> Map.update(acc, w, 0, $ + 1))
```

…has `count == 2`, `"a"` → 2, `"b"` → 1, and `"a"` first. The law
`update(m, k, d, f) == insert(m, k, f(unwrapOr(get(m, k), d)))` is asserted
directly for both the present and absent case.

**RED**: exactly that program plus the law.
**GREEN**: one native over step 5's insert path.
**MUTATE / KILL MUTANTS**: the absent-key branch is the subtle one — does it
apply `f` to `default`, or insert `default` raw (the Elixir reading)? The tally's
`"b"` → 1 assertion is what discriminates; confirm the mutant dies.
**REFACTOR**: assess.
**Done when**: criteria met, gate green, approved.

*This is the payoff step.* Six of the twelve workaround files wanted precisely
this.

### Step 9: `Map.mapValues`

**Acceptance criteria**: `Map.mapValues(m, f)` returns a `Map` with the same
keys in the same order and `f` applied to each value — `f` sees the **value
only**, never a tuple. `Map.mapValues([:], f) == [:]`.

**RED**: `Map.mapValues(["a": 1, "b": 2], $ * 10) == ["a": 10, "b": 20]`, plus
the empty case, plus a key-order assertion.
**GREEN**: one native.
**MUTATE / KILL MUTANTS / REFACTOR**: as above.
**Done when**: criteria met, gate green, approved.

*Earns its place by dodging the tuple-arity trap entirely for the single most
common transformation — see "Risks".*

### Step 10: Retire the workaround in two corpus programs

**Acceptance criteria**: `logstats.st` and `inventory.st` no longer declare
`LevelCount`/`WordCount`/`CategoryTotal` as `Map` stand-ins, use `Map` directly,
**and every one of their existing `test` items still passes unchanged in
meaning**. Net line count drops in both. Their header comments — which currently
explain the association-list workaround and cite the findings doc — are updated
to stop describing a limitation that no longer exists.

**RED**: not applicable in the usual sense — the existing tests in both files
*are* the regression net, and `stitch/tests/examples.rs` already gates them.
Where an assertion is stated in terms of the removed `prod` (e.g. comparing
against a `List<LevelCount>`), rewrite the assertion first, watch it fail
against the old implementation, then change the implementation.
**GREEN**: the rewrite.
**MUTATE**: not meaningful for example programs — skip, and say so in the commit
rather than silently omitting it.
**REFACTOR**: this step *is* the refactor.
**Done when**: `cargo nextest run -p stitch --test examples` green, both files
shorter, comments honest, approved.

*Chosen deliberately:* these two contain the same five-line tally written twice,
so they are the clearest before/after evidence that the API solves the problem it
was designed for. Ten more files remain as follow-up work.

### Step 11: Documentation

**Acceptance criteria**:
- `docs/language-design.md` describes `Map` as buildable and states the
  ordering, persistence, and combinator (`map`→`List` / `filter`→`Map`)
  contracts. Its `Key`/`Hashable` claim (line 209) is corrected to say the
  constraint is not yet enforced — it must not keep asserting a compile error
  that does not exist.
- `plans/stitch-examples-findings.md` gets an update note on the `Map` finding
  (the doc's own convention — it did exactly this for `Str.parseInt`), saying
  the gap is closed and that future programs should use `Map` directly.
- `cargo xtask links` passes.

**Done when**: the above, approved.

## Pre-commit quality gate

Per step, before presenting for commit approval:

1. `cargo nextest run -p stitch` — green.
2. `cargo xtask clippy` — clean (workspace-correct lint, not `cargo clippy
   --workspace`).
3. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble` —
   the standing gate.
4. **The `no_std` riscv64 lib build** — `cargo build -p stitch --lib --target
   riscv64gc-unknown-none-elf`. The interpreter ships on target
   (`workload=stitch-repl`), so a native reaching for `std` breaks the board,
   not the host suite. This bit before; check it every step.
5. Mutation report reviewed (steps 1–9).

## Risks

- **Ordering is a real contract, not an implementation detail.** Once
  `Map.keys` and map-aware `fold` are observable, insertion order is
  user-visible and cannot change later without breaking programs. A deliberate
  commitment: it is what makes a built map interchangeable with a literal, and
  any future hash representation is bound by it.
- **The tuple-arity trap is not fixed, only routed around.** `map(entries,
  (k, v) -> …)` faults with `"function expects 2 argument(s), got 1"` (measured),
  because that is a two-parameter lambda, not destructuring. Users must write
  `e -> match e { (k, v) => … }` — and *that* form is one arm away from the
  `semver.st` match-arm maximal-munch trap (proposal 2 of the improvements doc).
  `Map.mapValues` (step 9) removes the need for the common case; the general
  case stays awkward until proposal 2 lands.
- **O(n) lookup silently invites O(n²) programs.** The API makes the *shape*
  idiomatic while the cost stays linear. Acceptable at corpus scale; worth a
  sentence in the design doc so nobody is surprised.
- **`Map.get` vs `m[k]` divergence** — designed out in step 3 rather than left
  to discipline.

---
*On completion, `git mv` this file to `plans/legacy/` (project override of the
planning skill's "delete when complete" step) and run `cargo xtask links`.*
