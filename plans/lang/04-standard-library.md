# Stitch — standard library architecture

_Decision record. How the stdlib is structured and grown. Status: **prelude mechanism shipped**; full stdlib deferred behind modules + contracts + lazy `Seq`._

## The split: native core + Stitch-source prelude

The stdlib is **hybrid** (the model Rust/Python/etc. converge on):

- **Native core** (Rust, in `interp.rs`) — only what can't be expressed in Stitch: host I/O (`emit`/`span`), the list-touching primitives (`map`/`filter`/`fold`/`join`), and the built-in `Maybe`/`Result` constructors. `fold` is the bootstrap primitive — most list combinators derive from it.
- **Stitch-source prelude** (`stitch/src/prelude.st`, `include_str!`) — everything derivable on top, written *in Stitch* (dogfooding). Loaded into every program's globals **before** user code; user definitions shadow it. Currently: `count`, `total`, `any`, `all`, `contains`, `each`, `find`, and Maybe helpers (`isSome`/`isNone`/`unwrapOr`/`mapMaybe`/`andThen`).

Loading: `eval_program_with_telemetry` registers natives + built-in types, then `register_items(parse_program(PRELUDE))`, then the user's items, into one shared globals table.

## Why not "ship a stdlib" yet

A real, organized, versioned stdlib is blocked on prerequisites:

- **Modules** — no namespacing, so the prelude is **flat** (`count`, not `List.count`). A real stdlib needs `List.`/`Map.`/`Str.` modules first.
- **Contracts** — the type-class layer (`Show`/`Eq`/`Ord`/`Try`) needs dynamic dispatch (learning track). Concrete combinators are fine; the generic/trait API waits. (See [03-extensible-short-circuit.md](03-extensible-short-circuit.md) for `?`→`Try`.)
- **Lazy `Seq`** — the design's full combinator vocabulary (the loop-replacement) is half-built without it; the prelude is eager-only for now.
- **Static types** — documented signatures (`map: (List<A>, A -> B) -> List<B>`) need the type system.

So: grow the prelude incrementally now; revisit "organized stdlib" when the above land.

## Gotcha learned writing the prelude

`each` cannot be `fold(xs, (), (_, x) -> { f(x)  () })` — `f(x) ()` parses as a **call of `f(x)`'s result** (maximal-munch: `()` is a postfix call, not a new statement). Written as a bare `fold(xs, (), (_, x) -> f(x))` instead. General rule for hand-written Stitch: **don't follow a call with `(`** unless you mean to call its result. See [[stitch-maximal-munch-call-paren]].
