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

## Phasing (TDD increments, green between each) — ALL SHIPPED

1. ✅ **The lazy spine.** `Value::Seq(Rc<LazySeq>)` + `SeqState`/`Step` + `force` + memoization (`value.rs`). Finite `start..end` and inclusive `..=` evaluate to a `Seq` (`interp.rs::range_seq`). `toList` drains a finite `Seq` to a `List`.
2. ✅ **Infinite + `take`.** Open range `n..` is endless; `take(n, seq)` is lazy. Proven by draining a prefix of `1..` without hanging.
3. ✅ **Producers.** `iterate(seed, f)`, `repeat(x)` (`natives.rs`). `iterate` is fully lazy — applies `f` once per element actually demanded (proven by an emit-count test, no element computed ahead). `forever` not needed (`iterate`/`repeat` suffice).
4. ✅ **Lazy combinators.** `map`/`filter`/`fold` are **polymorphic** (DECIDED): a `List` stays eager, a `Seq` is lazy — same names, so `xs |> map(f) |> filter(g)` reads identically. `fold` forces a `Seq` to the end (diverges on infinite — that's `foldWhile`).
5. ✅ **`takeWhile` / `foldWhile`.** `takeWhile(seq, pred)` stops at the first failing element (element-based). `foldWhile(coll, init, f)` (DECIDED: **Maybe-returning step** — `f` returns `Some(acc)` to continue, `None` to stop with the prior acc) is the accumulator-aware terminator. Lazy `..=` landed in phase 1.

**Resolved decisions:** (4) polymorphic combinators over List/Seq, not separate Seq methods; (5) `foldWhile` signals stop via `Maybe` (reuses the type, no new convention).

## Deferred / out of scope

- **Static types** for `Seq<T>` (v0 is dynamic).
- **`Seq` as the return of the existing eager combinators** — keep `List` combinators eager; `Seq` is the opt-in lazy path (Kotlin's `asSequence()` split). No implicit laziness.
- The full lazy stdlib vocabulary (zip/scan/chunk/…) — grow incrementally once the spine + core combinators land.

## Why this matters

This is the feature that backs the language's "no loop keywords" claim. Until infinite producers + `takeWhile`/`foldWhile` work, the loop-replacement story is half-built (the prelude is eager-only). It also unblocks the organized stdlib ([04](04-standard-library.md)), which is gated on lazy `Seq`.
