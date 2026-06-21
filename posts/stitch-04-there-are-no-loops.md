# Stitch 4 — there are no loops

- Stitch has no `for` and no `while`. that was a design bet from the start — iteration is *lazy sequences plus combinators*, the way Kotlin and Clojure do it, not keywords. post 3 ended owing you the machinery that makes the bet pay off: infinite producers, and a way to stop them. this is that. by the end, `(1..) |> map(f) |> filter(g) |> takeWhile(p) |> toList` runs over an **endless** sequence and terminates — no loop in sight, nothing materialised that didn't need to be.
- the surprise this time was that laziness isn't a data structure, it's a **discipline about *when* work happens** — and the bugs all live in the timing, not the values. a lazy list that computes one element too early is still "lazy," still passes the obvious tests, and is still wrong the moment a step has a side effect.

## the shape — a sequence that hasn't happened yet

- a `Seq` is the lazy twin of the eager `List`. eager: `[a, b, c]`, all there, fully computed. lazy: **nil, or a head plus a *thunk* for the rest** — a delayed computation that produces the next cell only when something asks. force the thunk, you get the next head and the next thunk. force forever (if you dare), you get an infinite stream.
- this is the Clojure/Haskell "lazy seq," and the honest tree-walk version is small: a value that's either `Nil` or `Cons(head, tail)`, where forcing is **memoised** — computed once, then cached. so a sequence is a chain of cells that lazily unspool, and re-walking a sequence you've already forced is free.
- ranges are the simplest producer and the first thing to go lazy: `1..4` is `[1, 2, 3]`, `1..=3` is inclusive, and `1..` is **endless** — a sequence whose tail thunk always says "here's the next integer, and here's how to get the one after." nothing is computed until a consumer pulls.

## the subtlety — the thunk holds Rust, not Stitch

- here's the one that made me stop and think. a closure, since post 2, is *Stitch code plus a captured environment*. so my first instinct was: the lazy tail is a Stitch closure `() -> Seq`. it isn't, and can't be. the "rest" of a range, or of `iterate`, or of `map` over a sequence, is **Rust** logic — it adds one, or applies the user's function, or pulls the next element and transforms it. that logic happens to *call back into* the interpreter when there's a Stitch function involved (the `f` in `iterate(seed, f)`), but the thunk itself is native.
- so the tail is a boxed Rust function — `Rc<dyn Fn() -> Step>` — that, when forced, may run Stitch code. it's the post-2 picture turned inside out: there, a closure was a value holding Stitch code; here, a lazy cell is a value holding *Rust* code that runs Stitch code. code-as-data, one level up.

## the arguments that mattered

- **eager `List` and lazy `Seq` are different types, on purpose.** no implicit laziness. `[1,2,3] |> map(f)` does the work now and hands you a `List`; `(1..) |> map(f)` builds a `Seq` that does nothing yet. this is Kotlin's `asSequence()` split, and the reason is **honesty about cost** — a language where any pipeline *might* be lazy is a language where you can't see when work happens. you opt into laziness by reaching for a `Seq`, and then it's lazy all the way down.
- **but the combinators have one name each.** `map`, `filter`, `fold` are polymorphic over the two — a `List` argument stays eager, a `Seq` argument goes lazy, same name. so `xs |> map(f) |> filter(g)` reads identically whether `xs` is a finite list or an infinite stream; only the type at the front decides the strictness. one vocabulary, two execution models. (this was the call I went back and forth on: separate `Seq` methods, or polymorphic functions. polymorphic won — the pipeline shouldn't have to change shape when you swap a list for a stream.)
- **forcing memoises.** clone a `Seq` and both clones share the same lazy cells, so forcing through one is visible through the other — a given element is computed at most once. without this, `let s = expensive(); s |> a; s |> b` would do the work twice and, worse, fire any effects twice. the shared, write-once cell is the same trick as the globals table from post 2, applied to each cons cell.

## the part that was actually tricky — don't compute one ahead

- the heart of the whole thing, and the bug I wrote first. `iterate(seed, f)` is `seed, f(seed), f(f(seed)), …`. the naive implementation, when it hands you a cell, eagerly computes the *next* seed so the tail is ready. it passes every value-based test — the elements are right. but it has applied `f` **one time too many**: to produce `[0, 1]` from `iterate(0, +1)` it computes `f(1)` it never needed.
- for a pure `f`, that's a wasted addition, invisible. the instant `f` has an effect, it's a bug you can *see*: `take(2, iterate(0, x -> { emit("step", x)  x + 1 }))` should emit twice and emits three times. laziness that's off by one element is laziness that fires one extra side effect. so the fix is Clojure's exact discipline — the tail must defer `f(current)` until the tail itself is forced, never when the current cell is — and the test that pins it isn't "are the values right," it's "**how many times did `f` run.**" the emit count is the real spec.

## stopping is the whole point

- an infinite producer is useless without a way to stop pulling, so the terminating consumers are what make the feature real. there are two, and the difference is *what they look at*:
- **`takeWhile(seq, pred)`** stops at the first element that fails a test — it sees each *element*. `(1..) |> takeWhile(x -> x < 1000)` is the closest thing Stitch has to a `for` with a condition, except it's a value you can keep piping.
- **`foldWhile(seq, init, f)`** stops based on the *accumulator*, which `takeWhile` can't see — "sum until the total exceeds 100." its step returns a `Maybe`: `Some(acc)` to keep going, `None` to stop. reusing `Maybe` as the stop signal meant no new concept — the absence type already means "and here it ends." `fold` itself stays the diverges-on-infinite version; `foldWhile` is the one you reach for when the data doesn't end on its own.

## the soul — iteration without keywords

- the bet pays off. here is summing the first ten even squares, with no loop and nothing infinite ever materialised:

```
1.. |> map(x -> x * x) |> filter(x -> x % 2 == 0) |> take(10) |> fold(0, (a, b) -> a + b)
```

- every stage is lazy until `fold` pulls; `take(10)` is the only thing bounding an infinite stream; and it reads top-to-bottom as a description of *what*, not *how*. that's the entire argument for combinators-over-loops: the control flow is gone, replaced by a pipeline of values. the language kept its promise to not have the keyword.

## what i learned

- **laziness is a property of timing, and timing is only visible through effects.** the value-correctness tests all passed while the implementation was wrong; the bug only showed up when I counted side effects. for a lazy evaluator, "when does `f` run" is a first-class spec, not an implementation detail.
- **the honest split beats the clever default.** making laziness opt-in (`List` vs `Seq`) rather than pervasive means you can always answer "when does this work happen" by looking at the type. a language that's implicitly lazy everywhere is a language with a performance model nobody can hold in their head.
- **reuse the type you have.** `foldWhile` wanted a "keep going?" signal and `Maybe` already was one. same lesson as `?` riding the error channel in post 2 and the `Try` contract in post 3 — the best new feature is the one that's a new reading of plumbing you already built.

## what's next

- **modules and visibility** — the last thing standing between Stitch and a real, organised standard library (which is otherwise unblocked now that lazy `Seq` exists). it also draws the encapsulation boundary and pins down where `contract` coherence lives.
- then the **organised stdlib** itself — the prelude grows up from a flat handful of functions into `List.`/`Seq.`/`Str.` with the lazy vocabulary filled in (zip, scan, chunk, …).
- and still ahead, the two that were always the point: the **bytecode VM with a real GC**, where these lazy cells become explicit heap objects the collector walks, and the **capability effect system** wired to the telemetry protocol — a runtime narrating its own execution into the same Grafana as the kernel.
- but it iterates now, over the infinite, without a loop. the keyword that isn't there turned out to be a sequence that hasn't happened yet.
