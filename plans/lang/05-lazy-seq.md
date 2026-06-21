# Stitch — lazy `Seq` (walking-skeleton step 8)

_The "no loop keywords" payoff: iteration is lazy sequences + combinators, not `for`/`while`. Eager `List` already exists; this adds the lazy counterpart so infinite producers, `takeWhile`/`foldWhile`, and lazy ranges work._

## Decision — representation

**Thunk-cons, memoized** (the Clojure/Kotlin lineage the design cites). A `Seq` value is a reference to a lazy cell that forces to either nil or a head + lazy tail; forcing is **memoized** (computed once, cached), so re-traversing a `Seq` — `let s = …; s |> a; s |> b` — neither recomputes nor re-fires effects.

```
Value::Seq(Rc<LazySeq>)

struct LazySeq { cell: RefCell<SeqState> }
enum  SeqState { Unforced(ForceFn), Forced(Step) }
enum  Step     { Nil, Cons(Value /*head*/, Value /*tail: a Value::Seq*/) }
```

**The one subtlety:** the thunk holds *Rust* logic, not a Stitch closure. The "rest" of a range / `iterate` / `map` is Rust computation that calls back into `eval`/`apply_values` for any Stitch closures involved (the producer fn, the map fn). So:

```
type ForceFn = Rc<dyn Fn() -> Result<Step, RuntimeError>>
```

It captures whatever it needs (an `Env` clone to call Stitch functions, the producer closure, accumulator state). `force(&LazySeq) -> Step`: borrow the cell; if `Unforced(f)`, call `f()`, store `Forced(step)`, return it; if already `Forced`, return the cached `Step`. Cloning a `Value::Seq` is an `Rc` bump — clones share the same memo cell (correct: forcing through one clone is visible through all).

Equality/Debug: a `Seq` is opaque (lazy, possibly infinite) — `kind()` = "Seq"; `==` between Seqs is not structural (compare by `Rc::ptr_eq` or just disallow). Don't force in `Debug`.

## Phasing (TDD increments, green between each)

1. **The lazy spine.** `Value::Seq` + `LazySeq`/`SeqState`/`Step` + `force` + memoization. A finite range `start..end` evaluates to a `Seq` (each tail thunk yields the next integer or nil). `toList(seq)` native drains a finite `Seq` into a `List`. Test: `(1..4) |> toList == [1, 2, 3]` (exclusive; `..=` inclusive).
2. **Infinite + `take`.** Open range `n..` is an endless `Seq`; `take(n, seq)` returns the first n as a (lazy) `Seq`. Test: `take(3, 1..) |> toList == [1, 2, 3]` — proves laziness (drains a prefix of an infinite seq without hanging).
3. **Producers.** `iterate(seed, f)` = `seed, f(seed), f(f(seed)), …`; `repeat(x)` = `x, x, …`; `forever(f)`? (decide — likely `iterate`/`repeat` suffice; `forever` deferred). Natives building lazy Seqs.
4. **Lazy combinators over `Seq`.** `map`/`filter`/`fold` that are lazy when given a `Seq`. **Open sub-decision (resolve here):** do `map`/`filter` *dispatch on arg type* (List → eager, Seq → lazy), or does `Seq` carry its own methods via the `Functor`/contract machinery? Leaning: keep the bare `map`/`filter`/`fold` names polymorphic over List vs Seq (type-dispatch inside the native), so `xs |> map(f)` reads the same whether `xs` is eager or lazy. `fold` on an infinite Seq diverges (user's responsibility); `foldWhile` is the terminating form.
5. **`takeWhile` / `foldWhile` + lazy `..=`.** `takeWhile(seq, pred)` stops at the first failing element; `foldWhile` folds with an early-stop. Lazy inclusive ranges. These are the terminating consumers that make infinite producers useful.

## Deferred / out of scope

- **Static types** for `Seq<T>` (v0 is dynamic).
- **`Seq` as the return of the existing eager combinators** — keep `List` combinators eager; `Seq` is the opt-in lazy path (Kotlin's `asSequence()` split). No implicit laziness.
- The full lazy stdlib vocabulary (zip/scan/chunk/…) — grow incrementally once the spine + core combinators land.

## Why this matters

This is the feature that backs the language's "no loop keywords" claim. Until infinite producers + `takeWhile`/`foldWhile` work, the loop-replacement story is half-built (the prelude is eager-only). It also unblocks the organized stdlib ([04](04-standard-library.md)), which is gated on lazy `Seq`.
