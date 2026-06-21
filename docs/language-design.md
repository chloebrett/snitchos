# 🪡 Stitch — language design

_Stitch: a small, immutable-by-default managed language for SnitchOS — an **effects-and-observability language wearing comfortable, familiar data-modeling clothes**._

**Overview.** Stitch looks approachable — `prod`/`sum` for data, pipes, pattern matching, lightweight lambdas — but it exists for two things nothing mainstream does: **capabilities are tracked in the type system** (a function declares the authority it needs with `uses`; you can't touch authority you weren't handed), and **telemetry is first-class** (spans/metrics are ordinary `use <-` library calls, and the runtime narrates its *own* execution — GC, dispatch, allocation — as traces into the same Grafana as the kernel it runs on). Underneath it's deliberately Java-shaped — tree-walk interpreter graduating to a stack-based bytecode VM with a generational GC — because the project is partly an exercise in learning how that machinery actually works. The surface is intentionally familiar so the originality can live where it matters: making *authority* and *observability* into language primitives. A few unifying rules keep it coherent — `->` is always "maps to", `=>` always "case/condition", `|` always "alternation", `?`/`?.` a short-circuit family — and loops are a library (combinators over lazy sequences), not syntax. Source extension `.st`.

> **Name origin:** _snitches get stitches_ — SnitchOS snitches (observes and reports); Stitch is the language you write the snitching in. (Credit: E.W.)

Exploratory. Not on the milestone roadmap — a **parallel side project** that can move independently of the kernel track. Its first-class concern is the _implementer's_ education: how a Java-like language is actually built (front end → tree-walk → bytecode VM → generational GC). The novelty that earns it a place _on SnitchOS specifically_ is the capability and telemetry integration; the runtime techniques are deliberately conventional and well-trodden.

> **Status:** design only, nothing built. This page records the decisions made so far and the open questions still on the table, so the spine is written down and can be interrogated before any code exists.

# Primary goal: learn how Java-likes are implemented

The motivating goal is education, not novelty in the runtime. Concretely, the implementer wants hands-on familiarity with:

- A real front end (lexer → parser → AST) feeding two interchangeable back ends.
- A **tree-walking interpreter** (semantics-first), then a **bytecode VM** (implementation-technique-first).
- A **generational garbage collector** — the JVM-shaped target.

This frames every decision below. Where there's a choice between "exotic but interesting" and "conventional but exactly how the mainstream does it," we pick conventional. The interesting risk budget is spent on syntax, capabilities, and telemetry — not on the execution strategy or the collector.

# Decisions made

## Execution: host compiler → on-target runtime; staged tree-walk → bytecode VM

The compiler is a **normal Rust binary that runs on the dev host** — it does the heavy lifting (parse, typecheck, capability-effect analysis) in `std`, and never has to run `no_std` on the target. SnitchOS only ever sees the compiled artifact (AST or bytecode). This keeps the on-target footprint tiny and keeps the analysis-heavy front end out of the kernel/userspace constraints.

The runtime is **staged**, because the two stages teach two different things:

- **Stage 1 — tree-walk interpreter.** Recursively evaluate the AST. Mirrors the language's _semantics_ directly; fastest path to a working language. This is the platform on which we design and prove out the _interesting_ parts — syntax, capabilities, telemetry — end-to-end and early. Cost: slow (pointer-chasing, per-node re-dispatch, name-keyed variable lookups).
- **Stage 2 — bytecode VM.** Compile the same AST to a flat instruction array, run a tight dispatch loop over a **stack machine**. Removes exactly the costs above: linear cache-friendly instruction stream, no re-traversal, variables resolved to **stack-slot offsets at compile time**. _This is the shape of the JVM_ — building it is the core "how Java runs" learning objective.

The front end (lexer/parser/AST) is shared; only the back end is swapped. Mirrors the kernel's own `kernel-core` (pure, host-tested) vs `kernel` (target-only) split: the runtime core is host-testable Rust; only the syscall bridge is target-only. TDD discipline carries straight over.

## Memory: generational GC, grown from a simple collector

"Implicit allocation" means managed memory. The target is a **generational tracing GC** — young/old generations, collect the young generation frequently and cheaply (most objects die young). This is what Java's collectors are, so it's the right target for the stated goal.

Staged the same way as the runtime: start with a **simple correct collector** (mark-sweep or copying semispace), then **grow it into generational**. Correctness first, Java-shape second.

GC belongs to the **VM stage**, not the tree-walk stage. Rationale: in the tree-walk stage the host language is Rust, which has no GC, so we lean on `Rc` (or deliberately leak — demo programs are short-lived). The real collector arrives with the VM, because that's the first point where _we_ own the object heap layout and can find our own roots (walk the operand stack + call frames). Writing a real GC under the tree-walker would fight Rust's ownership for little benefit.

Immutable-by-default is a tailwind for the collector: immutable data forms cycles far less often, and write barriers (the generational GC's bookkeeping for old→young pointers) fire rarely.

## Process model: single process, conventional

The whole compiled program runs as **one SnitchOS userspace process** with one kernel `CapTable`. Conventional threads/tasks for concurrency (mapping onto the existing scheduler). No internal process isolation.

Consequence to stay honest about: capability typing _inside_ the language is therefore enforced by the **compiler and VM**, not the kernel. It's _language-level_ least-privilege — real and useful, but a soft boundary (a VM bug or `unsafe` escape voids it). The kernel still enforces the hard boundary at the process edge.

## Deferred: the actor language

An **actor model** — where the concurrency primitive _is_ the isolation unit _is_ an IPC endpoint, share-nothing message-passing, each actor its own kernel process — is recognized as the most elegant fit for SnitchOS (concurrency + isolation + capabilities collapse into one mechanism; messages are already traceable IPC frames). It is **explicitly deferred to a possible second language**, because (a) it's fully gated on IPC (v0.9, unbuilt) and (b) it's a whole-language identity commitment, not a runtime bolt-on. Filed here so the option isn't lost.

# Surface syntax

_Worked out interactively; firm enough to build a lexer/parser against, but pre-implementation, so treat as strong leanings. The capability and telemetry constructs below (`uses`, `span`) are shown in context but get their own design passes — see Open questions._

Design rule running underneath all of it: **two arrows, one job each.** `->` means _"maps to"_ everywhere — return types, function types, and lambdas. `=>` means _"case / condition"_ — the conditional and match arms. No token does double duty; that's what keeps it from reading as "Kotlin and Rust had a baby."

## Canonical sample

```
prod Reading(sensor: Str, celsius: Int)

average(nums: List<Int>) -> Int =
    nums.isEmpty() => 0 | nums.sum() / nums.len()

report(readings: List<Reading>) uses Telemetry {
    use <- span("report")

    let hot =
        readings
        |> filter($.celsius > 30)
        |> map($.celsius)

    emit("sensor.hot.avg_celsius", average(hot))
}
```

## Bindings — immutable by default

- `let x = …` — immutable binding (the common case).
- `let mut x = …` — mutable; mutation is the marked form.

Borrowed from Rust (a liked language; deliberately _not_ Kotlin's `val`/`var`, the single biggest "this is Kotlin" tell). The keyword-light "immutable binding has no keyword at all" variant was considered and dropped — the declare-vs-reassign disambiguation tax wasn't worth the saved keyword.

## Functions

- `name(params) -> Ret = expr` — single-expression body.
- `name(params) -> Ret { … }` — block body.
- **No `fn` keyword.** A function is the lightest declaration there is, so it carries no keyword; keywords are reserved for the _structural_ forms (`prod`/`sum`/`contract`/`on`). Unambiguous because a bare `name(…) -> …` only appears at module scope or inside `on`.
- Inside an `on` block, three modifiers describe a method's relationship to the receiver `@`: nothing = instance method, `mut` = may mutate `@`, `free` = no receiver (associated function). At module scope a function is inherently `free`, so the keyword isn't written there. See Type system → `on`.
- `name: Type` annotations, no semicolons, expression-oriented throughout — shared "modern tasteful" surface (Kotlin/Rust/Swift/TS), not a Kotlin tell, so kept.

## Conditionals & matching

- **Binary conditional expression:** `cond => a | b` — symbolic, deliberately _not_ on `?` (which is reserved for the error family below). Reads as a tiny truth-table.
- **Multi-way:** `match` (pattern matching is table stakes for this family; it subsumes any N-way `cond`).

## Pipes

- `|>` — left-to-right data flow. Immutable-by-default means writing transformations constantly; the pipe makes them read forward instead of inside-out, and is the most visible departure from Kotlin/Rust.

## Lambdas

One arrow-based form, no brace-as-delimiter, no `|x|`, no `\x`:

- `x -> body` — single named param.
- `(x, y) -> body` — multiple params (parens group them).
- `() -> body` — zero-arg thunk.
- `_ -> v` — ignore the arg → constant lambda (the one case the placeholder sugar structurally can't express).
- `x -> { stmts; result }` — block body; braces here are just an ordinary block _expression_, not lambda syntax.

**Placeholder sugar** for the trivial inline case:

- `$` — the implicit argument; shorthand for `$a`.
- `$a`, `$b`, `$c` … — positional implicit args (letters, not numbers — they read as names). Arity = highest letter referenced.
- A placeholder forms a lambda only when it appears as a **direct call argument**; its extent is that argument expression (delimited by the call's parens/commas). Anything nested or ambiguous → write the explicit `x -> …`. Keeps the magic shallow.

```
map($ * 2)            // \x   -> x * 2   ($ is always $a, so $ * $ is "square", arity 1)
fold(0, $a + $b)      // \a b -> a + b
sortBy($a.age < $b.age)
map(_ -> 0)           // constant
```

**Decision — placeholders are _positional_, not named.** Three rules were on the table: (1) position-by-occurrence (the _n_-th `$` in source order is arg _n_, names cosmetic); (2) **position-by-letter** (the letter _is_ the index: `$a`=arg 1, `$b`=arg 2, …); (3) sort-by-name (collect the distinct names, sort alphabetically, assign positions in that order). We take **(2)**. The deciding property is that **gaps become ignored params**, so a placeholder can _select_ a positional argument: `each(log($b))` ≡ `(_, $b) -> log($b)` — "ignore the first arg, use the second." Sort-by-name can't express that at all (`$b` alone would collapse to a 1-arg identity), and position-by-occurrence makes the same expression depend on textual order rather than the slot you mean. So: **arity = highest letter referenced; unreferenced lower slots are `_` holes.** The cost, accepted deliberately: a letter _is_ its index, so mnemonic names are out — `$x`/`$first` mean "arg 24"/"arg 6", not "the x one." Use `$a`/`$b`/`$c` contiguously. (Implementation: `parser.rs::collect_placeholders` gathers the referenced letters; `parse_arg` fills `0..=max` positionally, holes as `_`.)

**Operators as functions** — a bare operator in argument position _is_ its binary function: `fold(0, +)`, `reduce(max)`, `sortBy(<)`. Haskell-style sections (`(* 2)`) are deliberately _omitted_ — `$` already covers partial application, and one way is enough.

## Errors & absence — the `?` family

Three std types, split by intent:

- `Maybe<T>` (`Some`/`None`) — absence.
- `Result<T, E>` — the _intent-named_ fallible type; what `?` primarily targets.
- `Either<A, B>` — the genuinely-neutral two-arm sum (neither side is "error," e.g. `Either<Cached, Fresh>`).

Two short-circuit operators, both backed by **one trait that std implements for `Maybe`/`Result` and users can implement for their own types**:

- `x?` — _function-level_ try: if `x` is the failure case, return it from the enclosing function; else unwrap and continue.
- `x?.y` — _expression-level_ safe navigation: short-circuits the chain to the failure/empty value (staying wrapped), else accesses `.y`. `user?.address?.zip : Maybe<Zip>`.

`?` deliberately does **not** apply to `Either` — it has no canonical failure side, and that asymmetry _is_ the documentation that `Result` is the failure type.

## `use <-` (from Gleam) — block sugar, not a keyword zoo

`use <- f(...)` turns "the rest of this block" into a callback handed to `f`. It desugars:

```
use <- span("report")          //  ≡   span("report", () -> {
emit("x", 1)                   //          emit("x", 1)
// …rest of block…             //          // …rest of block…
                               //      })
```

General form `use x <- f(...)` ≡ `f(x -> { rest })`. This makes spans, resource scoping (the `defer` job), and mutex scoping all _ordinary functions_ instead of bespoke syntax — i.e. **telemetry-as-syntax becomes telemetry-as-library**. Accepted cost: `use` has "magic" control flow (everything after it is captured), which reads oddly until it clicks.

## String interpolation

- `"temp is {avg}°C"` — interpolate with `{expr}`; literal braces are `{{` / `}}` (format-string style). `\` stays plain string escapes (`\n`, `\"`) and `$` stays a lambda placeholder — three distinct roles inside a string, no overload.

## Building strings

Strings are immutable values, so they're built by **templating and folding**, not in-place mutation — the same grain as the rest of the data model:

- **Interpolation** (above) is the common case — most assembly is really templating.
- **`join(sep)`** is the canonical "concatenate N pieces" — a string-specialised fold, defined on `List`/`Seq` (`items |> map($.name) |> join(", ")`). It lives with the other combinators, not as bespoke string syntax.
- **`+` concatenates two strings** (`"a" + "b"`). Overloading `+` is unambiguous because v0 is strict — `1 + "x"` is a type error, not coercion — so `+` on two strings can only mean concatenation. (No separate `<>` operator; one way.)
- **Deferred escape hatch:** a `StringBuilder` with a `mut` buffer would be a *library type* (the explicit-mutable-cell case), reached for only if a hot loop ever proves interpolation + `join` insufficient — not a language feature.

# Type system

_Same status as Surface syntax: worked out, pre-implementation, strong leanings._

**Stance — data-first, no classes, no inheritance.** There are exactly three type-declaration forms — `prod`, `sum`, `contract` — plus `on` blocks that attach behavior. There is **no `class` keyword and no inheritance**: data is immutable by default with mutability opt-in per field (`mut x`) and per method (`mut`), and the _only_ polymorphism is `contract` conformance, dispatched dynamically. This is effectively **Rust's data-plus-trait model, GC'd and reference-semantic, with friendlier keywords** — a distinct middle between Java ("everything is a class, inherit freely") and Rust ("structs + traits + ownership"). Null does not exist (absence is `Maybe`), and the GC removes Rust's ownership ceremony.

> **Why this isn't a Rust or Java clone.** The data-declaration layer below is _intentionally_ familiar — borrowing the best-understood forms is the tasteful move, not the cloning move. The language's identity lives entirely in the parts nothing else has: `uses` capabilities-as-effects, the `?`/`?.` trait family, `use <-` making telemetry a library, and (unbuilt) the capability effect system + a VM that emits spans for its own GC/dispatch. It is an _effects-and-observability language wearing comfortable data-modeling clothes._
>
> The class-vs-no-class question was genuinely contested (see the keyword arc in the side-project memory): a Java-classes detour was explored for the implementation-learning value, but the dispatch/vtable lessons come through `contract` dynamic dispatch regardless, so the trait-like model wins without costing the education.

## Products & sums — one tree, two roots

A `prod` declares a product root (fields AND-ed); a `sum` declares a sum root (variants OR-ed); every sum variant is itself a product, so a `prod` is the degenerate one-variant sum. `prod` (∏) / `sum` (∑) keeps the mathematical symmetry; `prod` was chosen over `tup` (collides with anonymous tuples) and over the 7-char `product`.

```
prod Point(x: Int, y: Int)

sum Shape =
    | Circle(radius: Int)
    | Rect(w: Int, h: Int)
```

The std error/absence types are _just sums_ — the sign the algebra is load-bearing:

```
sum Maybe<T>     = Some(T) | None
sum Result<T, E> = Ok(T)   | Err(E)
sum Either<A, B> = Left(A) | Right(B)
```

- **Mutability is opt-in:** fields are immutable unless marked `mut` (`prod Counter(mut n: Int)`); everything is a GC reference, so a `mut` field is visible through all aliases (Java/Kotlin semantics, not value copies).
- **Equality:** structural for **all** `prod`/`sum`, mutable fields or not — two `Point(1, 2)` are equal (Kotlin `data class` semantics, which compare every field whether `val` or `var`). The mutable-hash-key footgun (a key whose hash changes out from under the table when the value is mutated) is designed out *not* by faking identity equality, but by separating equality from **key-eligibility**: only immutable types satisfy the `Key`/`Hashable` contract, so using a value with any `mut` field as a `Map`/`Set` key is a **compile error**. (Python forbids it at runtime — lists are unhashable; static typing turns that into a type error.) Structural equality on a mutable value therefore means "equal as of now," and the temporally-unstable case can never reach a hash table. `===` is always identity.
  - _Decision (B-vs-D):_ considered banning `mut` fields entirely (option **B** — fully immutable data, structural+stable equality, no `mut` method modifier). Chose **D**: keep mutable fields and mutating methods, give *everything* structural equality, and move footgun-prevention to the immutable-key constraint above. Equality was the only real argument for B and it's solved either way; D keeps conventional, ergonomic stateful data (Kotlin-shaped) and exercises the generational-GC write barrier heavily — at the accepted cost that the "races only around `mut`" concurrency caveat stays live.
- **Construction & update:** `Point(1, 2)` positional, `Point(x: 1, y: 2)` Swift-style labels, `Point(..p, x: 10)` functional update (copy with override).
- **Tuples** are the anonymous product: `(Int, Str)`.
- **GC dividends:** recursive sums need no `Box` (`sum List<T> = Cons(T, List<T>) | Nil` just works), and `match` over a sum is **compiler-checked exhaustive** (dovetails with the no-exceptions stance).

## on — methods & conformance

Behavior attaches to any type (`prod` or `sum`) via an `on` block. The receiver is the `@` sigil (`@x` is field `x` on the receiver) — distinct from the lambda placeholder `$`, so the two never collide inside one body (`@items |> map($.price)`). Method modifiers describe the relationship to `@`:

```
on Counter {
    value() -> Int = @n            // instance method, immutable @
    mut bump() { @n = @n + 1 }     // mutates @ — caller needs a `mut` binding
    free zero() -> Counter = …     // no receiver; called Counter.zero()
}

on Counter : Drawable {            // `: Contract` declares conformance
    draw() uses Canvas = renderBar(@n)
}
```

- `mut` methods may write `@`; calling one **requires a `mut` receiver binding** (Rust's `&mut self` discipline, no lifetimes).
- `free` is the only modifier that appears _inside_ `on` but not at module scope (every module function is already receiver-free).
- **The receiver is never implicit.** A method body calls a sibling method as `@other()`, never bare `other()` — a bare name is always a local/global, never a method on `@`. (Decided when contract default methods landed: a default that calls another method writes `@speak()`, which dynamically dispatches to the concrete type — late binding / open recursion, the template-method pattern.) This keeps name resolution one flat rule (bare = lexical/global, `@`-prefixed = receiver) and avoids the "is this a field, a local, or a method?" ambiguity an implicit `self` reintroduces. Consistent with `@field` access never being implicit either.

## contract — the only polymorphism

`contract` is the behavior-contract / trait / interface — the sole dispatch mechanism, with traits' discipline:

- **Definition-side coherence:** a type's `on` blocks and conformances live with the type, in its own module — no orphan/extension-from-afar (this is the "no extension" rule: behavior is defined once, with the type, and is always findable).
- **Default methods** in a `contract` give behavior composition; data composition is embedding a `prod` in a `prod`.
- **GC makes dynamic dispatch the easy path** (unlike Rust). A contract-typed value is just a heap object + vtable, like Go/Java — `render(d: Drawable)` taking any `Drawable` is the natural default; generics (`render<T: Drawable>(d: T)`) remain available for the monomorphized path. `@` needs no `&`/`&mut`/lifetimes.

## What the VM implements (the learning core)

The JVM-shaped lessons are intact: object headers (type ptr, GC bits, identity hash), **contract vtables/itables + dynamic dispatch**, type-test-via-contract, constructors, field layout. The one lesson dropped with inheritance — superclass-prefix vtable layout — is traded for interface/itable dispatch (and inline caches), the more interesting half of how real JVM dispatch works.

# Open questions — the interesting surface

These are where the design risk budget is deliberately spent, and the next things to work out. Each will get its own pass.

- **Capabilities as effects.** Functions declaring the authority they need (`log(msg: Str) uses TelemetrySink`), the compiler tracking the `uses` set up the call graph, startup caps arriving from `a0`/`a1` and threading down, affine/linear cap values so authority can't be forged or duplicated. The strongest reason the language exists on _this_ OS. How much of this is checked at compile time vs reified in the VM?
- **Telemetry as syntax.** Spans and metrics as first-class constructs (`span foo { ... }` auto-emitting SpanStart/SpanEnd over the existing `Frame` protocol; declared counters). Plus the reflexive win: the VM narrates _its own_ execution — GC pauses, allocation rate, cap checks, dispatch — as spans in the same Grafana as the kernel.
- **Syntax & type system.** Now substantially worked out — see [Surface syntax](#surface-syntax) and [Type system](#type-system) above. Remaining grammar gaps: generics beyond `List<T>` (bounds, variance), module/visibility (which also defines encapsulation + the `contract`-coherence boundary), and the precise `match` pattern grammar.

# Concurrency (parked — rides the effect system)

_Not designed in detail, not for v0. Recorded so the reasoning survives. Depends on the capability **effect system** + VM **continuations** existing first — the same machinery that powers `use <-` and the iteration north-star._

Already decided in passing:

- **Single process, in-process tasks.** Stitch is *not* the actor language (that was the deferred separate language where actor = process = IPC endpoint). Concurrency here shares one address space.
- **Immutable-by-default eliminates most data races by construction** — shared data is immutable, so concurrent reads are safe; races are only possible around `mut`, already the marked case.

Intended model:

- **No async/await coloring.** A function that suspends just declares the effect in its `uses` row — there is no async/sync function split (the most-regretted part of Rust/JS async). Concurrency lives in the effect row, not a second species of function.
- **Structured concurrency.** A `use scope <- nursery()` block bounds task lifetime: tasks spawned in it are joined or cancelled when the block exits (including early exit via `?`). No leaked tasks.
- **Capability-mediated.** Spawning needs a `Tasks` cap; tasks inherit a bounded cap set. Authority to create concurrency is grantable/revocable like any capability.
- **Observable for free.** A nursery is a span; child-task spans nest under it; task switches are already traced `ContextSwitch` frames on SnitchOS. The concurrency *is* the trace.
- **Channels are `Seq<T>`.** A channel fed by `send` is consumed as a lazy sequence (`ch |> filter |> each`), reusing the combinator vocabulary — no new receive syntax.
- **The scheduler is a swappable handler.** Because concurrency is an effect, `with scheduler(RoundRobin) { … }` vs `with scheduler(Deterministic.seed(1)) { … }` run identical code under real or reproducible-test scheduling. (The handler side is the least-designed part — only the `uses` declaration side is settled.)

```
fetch(url: Str) -> Result<Response, NetError> uses Net, Telemetry {
    use <- span("fetch")
    Net.get(url)
}

fetchAll(urls: List<Str>) -> List<Result<Response, NetError>> uses Net, Tasks, Telemetry {
    use scope <- nursery()                          // joined/cancelled at block exit
    use <- span("fetch_all")
    urls
    |> map(u -> scope.spawn(() -> fetch(u)))        // all start concurrently
    |> map(await)                                   // join each; suspends the task, not the hart
}
```

- **Maps onto the kernel:** `spawn` → a SnitchOS task; suspend → cooperative yield (preemptive once v0.8 lands); cross-process → IPC (v0.9).
- **Effect-set aliases** (e.g. `effect App = Net, Tasks, Telemetry`, then `uses App`) will be needed so rows stay readable — a concern for the capabilities pass.

**Throughline:** capabilities (`uses`), scoping (`use <-`), iteration, and concurrency all ride the **same algebraic-effects machinery**. Build delimited continuations + handlers once in the VM, and all of it falls out — which is why parking concurrency is safe: it waits on the same foundation everything else needs.

# Lineage

Stitch is a patchwork, deliberately — and the name owns it. For each job it borrows the best-understood form from a language that solved it well; the originality is the **stitching** (the unifying rules below) and the two things that are nobody else's (`uses` capabilities-as-effects, telemetry-as-language) — not the individual patches. A pile of borrowed features is a Frankenstein; a coherent language is a quilt. The seam work is the value.

- **Rust** — `let`/`let mut`, immutable-by-default, `?` try operator, `Result`, contract coherence / orphan rule, `..` functional-update + `..`/`..=` ranges, the monomorphised-generics path.
- **Kotlin** — eager `List` vs lazy `Seq` split; "sane defaults, unlike Java."
- **Gleam** — `use <-` block-callback sugar.
- **Elixir / F#** — the `|>` pipe (first-argument insertion).
- **Clojure / Kotlin** — lazy sequences; numbered-placeholder lineage (now `$a`/`$b`).
- **ML / Haskell** — the `sum`/`prod` algebra, `match`, lazy sequences, the categorical naming.
- **Scala** — `using`/`given` contextual parameters (the planned `uses` threading).
- **Koka / Unison** — effects as the model for `uses`, and the algebraic-effects north star (iteration today, concurrency later).
- **Swift** — argument labels; the value/reference distinction (informed the `prod` equality model).
- **Ruby** — `@` for the receiver.
- **Roc** — philosophically: "the platform provides the effects" ≈ "the OS provides the capabilities."

The stitches that make it one language, not a heap: `->` always "maps to", `=>` always "case/condition", `|` always "alternation", `?`/`?.` one short-circuit family, two-tier data (`prod`/`sum` + `contract`, no inheritance), and no loop keywords (combinators over lazy `Seq`).

# References

- [docs/observability-design.md](observability-design.md) — the `Frame` wire format the language's telemetry will target.
- [docs/capability-system-design.md](capability-system-design.md) — the kernel cap model the language's cap-effects sit on top of.
- [docs/ipc-design.md](ipc-design.md) — what the deferred actor language would ride on (v0.9).
- _Crafting Interpreters_ (Nystrom) — the jlox→clox arc this staging deliberately follows.
