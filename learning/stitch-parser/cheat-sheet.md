# Stitch parser — cheat sheet

Fast-lookup reference, built as we go. Assumes you've done the sessions.

## Front-end pipeline

```
source text → [lexer] → Tokens → [parser] → AST → [evaluator] → Value
```
- **Parser builds the AST; evaluator consumes it.** The AST is the contract between the two halves.

## S1 — Lexer

- **Maximal munch (longest match):** consume the longest valid token at each step → `->` is one token, not `-` then `>`. Owned by the *lexer*, not the parser, because whitespace is discarded — the parser couldn't reconstruct `->` vs `- >`.
- **Whitespace fully discarded**, no `Newline` token. Multi-line list literals work for free; no ASI continuation rules.
- **Int literals carry a parsed `i64`**, not text.
- **String interpolation is re-entrant:** lexer captures `{...}` contents as *raw text*; the front-end re-runs `parse(&raw)` on it to produce the inner `Expr`. Nesting (`"a {f("b {y}")}"`) recurses.

## S2 — The AST (`ast.rs`)

Four families:

| Family | Enum | Scope |
|---|---|---|
| Declarations | `Item` | top-level **only** |
| Statements | `Stmt` | inside a block |
| Expressions | `Expr` | almost everywhere (incl. function bodies, const values) |
| Patterns | `Pattern` | left of `=>` in a match arm |

- **Expressions are the workhorse.** `Item::Func.body` and `Item::Const.value` are `Expr`. Only `Item` is file-scope-exclusive.
- **Conditionals & match are expressions**, not statements → usable in binding position: `x = (y > 5 => "big" | "small")`.

### Dispatch-relevant shapes (the bridge to S7)

- **No `MethodCall` node.** `obj.m()` parses as:
  ```rust
  Call { callee: Field { object: Var("obj"), name: "m" }, args: [] }
  ```
  Identical `Field` shape to a plain field read `obj.x`; *meaning* decided at eval time.
- `Expr::SelfRef` = the receiver `@`.
- `Item::On { target, contract: Option<Type>, methods }` — `on T {}` (inherent) vs `on T : C {}` (conformance).
- `Item::Contract { name, generics, methods }`.
- `Method { name, modifier, params, ret, body: Option<Expr> }`:
  - `body == None` → abstract **contract signature**. `Some` → concrete (`on`) or **default** (`contract`) method.
  - `MethodModifier`: `Instance` (immutable `@`) · `Mut` (may mutate `@`) · `Free` (no receiver — using `@` is a bug).

### Live gaps (what dispatch must fix)

- `interp.rs::register_items` has a trailing `_ => {}` that **silently drops `Item::On` / `Item::Contract`** — methods aren't collected at all yet. Step one.
- `eval_field` errors `"X has no field m"` — no method-lookup fallback exists.
- **S7 fork (open):** intercept `Call` when callee is a `Field` (before evaluating the callee) vs. invent a "bound method" `Value`.

## S3 — Pratt / precedence climbing (`parser.rs::parse_expr`)

**Idea:** one loop + one number per operator replaces the tower of `parse_or → parse_and → …` functions. Each operator has a **binding-power pair** `(l_bp, r_bp)`. Higher = binds tighter.

```rust
fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, _> {
    let mut left = self.parse_prefix()?;          // first atom
    while let Some(op) = infix_op(self.peek()) {
        let (l_bp, r_bp) = binding_power(op);
        if l_bp < min_bp { break; }               // too weak → give operand back to caller
        self.bump();
        let right = self.parse_expr(r_bp)?;        // recurse with the right power
        left = Expr::Binary { op, left, right };
    }
    Ok(left)
}
```

Binding-power table (tightest at bottom):

| Op | `(l, r)` |
|---|---|
| `or` | (1, 2) |
| `and` | (3, 4) |
| `== != < <= > >=` | (5, 6) |
| `\|>` pipe | (7, 8) |
| `..` `..=` range | (9, 10) |
| `+ -` | (11, 12) |
| `* / %` | (13, 14) |

**The one inequality that matters:** next operator's `l_bp` vs the current frame's `min_bp` (which is the *enclosing* operator's `r_bp`). `l_bp < min_bp` → break (operand belongs to the looser outer context); else recurse (tighter op steals the operand).

**Associativity = pair asymmetry, not a grammar rule:**
- `(l, l+1)` (right > left, e.g. `+` = (11,12)) → **left-assoc**. `8-2-1` = `(8-2)-1`.
- `(l, l-1)` → **right-assoc**. (Lambdas get right-assoc *structurally* instead: body parsed with `parse_expr(0)`.)

**Non-associativity** (`a < b < c` rejected): the `(l,r)` numbers can't forbid chaining (they'd just left-associate). Extra check: `is_non_assoc(op)` (consults operator *identity*) **and** the next operator is same-level (`binding_power(next).0 == l_bp`). Parse-time guard standing in for the absent type checker (`a<b : Bool`, comparing it to `c` is the JS `false==0` footgun).

**Conditional `=>`** binds looser than every binary op → handled *outside* the loop, gated on `min_bp == 0`; branches parsed at `min_bp = 1` so nested `=>` must be parenthesised.

**Layering:** `parse_atom` (tightest) < `parse_prefix` (unary `-`/`not`, open-from ranges) < `parse_expr` (infix loop). Lambda lookahead (`at_lambda`) short-circuits at the top of `parse_expr`.

**Equivalent forms:** recursive Pratt ≡ explicit two-stack shunting-yard. recursion depth ↔ operator-stack height; `min_bp` param ↔ top-of-stack precedence; `break` ↔ stop-popping. Recursive = less bookkeeping + composes with recursive descent; explicit stack = when you can't recurse (overflow / VM).

## S4 — Lookahead & the tricky cases

**Lookahead vs backtracking:**
- **Lookahead** — peek N tokens to *choose a rule*, then parse it once, consuming forward. Never un-consume. Stitch is lookahead.
- **Backtracking** — speculatively parse, on failure *rewind the cursor* and try another rule. Can re-parse tokens. Stitch never does this.

**Three ambiguities, all resolved by lookahead:**

1. **lambda vs tuple** — `(a,b) -> …` vs `(a,b)`. `at_lambda` → `parens_then_arrow`: scan to the **matching** `)` (depth counter handles nested parens like `(x,(y,z)) -> …`), check if `->` follows. Iterates `self.tokens`, **never bumps `pos`** → pure lookahead. Cost = O(distance to matching paren), the price of never rewinding.

2. **placeholder → lambda** (`collect_placeholders` + `parse_arg`) — `$` desugars to a lambda **per call argument** (the boundary). Traversal **stops at `Expr::Lambda`** so an inner explicit lambda keeps its own `$`: `map(xs, x -> filter(x, $.ok))` → `$.ok` is filter's. Nesting works free because inner calls parse bottom-up and seal their `$` into a `Lambda` before the outer traversal arrives.
   - **Placeholder semantics = position-by-letter (rule #2):** the letter **is** the index (`$a`=0, `$b`=1…). Arity = highest letter referenced; **unreferenced lower slots → `_` holes**. So `$b` alone ⇒ `(_, $b) -> $b` — selects the 2nd arg. (`f($a + $c)` ⇒ `($a, _, $c) -> …`.) Cost: mnemonic names are out (`$x` = "arg 24"). Use `$a`/`$b`/`$c` contiguously. Impl: `positional_params(&BTreeSet<String>)` in `parser.rs`.

3. **guard `=>` collision** — `Some(x) if x > 0 => body`. Guard parsed at **`parse_expr(1)`**, not `(0)`. The inline conditional `cond => t | e` is only handled at `min_bp == 0` (bottom of `parse_expr`), so starting the guard at 1 structurally refuses to swallow the arm's `=>` separator. Same trick in subjectless-match arm conditions.

**`parse_postfix`** runs the postfix loop *above* `parse_atom`: `(` → call, `.` → `Field`, `?.` → `SafeField`, `?` → `Try`, `[` → `Index`. Left-to-right chaining, so `a.b().c` nests correctly.

## S5 — Declarations (`parser.rs::parse_item`)

`parse_item` dispatches on the leading token → an `Item`:

| Source | Item | Keyword? |
|---|---|---|
| `prod Name(fields)` | `Prod` | `prod` |
| `sum Name = v \| …` | `Sum` | `sum` |
| `contract Name { sigs }` | `Contract` | `contract` |
| `on Type { … }` / `on Type : C { … }` | `On` | `on` |
| `let name = v` | `Const` | `let` |
| `name(params) = body` | `Func` | **none** (`Ident`) |

- **Function has no keyword** → matched by `Token::Ident(_)`. Works because **top level is declarations-only** (no bare expressions — `Item` is file-scope-exclusive, S2). So `parse_func` *commits* on an ident: no lookahead/backtrack needed.

**`contract` vs `on` — same `parse_method`, one flag:**
- `parse_contract` → `parse_method(require_body=false)` → body optional.
- `parse_on` → `parse_method(require_body=true)` → body mandatory.
- `Method.body: Option<Expr>`: `None` = abstract contract signature; `Some` in a contract = **default method**; `Some` in an `on` = the implementation. Body-less method in an `on` = parse error (nothing to dispatch to).

**Two `Option`s — don't conflate (both dispatch-central):**
- `Item::On.contract: Option<Type>` — the `: C` **conformance** clause. `None` = inherent methods; `Some(C)` = declares the type conforms to contract C.
- `Method.body: Option<Expr>` — presence of an implementation.

### Runtime dispatch algorithm (the S7 target — derived in S5)

For `x.foo()`:
1. value `x` → its `type_name` (e.g. `Celsius`).
2. find all `On` items with `target == type_name`; scan their `methods` for name `foo` → found ⇒ run that `body`.
3. not found ⇒ collect the contracts named by those blocks' `contract: Some(C)` clauses (a type may have several `on` blocks); look for a **default** `foo` body there.
4. else ⇒ "no method `foo` on `Celsius`".

**Key architectural cut:** conformance checking (`: C` ⇒ does the type implement all of C? orphan/coherence) is a **static/compiler** concern. The dynamic tree-walker consults a `contract` **only** for default-method fallback (step 3) — otherwise contracts are ignored at eval time. Contracts out of scope = any the type never declared `: C` for (borrowing their defaults would be a coherence violation).

**Current gap:** `interp.rs::register_items` drops `Item::On`/`Item::Contract` (`_ => {}`); step 1 of S7 is collecting them into a method registry keyed by `type_name`.
