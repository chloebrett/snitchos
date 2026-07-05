# Redesign review: Stitch tokenizer, parser & interpreter

*Outcome of the [redesign-from-scratch](../redesign-from-scratch.md) exercise —
2026-07-05. Covers `lexer.rs`, `parser.rs`, `interp.rs` + `ast.rs`/`value.rs`/`env.rs`.
Run as three independent forks (lexer, parser, interp), then synthesized.*

**Central framing:** a *correct, honestly-tested tree-walker for a genuinely rich
surface language* (generics, contracts, `on`-blocks, patterns, interpolation,
placeholders, ranges, effect clauses; 504 passing) — built as the tree-walker's
**private pipeline**, right as that pipeline is about to become shared
infrastructure. Three consumers are arriving that the AST was never shaped for
(stim edits it, a mutation tester rewrites it, a bytecode VM will lower it), and
the runtime is about to grow two things it has no home for (an effect system, a
GC). This review was run as three independent passes (lexer, parser, interp); the
**triangulation is the signal** — each pass independently surfaced the same two
missing spines (source spans; an evaluator *context*), which is why they head the
list.

Ranked changes:

1. **Source spans, threaded lexer → AST → runtime fault — the diagnostic spine.**
   Smell: `Token` carries only *what*, never *where* (`lexer.rs:22`); `ParseError`
   is a bare `String` whose own comment admits "source positions are a later
   increment" (`parser.rs:18`), so errors read `expected …, found {got:?}`
   (`parser.rs:600`); AST nodes are position-free; `RuntimeError::Fault(String)`
   (`value.rs:343`) can't say where either; and malformed literals *silently*
   become `0` (`lex_number` → `parse().unwrap_or(0)`, `lexer.rs:174`). Redesign:
   byte-offset `Span` on every token, threaded into every node (a `Spanned<T>` or a
   node-id→span table) and into a single spanned `Diagnostic` type shared by all
   three stages. Buys: the precondition for essentially every downstream tool —
   stim's error squiggles + highlight, the mutation tester's "surviving mutant at
   line N", VM stack traces, and the effect checker saying *where* an undeclared
   `uses` was exercised. Tension: an observability-first project whose own compiler
   can't point at the offending character is off-thesis; and this is the purest
   "cheap at the time, load-bearing forever" omission — skipped only because the
   tree-walker alone doesn't strictly need it. Ranked #1 because every other item's
   payoff (better errors, tool integration) is capped by it.

2. **A reified evaluator with fuel + a trampoline + a call/effect stack.** Smell:
   evaluation is free functions threading `&Env` (`eval_*`, `interp.rs:401,518,806`)
   — there is *nowhere* to hang a step budget, a depth guard, or a handler stack; a
   closure call just re-enters `eval` on the Rust stack (`apply_values` →
   `eval(&closure.body)`, `interp.rs:806`), so unbounded Stitch recursion overflows
   the *Rust* stack into a process abort, not a catchable fault. Redesign: an
   `Interp`/`Vm` object carrying `fuel: u64` (per-step decrement), a call-frame
   stack, and a self-tail-call trampoline. Buys: **the only item blocking *now***,
   not just later — the mutation tester's fuel cap *requires* an eval-step budget to
   bound non-terminating mutants, and any Stitch-hosted loop needs bounded stack;
   it's also the home items 1 (fault call-frames), 5 (handler stack), and 6 (VM
   context) all need. Tension: "the AST is the program, evaluation is a pure
   function" (`interp.rs:1`) is elegant but leaves nowhere for an execution context
   to live.

3. **A faithful surface AST + one lowering pass — stop fusing parse with
   desugar.** Smell: desugaring is smeared across two phases and destroys the
   surface form — subjectless `match` is rewritten to nested `Expr::If` *at parse
   time* (`parser.rs:1207`, unrecoverable), placeholders/operator-refs become
   synthesized lambdas in `parse_arg` (`parser.rs:582`), constructor-vs-binding is
   decided by capitalization in the parser (`parser.rs:1273`), yet `use <-` is
   lowered *at eval time* instead (`ast.rs:274`, `interp.rs:1238`). And the runtime
   value embeds the AST — `Value::Closure` holds `body: Expr` (`value.rs:198`) — so
   there is no IR seam a bytecode backend could slot behind. Redesign: parser emits
   a faithful, round-trippable surface AST (keep `Placeholder`, a real
   `SubjectlessMatch`, an `OperatorRef`); a *separate* lowering pass produces a
   smaller core IR that both the tree-walker and a future VM consume; closures hold
   a code-ref + upvalues, not an `Expr`. Buys: the mutation tester + any
   formatter/LSP can round-trip source; desugaring lives in one pass, not split
   parse/eval; the documented bytecode-VM (`language-design.md`) becomes an additive
   backend, not a value-type rewrite. Tension: fusing parse+desugar was one fewer
   hand-written pass and the tree-walker eats it directly — the ceiling shows only
   now that the AST must serve three consumers, not one.

4. **Intern identifiers to a `Symbol` at lex time.** Smell: `Token::Ident(String)`
   (`lexer.rs:28`), `Var(String)`, `Param{name:String}`; the parser clones tokens
   to end borrows (`self.bump().clone()`, `parser.rs:1325,606`); every identifier is
   a heap alloc + repeated clone — feeding both the "leaks per-run" churn and the
   pervasive `.clone()` the project's own CLAUDE.md flags. Redesign: intern to a
   `Copy` `Symbol(u32)` at the lexer boundary — **the project already has an intern
   table in `kernel-core`**, so the mechanism is in-house; equality/lookup becomes
   integer compare. Buys: much less allocator pressure on the metal (`no_std`+alloc,
   where allocation is a real cost), faster resolution in both the tree-walker and
   the future VM. Tension: `String` was the obvious v0 move; interning is what the
   VM/GC genuinely wants and the tree-walker only limps without.

5. **Authority as a runtime handler-stack of capability *values*, not a string
   set.** Smell: `uses: Vec<String>` on the AST (`ast.rs:32`) → `authority:
   Rc<BTreeSet<String>>` on the env (`env.rs:59`), gated by `has_authority(&str)`
   (`env.rs:144`) and swapped in at the call boundary (`interp.rs:801`). A name-set
   can *gate* but cannot *carry* a capability value, *attenuate* it, or *interpose*
   on it; `use <-` rebuilds the rest-of-block into a fresh closure per call
   (`interp.rs:1238`) but reifies no resumable continuation. Redesign: `uses` becomes
   a structured, spanned effect-row; authority becomes a handler stack of
   unforgeable, received-only capability values — Q5's "handlers as membranes."
   Buys: the language-level half of the caps thesis, and the *soft mode* of stim's
   modes-as-authority (normal mode's handler set simply lacks the write effect). The
   current representation cannot grow into it. Tension: strings were enough to gate
   `emit`/`span` in v0; the thesis makes `uses` central, so they age badly at
   exactly the load-bearing seam.

6. **Values behind a GC handle, not `Rc<RefCell>` cycles.** Smell: every heavy
   `Value` variant is `Rc` (`value.rs:17`) and `Env` scopes hold
   `Rc<RefCell<Value>>` with capture-by-reference (`env.rs:74`); a `mut` binding
   holding a closure that captured its own scope is a reference cycle `Rc` can't
   collect — almost certainly the "leaks per-run." Redesign: values behind a GC
   handle/arena from the start, so the roadmap's generational collector is additive,
   not a rewrite. Tension: `Rc` was free and correct for a bring-up and is
   structurally opposed to the GC the roadmap wants — you cannot bolt tracing onto
   `Rc` woven through every clone and env link.

7. **Retire the flat native table + hand-maintained module-view map; the
   front-end's local hacks.** Smell: `NATIVES` is a flat `&[NativeFn]`
   (`interp.rs:86`) with `str`-prefixed names dodged into a hand-written
   `BUILTIN_MODULE_SPECS` map (`("upper","strUpper")`, `interp.rs:316`) — adding
   `Str.slice` this session touched *three* places, the same registry-sprawl the
   userspace review flagged. Alongside: interpolations are re-lexed *and* re-parsed
   from a raw `String` (`StrPart::Expr(String)`, `lexer.rs:17`; `parse(&raw)`,
   `parser.rs:166`) — double work, spans die inside `{…}`; and `parens_then_arrow`
   does an unbounded forward scan to disambiguate lambda-params from grouping
   (`parser.rs:386`, O(n²) on nesting). Redesign: natives declared *in* their module
   namespace; lex interpolations once into nested token groups; replace the paren
   scan with checkpoint/backtrack. Buys: less sprawl, interpolation spans, no O(n²)
   pathology. Tension: each was the cheapest local thing that worked; collectively
   they're the front-end's fragility surface (the `maximal_munch_call_paren` gotcha
   is the same family).

**Keep — the bones are good, enrich don't rewrite:**
- The **AST as a plain `#[derive(Debug, PartialEq, Clone)]` algebraic enum** — great
  for exact-tree tests and *already a good mutation-tester target*; the problem is
  missing spans + baked-in desugar, not the enum style.
- The **Pratt core** with an explicit `(l_bp, r_bp)` table + non-associative-chain
  rejection (`parser.rs:243,270`) and the layered `atom < prefix < postfix < expr`
  structure (`parser.rs:487`) — textbook and correct.
- **`eval` as one honest `match` on `Expr`** (`interp.rs:401`) — the *shape* is
  right; item 2 is the missing context *around* it, not the dispatch.
- The **effect seams**: `Telemetry` + `Platform` as `Rc<dyn>` on the env
  (`env.rs:48`) — swappable recording/host/on-target without the interpreter
  knowing, and exactly the seam stim rides.
- **Immutable env chain + write-once globals** (`OnceCell` letrec, `interp.rs:206`)
  — mutual recursion and import cycles fall out for free.
- **`..spread` functional update + structural data equality** (`value.rs:331`) — the
  immutable-value model that makes stim's buffer natural.
- **Lazy `Seq` with memoized force** (`value.rs:93`) and **strict, no-coercion
  typing** (`ops.rs:1`, a deliberate static-types preview).
- Lexer craft: per-kind helper dispatch, maximal-munch `eat()`, context-sensitive
  number lexing so `0..n` ranges survive (`lexer.rs:163`), nestable block comments,
  `{{`/`}}` escaping, `_` digit separators, unicode string content with ASCII idents.

**One-thing pick:** #2 (the reified evaluator with fuel + trampoline + call/effect
stack). #1 is the higher *leverage* long-term, but #2 is the one **blocking the
very next things you're about to build** — the mutation tester's fuel cap and any
Stitch-hosted loop both require it *today* — and it's the home the effect system
(5), spanned faults (1), and the eventual VM (3, 6) all need. If you do one thing
before stim proper, do this.

**Caveat:** every item here is visible only because the first pass *works* — a
correct, honestly-tested tree-walker over a rich surface language (504 green) is
exactly what lets you see that the missing pieces are *spans* and an *evaluator
context*, not the parsing or the evaluation logic. This is the second pass, now
that the AST is about to stop being the tree-walker's private input and become a
shared IR for three new consumers.
