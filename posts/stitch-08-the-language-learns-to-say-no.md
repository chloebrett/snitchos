# Stitch 8 — the language learns to say no

- Stitch was always pitched on two things nothing mainstream does: telemetry as a first-class language primitive, and **capabilities tracked in the language itself** — a function declares the authority it needs, and you can't touch authority you weren't handed. for seven posts, exactly *one* of those was real. spans and metrics went out to the same Grafana as the kernel. and capabilities? `uses` was a keyword the lexer recognized and **nothing else did**. it parsed to nothing. it meant nothing. the headline feature of the language was a reserved word.
- this is the post where `uses` starts meaning something. a function now declares the authority it exercises, and if it reaches for authority it didn't declare, the runtime **refuses it**. the language learned to say no.

## the pattern was already there

- telemetry had quietly taught me the shape. `emit` and `span` don't *do* anything themselves — they call through a `Telemetry` backend the environment holds: a recording one on the host (buffers events for tests), a syscall-backed one on the metal (frames on the wire). the native is just the trigger; the backend is swappable. that's the seam that made telemetry both host-testable and real-on-target without the interpreter knowing which it had.
- so capabilities got the same shape, generalized. the console, the filesystem, spawning processes — none of those are telemetry, none of them existed as language effects, and all of them want the same treatment. so: a `Platform` seam beside the telemetry one. a `read_line`, a `write`, a `hold` (what authority do I hold?). three impls, exactly like telemetry — a null one (the default; reads nothing, discards output), a **fake** one for host tests (scripted input, recorded output — so a shell's effects can be asserted in plain Rust), and a syscall-backed one for the metal. every new effect is a native that calls `env.platform().<op>()`. the seam was the work; the caps ride on top.

## the clause, and the gate

- `uses` is the clause. a function — or a method on a type — declares the capabilities its body may exercise:

```
primes(n: Int) -> List<Int> uses Telemetry {
    use <- span("primes.compute")
    let found = 1.. |> filter(isPrime) |> take(n) |> toList
    emit("primes.count", found |> count)
    found
}
```

- and the gate is the part that bites. `emit` and `span` now check for `Telemetry` authority; `print` checks for `ConsoleOut`; `readLine` checks for `ConsoleIn`. miss it and you get a clean refusal, not a silent success:

```
stitch> shout(s) = print(s)
stitch> shout("hi")
runtime error: print requires `uses ConsoleOut`
```

- the interesting question — the one I sat with — is *what "in scope" means at runtime*, because there is no type checker yet. the honest answer is that this is the **runtime reification** of capabilities, not static effect-checking. the compiler propagating a `uses` set up the call graph is a type-system feature, and the type system doesn't exist. so I had to pick a runtime rule that makes `uses` load-bearing *today*, with what I have: a set of capabilities the environment carries, threaded through evaluation.
- the rule I landed on: **a named function runs with exactly its declared `uses` — it does not inherit the caller's.** that's what makes the gate fire. if a function could use whatever its caller had, you'd declare `uses Telemetry` once at the top and everything below it could emit forever — the clause would be decorative. so a named function's body gets *only* what it wrote down. least privilege, enforced at the call boundary.
- with one necessary exception: **lambdas inherit, lexically.** they have to. the whole `use <- span("...")` idiom desugars the rest of a block into a lambda handed to `span` — so if lambdas didn't see their enclosing authority, the language's own sugar would deadlock against its own gate. so the rule is: named functions restrict to what they declare; lambdas are transparent to the scope that defined them. the language's syntax dictated the shape of its security model, which is a sentence I did not expect to write.
- and the entry point — the REPL prompt, `main` — holds the process's **ambient** authority, the caps the OS actually handed it at startup. you can `emit` at the prompt because the process *was* given a telemetry cap. it's the function boundaries *inside* the program where the narrowing happens.
- the consequence is that this is a breaking change, and that's the feature working: `main() = emit(...)` no longer compiles past the gate. it has to say `main() uses Telemetry = emit(...)`. nine test programs and the demo `.st` files had to declare what they'd been quietly assuming. every one of those diffs is the language making authority visible where it used to be ambient.

## read is not write

- the part I'm happiest with is the smallest. console isn't *one* capability — it's **two**: `ConsoleOut` and `ConsoleIn`, split. a program that prints needs `ConsoleOut`. a program that reads needs `ConsoleIn`. they are not the same authority, so they are not the same word.

```
greet() uses ConsoleOut {
    print("name? ")
    let who = readLine()   // runtime error: readLine requires `uses ConsoleIn`
    print("hi")
}
```

- holding the right to write the terminal does not give you the right to read it. that's least authority made *expressible* — "this program only talks, it never listens" is now a thing you can say in a function signature and have enforced. it's a tiny split, and it's the whole argument for capabilities in one line: authority isn't a switch, it's a set, and you should only ask for the elements you use.

## the soul — a clause that costs you something

- a capability system that only ever says yes isn't one. for seven posts `uses` cost nothing because it *did* nothing — you could write it or not, and the program ran identically. the moment that makes it real is the moment it can **refuse you**, and the moment a working program breaks because it was reaching for authority it never declared. that broken build is the feature announcing itself.
- and the throughline: capabilities in this language aren't really about gating your *own* `emit`. that's the warm-up. they're about **delegation** — handing another program exactly the authority it needs and nothing more, and being able to *see* the grant. the `uses` clause is the vocabulary for that; the `Platform` seam is the plumbing; and (in the same stretch of work, off to the side) the OS learned to load and run a program straight off its own filesystem, with a chosen set of caps and no others. those are the three pieces a powerbox shell is built from. they're all on the table now.

## what i learned

- **you don't need the checker to make the keyword load-bearing.** the "right" home for capabilities-as-effects is the type system, propagating `uses` up the call graph at compile time. but reifying it at *runtime* — a set the environment carries, checked at the boundary — is a real, demonstrable down payment, not a fake. it makes `uses` matter today, and the static version is an optimization of a thing that already works, not a prerequisite for it.
- **the sugar designs the security model.** I wanted a clean "a function only has what it declares" rule, and `use <- span` — Stitch's own block-callback sugar — broke it instantly, because that sugar is a lambda and lambdas have to inherit. the gate's exact shape (named-fn restricts, lambda inherits) wasn't a security decision I made top-down; it fell out of making the existing language keep working. the features you already shipped constrain the features you add.
- **least authority is a syntax choice before it's a runtime one.** splitting console into read and write is two enum variants and two `has_authority` checks — trivial. but it's also the difference between "this program can use the terminal" and "this program can only print," and only the second is honest. the enforcement is cheap; deciding the granularity *is* the design.

## what's next

- the **shell**. all three pieces exist now — a language that declares authority, a seam the effects ride on, and an OS that spawns programs off its filesystem holding exactly the caps you pass. so the powerbox is finally buildable: `view notes` spawns a viewer that holds *only* `read(notes)`, the grant itself a `CapEvent` on the wire — "grant, then watch." the verb is the grant; the child is born with nothing it wasn't given; and you can see it happen.
- `uses` cost nothing for seven posts. now it can refuse you. that's the half of the thesis that was vaporware, made real — and it turns out the proof that a capability is real is that the build breaks when you forget to ask.
</content>
