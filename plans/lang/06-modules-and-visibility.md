# Stitch — modules & visibility (iteration 1)

_The last thing between Stitch and an organised standard library. Turns the one
flat global namespace into many named modules, adds a `pub` boundary, and pins
where `contract` coherence lives. **This file scopes iteration 1 only** — the
decisions below are first-cut, not the final module system._

## Decisions for iteration 1 (locked)

1. **A module is a file.** Each `.st` file is one module; its name is the file
   stem (case-sensitive). `use seq` resolves to `seq.st`; the stdlib ships as
   built-in modules `Seq` / `List` / `Str` (PascalCase stems, so `Seq.map` reads
   right). **No in-file `module { }` blocks this iteration** — one file, one
   module.
2. **Stdlib namespaces are modules, not types.** `Seq` is a *module* holding free
   functions; `Seq.map(xs, f)` is a module-path access. The combinators stay
   plain functions, just grouped. Pipe is unaffected: `xs |> Seq.map(f)`.
3. **`use` brings names into scope.** `use Seq` binds the module; access its
   exported members by path (`Seq.map`). `use Seq.{map, take}` binds the named
   members unqualified. The keyword is **`use`** — Rust's exact word, and apt
   ("bring names in *to use* them"). It overloads the existing `use <-` block
   sugar, but the two never share a position (imports are top-level, `use <-` only
   appears inside a block), so it parses cleanly; the through-line is `use` =
   "bring into scope" (a module's names, or a callback's binding). A deliberate,
   position-disambiguated bend of "one token, one job."
4. **`ext` is item-level only.** Top-level value items (functions, `prod`/`sum`,
   `const`) are **private by default**; the `ext` modifier exports them. A public
   type exposes its constructor and fields wholesale — **opaque types
   (field/constructor visibility) are deferred.** Mirrors the language's
   "exposure is the marked form" grain, the twin of `mut`/`uses` (mark the
   escalation, not the safe default).

   *Keyword decision (`ext`, after `pub`→`out`→`ext`):* a sigil (`+`/`*`) was
   rejected — it double-duties an arithmetic token, the one thing the language's
   "no token does two jobs" spine forbids. Public-by-default was rejected — it
   inverts the grain (exposure becomes the *unmarked* thing; the Java footgun).
   `ext` is a word modifier that sits uniformly beside `mut`/`free`/`uses`
   (`ext mut insert(x)`), reads "external / exported," and never misreads as an
   identifier. (`out` was the prior pick; `ext` won for reading the same in every
   slot, and pairs with `use` as Rust's `pub`/`use` do — unrelated words, each the
   best fit for its slot, not a forced symmetric pair like `in`/`out` or
   `push`/`pull`.) `contract`/`on` carry no `ext` (cross-module conformance/typing
   is deferred) — `ext contract` is a parse error, not a silent no-op.

## The testability seam — modules are a name→source map, not a directory

The kernel-core lesson carries over: the *core* is host-testable Rust; only the
edge touches the filesystem. So the loadable unit is a **module set** —
`HashMap<String, String>` (module name → source) — not a path on disk.

- `main.rs` / the CLI populates the set by reading `entry.st` + walking imports
  to sibling `.st` files in the entry directory, then hands the map to the runner.
- Tests populate the map **directly**, in-memory, no temp files. Every
  cross-module behaviour (path access, `pub` enforcement, coherence, cycles) is a
  pure `cargo test` with a literal `&[(name, src)]` fixture.
- Built-in stdlib modules (`Seq`/`List`/`Str`) are embedded source constants
  merged into the set before user modules, so they're always resolvable and the
  tests stay hermetic.

`run_program_source(src)` stays as the single-module convenience wrapper
(entry module named e.g. `main`, no imports) so the whole existing test corpus
is untouched.

## Resolution model — represent a module as a `Value`

The minimal-surgery insight: today closures already capture their defining
`Env`, and `.` is already field/method access. So:

- **Each module gets its own `Env`** whose globals = prelude + the module's own
  items + its imported names. A function defined in module A resolves its free
  names in A's namespace *for free* — closures already capture env.
- **A module is a first-class `Value::Module(name)`.** `import Seq` binds
  `Seq -> Value::Module("Seq")` into the importing module's globals. Then
  `Seq.map` is **field-access on a module value** — it reuses the existing `.`
  dispatch in `eval`: if the LHS evaluates to `Value::Module(m)`, look `map` up
  in module `m`'s *public* exports; else it's the existing field/method path.
  No new path-expression AST node, no new resolver — one new arm.
- **A module registry** maps name → that module's public exports
  (`HashMap<String, Value>`), built once during registration. Both path access
  (`Seq.map`) and select-import (`import Seq.{map}`) read it; both error on a
  private or missing member.

## Two-phase registration makes import cycles free

A genuine finding to surface (and the iteration's teaching beat): with **lazy
name resolution**, import cycles need *no* cycle-detection algorithm.

- **Phase 1 — declare.** For every module in the set, build its `Env` and
  register its own items (prelude + locals). No imports yet. (This is just the
  existing `register_items` per module.)
- **Phase 2 — link.** Now every module's exports exist, so process every
  module's `import`s: bind module values and copy select-imported names. A→B→A
  cycle resolves because by phase 2 both export tables are already populated.

So the only errors are **missing module** and **missing/private export** — not
"cyclic import." (Cycle detection would only be forced by *eager recursive
file-loading*; discovering the whole set first sidesteps it.) Document this as a
finding rather than building machinery we don't need.

## Coherence — where an `on` block may live

The orphan rule the design doc already gestures at, now enforceable because
"module" exists:

- `on Type { … }` (inherent methods) must live in **`Type`'s module**.
- `on Type : Contract { … }` (conformance) is allowed if **either `Type` or
  `Contract` is local** to the module (Rust's rule).
- Enforced at registration: error if neither is local. Dispatch itself is
  unchanged (still by runtime type name into the global method table) — this is
  purely a *write-site* rule that keeps behaviour findable with its type.

## Phasing (TDD increments, green between each)

1. ✅ **Module set + path access.** `eval_modules(modules, entry)` takes a module
   set (`Module { name, items }`); `Value::Module(Rc<ModuleHandle>)`; `M.member`
   resolves an export via the existing `.`-dispatch (a module arm in both
   `eval_field` and `eval_method_call`). Cross-module *call* works
   (`other.helper(x)`); each module evaluates in its own namespace (closures
   capture their module env), sharing one program-wide method/field-mut table +
   telemetry sink via `Env::sibling_module`. Two-phase registration: declare all
   module globals (seeded from a once-registered prelude base), then link sibling
   module values in. Privacy not enforced yet (everything exported); sibling
   modules auto-visible by name. Errors: unknown member, unknown entry module.
   *Deferred to a later refactor:* unify `eval_program` (single flat module) onto
   `eval_modules` to drop the duplicated natives/prelude bootstrap. *Noted gap:*
   top-level `let` consts parse but aren't registered as globals yet — orthogonal
   to modules, surfaced while testing path access.
2. ✅ **`ext` + privacy.** `ext` modifier parsed on `Func`/`Prod`/`Sum`/`Const`
   (an `Item.public` flag); items default-private. `collect_exports` now returns
   `(public map, private name-set)`; the module-access arms resolve only exports
   and distinguish a refused **private** member ("member `x` of module `m` is
   private") from a missing one. Privacy gates *cross-module* path access only —
   intra-module calls and the whole single-module (`eval_program`) corpus are
   unaffected, since `ext` only feeds the export table. Adding `Item.public`
   churned 23 parser AST snapshots (one `public:` field each), regenerated via
   `INSTA_UPDATE=always` after confirming the diffs were field-only. Deferred:
   `ext` on `contract`/`on` (rejected as a parse error for now); field/variant
   visibility (opaque types). (Keyword landed as `out` then renamed to `ext`.)
3. ✅ **`use` imports.** `Item::Use { module, names }` parsed at top level:
   `use M` (whole module) and `use M.{a, b}` (selection). The iteration-1 *auto*
   sibling-visibility is **removed** — a module is invisible until imported. Link
   step (`link_imports`): a whole-module import binds the `Value::Module` under
   its name; a selection binds each named export directly (reachable unqualified,
   pipes included). Errors: unknown module, missing/private selected member (reuses
   `access_error`). **Import cycles are free** — every module's export table is
   built before any `use` is linked, and names resolve lazily at call time, so
   `a`↔`b` mutual imports just work (proved by a cycle test). The 7 cross-module
   tests from increments 1–2 gained an explicit `use` (the finalized semantics).
4. **Built-in stdlib modules.** Re-home the prelude combinators into embedded
   `Seq` / `List` / `Str` module sources; `Seq.map` etc. resolve. Keep the most
   common names auto-in-scope via the prelude so everyday code (`xs |> map(f)`)
   is unbroken. (Exact split of "auto-prelude vs must-import" is a sub-decision
   for this step.)
5. **Coherence check.** Reject an `on` block whose target and contract are both
   foreign to the module.

## Deferred (explicitly out of iteration 1)

- **In-file `module { }` blocks** (nested namespaces in one file).
- **Opaque types** — `ext` on fields/constructors/variants; a public type with a
  private constructor. The expressive encapsulation case.
- **Re-export / `ext use`**, import aliasing (`use Seq as S`), glob import.
- **Module-path *types*** (`other.Shape` in a type annotation) — types are still
  parse-and-ignore in v0, so cross-module type references need nothing yet.
- **Filesystem niceties**: nested directories / package roots / a manifest.
  Iteration 1 is flat: imports resolve to sibling `.st` files of the entry.
- **Visibility on `contract` members** beyond the contract's own `pub`.

## Why this matters

It's the gate on the organised stdlib (the `List.`/`Seq.`/`Str.` reorg from
[04](04-standard-library.md), now unblocked since lazy `Seq` exists), and it's
where encapsulation and `contract` coherence finally have a boundary to be
defined against. The `pub`-marks-exposure choice keeps the language's grain
("the deviation from the default is the marked form") consistent from `mut`
through `uses` to visibility.
