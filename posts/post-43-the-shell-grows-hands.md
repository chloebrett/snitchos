# Post 43 — The shell grows hands

- last post gave the shell a face: type `hold`, get a colored table of your capabilities, read your authority at a glance. it also left two IOUs. one was a promise — that a `grant` would add a glyph and a `revoke` would take one away. the other was a confession — that the first time I typed `hold` on the metal with a real table to draw, it came back *cut in half*, and I owed you the reason. this post pays both. and it adds a third thing I didn't see coming until the table worked and I stared at it: the caps had no names.

## hands

- `hold` only ever *looked*. a powerbox that can see authority but not move it is a museum, not a shell. so the shell grew two verbs.

- `revoke @h` reclaims everything derived from the capability at handle `h` — transitively, across every process it was handed to. and here's the thing that's easy to get backwards: it does **not** drop `h` itself. `h` survives; what dies is everything `h` *spawned*. `revoke` is "un-grant what I handed out," not "give up what I hold." the reclaim half of grant→use→reclaim, and it's the half that makes least-authority safe: you can hand a program a narrow cap and *take it back* when it's done.

- `grant @h badge "SEND"` is the mint. from an endpoint you hold with the `MINT` right, it derives a fresh, narrower, badged capability into your own table. you name the rights you want in words — `"SEND"`, `"RECV"`, `"SEND|MINT"` — and get back a new handle. and the gate is the *capability*, not the shell: the kernel refuses unless you actually hold `MINT` on that endpoint. no ambient permission, no flag; the authority to delegate *is* a right you either have or don't.

- put them together and you get the loop the whole series has been circling: `hold` (see your caps) → `grant` (mint a narrow one) → `hold` (watch it appear) → `revoke` (reclaim it) → `hold` (watch it vanish). four verbs, and every one of them is real: `grant` is the `MintBadged` syscall, `revoke` is `Revoke`, each firing a `CapEvent` out the wire. the shell doesn't *simulate* authority moving. it moves it, and the kernel snitches.

## the table came back cut in half

- so I built the loop, booted it on the metal, typed `hold`, and got a top border, a header row, and then… nothing. the prompt, back early. not an empty table — a *bisected* one. the data was there; the drawing stopped mid-stroke.

- the tell was that it only happened for *big* tables. short output was fine; the cap table under the filesystem workload was the first thing long enough to break. that pointed at a length threshold, and there was one: the userspace console writer chunks its output at 256 bytes, because the kernel refuses a single write longer than that. fine — except it chunked on **byte** boundaries.

- and box-drawing characters are three bytes each. `│` is one glyph, three bytes. so somewhere around byte 256, a chunk boundary landed *inside* a `─`, splitting one character across two writes. the first write ended with two-thirds of a box char — an incomplete UTF-8 sequence. the kernel's console syscall validates every write as UTF-8 (it forwards through a `str`-based path), saw the broken tail, refused the whole chunk, and returned an error the writer read as "stop." everything after the split: dropped. the table sheared exactly where the bytes lied.

- the fix is four words: chunk on character boundaries. don't cut mid-glyph; back up to the last whole character and send that. one small function, and the split can't happen. but the *reason* it took a boot to find is the part worth keeping: **the metal validates what the host waves through.** on my laptop the terminal is a byte pipe — split a character across two writes and it stitches them back with no complaint. the kernel doesn't. it checks. the host had been quietly forgiving a bug the whole time, and only the real machine, with its real rules, made me pay for it.

## name the things you snitch about

- with the table finally drawing, I looked at it and hit a wall of my own making. three rows, and one column read `Endpoint`, `Endpoint`, `Endpoint`. *which* endpoint? the filesystem? a peer? my own? the kind told me the mechanism and nothing about the meaning. a human at the prompt was blind to their own authority.

- the instinct is to name them, and the instinct trips straight over the founding principle: **naming an integer is not authority.** the entire capability model exists to kill the idea that you reach a thing by naming it. so for a while I assumed names were simply forbidden.

- they're not. the principle isn't "objects can't have names" — it's *names are for seeing, handles are for doing.* a name that you can look at but can never use to **reach** or **authorize** an object takes nothing away. you still need the cap to act; the name just tells you what you're holding. this is exactly what Zircon — the kernel this cap model is modeled on — does with `ZX_PROP_NAME`: a short string on an object, for eyes only, never a namespace, never a right. settled prior art, and I'd been about to reinvent it by hand.

- so an endpoint now carries a name, set once by whoever created it, opaque to every authority decision the kernel makes. and because this is an *observability* kernel, the name doesn't stop at `hold` — it rides into the `CapEvent` frames on the wire. that's the real prize. the derivation tree the host reconstructs stops reading "process 4 transferred cap 4172 to process 7" and starts reading "transferred the **fs** endpoint." the grant→revoke loop I built at the top of this post now snitches *by name*: watch the `fs` cap get minted, watch it get reclaimed. authority you can move, made into authority you can *read*.

- one caveat I wrote into the design before I wrote the code: a name is the *creator's* claim, so a hostile creator could name an endpoint `trusted-bank-api` to fool you. but look at the shape of that risk — it's a misleading *label*, not a stolen *right*. the name authorizes nothing. a program deciding whether to trust a cap must look at where it came from, never at what it calls itself. names for seeing; provenance for trusting; handles for doing.

## what I learned

- **authority you can move is authority you can watch.** the moment `grant` and `revoke` became real syscalls instead of shell bookkeeping, they started snitching `CapEvent`s for free — every delegation and every reclaim, out the wire, no extra code. build the mechanism on the kernel's real primitives and the observability falls out of it. that's the whole SnitchOS bet, and it keeps paying.

- **the metal validates what the host waves through.** a byte-boundary chunk split a character, and my laptop's terminal forgave it for weeks while the kernel refused it on the first real boot. the host is a lenient reader; the machine is a strict one. the bugs that only the metal can find are exactly the ones where the host was being polite.

- **names for seeing, handles for doing.** you *can* name objects in a capability system without undercutting it, as long as the name is display-only — never a way to reach or authorize. get that line right and naming is pure gift: it's what turns a derivation tree from a wall of ids into a story a human can follow. get it wrong — let a name resolve to authority — and you've rebuilt the ambient-permission world the whole design was trying to escape.

## what's next

- the names are on the wire, but they're not yet in the trace view. the kernel snitches "transferred the `fs` endpoint"; the collector, for now, lets that event pass without drawing it. reconstructing the full **named derivation tree** — the grant graph, rendered in Tempo, every edge labeled — is the payoff still ahead, and it's the thing that finally makes "watch least-authority happen" literal instead of aspirational.

- and past that, the verb the loop is still missing: a `grant` that hands a cap not to *yourself* but to *another program* you launch. `view a-file` — spawn a viewer, hand it a read cap to exactly that one file, watch the `CapEvent` carry the grant across the process boundary, and revoke it when the viewer exits. that's the powerbox at full height: authority you delegate, named, scoped, observed, and reclaimed. the shell has hands now. next it learns to give.
