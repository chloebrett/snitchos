# Post 40 — The pipe reads before it runs

- two posts ago a program learned to declare its interface — input, output, the capabilities it uses — in a manifest the shell can read without running the thing. that was the whole point, but a manifest nobody reads is just a comment with extra steps. this post is where something finally *reads* it: the pipe. you type `5 ~> double` at the prompt, and the shell reaches out to the filesystem, finds a program, reads its type off the disk, checks that a `5` fits, and then runs it. the manifest stopped being a promise and became a precondition.

- and the promise post 38 actually made was the sharp one: **typecheck the pipe before it runs.** the Unix bargain is "connect the hoses, run it, see if the output is garbage." SnitchOS wanted the other bargain — "read what the stages say they are, refuse the pipe if the shapes don't fit, *then* run." this is the post where that bargain closes, and it closes in a way I didn't quite expect: not with one check, but with two, at two different moments.

## two pipes, and why the second one gets a new glyph

- Stitch already had a pipe. `|>` — `lhs |> f` feeds the left into the function on the right, in-process, the same address space, a plain function call. it's cheap and it's everywhere. `data |> filter(hot) |> take(3)` reads left to right and never leaves the interpreter.

- `~>` is the other pipe: the one that crosses. same precedence, same left-to-right reading, but the right-hand side isn't a function you already have — it's a **program**, a stage that lives as a file and is meant to run somewhere else. i gave the crossing its own operator on purpose, because the crossing is where all the interesting SnitchOS questions live. does the next stage accept what this one produces? what capabilities does it need to be granted? those are questions you only ask at a boundary, and `~>` is the mark that says *here is the boundary* — right on the exact stage that crosses, not wrapped vaguely around a region.

- the right side is a bare name. `5 ~> double` doesn't look up a variable named `double`; it treats `double` as the name of a *program*, and goes to the filesystem for `double.st`. that's a small piece of magic — a word in that position means something different than it means anywhere else in the language — but it's the shell's whole mental model. `cat | grep` never meant "the variable grep."

## the manifest, read off the disk, is the stage's type

- so what happens when the interpreter hits `~>` is almost boring, which is how I know it's right. it reads `double.st` off the filesystem — the same `fs_read` a `view` uses, no special channel. it parses it. and then it reads `main`'s signature: `main(x: Int) -> Int uses Telemetry`. that signature *is* the manifest — input `Int`, output `Int`, uses `Telemetry` — no extraction step, no separate file, the type is the declaration is the manifest. Stitch has it easy here, exactly as post 38 said: the interpreter still holds the parse, so a stage's interface is just `main` read back.

- and then it checks. the input value is a `5`; the stage says it wants an `Int`; do they agree? they do, so it runs. hand it `"hi"` instead and it refuses — `"hi"` is a `Str`, the stage wants an `Int`, and the pipe never fires. the manifest the shell read off the disk became the gate the value had to pass.

## two checks, at two moments

- here's the part that turned out deeper than "does it fit." there are two typechecks in a pipe, and they happen at two different times, and they answer two different questions — and I only saw the second one was necessary once I'd built the first.

- the **static** check is schema against schema. in `a ~> b ~> c`, before anything runs, the shell can read `b`'s declared output and `c`'s declared input and ask Hitch: *are these two shapes compatible?* pure types, no values in sight. and if they're not — `b` outputs a `Str`, `c` wants an `Int` — the pipe is rejected **before `b` even runs.** that's the one that matters, because `b` might do something. it might write a file, burn a second, emit a metric. catching the mismatch after `b` ran is catching it too late; the side effect already happened. so the static check fires up front, on the declared shapes, and a broken pipeline costs nothing.

- the **dynamic** check is schema against *value*. when the interpreter actually reaches the boundary, it's holding a real thing — the `5`, or whatever the previous stage returned — and it asks a different question: *does this actual value fit the stage's input?* not "are the declared types compatible," but "is this specific value acceptable." I needed both because the declared types aren't the whole truth. the head of a pipeline is an expression, not a stage — it has no manifest to read, so its shape is only knowable as a value, at runtime. and a stage's *declared* output is a promise it might not keep — Stitch checks the annotation but doesn't enforce it, so a stage that says `-> Int` could hand back a `Str` and the static check would have believed the label. the dynamic check is the one that trusts the value, not the label.

- Hitch, from post 39, is what makes both possible: the static one is `compatible` on two `TypeSchema`s, the dynamic one is `accepts` — a schema against a hitched value. same model, two questions. it's the same "who knows the type, and when" theme from the last post, wearing pipe clothes.

## it runs on the metal — and it doesn't isolate, yet

- the end-to-end is a real one. boot the OS, get the Stitch REPL, type `5 ~> double`. the interpreter reads `double.st` off the actual filesystem over IPC, reads its manifest, checks the `5`, runs the stage — which doubles it and emits `pipe.out` — and the metric shows up on the wire as `10`. the whole path, from a character I typed to a value on the telemetry channel, going *through a program read off the disk*. ten runs, no flakes.

- and now the honesty, because it's the honest part that keeps this from overclaiming. `~>` marks a crossing, but right now the stage runs **in-process.** it's a fresh namespace sharing the caller's terminal and telemetry, and — the one real guarantee — the stage runs with exactly the capabilities its `uses` row declares, nothing more. that's *soft* authority: the interpreter enforces it, and a bug in the interpreter voids it. the *hard* boundary — spawn the stage as its own kernel process, marshal the value across as a hitch, let the kernel enforce the cap set so even a compromised VM can't exceed it — that's the next milestone. the pipe runs and checks and confines; it doesn't yet *isolate*. I'd rather ship the honest intermediate and say so than pretend the boundary is a wall when it's a firm suggestion.

## what I learned

- **a typecheck has a "when," and the good one is early.** I thought "does the pipe fit" was one question. it's two, and the valuable one is the static check that fires *before* a stage with side effects gets to run. catching a shape mismatch after the upstream already wrote to disk is catching it too late. the whole reason to read a program's type without running it — post 38's entire thesis — pays off exactly here: you can reject the pipe before a single stage executes.

- **the declared type is a promise; the value is the truth.** the static check trusts the manifest, and the manifest can lie — a stage's return annotation isn't enforced. so there's a second check at the boundary that trusts the actual value, not the label. types on the outside of the box are a contract; the value inside is what you actually received, and a careful runtime checks both.

- **give the boundary its own operator.** `~>` costs almost nothing over `|>` and buys enormous clarity: the crossing is marked on the precise stage that crosses. every hard question — typecheck, capability grant, isolation — hangs off that one glyph. when the concept is "here is a boundary," the syntax should be able to point at it.

- **soft before hard is a real place to stand.** the pipe runs, checks its shapes, and confines the stage to its declared authority — all in-process, all VM-enforced. that's genuinely useful and genuinely not the same as kernel-enforced isolation, and naming the difference is more honest than blurring it. the intermediate ships; the wall comes next.

## what's next

- the wall. turning the in-process stage into a spawned process is three pieces I can already see: teach Stitch to spawn (it can't yet — spawning is a runtime thing Rust programs do, not a native the language exposes), a little protocol to marshal the value across as a hitch and carry the result back, and a harness on the far side that un-hitches the input, runs `main`, and hitches the output home. when those land, `~>` graduates from "the interpreter promises" to "the kernel enforces," and the exact same manifest and the exact same two checks drive it — the typecheck doesn't change, only who guarantees the boundary.

- and there's a quieter thing that landed this stretch and deserves its own post: the shell can now *render*. type `hold` and you don't get a blob, you get a table — a real box-drawn table, columns from the record's field names, because the value knew its own shape. a single record draws as a key/value table, a sum variant draws as a tree. the pipe is how typed data moves; the renderer is how you finally *see* it. next post is the one where the output stops being a debug print and starts being something you'd want to look at.
