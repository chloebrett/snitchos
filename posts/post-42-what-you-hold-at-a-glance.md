# Post 42 — What you hold, at a glance

- last post built the table. this one gives it a face. because a table that renders your capabilities and then prints the `rights` column as `6` has technically told you the truth and practically told you nothing. `6` is a bitmask — it's the number `SEND | RECV` happens to be. nobody reads a bitmask. so `hold` printed a correct, useless integer, and the whole point of a capability shell — *see what you can do* — died in a column nobody could parse.

- so this post is about making authority legible: rights become glyphs, glyphs get colors, numbers line up, and a nested thing draws itself as a tree. small changes, every one of them, and together they turn `hold` from a data dump into something you read at a glance. and the surprising part — the part worth the post — is that the renderer learned none of it. it still doesn't know what a capability is.

## the number was the wrong answer

- `hold` returns one record per cap: `handle`, `kind`, `rights`, `badge`. post 41 made that a table. but `rights` was a `u32` and it printed as one — `6`, `2`, `14`. the shape was right (a column) and the content was noise.

- the fix is *not* "teach the table about capabilities." the table dispatches on **shape** — list-of-records, record, variant, scalar — and it has to stay that way, or every new domain drags its vocabulary into the renderer. the fix is to render the rights at the one place that still knows they're rights: `hold` itself, the same spot that already turns the `kind` enum into the word `Endpoint`. the domain paints its own opaque field on the way out.

## rights as glyphs

- so a rights mask becomes emoji, one per *category* of authority:

  - 🪴 **mint** — the right to hand out more caps. mint is a herb; herbs grow; minting grows new authority. the pun is load-bearing.
  - 👀 **read** — the consumer ends (`RECV`, `WAIT`). you can watch.
  - 📝 **write** — the producer ends (`EMIT`, `SEND`, `SIGNAL`). you can change something.

- six rights bits collapse to three glyphs, because what you want to know at a glance is *can this thing read, write, or grant* — not which of three write-shaped bits is set. `SEND|RECV` is `👀📝`. a bare telemetry cap is `📝`. the number `6` is gone, and good riddance.

## color, and the seam that keeps the renderer innocent

- then color: 🪴 green, 👀 blue, 📝 amber. and here's the design I'm proud of, because it's the same move as the glyph, one layer out.

- the renderer must not know that green means mint. it lays out boxes; it knows shapes. so color is a `TableStyle` that takes a **colorizer** — a plain function `&str -> String` — and runs it over each cell. the box style knows "apply this function to the content"; it does not know what the function *does*. the domain supplies the function (`colorize_rights`, which wraps the three glyphs in ANSI), exactly as it supplied the glyphs. the renderer never names a color.

- the glyph is the seam. it's the token that carries meaning out of the domain and into the presentation, and both sides only have to agree on the token. *what a cap is* lives in one file; *how a cap looks* in another; the emoji is the handshake between them. the value flowing through the shell stays clean data — no escape codes baked in — right up until the last inch, where a style paints it.

- one detail that matters more than it looks: the color goes on **after** the width is measured. an ANSI escape is bytes the terminal eats but a measurer counts, so if you colorize before you pad, every border drifts by the length of `\x1b[33m`. measure the naked glyph, pad to that, *then* paint. the fill is computed on the truth; the color is a coat on top.

## the craft of lining up

- three smaller things, because a table that's legible but crooked isn't legible.

- **numbers right-align, and the renderer reads that off the value.** a column of `Int`s justifies right, like a spreadsheet; text stays left. but it doesn't sniff the string to guess "is this numeric" — it asks the `Value`. `Int`/`Float` → right. and a column is right-aligned only if *every* cell is a number; one label in the column and the whole thing falls back to left. the alignment is a fact about the data, read from the data.

- **a record inside a record becomes a tree.** `Cap(handle: 0, kind: Endpoint(id: 3, badge: 7))` used to flatten the nested `Endpoint` into one cell as punctuation. now the whole thing trees — `└─ kind: Endpoint` with `id` and `badge` hanging off it — and the recursion was already written, so it cost one dispatch arm. the structure was always there; it just needed to not be crushed flat.

- **you measure widths in cells, not characters.** post 41 learned to count characters instead of bytes, so box-drawing (three bytes, one glyph) lined up. this post learned the next layer down: an emoji is *one* character and *two* terminal cells. count characters and the emoji column shears exactly as badly as counting bytes did. so widths go through a display-width measure now, and the padding lays down spaces to fill the cells the glyph will actually take.

## the glyph that wouldn't sit still

- and then a real one — the kind you only find on the metal. the first write glyph was ✏️, a pencil. it rendered perfectly in my terminal and sheared by one column in the QEMU console. same bytes, different width, no bug in sight.

- ✏️ is a trap: it's `U+270F` (a one-cell pencil) followed by `U+FE0F`, the "render the thing before me as an emoji" selector. whether that pair is one cell or two is **not specified** — it's up to the terminal and the font. my width library said two; the console drew one; the border split the difference and lost. the honest lesson is that emoji width isn't a property of the text, it's a negotiation with the display, and VS16 sequences are the worst of it.

- the fix was to stop using the ambiguous glyph. 🪴 and 👀 are single-code-point emoji from the high planes, which terminals render as two cells far more reliably. so the pencil became a memo — 📝, the same kind — all three glyphs now live in one width class, and the library and the terminal finally agree. a small correct thing standing in for a small wrong one — and I'd never have caught it on the host, because on the host the pencil *was* two cells and everything lined up.

## what I learned

- **legibility is a rendering concern, but *meaning* belongs to the domain — so hand the domain a brush, don't teach the renderer.** the table stays shape-only forever; `hold` paints its own rights; a color style takes a function it doesn't understand. every seam here is the same seam — the *what* apart from the *how*, with a glyph as the handshake — and it's why four features fit without the renderer growing a single line about capabilities.

- **the value carries its own truth; read alignment and shape off it, not off its printed form.** numbers right-align because they're numbers, not because they look like numbers. records tree because they're records. the moment you sniff the rendered string to recover what the value already knew, you've thrown away the structure post 41 spent a whole post celebrating.

- **width is a lie you tell the terminal, and the terminal gets a vote.** bytes, then characters, then cells — each layer of "measure it right" was really a layer of "stop assuming the display agrees with you." emoji width, and VS16 especially, is where that assumption breaks in the open, and the only safe move is to pick glyphs whose width nobody argues about.

## what's next

- there's a bug I haven't told you about yet. the first time I typed `hold` on the metal and the caps were rich enough to fill a big table, it came back *cut in half* — top border, header, and then nothing, the prompt returning early. it wasn't the renderer. it was every long line of box-drawing marching straight into a UTF-8 trap between the shell and the kernel. finding it is the next post.

- and past that, the thing this has all been walking toward: the color isn't decoration, it's the shell learning to *show authority*. green for grant, blue for read, amber for write — type `hold`, see exactly what a program can do, watch a `grant` add a glyph and a `revoke` take one away. least authority, read at a glance. the powerbox got its verbs; now it's getting a face.
