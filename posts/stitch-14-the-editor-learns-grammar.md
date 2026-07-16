# Stitch 14 — the editor learns grammar

- two posts ago stim shipped: forty lines of pure Stitch that could move a cursor, edit a line, and save a file, narrating every move as a span. it could do `h j k l`, `i`, `x`, `:w`. a floor. this post is the rest of vim's Normal mode — `dw`, `d$`, `dd`, `3dd`, `ciw`, `di"` — and the thing I want to say up front is that those are **not commands.**
- here's the realization the whole post hangs on. `dw`, `d$`, `dj`, `dd`, `2dd`, `diw` look like six things you'd implement six times. they are one thing: `operator motion`, instantiated over a small set of operators and a small set of motions, times a count. **`dd` is `d` applied to its own linewise self.** get `d` and the motions right and `dw`/`d$`/`dj`/`dd` all fall out; add `c` and `y` and you get two more entire rows of that cross product for free — `c` is `d`-then-insert, `y` is `d`-without-the-delete.
- so vim's Normal mode isn't a command table. it's a tiny **grammar** — `[count] operator motion` — and the job was never to enumerate the cross product. it was to build the productions and the two small sets they range over, and let the cross product be free. that reframing is the same move the rest of this OS runs on: don't build the bespoke command, build the production. don't build the twist, ride the axis.

## one motion, two readers

- the load-bearing decision, the one everything else stands on: a motion is defined **once**, as a function from state to a target position, and consumed two different ways.

```
motionTarget(state, key) = match {
    key == "l" => Some(Target(row: state.row, col: state.col + 1, wise: Charwise, inclusive: false))
    key == "$" => Some(Target(row: state.row, col: lineEndCol(state),      wise: Charwise, inclusive: true))
    key == "w" => Some(Target(row: state.row, col: wordForwardCol(line, c), wise: Charwise, inclusive: false))
    key == "j" => Some(Target(row: clampRow(state, state.row + 1), col: state.col, wise: Linewise, inclusive: false))
    _ => None
}
```

- read it bare and it moves the cursor: `moveTo(state, target)`. read it under an operator and it's a range endpoint: the span from the cursor to the target, which `d` deletes, `c` deletes-then-inserts, `y` copies. **one `wordForwardCol` definition powers both `w` and `dw`.** I never wrote `dw`. I wrote `w` as a target and `d` as a thing that consumes a target, and `dw` is what happens when they meet.
- the two little flags on `Target` are where vim's fiddliness lives. `wise` is charwise-vs-linewise: `dl` deletes a character, `dj` deletes whole lines. `inclusive` is the exclusive/inclusive distinction that makes `d$` delete *through* the last character while `dw` stops *before* the next word. two booleans, and the operators read them instead of special-casing each motion.

## the accumulator is a parser

- the floor version of Normal mode was a flat table: every key a complete command. `dw` is two keys forming one command, so `step` — which sees one key at a time — has to remember, after `d`, that a delete is pending and a motion is owed. that memory is the whole unlock. Normal mode stops being a table and becomes a small parser with a pending-command accumulator.

```
stepNormal(state, key) = match {
    state.pending.object != NoObj => stepObjectPending(state, key)   // mid text-object
    isCountDigit(state, key)      => redraw(foldDigit(state, key))   // building a count
    state.pending.op == OpNone    => stepTopLevel(state, key)        // fresh command
    _                             => stepOperatorPending(state, key) // operator owed a motion
}
```

- and the doubled-operator special case — my favorite line in the editor — is just: the same operator key, pressed again, means "apply me to my own line."

```
stepOperatorPending(state, key) = match {
    key == "Esc"                    => redraw(clearPending(state))
    isSameOp(state.pending.op, key) => applyLinewiseSelf(state)      // dd, cc, yy
    _ => match motionTarget(state, key) {
        Some(target) => applyOpOverMotion(state, key, target)        // dw, d$, dj, ...
        None         => stepObjectOrCancel(state, key)               // di", or give up
    }
}
```

- counts rode on top of this almost for free. a digit folds into the accumulator (`0` is the exception — it's the line-start motion until a count is already building, then it's a digit), and a `repeat` combinator applies a motion or an edit that many times. `3j`, `5x`, `2dd`, `d2j`. the accumulator that remembered the operator just learned to also remember a number.

## every operator is a span

- here's the part that keeps this honest to the rest of the OS. when the floor shipped, every edit already emitted a telemetry span — `stim.delete-char`, `stim.join`. so as the operators grew, I didn't bolt observation on afterward. `d` emits `stim.delete`, `c` emits `stim.change`, `y` emits `stim.yank`, `p` emits `stim.paste`. the **edit history is a trace on the wire, by construction.**
- and it's honest about nothing-happened. the emit goes through one helper — `editIfChanged` — that compares the state before and after and only spans a real mutation; `dw` at the end of a line, `x` on an empty line, a motion that selected nothing, all fall back to a plain redraw and narrate nothing. the trace is edits, not keystrokes. which means undo, someday, doesn't need a bespoke history structure — it can read the wire. the grammar and the observability turned out to be the same object seen twice.

## text objects break the model, on purpose

- then `diw` — "delete inner word" — and it wouldn't fit. every range so far was anchored at the cursor: `[cursor, target]`. but `diw` deletes the whole word *no matter where in it the cursor sits* — the region extends left and right of you at once. neither end is the cursor. the cursor-anchored model, the one that had carried the entire grammar, simply cannot say it.
- so text objects needed a genuinely new thing: a **cursor-independent range**, both ends set by the structure around you, not by a target you move to.

```
prod Range(startRow: Int, startCol: Int, endRow: Int, endCol: Int, wise: Wise)
```

- `iw` walks outward from the cursor in both directions to the edges of the word (a run of same-class characters); `i"` pairs up the quotes on the line and finds the pair you're inside; `i` gives you the contents, `a` gives you the contents plus the delimiter or the trailing space. the operators didn't have to change — a `Range` is a `Range` whether a motion or an object produced it, and `applyOpRange` deletes/changes/yanks it the same way. `diw`, `ciw`, `ci"`, `da"` all came from one small object-resolver and the machinery that was already there.
- the nicest tell that the abstraction was right: `diw` deletes the same word whether the cursor is on its first letter or its last. that's not a feature I coded. it's what a cursor-independent range *is*.

## what the language did, and didn't

- post 12 was a tour of everything the editor found broken one layer down — a missing escape character, a filesystem that couldn't truncate, a REPL that couldn't import. this time I kept waiting for the same audit and it mostly didn't come. the grammar is a lot more Stitch than the floor was — a real recursive parser, string-scanning for words and quote pairs, a repeat combinator built on lambdas — and the language just... ran it. that's its own kind of milestone: the substrate got real enough that I could build a real language's grammar on top of it and spend my time on the grammar.
- what the language *did* make me pay were two small parser manners, both found the same way — a mass of green tests going suddenly red with a parse error. Stitch caps a chained conditional at two cases: `a => x | b => y | z` is a syntax error, you have to reach for `match`. and a `match` guard can't *start* with a parenthesis — `(a == b) == c =>` makes the parser try to read `(a == b)` as a tuple pattern and give up. both are one-line fixes once you see them, and both are the sort of thing you only trip over when you write enough of the language to hit its edges. the editor is still auditing the platform. it's just down to grammar-of-the-grammar now.

## what I'm not pretending

- the words are whitespace-delimited — my `w` is really vim's `W`, so it doesn't stop at punctuation yet. motions stay within a line; `dw` on the last word deletes to the end of line instead of eating into the next one (which, by luck, is what vim does there anyway). the double-count `2d3w` reads the `3` as more digits of the `2` instead of multiplying — a single accumulator, honestly limited. bracket objects — `di(`, `ca{` — are designed and deferred. and there's no visual mode, no undo, no search yet; those are the next phases, and two of them are really other axes of this OS wearing vim keybindings (undo is replay, taint-aware yank is information-flow).
- but the shape is right, and the shape was the whole point. I didn't write `dw` or `dd` or `diw`. I wrote a handful of motions as targets, three operators that consume a range, an accumulator that remembers a half-typed command, and one object-resolver that produces a range out of thin air. the commands are what fall out when those meet. the editor didn't learn a list of keystrokes. it learned a grammar — and it did it as a program written in a language I wrote, running on an OS I wrote, narrating every edit it makes onto the wire.
