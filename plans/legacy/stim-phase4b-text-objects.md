# Design: stim Phase 4b — text objects

**Status**: **IMPLEMENTED — Phase 4 complete.** P4b-0 (accumulator consolidation),
P4b-1 (`Range` + object-pending + `iw`/`aw`), and P4b-2 (quote objects `i"`/`a"`)
all shipped; see the P4b entries in [stim-grammar.md](../stim-grammar.md) for the
as-built record. Bracket objects (`i(`/`i{`/`i[`) were deferred past v1.

This file remains as the design rationale behind that work. The payoff of the range
model and the one grammar phase that needs real data-structure evolution. Builds on
P4a (word motions).

## What a text object is (and why it doesn't fit the motion model)

`diw` ("delete inner word"), `ci"` ("change inner quotes"), `da(` ("delete a
parenthesised group") — an operator applied to a region **defined by structure around
the cursor**, not by moving the cursor to a target. The cursor can sit anywhere inside
the word/quotes/brackets and the object still resolves to the same region.

That is exactly what the current operator model *cannot* express. Today an operator's
span is `[cursor, target]` — `motionTarget` returns a single endpoint and the range is
implicitly anchored at the cursor. A text object's region has **both ends independent
of the cursor** (`diw` mid-word extends left *and* right). So P4b introduces the two
things P4a didn't need:

1. a cursor-independent **`Range`** that an object produces and the operators consume;
2. a **text-object-pending sub-state** (the `i`/`a` prefix awaiting the object char),
   which reworks the operator-pending `None` branch.

`i` = *inner* (contents only); `a` = *around* (contents + the delimiters/whitespace).

## 1. The `Range` data structure

```
prod Range(startRow: Int, startCol: Int, endRow: Int, endCol: Int, wise: Wise)
```

Convention (matches the existing primitives so no rewrite is needed):
- **Charwise**: a half-open column span `[startCol, endCol)` on `startRow` (== endRow
  for every v1 object). This is exactly what `deleteCharRange(state, row, lo, hi)`
  already takes.
- **Linewise**: whole rows `[startRow, endRow]` inclusive — what `deleteLines(lo, hi)`
  takes. (No v1 object is linewise; this is here for future paragraph objects.)

**Motions are NOT refactored.** They keep their `[cursor, target]` path — it works and
touching it is churn for no gain. `Range` is added *alongside*, consumed by a new
`applyOpRange(state, range)` that reuses the existing operator primitives:

```
applyOpRange(state, range) = match state.pending.op {
  OpDelete => editIfChanged over deleteCharRange(state, range.startRow, range.startCol, range.endCol)
  OpChange => edit over enterInsert(deleteCharRange(...))            // delete the region + insert
  OpYank   => editIfChanged over setClip(rangeText(...), Charwise)   // copy, no delete
  _        => redraw
}
```
`deleteCharRange` already cuts to the register; `rangeText(state, row, lo, hi)` is a
one-liner (`Str.slice` clamped) — the charwise sibling of `joinLinesRange`. So the
operators consume `Range` with almost no new operator code — the object *machinery* is
the new part, not the operators.

## 2. The text-object-pending sub-state

After an operator, `i`/`a` is not a motion — it starts a two-key object (`i` then
`w`/`"`/`(`). The FSM must remember "operator pending AND awaiting an object char,
inner-or-around" across two keystrokes. New accumulator field:

```
sum ObjKind = NoObj | Inner | Around
prod Pending(op: Op, count: Int, object: ObjKind)   // the whole partial command
```

**Decision (2026-07-15): consolidate the accumulator into `Pending`.** The operator,
the count, and the object-prefix are one logical thing — the *partial command being
accumulated* — so P3's `Editor.count` moves back into `Pending` and `object` joins it.
This is the cleaner long-term model: count/object are meaningless except relative to a
pending command; the record resets as a unit (one `clearPending`, no multi-field reset
to keep in sync); it matches the grammar doc's original `Pending(count, register, op)`;
and it's the natural home for P5's `register`. Cost is a one-time churn of ~30
`Pending(op: …)` test constructions and the `count` references — done as a prep
refactor with no behaviour change. `clearPending` resets the whole record to
`Pending(OpNone, 0, NoObj)` — one reset point.

## 3. Dispatch changes

```
stepNormal(state, key) = match {
  isCountDigit(state, key)   => fold digit                       // (unchanged)
  state.object != NoObj      => stepObjectPending(state, key)    // NEW: resolve object char
  state.pending.op == OpNone => stepTopLevel(state, key)
  _                          => stepOperatorPending(state, key)
}
```

The operator-pending `None` branch (today: cancel) becomes the object-prefix entry:

```
stepOperatorPending … None branch:
  key == "i" => redraw(Editor(..state, object: Inner))     // await object char
  key == "a" => redraw(Editor(..state, object: Around))
  _          => redraw(clearPending(state))                // cancel (as before)
```

And the new object-char resolver:

```
stepObjectPending(state, key) = match {
  key == "Esc" => redraw(clearPending(state))
  _ => match textObjectRange(state, key) {                 // key = w " ( ) { } [ ]
        Some(range) => applyOpRange(clearFlags(state), range) wrapped in editIfChanged
        None        => redraw(clearPending(state))          // not an object char → cancel
      }
}
```

`Esc` explicit here too (the `None`-branch rework is exactly why P2.8 gave `Esc` its
own arm). `textObjectRange` reads `state.object` (Inner/Around) to pick bounds.

## 4. The objects (all single-line for v1)

`textObjectRange(state, key)` → `Maybe<Range>`, charwise, on the cursor's row.

**Word — `iw`/`aw`** (reuses the P4a scanners):
- inner: `[wordStart, wordEnd)` — the non-space run containing the cursor
  (`wordStart` via a left-scan to a space; `wordEnd` = `scanWordEnd`).
- around: inner + trailing spaces (`scanSpaces` past `wordEnd`); if none, include
  leading spaces instead — vim's rule.

**Quotes — `i"`/`a"`** (and `i'`): pair the line's `"` left-to-right (1st–2nd, 3rd–4th,
…); pick the pair enclosing the cursor (or the next pair after it).
- inner: `(open+1, close)`; around: `(open, close+1)`.

**Deferred past v1** (documented, not built): **bracket objects `i(`/`a(`/`{`/`[`**
(depth-matched pair scan — decided out of the v1 set on 2026-07-15), multi-line objects
(paragraph `ip`, cross-line brackets/quotes), tag objects `it`/`at`, sentence objects,
and the cursor-on-whitespace `iw` nicety. All are *more object arms / a linewise Range
path* — no further architecture.

## Staging

- **P4b-0 — prep refactor (no behaviour change).** Consolidate the accumulator:
  `Pending(op, count, object)`; move `Editor.count` in, add `object`. Update refs +
  the ~30 test constructions; suite stays green.
- **P4b-1 — the mechanism + the word object.** `Range`, `applyOpRange`, `rangeText`,
  the dispatch rework (object-pending sub-state), and `iw`/`aw`. Proves the whole path
  end-to-end (`diw`, `ciw`, `yaw`). **← stop here for review.**
- **P4b-2 — quote objects.** `i"`/`a"` (and `i'`) — new `textObjectRange` arms only;
  no new infrastructure. (Bracket objects deferred past v1.)

Each increment TDD'd through `stitch/tests/stim_fsm.rs`, driving op-pending +
object-pending states directly (as the P2/P3 tests do).

## Decisions (resolved 2026-07-15)

1. **Accumulator consolidated into `Pending(op, count, object)`** — the cleaner
   long-term model; count moves back off `Editor` (see §2).
2. **v1 object set = word + quotes** (`iw/aw`, `i"/a"`); bracket objects deferred.
3. **Register wiseness = charwise** for all v1 objects (intra-line spans).

---
*Fold into `plans/stim-grammar.md` once P4b ships.*
