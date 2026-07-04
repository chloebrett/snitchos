# Post 38 — The program comes with a manifest

- the shell's next trick is the pipe. `a ~> b` — run `a`, feed what it produces into `b`. but SnitchOS wants to do the thing Unix can't: **typecheck the pipe before it runs**. does `b` actually accept what `a` produces? and — because everything here is authority — *what does the shell need to grant `b` to do its job?* to answer either question the shell has to know `a`'s output type, `b`'s input type, and `b`'s appetite for capabilities. it has to know a program's **interface**. and a Unix program doesn't have one: its type is `bytes → bytes`, its authority is "whatever your uid can reach." there's nothing to read.

- so this is the post where a program stops being an opaque byte-shovel and starts **declaring itself** — its input, its output, and the capabilities it uses — in a way the OS can read *without running it*. a program comes with a manifest, the manifest lives in the file, and the shell reads it before it ever spawns the thing.

## a program's type is `bytes → bytes`, and that's the whole problem

- the reason Unix pipes re-parse text at every stage — `ls | awk | cut | sort`, each one squinting at the last one's output guessing where the columns are — is that nobody in the pipeline knows anything about anyone else. a program is a `bytes → bytes` function and the shell is a plumber connecting hoses it can't see inside. it works, gloriously, but nothing is ever *checked*. you find out the shapes didn't line up when the output is garbage.

- SnitchOS already decided pipes should carry typed values, not bytes — that's **Hitch**, the value model I spent this stretch building. but a typed pipe is only as good as the shell's ability to *check* it, and checking needs the stages to publish their shapes. `a ~> b` should be a compile-time-ish error — "`a` outputs a `Table`, `b` wants a `Row`" — caught before either runs, not a runtime surprise. for that, a program has to wear its signature on the outside.

## the signature is the manifest

- so a SnitchOS program declares one. in Rust it's an attribute on `main`:

  ```rust
  #[entry(in = Row, out = u64, uses = [ConsoleOut])]
  fn main() { /* ... */ }
  ```

- that reads exactly like what it is — a function signature lifted out of the function. this stage takes a `Row`, produces a `u64`, and needs the `ConsoleOut` capability. in Stitch it's even more literal: a stage's interface *is* `main`'s typed signature, `main(x: T) -> U uses C`, read straight off the parse. the manifest isn't metadata *about* the program bolted on the side; it's the program's type, externalized.

- and notice the third field. `uses`. the manifest carries the shape **and the authority** in one declaration — the type system and the capability system are the same sentence. the shell reads a program's interface and learns both "what fits the pipe" and "what to grant." that convergence is the whole reason to do this on *this* OS: a type and a permission, declared together, read together, before anything runs.

## you can't serialize at runtime into a thing that never runs

- here's where it got interesting, and where Rust fought me. i want the manifest *in the ELF* — a section the shell can read off the file on disk without executing a single instruction. an ELF section is initialized data; in Rust that's a `static`, and a `static` must be **const-initialized**. but a program's schema comes from `Schema::schema()`, which runs at runtime and allocates. you cannot runtime-serialize into a static. the obvious move is impossible.

- the deeper problem underneath it is that **Rust erases types**. by the time you have a compiled binary, `main`'s `Row` argument is gone — there's no reflection to recover it. Stitch has it easy: the interpreter still holds the parse, so it just reads the signature. Rust has to *capture* the type before the compiler throws it away.

- so the derive does exactly that. `#[derive(Schema)]` walks a type at compile time and emits its shape as a **`const`** — not a runtime value, a `ConstSchema` built from `&'static str` and slice literals, legal in a `static`. and then a `const fn` serializer encodes that const into a byte array, also at compile time. the manifest is computed, encoded, and placed in the ELF entirely before the program exists. i wrote a serializer that runs in `const` context — recursion, a little cursor, byte-by-byte — and the first time `const BYTES: [u8; N] = encode_manifest(&M)` compiled, i actually said something out loud. the types the compiler was about to forget got frozen into the binary on the way out.

## the linker ate it (once)

- a war story, because it's a good one and it cost me a confused hour. i named the section `.note.snitch.iface`, built it, dumped the sections — gone. the static was there, `#[used]` and all, and the section simply wasn't in the output. the culprit was one line in the userspace linker script: `/DISCARD/ : { *(.note .note.*) }`. the `.note.*` glob is the GNU convention for note sections, and my name walked right into the shredder. renamed it `.snitch.iface`, added a `KEEP`, and it stuck. lesson filed under "the linker has opinions about names."

## from the binary to the filesystem

- a section in an ELF is readable, but not conveniently — the shell would have to parse ELF headers to find it. so the last hop moves the manifest to where a filesystem naturally puts per-file metadata: an **extended attribute**. when the build bakes a program into the filesystem image, it parses the ELF, lifts the `.snitch.iface` note out, and writes it as the file's `user.iface` xattr. the FS gained xattr storage for exactly this — inode-attached, so it rides along with the file — and a `GetXattr` op over the IPC protocol so a client can ask for it.

- so now the whole path exists, and it's one IPC call: the shell looks up `bin/whatever`, asks for its `user.iface` xattr, decodes the bytes back into a manifest, and reads off `(in, out, uses)`. **no execution.** the program's typed interface is a filesystem attribute, and reading a program's type costs the same as reading a program's size.

- the end-to-end test is the thing i'm proudest of. a real userspace client, running on the metal, reads `manifest_demo`'s `user.iface` over IPC, decodes it, and checks the shape matches `#[entry(in = Row, out = u64, uses = [ConsoleOut])]` — and it does, byte for byte, through the entire chain: the attribute i wrote in Rust source became a compiled note, became an xattr at build time, crossed a process boundary, and decoded back into the exact shape i declared. a program said what it was, and something else read it, and they agreed.

## what I learned

- **an interface is data, not behavior.** the whole point is that you *read* a program's type, you don't *run* the program to discover it. that's the inversion of the Unix bargain: instead of "run it and see what comes out," it's "read what it says it does, and check before you run." the pipe can be wrong at build time instead of at runtime, because the shapes are on the outside of the box.

- **the const wall is real, and the derive is the way through it.** you cannot serialize at runtime into a static, and Rust erases the types you'd want to serialize. the only way to get a type into a compiled binary is to capture it as a `const` at derive time and encode it with a `const fn`. it felt like smuggling — freezing the thing the compiler is about to forget, into the artifact it's producing.

- **the type and the authority are one declaration.** `uses` sits right next to `in` and `out` because on a capability OS they're the same kind of fact: what a program consumes. the shell reads one manifest and learns both the pipe check and the grant list. i keep finding that SnitchOS's two obsessions — types and capabilities — want to be written in the same place.

- **build the two ends before the middle.** i had the note (produce the manifest) and `GetXattr` (serve the manifest) working, each tested on its own, before a single byte of manifest data existed to flow between them. then the middle — extract the note into the xattr — was a short, boring build-script hop, and the first time i ran the whole chain it just lit up. two proven ends make the middle almost free.

## what's next

- the pipe. all the primitives are on the bench: a program declares its interface, the interface reaches the filesystem, the shell can read it with one call, and Hitch already knows how to ask "is this output shape compatible with that input shape?" the Stitch-to-Stitch case even works today — pipe two `.st` stages and the mismatch is caught before either runs. what's left is the last wire: teach the interpreter to read a *Rust* program's `user.iface` the same way it reads a `.st` stage's parsed signature, so a pipeline can cross the language boundary and still typecheck. it's a small native and a branch — the hard part, making a program legible without running it, is done.

- and then the shell, which is where all of this has been walking. grant, use, watch, reclaim — post by post the powerbox got its verbs — and now, before any of that, **typecheck**. the shell will read a stage's manifest, refuse the pipe if the shapes don't fit, and grant exactly the `uses` the manifest names — least authority, computed from the program's own declaration, checked before it runs. a program that comes with a manifest is a program you can reason about before you trust it. which is, more or less, the entire pitch.

## addendum — a note on the name

- the value model under all of this is called **Hitch**, and the name did a suspicious amount of work, so it's worth the aside. the OS is *Snitch* (it tells on itself), the language is *Stitch* (it sews the platform's effects into programs), and the value format that rides between them is *Hitch* — same `-itch` family, and every meaning of the word earned its place. a hitch is a knot, which keeps the textile metaphor Stitch started. it "hitches a ride" on the IPC and telemetry channels — it's the payload, not the transport. and the verbs fell out for free: you `hitch` a value onto the wire and `unhitch` it off. i briefly wanted *Switch*, until i remembered this kernel is full of context-*switches* and the collision would be misery forever.

- the pun i kept, because it's also the design: "hitched" means *married*. a self-describing hitch is married to its schema — the shape rides inline, inseparable from the data. the packed form, the one that goes in a manifest, is the opposite: divorced from its schema, which lives apart in the program's declaration. naming the two encodings after a wedding was almost too cute to ship, and i shipped it anyway.

- there's more from this stretch that didn't fit the manifest story and wants its own post — how Hitch serializes at all, the two encodings, and the small security gem where a kernel information-leak became a *compile error* (a derive that refuses to build a type whose padding bytes would leak). that's the next devlog. this one was about a program declaring itself; the next is about the thing it declares itself *in*.
