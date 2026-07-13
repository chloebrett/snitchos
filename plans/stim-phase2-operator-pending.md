# Design: stim Phase 2 — the operator-pending core

**Status**: ✅ SHIPPED. The load-bearing phase of the stim grammar
(`plans/stim-grammar.md`). Phase 0 + Phase 1 were shipped; this introduced the
`pending` accumulator that turns `stepNormal` from a flat table into a tiny parser.
All 8 increments (2.1–2.8) landed, TDD'd through `stitch/tests/stim_fsm.rs` (47 FSM
tests) + the driver tests. Delivered: `d`/`c`/`y` × motions, `dd`/`cc`/`yy`, `Y`,
`p`/`P`, deletes-cut-to-register, and clean operator-pending cancel (explicit `Esc`
arm + unknown-key swallow). Notes vs the design below: (1) `Target` gained an
`inclusive: Bool` (only `$` is inclusive) so `d$` deletes through EOL; (2) `x`/`X`/`D`
were refactored to one-liners over `deleteCharRange` and inherit the register cut;
(3) the range-attribute-on-span decision held — operators emit names
(`stim.delete`/`change`/`yank`/`paste`), range payload deferred.

## The one generalization

Every Phase 1 key was complete in one keystroke (`r{c}` was the gentle exception —
two keys via a `Replace` *mode*). Phase 2 is different: `d` then `w` is **two keys
forming one command**, and after `d` the editor must remember "operator `d` is
pending, awaiting a motion." That memory is the *operator-pending* sub-state.

Crucially, operator-pending is **within Normal mode** — the cursor still shows, the
mode stays `Normal`. So it is not a new `Mode` (that was right for `Replace`, which
is a genuinely different input context). It is a new **field on `Editor`** — the
`pending` accumulator — because it also has to accrete a count (Phase 3) and a
register (Phase 5) later. The doc's evolution table:

- P2: `+ pending: Pending{op}` `+ clipboard` (unnamed register, for `y`/`p`)
- P3: `pending{count}`
- P5: `pending{register}` + a named-register map

## State-shape changes

```
sum Op   = OpNone | OpDelete | OpChange | OpYank      // OpNone = not operator-pending
sum Wise = Charwise | Linewise                        // a motion's / register's shape
prod Pending(op: Op)                                  // grows count (P3), register (P5)
prod Register(text: Str, wise: Wise)                  // the unnamed clipboard
prod Editor(lines, row, col, mode, pending: Pending, clipboard: Register)
```

`initialState` seeds `pending: Pending(OpNone)` and `clipboard: Register("", Charwise)`.

## Motions become `state -> Target` (the load-bearing reuse)

Today motions are `state -> state` — they move the cursor directly (`moveLeft`,
`moveDown`, `moveLineEnd`, …). For an operator to consume a motion, the motion must
instead yield a **target position + a wiseness**, which the caller uses two ways:

```
prod Target(row: Int, col: Int, wise: Wise)

motionTarget(state, key) -> Maybe<Target>    // "" if key isn't a motion
```

- **Bare** (just `w`): `moveTo(state, target)` sets the cursor to `(target.row,
  target.col)` clamped. The wiseness is ignored.
- **Under an operator** (`dw`): the operator takes the range `[cursor, target]` and
  the wiseness decides charwise-vs-linewise. **One `motionTarget` definition powers
  both `l` and `dl`.** This reuse *is* the phase.

Phase 2 wires the **existing** motions as targets (word-motions `w b e` are Phase 4):

- charwise: `h l 0 $ ^`  — all intra-line in Phase 2 (none cross a line boundary)
- linewise: `j k`

The Phase 1 `moveLeft`/`moveRight`/`moveLineStart`/… bodies are refactored to
produce a `Target`; the bare-motion dispatch arms route through `moveTo` and must
stay green (regression guard on every existing motion test).

## The range model

Given cursor `C = (row, col)` and target `T = (trow, tcol, wise)`:

- **Charwise** (Phase 2: same line): slice `[min(col,tcol), max(col,tcol)]` on `row`.
  Delete/change/yank that intra-line span. (Cross-line charwise arrives with `w` /
  text-objects in Phase 4 — noted as the extension point; Phase 2 charwise = one line.)
- **Linewise**: whole lines `[min(row,trow), max(row,trow)]`. The op affects entire
  lines.
- **Doubled operator** (`dd`/`cc`/`yy`): linewise self — `count` whole lines from the
  cursor (count = 1 until Phase 3). Range `[row, row]`.

## Operators consume a range

```
extractRange(state, range) -> Register        // the text + wiseness the op acts on
applyDelete(state, range)  -> state           // remove range, stash into clipboard
applyChange(state, range)  -> state           // = applyDelete then enterInsert
applyYank(state, range)    -> state           // stash into clipboard, no removal
```

`c` = `d`-then-insert; `y` = `d`-without-the-delete. Define the range extraction +
deletion once; `c` grafts `enterInsert`, `y` skips the removal. Add `d/c/y` × the
motions **and** `dd/cc/yy`, and the cross-product falls out of these three
definitions — the whole point of building the production, not the commands.

`Y` (deferred from Phase 1) = `yy`, and now works because `clipboard` exists.

## stepNormal becomes a parser

```
stepNormal(state, key):
  pending.op == OpNone:                        // top level (today's flat table)
    key is an operator (d/c/y) → set pending.op, stay Normal, Redraw   // enter op-pending
    key is a motion            → moveTo(state, motionTarget), Redraw    // (unchanged)
    key is an action (x X ~ …) → run it                                 // (unchanged)
  pending.op != OpNone:                        // operator-pending
    key == the same operator   → linewise self (dd/cc/yy): op × whole line(s), Edit
    key is a motion            → op over [cursor, motionTarget], Edit
    key == Esc                 → cancel, clear pending, Redraw
    else                       → cancel (unknown key), clear pending, Redraw
  (every arm that leaves op-pending resets pending.op = OpNone)
```

`step` still dispatches on `mode` (still `Normal` throughout) — the operator-pending
branch lives *inside* `stepNormal`, keyed on `pending.op`. That is exactly why
`pending` is a field and not a mode.

## Paste — `p` / `P`

Reads the `clipboard: Register(text, wise)`:

- **charwise**: insert `text` after (`p`) / before (`P`) the cursor on the current
  line; cursor lands on the last pasted char.
- **linewise**: open a new line below (`p`) / above (`P`) holding `text`; cursor to
  its start.

The wiseness is why the register carries it: `yy`+`p` opens a line, `yl`+`p` inserts
inline. Deletes populate the clipboard too (vim's unnamed register), so `x`/`dd`/`D`
then `p` works — cheap once the register exists.

## Observable effects extend

Each operator is a span: `stim.delete`, `stim.change`, `stim.yank`, `stim.paste`
(vs. Phase 1's per-key `stim.delete-char` / `stim.join` / …). Same `editIfChanged`
honesty — a `d` with an empty range (`d` at EOL) spans nothing.

**Open point (the range payload):** the doc wants each operator span to *carry its
range*. Today `Effect::Edit(Str)` holds only a name, and the driver's `Telemetry`
trait's `span_open(name)` takes only a name — so a structured range attribute needs
a `Telemetry` extension. Recommendation: **Phase 2 ships the operator names as
spans** (the observable-operator granularity) and defers the range *attribute* to a
small follow-up once we decide how the trait carries span attributes. The grammar
architecture doesn't block on it.

## Forward direction: the clipboard service (build toward, not away from)

The stim register is the **soft form** of the future clipboard primitive
([docs/clipboard-design.md](../docs/clipboard-design.md)): eventually there is *one*
capability-scoped clipboard *service*, vim's registers are **named slots in it**, and
an entry is an immutable record `{value, schema, provenance, label, caps}` reached
over IPC. Phase 2 must not paint us into a corner against that. Concretely:

- **Keep `Register` a distinct, growable value type** (not a bare `Str`) — same
  discipline as `Pending`. When the service lands, `Register` grows toward `Entry`
  (provenance `{file cap, lines, time}`, an IFC label, optional caps) without a
  signature churn, and `y`/`p` re-target from the in-state field to service IPC.
- **Yank/paste are already spans** (`stim.yank` / `stim.paste`) — that *is* the
  clipboard doc's "copy/paste are spans, the history is a trace on the wire"
  alignment. We get it for free by staying on the observable-effect model.
- **`wise` is the first bit of entry metadata.** Charwise/Linewise is stim-specific
  shape that, in the service world, lives in the entry's schema/attributes. Modelling
  it on `Register` now is the seed of typed entries (clipboard P2), not a throwaway.
- **Explicitly out of stim P2:** provenance, IFC labels, capability-carrying entries,
  named registers, history. Those ride the clipboard *service* (its P1–P5), not the
  editor. Stim P2 is a single in-state unnamed register whose *shape* is
  entry-compatible — structural alignment, not feature parity. Named registers (`"a`)
  are grammar Phase 5, and land as service slots.

The one-line rule: **stim's register is a local, immutable, span-observed,
wiseness-tagged value that the clipboard service can later absorb as an entry.**

## TDD increments (each RED→GREEN through `stitch/tests/stim_fsm.rs`)

1. `Op`/`Wise`/`Pending`/`Register` + Editor fields; `initialState` seeds them;
   `d`/`c`/`y` enter operator-pending (mode stays Normal, `pending.op` set), Redraw.
2. `motionTarget` + `moveTo`; refactor Phase 1 motions to `Target`; **all existing
   bare-motion tests stay green** (regression).
3. `d` + charwise motion (`d$ d0 dl dh d^`) — intra-line delete → `Edit("stim.delete")`.
4. `dd` — doubled-operator linewise self.
5. `dj` / `dk` — linewise multi-line delete.
6. `c`/`cc` (= delete + Insert) and `y`/`yy`/`Y` (yank, no delete) → clipboard.
7. `p`/`P` — charwise + linewise paste; deletes populate the clipboard.
8. `Esc` / unknown-key cancels operator-pending cleanly.

## Open decisions (confirm before I start)

1. **Range payload on the span** — names now, range attribute deferred (recommended),
   or extend the `Telemetry` trait this phase?
2. **Register carries wiseness** — `Register(text, wise)` so paste is correct
   (recommended yes; required for `yy`+`p` vs `yl`+`p`).
3. **Charwise = intra-line for Phase 2** — scope charwise ranges to one line now, add
   cross-line with `w`/text-objects in Phase 4 (recommended yes).
4. **Deletes populate the unnamed register** — so `x`/`dd`/`D` then `p` works
   (recommended yes; cheap once `clipboard` exists).

---
*Fold into `plans/stim-grammar.md` (or delete) once Phase 2 ships.*
