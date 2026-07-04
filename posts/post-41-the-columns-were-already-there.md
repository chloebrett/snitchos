# Post 41 — The columns were already there

- last post ended on a promise: type `hold` at the prompt and you get a table, not a blob. this is that post. it's a small one — a few hundred lines of pure formatting, no kernel, no unsafe — but it closes a loop the whole series has been circling, and it does it by *not* doing the hard thing everyone else has to do. the hard thing is finding the structure. SnitchOS never lost it.

- because here's the reveal, and it's the whole point: nobody wrote a table formatter for capabilities. i did not sit down and decide that `hold` prints a column called `kind`. the renderer read the columns straight off the value, because the value already knew its own shape. the structure a Unix tool spends its life reconstructing from text was sitting in the data the entire time. all the renderer had to do was look.

## the before: one renderer, and it was a blob

- Stitch had exactly one way to turn a value into text — `display`, the thing string interpolation uses. it's recursive and flat and inline. a list is `[a, b, c]`, a record is `Name(field: value, …)`, and it all goes on one line with no idea how wide the terminal is or that a column is a column. fine for a `42`. useless for `hold`, which returns your whole capability table and printed as one unreadable comma-smear that ran off the edge of the screen.

- and the maddening part was that the data was *right there*. `hold` doesn't return a string; it returns a list of records, each one with named fields — `handle`, `kind`, `rights`, `badge`. the shape of a table was already in the value. `display` just refused to see it and printed the punctuation instead.

## rendering is dispatch on shape

- so the new renderer doesn't ask "how do i print a `Value`." it asks "what *shape* is this," and the shape picks the presentation:

  - a **list of records**, all the same shape → a table. columns are the field names, rows are the records.
  - a **single record** → a two-column key/value table, field on the left, value on the right.
  - a **sum variant** → an indented tree, `├─` and `└─`, recursing into whatever it holds.
  - a **scalar**, or anything that doesn't fit → back to plain `display`.

- this is the nushell / PowerShell idea — the presentation follows the shape of the value, not a format string you wrote — and it's the right idea, but it lands differently here. nushell's whole heroic effort is *manufacturing* that shape: it wraps `ls` and `ps` and `df`, catches their text output, and parses it back into records so it has something to make a table out of. it's forever squinting at columns that were flattened into text by a program that didn't care. SnitchOS output was never flattened. `hold` returns records because caps *are* records; the field names ride along in the value. the renderer skips the parse-and-pray entirely and reads the columns off the thing directly. post 39 promised "the bytes told it the columns," and this is the collection: they did.

## a model, and a style that draws it

- i split the renderer in two, and i'm glad i did. there's a `Table` — the columns and the cells, already turned to strings, pure data, no idea how it'll be drawn. and there's a `TableStyle` — a trait, one method, "turn a `Table` into text." the box-drawing look (`┌─┬─┐`, the one you're picturing) is one implementation of that trait. a dashes-and-spaces look, a colored look, whatever comes later — each is just another `impl`, and the model doesn't move.

- the payoff of the split is boring and enormous: the whole thing is testable without a terminal. the model is a value you can build by hand; the style is a pure function from that value to a string; a test asserts the exact box, byte for byte, with no QEMU and no escape-sequence guessing. i wrote the expected table as a raw string in the test and let the code prove it matched. the model-versus-style seam is the same seam i keep cutting all over this project — the *what* apart from the *how* — and it pays every single time.

- one gotcha worth a sentence, because it's invisible until it isn't: you measure column widths in **characters, not bytes.** the box-drawing characters are three bytes each; a `│` is one glyph and three bytes. measure the widths in bytes and every border drifts a little wider than its column and the whole table shears. count `chars`, and it lines up. a small correct thing hiding behind a small wrong one.

## the line between a product and a variant — and the robot that found it in pencil

- the tree case has a boundary in it that i drew, felt good about, and then watched a machine prove i'd drawn wrong.

- a `prod` — a plain record, `Point(x: 1, y: 2)` — becomes a key/value table. fields and values, no type name shown, nushell-style; a record is just its contents. but a **sum variant** — `Ok(…)`, `Some(5)`, `Circle(r: 1)` — becomes a tree, and it has to, because a variant has a *name that carries meaning.* if i rendered `Circle(r: 1)` as a key/value table, you'd get a little box with `r │ 1` in it and no `Circle` anywhere — the variant name, the entire point of a sum, gone. so products table and variants tree, and the line between them is "does the name matter."

- i wrote that line and moved on. then mutation testing — the thing that flips a condition in your code and asks whether any test notices — flipped exactly that boundary and *nothing failed.* my tests happened to use variants whose fields were positional, and positional fields fall through the table path for an unrelated reason, so the case I thought pinned the boundary was passing by accident. the line was drawn in pencil and i couldn't see it. so i added the two cases that sit right on it: a variant *with a named field* (must be a tree, not a table — or the name vanishes) and a product *with a positional field* (must be plain text, not a tree). now the boundary is in ink, and the mutation that erased it dies. the bug i was most likely to reintroduce someday is the one a robot found by trying to, before i shipped it.

## wired in one line, tables everywhere

- the actual wiring was a single line. the REPL's result printer said `display(value)`; now it says `render(value)`, with one tweak — a multi-line result gets its own line instead of being jammed after the `=>`. and because that result printer is the one path *both* the desktop prompt and the on-the-metal REPL run through, the change lit up everywhere at once. boot the OS, type `hold`, and a box table draws itself out the UART, columns and all. the same expression that used to smear across the screen now frames itself.

## what I learned

- **rendering is dispatch on shape, not a formatter per type.** you don't teach the renderer about capabilities or points or results. you teach it about *shapes* — list-of-records, record, variant, scalar — and the value routes itself to the right one. four cases cover an open-ended set of types, because types with the same shape want the same presentation.

- **the structure was never lost, so stop re-parsing it.** the single hardest, most thankless job in a shell like nushell is reconstructing structure from text the OS already threw away. SnitchOS didn't throw it away — the field names live in the value — so the renderer reads columns instead of guessing them. "born structured" sounds like an abstract virtue right up until the moment you write the renderer and it's just *easy*, and then you understand what it bought you. it's a gift you collect at the very end of the pipeline.

- **split the model from the style.** a `Table` is data; a `TableStyle` is a pure function; keeping them apart made the look swappable and the whole thing testable without a terminal. i have cut this exact seam — the *what* from the *how* — a dozen times in this project, and it has never once been the wrong call.

- **a boundary you can't see is a bug you'll ship, and mutation testing sees it.** the product-versus-variant line looked pinned and was passing by luck. a machine flipping conditions found the gap i'd have found in production. the test that proves *where a line is* is as much a part of the code as the line itself — and sometimes you only learn the test was missing when a robot deletes your invariant and nobody screams.

## what's next

- there are more shapes to grow — a record nested inside a record should probably tree rather than flatten, numbers might want to right-align, and color is a whole Tier-0.5 of its own (a green for a read cap, an amber for a write, a dim for one that's exited). all of that is another `TableStyle` or another dispatch arm, riding the same model. it's polish, and it's the fun kind.

- but the real next thing is the two halves finally meeting. post 40 gave the pipe — the *input*, typed values flowing through stages read off the disk. this post gave the render — the *output*, a value that draws itself according to its shape. what sits between them is a person at a prompt: type a pipeline, watch a table come back. grant, use, watch, reclaim — the powerbox got its verbs post by post — and now it has an input and an output and a face. the shell has been the horizon this whole series walked toward. it's close enough now to see the columns.
