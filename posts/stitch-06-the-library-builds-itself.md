# Stitch 6 — the library builds itself

- modules ([post 5](stitch-05-a-module-is-a-boundary.md)) existed for a reason: a standard library needs somewhere to put the dot. so this milestone is the payoff — `Str.split`, `Str.trim`, the everyday combinators `reverse`/`drop`/`zip`/`sort`/`flatMap`, and the small derived helpers `min`/`max`/`first`/`last`/`flatten`. strings went from "you can `join` them and interpolate them" to actually usable; the flat combinator core went from a handful to a vocabulary.
- but the thing i actually learned isn't in the function list. it's that a standard library is **small** — not in lines, in *ideas*. a few primitives that have to be native, a couple of consistency rules, and then most of the "library" is one of two things: a one-liner standing on the functions below it, or a correction the next function forces on the last one. i sat down to write a library and mostly discovered one.

## primitives and vocabulary

- the first question every stdlib function asks is: does this *have* to be Rust? most don't. the native floor is small and specific — the things that touch machinery the language can't reach. lazy-sequence cells (`map`/`take`/`drop` over a `Seq` weave new thunks); string manipulation (`split`, `upper` — host string primitives); sorting (you need the actual algorithm); `zip`/`enumerate`/`concat` (paired or indexed iteration). everything else is *vocabulary*, and vocabulary is just Stitch.
- the split that decides where a function lives is the one from [post 4](stitch-04-there-are-no-loops.md): the **polymorphic core stays flat**. `map`/`filter`/`fold` are one name each, eager on a `List`, lazy on a `Seq` — so they can't become `List.map`/`Seq.map` without reintroducing the split the language refused. the *namespaces* hold only what's genuinely type-specific: `Seq` for the lazy producers, `Str` for string ops. and the payoff of that boundary is a name clash that becomes a feature — `Str.contains` (substring) lives next to the flat `contains` (element membership), two functions, one name, and the dot is the whole difference.

## the library builds itself

- here's the part that's quietly satisfying. once `fold` exists, a startling amount of the library is *already written* — you just have to spell it. the extremes:

```
min(xs) = fold(xs, None, (acc, x) -> match acc { None => Some(x)  Some(m) => x < m => Some(x) | acc })
last(xs) = fold(xs, None, (_, x) -> Some(x))
```

- `min`/`max`/`first`/`last`/`find`/`any`/`all`/`count` are all *one fold each*, returning a `Maybe` where absence is real. and the compounding goes one level further: `flatMap` (native) begets `flatten` for free —

```
flatten(xss) = flatMap(xss, xs -> xs)
```

- that's the standard library standing on itself: a function whose entire body is another function. it's the same dogfooding the language has done since post 2 (the prelude is Stitch, not Rust), but now it's *layered* — nine of this milestone's functions are derived, only the genuinely-primitive ones are native. the more you build, the more the next thing is a line. a library isn't a pile of functions; it's a thin native floor and a tall stack of vocabulary that mostly writes itself.

## the second function audits the first

- the sharp lesson, the one i didn't expect. i added `drop` to sit next to `take`, made it collection-first so it would pipe — `seq |> drop(2)` — and then went to write the test and `take` wouldn't compose with it. because `take` was `take(count, seq)`. **count-first.** so `seq |> take(3)` fed the sequence in as the *count*, and errored.
- this had been wrong the whole time and i hadn't seen it, because nothing had ever been added next to `take` to contrast with. every other combinator — `map`, `filter`, `fold`, `takeWhile` — is collection-first, so it pipes. `take` was the one exception, and the exception is invisible until you put a second, consistent thing beside it. worse: the north-star example from post 4, `1.. |> map(f) |> filter(g) |> take(10) |> fold(…)`, **never actually ran as written** — the prettiest line in the whole series was a lie, and only the act of building `drop` exposed it.
- the fix was a one-line argument flip, and now the post-4 pipeline runs for real. but the lesson is bigger than `take`: **consistency is discovered, not designed.** you can't see that an interface is irregular by staring at it alone; you see it the moment you add the next sibling and they don't rhyme. the second function is an audit of the first. a library that's only ever had one of each thing has consistency bugs it can't possibly have noticed yet.

## the small change with the long reach

- one more, because it's the opposite shape — a tiny change that rippled. `sort` needs to compare elements, and comparison lived only in the `<` operator, which only knew numbers. i pulled the ordering logic into one shared function and taught it strings (lexicographic). suddenly `sort` worked on text — but so did `min`, so did `max`, and so did `"apple" < "apply"` in plain expressions, none of which i touched. one primitive (`value_order`) sits under the operator *and* the combinator, so widening it once widened everything above it. the inverse of the `take` story: there, adding a function exposed a rule; here, fixing a rule lifted every function resting on it.

## what i learned

- **a standard library is a small idea wearing a lot of functions.** the native floor is tiny and load-bearing; the rest is vocabulary, and good vocabulary is mostly composition. if a new function isn't almost a one-liner, ask whether the primitive below it is missing.
- **consistency is an emergent property you only test by extending.** the `take` bug was unfindable in isolation and obvious the instant a sibling stood beside it. the corollary: be suspicious of any part of an interface that's still the only one of its kind.
- **share the primitive, not the behaviour.** one `value_order` under both `<` and `sort` meant strings became orderable everywhere at once. duplicated comparison logic would have meant remembering to widen it in N places; a shared floor widens once.

## what's next

- the stdlib is genuinely usable now, so the two that were always the point come back into view: the **bytecode VM** with a real garbage collector — where these eager lists and lazy cells stop being Rust `Rc`s and become objects a collector walks — and the **capability effect system**, the reason the language exists on this OS.
- but increasingly the thing standing in front of both is the **type checker**. every deferral has the same shape — opaque types are *unforgeable* but not yet *sealed*; `@` parses but means nothing; `uses` is decoration. all of it is waiting for the same machinery: a pass that knows what type an expression has. the dynamically-typed tree-walker has carried the language a long way, further than i expected. but the next real altitude — sealing representations, checking authority, monomorphising generics — is the same altitude, and it has a name.
- a library that builds itself is a good sign. it means the primitives underneath are the right ones. now to give them types.
