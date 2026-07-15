# Design: the stim grammar

**Status**: Design (post-v1 follow-up). Phases below are the natural commit/review
milestones; not all will be built, and the later ones are gated on other axes landing.

## The insight

vim's Normal mode is not a list of commands — it is a tiny **language**, and its
grammar is roughly:

```
command := [count] [register] ( operator [count] motion | operator operator | action )
motion  := charwise | wordwise | linewise | find | search | text-object
```

`dd`, `dw`, `d$`, `3dd`, `diw` are not five commands. They are one production —
`operator motion` — instantiated over a small set of operators and a small set of
motions, times a count. **`dd` is `d` applied to its own linewise self** (the
doubled-operator special case). So the job is to design the *productions and the
two small sets they range over*, not to enumerate the cross product. Get `d` and
the motions right and `dw`/`d$`/`dj`/`dG`/`dd` all fall out; add `c` and `y` and
you get another two full rows of the cross product for free (`c` is `d`-then-insert,
`y` is `d`-without-deleting-into-a-register).

This is the same principle the rest of the stim design runs on ("don't build the
bespoke twist; ride the axis"): don't build the bespoke command; build the
production.

## The architecture stim has today, and the one thing it's missing

stim is a pure FSM: `step(state, key) -> Step{state, effect}`, dispatched by
`state.mode` (`Normal | Insert | Command`). Normal mode today is a flat table of
**single-key** commands — every key is a complete command (`h`, `x`, `o`, `:`).

The whole grammar is unlocked by one generalization: **Normal mode needs to
accumulate a partial command across keystrokes.** `d` then `w` is two keys forming
one command; `step` sees one key at a time, so after `d` the state must remember
"operator `d` is pending, awaiting a motion." This is the *operator-pending*
sub-state — and stim already has the shape of it: the `Command` mode (`:`) is
exactly "accumulate keys until the command completes." The grammar generalizes
that accumulator from the `:` line to the Normal-mode command.

Concretely, the `Editor` record grows a **pending-command accumulator**:

```
prod Pending(count: Int, register: Str, op: Op)     // Op = None | Delete | Change | Yank | …
prod Editor(lines, row, col, mode, pending, registers)
```

and `stepNormal` becomes a small parser:

- `pending.op == None`:
  - a **digit** → fold it into `pending.count` (count prefix).
  - a **motion** key → move the cursor by that motion × count; clear pending.
  - an **operator** key → `pending.op = <that op>`; stay in Normal (operator-pending).
  - an **action** (`x`/`i`/`o`/`p`/…) → run it × count; clear pending.
- `pending.op != None` (operator-pending):
  - a **motion** key → compute the range `[cursor, motion-target]`, apply the op to
    it, clear pending. **This is `dw`, `d$`, `dj`, …**
  - the **same operator** key again → linewise self: apply the op to `count` whole
    lines. **This is `dd`, `cc`, `yy`.**
  - `Esc` → cancel, clear pending.

The load-bearing reuse: **motions are defined once** as `state -> position` (or
`state -> state` when used bare), and consumed two ways — bare (move the cursor) and
as a range endpoint (for an operator). One `wordForward` definition powers both `w`
and `dw`. That reuse *is* the design.

## The taxonomy (the two small sets, plus the rest)

**Motions** (`state -> target`, with a wiseness = charwise | linewise):
- charwise: `h l 0 $ ^ | f{c} t{c} ; ,` (already have `h`/`l`)
- wordwise: `w b e W B E ge`
- linewise: `j k gg G {count}G` (already have `j`/`k`)
- find/search: `f`/`t`/`/pattern`/`n`/`N` (search rides its own phase)
- text-objects: `{a|i}{w p " ' ( ) { } [ ] < > t}` — a motion that yields a *range*
  directly rather than a target (`diw`, `ci"`).

**Operators** (`range -> edit`, consume a motion or double for linewise):
- `d` delete, `c` change (= delete + Insert), `y` yank (delete-shape without the
  delete, into a register), `>` `<` indent, `gu` `gU` `g~` case, `=` reformat.
- doubled: `dd cc yy >> <<` — linewise self.

**Actions** (complete single/prefix commands, no motion):
- inserts: `i I a A o O` (have `i`/`o`) — each is "position the cursor, enter Insert."
- edits: `x X` (delete char under/before), `r{c}` (replace char), `s` (substitute =
  `cl`), `D`/`C`/`Y` (= `d$`/`c$`/`yy`), `~` (toggle case), `J` (join lines).
- paste: `p P` (from a register).
- undo/repeat: `u`, redo, `.` (dot-repeat the last change).
- macros: `q{reg}` … `q` record, `@{reg}` replay.

**Counts**: `[count]` prefix, multiplies a motion or repeats an action/operator
(`3w`, `2dd`, `5x`). Also mid-command (`d3w`). One accumulator, folded into `pending.count`.

**Registers**: `"{reg}` prefix selects the register for the next yank/delete/paste.
The unnamed register is the default clipboard.

**Command-line (`:`)**: generalize today's direct `:w`/`:q` recognizer into a real
accumulated line — `:w`, `:q`, `:wq`, `:q!`, `:w {name}`, `:e {name}`, ranges
(`:%s/a/b/`, `:1,5d`). This is the same accumulator as the operator one, in a
different mode.

**Visual mode** (`v V Ctrl-V`): a fourth mode — select a range interactively, then an
operator acts on the *selection* instead of a motion. Reuses the operator machinery
with the range coming from the visual selection rather than a motion.

## The cross-cutting ties — half the "grammar" is the deferred axes

Several of these grammar features are not editor features at all; they are the
stim design's cross-cutting **axes** wearing vim keybindings. Each has a **soft
form** buildable now and a **hard form** that lands when its axis does — the same
pattern as the rest of stim.

- **undo / redo / `.` dot-repeat / macros → the replay axis (time-travel scrub).**
  undo is "replay the state history backwards"; `.` is "replay the last change";
  a macro is "record a keystroke span and replay it." Soft form: an in-state
  history vector. Hard form: the editor's edit stream *is* a replayable trace —
  and stim already emits a span per effect, so **the undo history and the telemetry
  trace are the same object**. This is the single most on-thesis feature in the
  whole grammar: the edit history is observable by construction, so undo/scrub read
  the wire.
- **yank / paste / registers → the IFC (taint) axis.** Soft form: a plain
  clipboard in the state. Hard form: a yanked span carries the taint of its source;
  pasting into a lower-integrity context is a checked flow. `"ap` becomes an
  authority question, not just a copy.
- **`:w`/`:e`/multi-file / sessions → the checkpoint / persistence axis.** Soft
  form: write-through the file cap. Hard form: a session is a checkpoint you can
  restore.
- **`q`/`@` macros with big counts → the budgets axis.** A replayed macro is
  bounded work; `1000@a` wants a fuel budget (which the interpreter already has).

So the grammar is not one milestone — it is the surface syntax of several axes, and
the exotic keys *earn their hard form as their axis lands*. Build the soft forms
where they're cheap; wire the hard forms when the axis is there.

## Phases

Each phase is TDD'd through the FSM harness (`stitch/tests/stim_fsm.rs`) exactly
like the v1 grammar; the driver is unchanged (every key is still a byte the
`byte_to_key` map already handles, except the few multi-byte sequences noted).

**Phase 0 — the single-key floor. ✅ DONE (v1 + the first fast-follow batch).**
`h j k l`, `i o`, `x`, `Esc`, `Enter`, `Backspace`, printables, `:w :q`. Flat
Normal-mode table; no accumulator.

**Phase 1 — the rest of the flat single-keys (cheap, no new machine). ✅ DONE.**
The inserts `a A I O`, the line motions `0 $ ^`, and the flat edits `X r{c} ~ J
D C s`. All are complete-in-one-key (or one-key-plus-one-arg for `r`), so they
extend the flat table with no accumulator. High value, low cost — this is the
"feels like a real editor" batch. (`r{c}` needs a one-key operator-pending-lite:
`r` then the replacement char — a two-key mini-command, the gentlest introduction
to accumulation; shipped as a fourth `Mode = … | Replace`, keeping the state shape
`(lines, row, col, mode)` unchanged — the `pending` field is still Phase 2's.)

Two deviations from the original list, both deliberate:
- **`Y` slipped to the register phases (2/5).** `Y` = `yy` needs a register to yank
  into, and registers don't exist until Phase 2's unnamed clipboard / Phase 5's
  named ones. Faking a clipboard in Phase 1 would have added state the phase doesn't
  otherwise need. `p`/`P` (already Phase 2 in the list) travel with it.
- **The observable-effect model landed a phase early.** The doc calls for building
  the grammar "as a set of observable effects from Phase 2 on"; we did it from
  Phase 1. `Effect` gained `Edit(Str)` (the mutation's telemetry span name); a
  helper `editIfChanged(old, new, name)` emits `Edit` only when the buffer actually
  changed (a clamped no-op falls back to `Redraw`, so no-op keystrokes span
  nothing); the driver opens/closes that span per edit, exactly as it already does
  for `stim.save`. So the edit history is a trace on the wire as of Phase 1. The
  `Edit(Str)` carries only a name today; Phase 2 adds the range field.

TDD'd through `stitch/tests/stim_fsm.rs` (FSM logic) + `stitch/src/stim.rs` tests
(driver: the edit span reaches the wire, the two-key `r` survives the per-byte loop).

**Phase 2 — the operator-pending core (the load-bearing phase).**
Introduce `pending.op` + the operator-pending branch of `stepNormal`. Define the
motions as reusable `state -> target` + a wiseness. Ship `d` × the current motions
+ **`dd`** (the doubled-operator linewise self — your special case, and the proof
that the production works). Then `c` and `y` fall out (`c` = the delete range +
`enterInsert`; `y` = the range into the unnamed register, no delete). Also `p`/`P`.
This phase is the whole architecture; everything after it is filling in the two sets.

**Phase 3 — counts. ✅ DONE.**
A count accumulator, folded from digit keys, multiplying motions and repeating
operators/actions (`3j`, `2dd`, `d2j`, `5x`, `2p`). Rode Phase 2. Deviations from the
sketch: (1) the count lives on **`Editor` (`count: Int`)**, not inside `Pending` —
Stitch prods have no default fields, so keeping `Pending` single-field avoided
editing 28 `Pending(op:)` test constructions, and the count is genuinely a
pre-operator accumulator anyway; (2) digit `0` disambiguates by context (line-start
motion when the count is empty, digit once one is accumulating); (3) a single count
accumulator, so the double-count `2d3w` (= 6) is **not** supported (it reads `23`) —
single count on either side (`2dd`, `d3w`-style) works; (4) count applies to motions
`h l j k` (not the absolute `0 $ ^`), `x`, `p`/`P`, `Y`, and operator line-spans;
`5x`'s register holds only the last char (repeat-delete), a documented fidelity gap.

**Phase 4 — word motions + text objects.**

*P4a — word motions `w b e`. ✅ DONE.* New `motionTarget` arms (charwise; `w`/`b`
exclusive, `e` inclusive), instantly usable bare *and* under operators (`dw`, `cw`,
`ye`) and with counts (`2w`, `d3w`) — all via the existing machinery, no new data
structures. Deviations: (1) **whitespace-delimited words** for v1 — `w` == `W` (a run
of non-space chars), so punctuation is not yet its own word (finer `\w`-vs-punct
semantics deferred); (2) **within-line only** — cross-line word motion deferred
(`dw` on the last word clamps to EOL, which happens to match vim); (3) the `cw`==`ce`
vim special-case is skipped (`cw` changes over `[cursor, w-target)`).

*P4b — text objects `{a|i}{w " ( { …}`.* The payoff of the range model. **Needs the
data-structure evolution**: a cursor-independent `Range` (both ends set by the object,
unlike a motion's `[cursor, target]`) that the operators consume, and a
text-object-pending sub-state (the `i`/`a` prefix awaiting the object char) that
reworks the operator-pending `None` branch (`Esc` already has its explicit arm to
survive this). `i` = contents only, `a` = contents + delimiters/whitespace.

**Phase 5 — registers + the real command-line.**
Named registers (`"{reg}` prefix; soft clipboard), and generalize `:` from the
direct recognizer to an accumulated ex line with ranges (`:wq`, `:q!`, `:w name`,
`:%s/a/b/`, `:1,5d`). The `:s` substitute wants the search phase (6).

**Phase 6 — search & find.**
`f{c}`/`t{c}`/`;`/`,` (charwise find, usable bare and under operators — `df.`), and
`/pattern`/`n`/`N` (search mode, another accumulator + a match motion). `:s` builds
on this.

**Phase 7+ — the axis features (ride the axes; soft now, hard when the axis lands).**
- undo/redo/`.`/macros → the replay axis. Soft: in-state history + a recorded
  keystroke span. Hard: undo reads the telemetry trace; scrub is the same view.
- visual mode → a fourth mode reusing the operator/range machinery over a selection.
- taint-aware registers → the IFC axis.
- session/checkpoint → the persistence axis.

## State-shape evolution (what `Editor` grows, per phase)

- P0/P1: unchanged (`lines, row, col, mode`).
- P2: `+ pending: Pending{op}` (operator-pending) `+ registers` (unnamed, for `y`/`p`).
- P3: `pending{count}`.
- P4: nothing new (text objects are motions).
- P5: `pending{register}` + a `registers` map.
- P7: `+ history` (soft undo) — or, in the hard form, no new field: the trace *is*
  the history.

Note the discipline: the state grows *only* where a phase genuinely needs memory
across keystrokes. Motions and operators are pure functions over the existing
fields; only the *accumulator* (pending op/count/register) and the *clipboard/history*
are new state.

## What's explicitly out (the un-vim parts)

stim is "the interactive app where explicit authority and total observability meet a
human hand," not a vim-completeness project. So: folds, marks/jumplist, windows/tabs,
`:g`/global commands, complex regex, modelines, plugins — all out unless a specific
axis wants them. The grammar serves the demo (and the axes); it stops where vim
stops being about *editing* and starts being about *being vim*.

## The observability seam (why this is on-thesis, not just an editor)

Every operator is already an effect, and the driver already opens a span per
effect. Extending that — `stim.delete`, `stim.change`, `stim.yank`, each a span
carrying its range — makes the **edit history a trace on the wire**. Which means
Phase 7's undo/scrub don't need a bespoke history structure: they replay the trace.
The grammar and the observability and the replay axis are the same thing viewed
three ways. That convergence is the reason to build the grammar *as a set of
observable effects* from Phase 2 on, rather than bolting telemetry on later.

---
*Delete this file when the grammar is built out (or folded into a broader stim
roadmap). If `plans/` is empty, delete the directory.*
