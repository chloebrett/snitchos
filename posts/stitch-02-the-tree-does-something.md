# Stitch 2 — the tree does something

- last post ended with a front-end that turns text into a tree, and a promise: next, the tree *does something*. this is that — the tree-walk evaluator, the moment Stitch stops being a shape and starts running. by the end of it the canonical sample actually executes, emits telemetry, and prints. a language earns the interesting parts by first being able to print a number; it can now print a number.
- but the surprise of this stage, same as the last, was that **the hard part wasn't the walking — it was the arguments.** the code that evaluates `1 + 2` is four lines. deciding what `1 + 2.0` should *do*, or what `==` means for a mutable record, or whether a closure sees a variable change after it captured it — those took the real thinking, and a couple of them overturned beliefs i started with.

## the shape

- a tree-walk interpreter is the most honest runtime there is: a recursive function `eval(expr, env)` that pattern-matches each AST node and computes a value. no compilation, no bytecode — **the AST *is* the program.** it mirrors the parser exactly. the parser recursed over tokens to build the tree; the evaluator recurses over that tree to run it. writing the second after the first feels like walking back down a path you just cut.
- this is stage one of two on purpose. the tree-walker is semantics-first: fastest path to a working language, where i can prove out the *interesting* parts — the syntax, the telemetry, the capability story — end to end and early. the bytecode VM comes later, and that's the "how does the JVM actually run" lesson. building the slow, obvious version first means i'll understand exactly what the fast version is optimizing.
- it also reuses the project's spine: the runtime core is plain host-tested Rust, ~290 tests, every increment red-green-refactor. same discipline as the kernel it'll eventually run on.

## the arguments that mattered

- **no silent number coercion.** `1 + 2.0` is a type error, not `3.0`. most scripting languages would widen the int and move on; Stitch refuses. the reason is that it's *dynamically* typed today but *statically* typed later, with `Int` and `Float` as genuinely distinct types — so the dynamic runtime should preview that discipline rather than contradict it. better to be strict now than to teach people a coercion i'll have to take away.
- **the equality belief i had to give up.** i started certain of a rule: a record with a mutable field should get *identity* equality, because otherwise you could use it as a hash-map key, mutate it, and silently lose it — Java's classic footgun. clean, defensible, and — it turns out — more conservative than even Kotlin. a Kotlin `data class` with a `var` field still gets structural equality; you just shouldn't key on it. the realization was that i'd **conflated two separate questions**: "what does `==` mean?" and "can this be a key?". untangle them and the answer is nicer than my original: *everything* gets structural equality, and *key-eligibility* is the thing that requires immutability (a compile error to key on a mutable value, the way Python makes lists unhashable). structural-but-honest equality, footgun designed out at the key boundary instead of by faking identity. i killed a rule i was attached to and the language got simpler.
- **closures capture by reference, not by snapshot.** when a closure captures a `mut` local and the local is later reassigned, does the closure see the new value? i went back and forth and landed on yes — capture the *binding cell*, not its current value. the deciding argument: it makes one rule instead of two. "`mut` is visible through every alias" already governs mutable fields; extending it to mutable locals captured by closures keeps it a single idea. and you can always turn capture-by-reference into a snapshot with one explicit copy; you can't cheaply go the other way.

## the parts that were actually tricky

- **mutual recursion is a chicken-and-egg.** top-level functions all have to see each other — `isEven` calls `isOdd` calls `isEven`. but each function's closure captures the environment at the moment it's built, and while i'm building them the environment isn't finished. the fix is a shared, write-once cell: every top-level closure captures a handle to the *same* globals table, and i fill the table after building all of them. by the time anything runs, everyone sees everyone. that shared table *is* letrec — the thing that makes recursion live in named functions and not in `let`.
- **a closure is captured code plus a captured environment, bolted into one heap object.** the fork here was a Rust one: store the function body as a borrowed reference into the program tree (zero-copy, but a lifetime goes viral through every value and every signature), or copy the body into a reference-counted object once at closure-creation. i took the second — simpler types, and the better mental model: a closure is *code + environment*, one allocation, cheap to pass around. that's also exactly the shape the bytecode VM will make explicit later (a code chunk plus an upvalue array).
- **`?` is control flow smuggled through the error channel.** the try operator unwraps `Some`/`Ok`, or — on `None`/`Err` — bails out of the *enclosing function*, returning the failure as that function's result. in a tree-walker, "return from the enclosing function" is a non-local jump, and the cleanest way to do a non-local jump is to reuse the machinery you already have for errors: my evaluation-error type grew a second case, "return this value," which propagates up exactly like an error until it hits a function boundary, where it's caught and turned into the call's value. real faults sail past that boundary; `?`'s early return stops there. one enum, two meanings, and the existing `?`-operator in the Rust does all the propagation for me.
- **`use <- f(...)` is the rest of the block, handed to f as a callback.** this is the Gleam-borrowed sugar that makes spans and scoping into ordinary function calls instead of bespoke syntax. evaluating it means: take every statement *after* the `use`, wrap it into a lambda, and append that lambda as the last argument to the call. so `use <- span("report")` becomes `span("report", () -> { ...the rest... })`, and `span` is just a function that opens a span, runs the thunk, and closes it. telemetry-as-syntax quietly becomes telemetry-as-library.

## the soul shows up

- the whole reason this language exists on *this* OS is observability, and this is the stage where it stopped being a promise. `span` and `emit` are real now (stubbed to an in-memory sink, the placeholder for the eventual wire protocol), and the canonical sample — filter some readings, map a field, emit an average inside a span — runs top to bottom and produces a span tree:

```
span report {
  emit hot.count = 2
  emit hot.values = [35, 40]
}
```

- that's the payoff of `use <-` plus `span` plus `emit` plus pipes plus closures all clicking together. none of them know about each other; they just compose. and it runs from a real file now — there's a `.st` runner and a REPL, so Stitch went from "a test suite" to "a thing you can run."

## dogfooding bit back

- i started the standard library the right way: a small native core (the handful of things you can't express in the language — host i/o, the list primitives, `fold`) and a **prelude written in Stitch itself**, loaded before your code. `count` is literally a fold. `any` is a fold. writing the stdlib *in the language* is the best possible test of it.
- and it immediately caught a bug i'd written down as a hazard in post 1 and then forgotten. i wrote `each` as `fold(xs, (), (_, x) -> { f(x)  () })` — run the effect, return unit. it blew up with "cannot call a Unit." because Stitch has no statement separators, `f(x)  ()` doesn't parse as two statements — it parses as `f(x)()`, *calling the result of f(x)*. the exact maximal-munch corner i'd flagged when i chose not to make newlines significant. the language's own design biting its own standard library is a very on-brand way to be reminded that deferred corners are still corners.

## what i learned

- **the design is still the long pole.** same lesson as last time, one layer down. the evaluator's mechanics are well-trodden; what took thought was deciding semantics — and the best outcomes came from *giving up* a position (the equality rule) once i looked hard at what mainstream languages actually do versus what i'd assumed.
- **separate the questions that got stapled together.** "what does equality mean" and "what can be a key" felt like one decision and were two. half of language design is noticing when you've welded two things that should slide independently.
- **reuse the channel you have.** non-local return wanted to be a whole new control-flow mechanism and was really just "the error path, with a second meaning." the most satisfying implementations are the ones where the new feature falls out of plumbing that already exists.
- **dogfooding finds the corners you wrote down and ignored.** the maximal-munch bug wasn't a surprise — i'd documented it. writing real code in the language is what turned "documented" into "felt."

## what's next

- **dynamic dispatch** — `on`/methods and `contract`s, the one polymorphism Stitch has. it's the core "how Java's `invokevirtual` works" lesson, so i'm writing it by hand as a deliberate exercise rather than letting it be generated. once it exists, the hardcoded `?` becomes an open mechanism — any type with a success/failure split can opt into short-circuiting, not just the built-in `Maybe`/`Result`.
- then **lazy sequences** (the loop-replacement story isn't finished until infinite producers work), and eventually the two things that were the entire point: the **bytecode VM with a real garbage collector**, and the **capability effect system** wired to the actual telemetry protocol — a runtime that watches itself, snitching its own GC pauses and dispatch into the same Grafana as the kernel underneath it.
- but it runs now. it prints a number. it emits a span. everything after this is making it faster, safer, and weirder — in that order.
