# Stitch — extensible `?` / `?.` (the `Try` contract)

_Design note. Captures the intended generalization of the short-circuit operators from hardcoded-for-`Maybe`/`Result` to a user-implementable contract. **Gated on contract dispatch** (`on`/`contract`), so this is a payoff of that work, not a separate feature._

## Status

- **v0 (built):** `?` and `?.` are **hardcoded** for the built-in `Maybe`/`Result` (`interp.rs::eval_try` / `eval_safe_field`). Success = `Some`/`Ok`; failure = `None`/`Err`.
- **Intended (this note):** `?`/`?.` become **sugar over a contract**, so user types with a success/failure split opt in. This is the design's stated intent — see `docs/language-design.md` ("one trait that std implements for `Maybe`/`Result` and users can implement for their own types").

## Mechanism

`?` desugars to "ask the value if it's the failure case; if so short-circuit, else unwrap." In the **dynamic** interpreter this is _simpler than Rust's `Try`_ — there's no enclosing-return-type known at runtime, so we **drop `from_residual` / cross-type conversion**: `?` short-circuits by returning the failure value **unchanged**, no re-wrap.

Minimal contract:

```
contract Try {
    isFailure() -> Bool    // am I the short-circuit case?
    unwrap()    -> @        // success payload (called only when not failure)
}
```

`x?` ≡ `x.isFailure() => return x | x.unwrap()` (the `return` is the function-boundary unwind already implemented via `RuntimeError::Return`). Std `impl`s `Try` for `Maybe` (failure = `None`) and `Result` (failure = `Err`); a user impls the two methods for their own type.

When dispatch lands: re-express `eval_try` to dispatch `isFailure`/`unwrap` through the vtable instead of matching `("Maybe","None")` etc. The built-ins get a std `Try` impl so behavior is unchanged.

## `?.` is a _different_ contract

`?` is _try_ (branch + unwrap). `?.` is _functor-map_ — it must **re-wrap** (`Some(v.y)`), which needs a "rebuild in the same shape" op, not just unwrap. So `?.` rides a `Map`/functor contract, `?` rides `Try`. Don't conflate.

## Scope discipline

- **Same-type only.** Resist Rust's `from_residual` cross-type generality (`?` an `Option` inside a `Result` fn) — it's where Rust's `Try` got genuinely hard. Same-type covers ~all real uses; defer cross-type indefinitely.

## Why bother (uses)

`?` is monadic short-circuit, so anything with a success/failure split benefits. Custom-`Try` is a power-user feature (most code uses built-in `Maybe`/`Result`) — it earns its place by _coherence_ (one open mechanism, not a hardcoded pair):

- **Domain results, on-brand:** `sum Permission = Granted(T) | Denied(reason)` — `cap?` bails on `Denied`.
- **Parsers:** `sum Parse = Done(value, rest) | Failed(at)` — `?` chains steps.
- **Validation / staged state:** `Checked<T>`, `Loading | Ready(T)` (cf. Rust `?` on `Poll`).

Far horizon: a step toward the algebraic-effects north star (`use <-`, iteration, `?`, concurrency on one handler mechanism).

## Prerequisite

Contract dispatch (`on`/`contract` + dynamic method resolution) — see the learning track in `learning/stitch-parser/` (user is implementing dispatch themselves). Build `?`→`Try` _after_ dispatch exists.
