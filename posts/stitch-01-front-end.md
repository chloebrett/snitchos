# Stitch 1 — familiar on purpose

- a side project that got out of hand. SnitchOS already has a userspace runtime, and the temptation was to grow it into a *language*. but a generic toy language is boring, so the question became: what would a language for *this* OS actually be for? the answer is the two things SnitchOS already cares about — **capabilities** and **telemetry** — and everything else is deliberately ordinary. this post is the arc from "what should it look like?" to a working lexer and parser, all test-driven. it's called **Stitch**: *snitches get stitches*, and it stitches together borrowed ideas from a dozen languages.

## the bet

- the whole design rests on one wager: **spend the novelty budget where it counts, and make everything else familiar.** nobody's life is improved by a clever new `struct` syntax — borrowing the best-understood form for the boring parts is the tasteful move, not the cloning move. so the data layer reads like Rust-and-Kotlin had a baby, on purpose.
- the soul is the two things nothing mainstream does. **capabilities as effects**: a function declares the authority it needs (`uses Telemetry`), the compiler tracks it up the call graph, and you can't touch authority you weren't handed. **telemetry as a language construct**: spans and metrics are first-class, and the runtime narrates its *own* execution — GC, dispatch, allocation — into the same Grafana as the kernel it runs on. that's the part worth being weird about.
- the framing that kept me honest: it's *an effects-and-observability language wearing comfortable data-modeling clothes*. when someone asks "is this Rust or Java?" the answer is "neither — those are just where it shops for the parts it doesn't want to reinvent."

## the decisions that stuck

- a long design conversation settled the surface before a line of code, and a few rules did most of the work.
- **two arrows, one job each.** `->` always means "maps to" (return types, function types, lambdas); `=>` always means "case / condition" (the `cond => a | b` conditional, match arms); `|` always means "alternation." learn four symbols, stop being surprised. this rule paid for itself over and over in the parser — every time two constructs could have collided, the rule said which was which.
- **no loop keywords.** iteration is library combinators over lazy sequences; "loop forever / until / break" are just `forever` / `takeWhile` / `first`. control flow becomes data, which is very on-brand for a match-everything language. the imperative loop still exists — exactly once each, inside a dozen runtime combinators — you just never write one.
- **data-first, no inheritance.** `prod`/`sum` for immutable data; no `class`; polymorphism is `contract`s. null doesn't exist — absence is `Maybe`. and **no `fn` keyword** — a function is the lightest declaration there is, so it carries none.
- the keyword set stayed tiny enough to feel deliberate, which surfaced a real collision late: `sum` is a keyword, but the std combinator vocabulary wanted a `sum` too. the Kotlin precedent settled it — `when` is reserved, so Mockito uses `whenever` — keywords are keywords, libraries rename around them. the combinator became `total`.

## building it test-first

- every increment was red → green → refactor, with `insta` snapshots capturing the AST so a wrong tree shows up as a diff instead of a green test lying to me. ~120 tests later, the front-end parses whole programs.
- the lexer came first and was mostly mechanical, except string interpolation: `"hi {name}!"` lexes into literal and expression *parts*, with the raw `{…}` source captured to be sub-parsed later — a string is a little template, not an atom.
- the parser is a Pratt (precedence-climbing) parser. the satisfying thing about Pratt is that the precedence table *is* the parser — once i wrote down the binding powers, the operator grammar fell out. the less satisfying thing is that a handful of cases needed real thought.

## the parts that were actually tricky

- **lambda detection needs unbounded lookahead.** is `(a, b)` a tuple or a lambda parameter list? you cannot know until you scan to the matching `)` and check whether a `->` follows. so the parser does exactly that — peeks past the parens for the arrow before committing. the same trick later told tuples apart from grouping.
- **placeholders desugar at the call, not the atom.** `map($ * 2)` becomes `map($a -> $a * 2)`. the temptation is to handle `$` where you parse it; the right move is to wrap the *whole argument* into a lambda at the enclosing call. and doing it per-argument made the gnarly "innermost call binds it" rule fall out for free — nested calls capture their own placeholders first, so by the time the outer call looks, there's nothing left to grab.
- **the `=>` collision.** a match guard `x if x > 0 => ...` — parsing the guard `x > 0` with the normal expression parser greedily ate the arm's `=>` as a `cond => a | b` conditional. the fix is a one-character lie: parse guards at a binding power that admits every binary operator but sits just above the conditional, so the `=>` is left for the arm. the "two arrows" rule made the bug obvious and the fix small.
- **no semicolons, no layout rule.** statements separate by *maximal munch*: each statement is a complete expression, and the next begins where the current can't extend. `f(x) g(y)` is two statements because `g` can't continue `f(x)`. it has the classic automatic-semicolon-insertion hazard (`a` ⏎ `-b` parses as `a - b`), which i wrote down as a known corner rather than reaching for significant newlines — a whole lexer subsystem the common cases don't need.
- **constructors vs bindings, by case.** in a pattern, `Circle(r)` is a constructor and `x` is a binding — told apart by whether the identifier starts uppercase, no type information required. it's a convention, not an inference, and it's the kind of small decision that keeps the parser ignorant of types (which it should be).

## what i learned

- **design is the long pole, not parsing.** i spent far more thought on *what the language should be* than on making the parser accept it. once the surface was decided — and decided *coherently*, around a few unifying rules — the parser was a pleasant grind. the rules weren't decoration; they were what made the grammar unambiguous enough to be easy.
- **a good invariant disambiguates for you.** "`->` is always maps-to, `=>` is always a case" isn't just readability — it's the thing that told the parser, at every fork, which construct it was looking at. picking conventions that are *globally consistent* pays off precisely where a parser would otherwise need a special case.
- **do the desugaring at the right altitude.** the placeholder rewrite felt like an atom-level concern and is actually a call-level one; moving it up made a hard scoping rule disappear. when a transformation is fighting you, check whether you're doing it one layer too low.
- **snapshot tests let you move fast without trusting yourself.** every increment, i minted the AST and *read it* — the test isn't "does it parse," it's "is this the tree i meant." for a parser, where the bug is almost always a subtly-wrong shape, that's the whole game.
- **write the deferred corners down.** maximal-munch's ASI hazard, non-associative chains, the `uses` clause — each is a real gap, and noting it in the spec the moment i chose to skip it is the difference between "deferred" and "forgotten."

## what's next

- the tree-walk **evaluator** — the moment Stitch stops being a shape and starts *running*. literals → arithmetic → `let` and scope → functions and closures → `match`, with `span`/`emit` stubbed to `println!` so the observability story has somewhere to land even before the real wire format. the front-end turns text into a tree; next, the tree does something.
- and then, eventually, the parts that were the entire point: the capability effect system, and a runtime that watches itself. but a language earns those by first being able to print a number. that's the next post.
