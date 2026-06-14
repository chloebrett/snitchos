# Stitch — grammar & precedence spec (v0)

_The one design artifact that must be pinned before any parser code, because the parser **encodes** these decisions and they're expensive to retrofit. Everything semantic (methods, caps, telemetry) is a cheap incremental add on top and is deliberately **out of scope** here._

High-level language design: [docs/language-design.md](../../docs/language-design.md). This file is the implementation-facing surface: tokens, precedence, the two novel parsing rules, and the v0 parser scope.

> **Status:** spec, pre-implementation. Decisions marked _(confirm)_ are sensible defaults chosen here that weren't explicitly settled in design chat — flag if you disagree before building the parser.

---

## 1. Lexical grammar

### Whitespace & comments
- Whitespace is insignificant (no layout rule). Newlines separate declarations and statements but are otherwise not load-bearing.
- `// line comment` to end of line.
- `/* block comment */`, **nestable** _(confirm)_.

### Identifiers
- `[A-Za-z_][A-Za-z0-9_]*`.
- **No trailing `?` in identifiers** — `?` is exclusively the try operator, so `hot?` lexes as `hot` then `?`. (The Lisp-style `predicate?` naming floated in chat is dropped to keep `?` unambiguous.)

### Keywords (reserved)
```
prod  sum  contract  on  let  mut  free  use  uses  match  and  or  not  true  false
```
That's the whole set for v0. Notably absent: `fn`, `class`, `if`, `else`, `when`, `interface`, `null`, **and every loop keyword** (`for`, `while`, `loop`, `break`, `continue`) — iteration is library combinators over (lazy) sequences, not syntax; see §5a / §6a. `_` is a token (wildcard / catch-all), not a keyword. Boolean logic is the words `and`/`or`/`not` (not `&&`/`||`/`!`) — see operator table for why.

### Literals
- **Int:** `123`, `1_000` (underscores ignored). Decimal only in v0; hex/binary deferred (additive later).
- **Float:** `3.14`, `1_000.5`.
- **Bool:** `true`, `false`.
- **Str:** `"…"` with escapes `\n \t \" \\` and **interpolation** `{expr}` — `"temp is {avg}°C"`. Literal braces are `{{` / `}}` (format-string style). `\` is plain escapes only; `$` stays a lambda placeholder — three distinct roles in a string (`{` interpolate, `\` escape, `$` literal), no overload.
- **List:** `[1, 2, 3]` — eager `List<T>`.
- **Map:** `["host": 1, "port": 2]` — eager `Map<K, V>`; empty map is `[:]` (since `[]` is the empty list). List vs map disambiguated by the `:` in entries.

### Operators & punctuation
```
+  -  *  /  %                  arithmetic
== != <  <= >  >=              comparison
and  or  not                   boolean (keywords, not symbols — frees `|` to mean alternation only)
|>                             pipe
.. ..=                         range, exclusive / inclusive (1..10, 0..=n)
->                             "maps to": lambda, return type, function type
=>                             "case": conditional + match arm
|                              alternation / "or": sum variants, or-patterns, conditional-else
?   ?.                         try / safe-navigation
@                              receiver (self); @x = field x on receiver
$  $a $b $c …                  lambda placeholders ($ ≡ $a)
.                              field / method access
..                             also spread in construction (Point(..p, x: 10)); range (infix) vs spread (prefix) disambiguated by position, à la Rust
:                              type annotation; contract conformance (on T : C)
=                              binding / assignment
,  ;  (  )  {  }  [  ]  <  >   delimiters
```

---

## 2. Precedence & associativity

Tightest (binds first) → loosest (binds last). This is the table the Pratt/precedence-climbing parser is built from.

| Lvl | Operators | Assoc | Notes |
|----:|-----------|-------|-------|
| 1 | `f(...)`  `a[i]`  `.x`  `?.x`  `e?` | left | postfix; tightest |
| 2 | prefix `-e`  `not e` | — | unary |
| 3 | `*  /  %` | left | |
| 4 | `+  -` | left | |
| 5 | `..`  `..=` (range) | non-assoc | tighter than pipe, looser than `+`: `1..n+1` = `1..(n+1)`, and `0..n \|> collect` = `(0..n) \|> collect` |
| 6 | `\|>` | left | looser than arithmetic/range (`a + b \|> f` = `(a+b) \|> f`) but tighter than comparison (`x \|> f == y` = `(x\|>f) == y`) |
| 7 | `==  !=  <  <=  >  >=` | non-assoc | chains like `a < b < c` are a parse error; use `and` |
| 8 | `and` | left | |
| 9 | `or` | left | |
| 10 | `cond => a \| b` (conditional) | non-assoc | the inline binary match; `b` is the bare else-branch |
| 11 | `x -> body` (lambda) | right | loosest — a lambda body swallows everything to its right (`x -> cond => a \| b` = `x -> (cond => a \| b)`) |

Mixing `|>` or `..` with `and`/`or`/comparison requires parens — rare in practice, so not worth special levels.

> **Deferred parser checks (TODO):** comparisons and ranges are non-associative, and the `=> |` conditional is non-associative too — chaining it (`c1 => a | c2 => b | c`) should be a **parse error that points the user at `match`** ("chained conditionals aren't allowed — use `match`"). None of these are enforced yet; the parser currently accepts them leniently.

Type-level `->` (function types `A -> B -> C`) is **right-associative** = `A -> (B -> C)`.

---

## 3. The placeholder-lambda extent rule _(novel — pin carefully)_

A placeholder (`$`, `$a`, …) appearing inside a **call argument** turns that argument expression into a lambda.

- **Extent:** the lambda body is the *whole argument expression*, bounded by the enclosing call's parens and the surrounding commas. `map($ * 2 + 1)` → `(x) -> x*2+1`. `f($ > 30, other)` → first arg is `(x) -> x > 30`, `other` is unaffected.
- **Arity:** the highest placeholder letter referenced (`$` ≡ `$a`), min 1. `$ * $` is arity 1 (square); `$a + $b` is arity 2.
- **Binding:** a placeholder binds to the **innermost enclosing call's** argument list.
- **Shallow-magic guardrail:** this only fires for a placeholder that is a *direct* sub-expression of a call argument. For anything nested, conditional, or where the intended extent isn't obvious, the parser does **not** guess — write the explicit `x -> …`. (Implementation: when a guardrail-violating `$` is found, error with "use an explicit `x ->` lambda here.")

---

## 4. Pipe semantics

`|>` inserts its left operand as the **first argument** (Elixir-style, not F#-style — confirmed) of the call on its right. The right side is either a **call** or a bare **function reference**:

- `LHS |> f(a, b)`  ≡  `f(LHS, a, b)`     (call: LHS inserted first)
- `LHS |> f`        ≡  `f(LHS)`            (bare reference — no empty `()` needed)

The bare-reference form is the "no extra args" stage and keeps pipelines bracket-free:
```
[1, 2, 3] |> toSeq |> max        ≡  max(toSeq([1, 2, 3]))
0..n |> map($ * $) |> filter($ > 10) |> total
```
A pipe stage is therefore one of: a bare name (`max`, `sum`), an operator-as-function (`fold(0, +)`), or a call with its remaining args (`map($ * 2)`) — never a stray `()`. "Bare reference" means an identifier or path (`Math.sqrt`); anything more complex on the right must be an explicit call.

This composes with the placeholder rule: `readings |> filter($.celsius > 30)` ≡ `filter(readings, (x) -> x.celsius > 30)`. So std collection functions are declared subject-first: `filter(list, pred)`, `map(list, f)`, `fold(list, init, f)`.

---

## 5. Grammar (EBNF-ish)

`*` = zero-or-more, `?` = optional, `|` = alternation, `…` = elided. Quoted tokens are literal.

```ebnf
program     = item* ;
item        = prodDecl | sumDecl | contractDecl | onDecl | funcDecl | constDecl ;

prodDecl    = "prod" Ident generics? "(" fieldList? ")" ;
fieldList   = field ("," field)* ;
field       = "mut"? Ident ":" type ;

sumDecl     = "sum" Ident generics? "=" "|"? variant ("|" variant)* ;
variant     = Ident ("(" fieldList? ")")? ;          (* fieldless variant = bare Ident *)

contractDecl= "contract" Ident generics? "{" sig* "}" ;
sig         = "mut"? "free"? Ident "(" paramList? ")" ("->" type)? usesClause?
              ( "=" expr )? ;                          (* "=" present = default method *)

onDecl      = "on" type (":" type)? "{" method* "}" ;
method      = ("mut" | "free")? Ident "(" paramList? ")" ("->" type)? usesClause? body ;

funcDecl    = Ident "(" paramList? ")" ("->" type)? usesClause? body ;   (* no "fn" keyword *)
constDecl   = "let" "mut"? Ident (":" type)? "=" expr ;

usesClause  = "uses" type ("," type)* ;                (* parsed; ignored in v0 *)
body        = "=" expr | block ;
block       = "{" stmt* "}" ;
stmt        = constDecl | assignment | useExpr | exprStmt ;   (* no loop stmts — iteration is combinators, §5a/§6a *)
assignment  = lvalue "=" expr ;                        (* lvalue: @x, name (mut only) *)
useExpr     = "use" (Ident "<-")? callExpr ;           (* scoping/callbacks only — NOT iteration *)

paramList   = param ("," param)* ;
param       = Ident (":" type)? ;
generics    = "<" Ident ("," Ident)* ">" ;             (* declaration side; bounds deferred *)

expr        = lambda ;
lambda      = (Ident | "(" paramList? ")") "->" expr   (* level 10 *)
            | conditional ;
conditional = orExpr ("=>" expr "|" expr)? ;           (* level 10 *)
orExpr      = andExpr ("or" andExpr)* ;
andExpr     = cmpExpr ("and" cmpExpr)* ;
cmpExpr     = pipeExpr (cmpOp pipeExpr)? ;              (* non-assoc *)
pipeExpr    = rangeExpr ("|>" rangeExpr)* ;
rangeExpr   = addExpr ((".." | "..=") addExpr)? ;      (* non-assoc *)
addExpr     = mulExpr (("+"|"-") mulExpr)* ;
mulExpr     = unary (("*"|"/"|"%") unary)* ;
unary       = ("-"|"not") unary | postfix ;
postfix     = primary postOp* ;
postOp      = "(" argList? ")" | "[" expr "]" | "." Ident | "?." Ident | "?" ;
primary     = literal | Ident | "@" | "@" Ident | placeholder
            | "(" expr ("," expr)* ")"                 (* grouping or tuple *)
            | listLit | mapLit
            | matchExpr | block | construct ;
construct   = Ident "(" argList? ")" ;                 (* prod/variant ctor; shares call syntax *)
argList     = arg ("," arg)* ;
arg         = (Ident ":")? expr ;                      (* Swift-style labels; ".." spread for update *)
placeholder = "$" | "$" Ident ;                        (* $a etc; see §3 *)
listLit     = "[" (expr ("," expr)*)? "]" ;
mapLit      = "[" ":" "]" | "[" mapEntry ("," mapEntry)* "]" ; (* [:] = empty map *)
mapEntry    = expr ":" expr ;                          (* the `:` distinguishes map from list *)

matchExpr   = "match" expr? "{" arm* "}" ;             (* subject optional: subjectless = cond table *)
arm         = pattern ("if" expr)? "=>" expr ;         (* guards; "_" is catch-all *)
pattern     = "_" | literal | Ident
            | Ident "(" patternList? ")"               (* constructor / destructure *)
            | "(" patternList? ")"                     (* tuple *)
            | pattern ("|" pattern)+ ;                 (* or-pattern *)

type        = Ident generics?                          (* parsed, NOT checked in v0 *)
            | type "->" type                           (* function type, right-assoc *)
            | "(" type ("," type)* ")" ;               (* tuple type *)
```

Note `if` appears only as a match-arm guard, never as a statement — consistent with "no if/else."

---

## 5a. Collections & sequences

Two eager collections + one lazy sequence. You tell eager from lazy **by the type**, and the type follows from how the value was produced.

- **`List<T>`** — finite, eager, in-memory, structural equality. The default. Literal `[1, 2, 3]`.
- **`Map<K, V>`** — eager. Literal `["host": 1, "port": 2]`, empty `[:]`. Indexing `m[k]` returns **`Maybe<V>`** (no null) — chains with `?`/`?.`.
- **`Set<T>`** — eager; **no literal** (`[1,2,3] |> toSet`). A set literal can't be told from list/map without stealing `{…}` (blocks), and sets are rare.
- **`Seq<T>`** — lazy, pull-based, possibly infinite. Produced by `iterate`/`repeat`/`forever`, ranges, or `list.lazy`. Materialize with `toList`/`toSet`/`toMap`.

**Ranges are `Seq<Int>` (lazy):** `0..n`, inclusive `0..=n`, open/infinite `n..`. The rule is clean — **literals (`[…]`) are eager, ranges/producers are lazy.** Lazy ranges give pipeline fusion for free (`0..1_000_000 |> map(f) |> first(...)` allocates nothing) and make infinite producers possible.

**Combinators are stdlib functions, not keywords** — zero grammar cost, cheap to rename, no parser impact. Defined on both `List` (eager → returns `List`) and `Seq` (lazy → returns `Seq`), dispatched on the receiver (Kotlin's model). Starter vocabulary:

| group | functions |
|---|---|
| transform | `map` `filter` `flatMap` `zip` |
| reduce/consume | `fold` `foldWhile` `each` `total` `count` `any` `all` |
| bound/search | `take` `drop` `takeWhile` `dropWhile` `first` `find` |
| produce (lazy) | `iterate` `repeat` `forever` |
| materialize | `toList` `toSet` `toMap`; `toSeq` / `.lazy` (List→Seq) |

`fold` takes an explicit init; `find(pred)` returns `Maybe<T>`. See §6a for how this vocabulary replaces loop syntax.

> **Keyword collision (decided):** `sum`/`prod` are **hard keywords** — reserved everywhere, not just at declaration position (the lexer already does this). So the sum *combinator* is named **`total`**, not `sum` (a list-product combinator is rare; no `prod` combinator). This is the Kotlin precedent: `when` is reserved, so Mockito uses `whenever`. Contextual keywords (reserving `sum`/`prod` only at item position) were rejected — they add parser ambiguity for little gain. A future `` `sum` `` backtick-escape could re-admit the identifier if ever needed, but not in v0.

---

## 6. v0 parser/interpreter scope

**Build first (the walking skeleton — enough to run the `average`/`report` sample with `span`/`emit` stubbed to `println!`):**
- Lexer for §1; Pratt parser for §2–§5; `insta` snapshot tests on the AST.
- Eval: `let` bindings + lexical scope, functions + closures, arithmetic/comparison/boolean (`and`/`or`/`not`), `prod`/`sum` construction + field access, `match` (incl. subjectless), lambdas + placeholders, pipes (incl. bare-reference stages), strings + interpolation.
- Collections: eager `List`/`Map` + literals (`[…]`, `["k": v]`, `[:]`), finite ranges, and the eager combinators (`map`/`filter`/`fold`/`each`/`find`/`toList`).
- **Dynamically typed:** parse type annotations, do **not** check them. Defers the entire type system.
- `Maybe`/`Result` as built-in sums; `?`/`?.` hardcoded for them. `map[k]` returns `Maybe`.
- `span`/`emit`/`use <-` present; `span` = a host fn that prints.

**Defer (each a later TDD increment, designed against the running language):**
- **Lazy `Seq`** + infinite producers (`iterate`/`repeat`/`forever`) + `takeWhile`/`foldWhile` + lazy ranges — the increment right after the eager skeleton; this is where the loop-replacement story fully lands (and the first place laziness/TCO-avoidance matters).
- `on`/`@`/`contract` methods + dynamic dispatch.
- Static types, generics bounds/variance, inference, exhaustiveness checking.
- Capabilities effect-checking (`uses` is parsed-then-ignored; caps are plain runtime values).
- Real telemetry (`Frame` protocol), the bytecode VM, the GC, modules/visibility.
- User-implementable `?` trait.

---

## 6a. Iteration — no loop keywords

There are **no loop statements.** Iteration is library combinators (§5a) over lists and lazy sequences; the only real loops live inside ~12 stdlib combinators (`each`/`takeWhile`/`forever`/…) implemented in the runtime. Every imperative construct maps to one:

| imperative | declarative |
|---|---|
| `for x in xs { … }` | `xs \|> each(\x -> { … })` |
| `while cond { … }` | `iterate(seed, step) \|> takeWhile(\s -> cond) \|> each(…)` |
| `break` (found it) | `find` / `first` / `takeWhile` |
| `continue` (skip) | `filter` (before `each`) |
| `loop { … }` (forever) | `forever(\-> { … })` |
| accumulate + early exit | `foldWhile(init, \acc x -> … => Done(r) \| Continue(s))` |

Examples that justified `while`:
```
forever(\-> { let msg = receive(inbox)  handle(msg) })          // event loop
iterate(n, \i -> i - 1) |> takeWhile(\i -> i > 0) |> each(\i -> emit("tick", i))
repeat(attempt) |> first(\r -> r.ok)                            // retry until success = "break"
```

`break`/`continue` aren't keywords — they're either combinator choices (`first`/`takeWhile`/`filter`) or values (`Done`/`Continue`). Control flow becomes data — on-brand for a match-everything language.

**TCO not required for common cases:** `each`/`takeWhile`/`forever` loop *internally* in the runtime, so event/condition loops cost no user stack. TCO matters only for hand-written recursion (the escape hatch), so it stays deferred. The enabling feature is **lazy `Seq`** (a thunk/iterator protocol) so infinite producers don't materialize.

> **The crazy north star (post-v0):** make iteration an *algebraic effect* (a `yield`ing generator with the consumer as handler), unifying it with the capability (`uses`) and telemetry effect machinery. Koka/OCaml-5 territory; noted, not built.

## 6b. Considered & rejected

- **Imperative loop keywords (`for`/`while`/`break`/`continue`/`loop`)** — rejected as too imperative for an immutable, expression-oriented language. Replaced by the combinator/lazy-`Seq` model above (§6a): loops are a library, not syntax. Cost accepted: lazy-sequence machinery in the runtime.
- **`use <- each(...)` for iteration** — iteration via the `use <-` mechanism (Gleam-style) was considered, but `use <-`'s "rest of the block is the body" semantics fits *spans/scoping* and mis-fits *loops* (you usually loop then continue). Iteration is the ordinary HOF `each` with an explicit arrow lambda (`each(x -> { … })`), whose braces delimit the body cleanly; `use <-` stays scoping-only.
- **Kotlin-style trailing-lambda sugar (`f(x) { … }`)** — excluded. It needs `{ … }` to read as a lambda literal, but here `{ }` is a *block* and lambdas are the arrow form (`x -> …`); adding it would resurrect the dropped brace-lambda and give two ways to pass a lambda. Its use cases are already covered: `use <-` for scoping/DSL/resource blocks (the `withLock { }` case), pipes for data flow, explicit arrow lambdas otherwise.
- **Dart-style cascade (`obj..m()..n()`)** — returns the receiver so you can fire a run of mutating/setter calls at one object. Rejected: it's a *mutation* idiom (builder configuration), which fights immutable-by-default; we cover the ground with functional update (`..p`), named-arg construction, and pipes. It would also overload `..` a third time (spread + range + cascade). `..` stays spread + range only.
- **`&&` / `||` / `!`** — replaced by `and`/`or`/`not` (keywords). Reason: keeps `|` meaning alternation *only* (sum variants / or-patterns / conditional-else), removing the `|` vs `||` near-collision, and reads better. Cost accepted: three more keywords; `!=` keeps its `!` (so `not x` but `x != y`, Python-style).

## 7. Resolved decisions
1. **Block comments nestable** — yes.
2. **Int literals: decimal only in v0** — hex/binary deferred (additive later). Bitwise `&`/`|` also deferred; since `|` already means alternation, bitwise ops — if ever needed — would be **library functions** (`bitAnd`/`bitOr`), never operators.
3. **Pipe inserts the left operand as the first argument** (Elixir-style) — confirmed. Load-bearing: stdlib is declared subject-first (`filter(list, pred)`, `map(list, f)`, `fold(list, init, f)`).
4. **Crate layout: single `lang` host crate to start**, split into lexer/parser/interp if it grows — detail for the `02-*` plan.
