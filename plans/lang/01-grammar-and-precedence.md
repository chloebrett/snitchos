# Lang — grammar & precedence spec (v0)

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
prod  sum  contract  on  let  mut  free  use  uses  match
and  or  not  for  in  while  break  continue  true  false
```
That's the whole set for v0. Notably absent: `fn`, `class`, `if`, `else`, `when`, `interface`, `null`, `loop`. `_` is a token (wildcard / catch-all), not a keyword. Boolean logic is the words `and`/`or`/`not` (not `&&`/`||`/`!`) — see operator table for why.

### Literals
- **Int:** `123`, `1_000` (underscores ignored). Hex `0x1F` _(confirm — defer if not needed)_.
- **Float:** `3.14`, `1_000.5`.
- **Bool:** `true`, `false`.
- **Str:** `"…"` with escapes `\n \t \" \\` and **interpolation** `\(expr)` — `"temp is \(avg)°C"`. (`\(` opens an embedded expression; `$` is NOT interpolation here, it stays reserved for placeholders.)

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

`|>` inserts its left operand as the **first argument** _(confirm — Elixir-style, not F#-style)_ of the call on its right:

- `LHS |> f(a, b)`  ≡  `f(LHS, a, b)`
- `LHS |> f`        ≡  `f(LHS)`

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
stmt        = constDecl | assignment | useExpr | forStmt | whileStmt
            | "break" | "continue" | exprStmt ;
assignment  = lvalue "=" expr ;                        (* lvalue: @x, name (mut only) *)
useExpr     = "use" (Ident "<-")? callExpr ;           (* scoping/callbacks only — NOT iteration *)
forStmt     = "for" Ident "in" expr block ;            (* over ranges or collections *)
whileStmt   = "while" expr block ;                     (* condition loop; `while true` = infinite *)

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
            | matchExpr | block | construct ;
construct   = Ident "(" argList? ")" ;                 (* prod/variant ctor; shares call syntax *)
argList     = arg ("," arg)* ;
arg         = (Ident ":")? expr ;                      (* Swift-style labels; ".." spread for update *)
placeholder = "$" | "$" Ident ;                        (* $a etc; see §3 *)

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

## 6. v0 parser/interpreter scope

**Build first (the walking skeleton — enough to run the `average`/`report` sample with `span`/`emit` stubbed to `println!`):**
- Lexer for §1; Pratt parser for §2–§5; `insta` snapshot tests on the AST.
- Eval: `let` bindings + lexical scope, functions + closures, arithmetic/comparison/boolean, `prod`/`sum` construction + field access, `match` (incl. subjectless), lambdas + placeholders, pipes, ranges, `for`/`while`/`break`/`continue`, strings + interpolation.
- **Dynamically typed:** parse type annotations, do **not** check them. Defers the entire type system.
- `Maybe`/`Result` as built-in sums; `?`/`?.` hardcoded for them.
- `span`/`emit`/`use <-` present; `span` = a host fn that prints.

**Defer (each a later TDD increment, designed against the running language):**
- `on`/`@`/`contract` methods + dynamic dispatch.
- Static types, generics bounds/variance, inference, exhaustiveness checking.
- Capabilities effect-checking (`uses` is parsed-then-ignored; caps are plain runtime values).
- Real telemetry (`Frame` protocol), the bytecode VM, the GC, modules/visibility.
- User-implementable `?` trait.

---

## 6a. Loops — model

- **Transformation** is HOF + pipes (`fold`/`map`/`filter`), not loops — the accumulate-into-a-variable `for` doesn't exist (it's `fold`).
- **`for x in iterable { }`** — bounded side-effecting iteration over ranges/collections. Brace-delimited (body ends at `}`, code may follow).
- **`while cond { }`** — condition/unbounded loops (event loops, retry). `while true` is the infinite loop; no separate `loop`.
- **`break` / `continue`** in both. Loops are **statements** (return unit); `match` remains the expression-valued brancher.
- TCO is **deferred** — real loops mean event loops don't require tail-recursion up front.

## 6b. Considered & rejected

- **`use <- each(...)` for iteration** — iteration via the `use <-` mechanism (Gleam-style) was considered, but `use <-`'s "rest of the block is the body" semantics fits *spans/scoping* and mis-fits *loops* (you usually loop then continue). So iteration gets brace-delimited `for`; `use <-` stays scoping-only.
- **Dart-style cascade (`obj..m()..n()`)** — returns the receiver so you can fire a run of mutating/setter calls at one object. Rejected: it's a *mutation* idiom (builder configuration), which fights immutable-by-default; we cover the ground with functional update (`..p`), named-arg construction, and pipes. It would also overload `..` a third time (spread + range + cascade). `..` stays spread + range only.
- **`&&` / `||` / `!`** — replaced by `and`/`or`/`not` (keywords). Reason: keeps `|` meaning alternation *only* (sum variants / or-patterns / conditional-else), removing the `|` vs `||` near-collision, and reads better. Cost accepted: three more keywords; `!=` keeps its `!` (so `not x` but `x != y`, Python-style).

## 7. Open decisions to confirm before parser work
1. Block comments nestable? (assumed yes)
2. Hex/other int literals in v0? (assumed no)
3. Pipe = first-arg insertion? (assumed yes, Elixir-style)
4. Crate layout: single `lang` host crate to start, split into `lang-lexer`/`lang-parser`/`lang-interp` later — or split now? (next plan doc)
