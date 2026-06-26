# Stitch — the organised standard library

_Now that modules exist ([06](06-modules-and-visibility.md)), the flat handful of
combinators grows into namespaced vocabulary. This is the payoff of the module
work: somewhere to put the dot._

## The split (settled in 06, increment 4)

- **Polymorphic core stays flat / auto-prelude** — `map`/`filter`/`fold`/`take`/
  `takeWhile`/`foldWhile`/`toList` and the prelude helpers. One name over `List`
  **and** `Seq`, never split into `List.map`/`Seq.map`. New *polymorphic*
  combinators (reverse, sortBy, zip, flatMap, drop…) join this flat core.
- **Namespaced modules hold the type-specific vocabulary** — `Seq` (lazy
  producers), `Str` (string ops). `List` waits until it has genuinely
  list-specific members.

Built-in modules are Rust-assembled views (`interp::BUILTIN_MODULE_SPECS`) onto
native functions; growing a module = adding natives + listing them in its spec.

## Iteration A — `Str` ✅ SHIPPED

Strings only had `join`, interpolation, and `+`. Make them usable. All native
(string manipulation is host primitive), all free functions reached by path or
pipe (`name |> Str.trim |> Str.upper`) — methods-on-primitives isn't a language
feature yet.

| function | meaning |
|---|---|
| `Str.length(s) -> Int` | char count |
| `Str.upper(s)` / `Str.lower(s) -> Str` | case |
| `Str.trim(s) -> Str` | strip surrounding whitespace |
| `Str.contains(s, sub) -> Bool` | substring test |
| `Str.startsWith(s, prefix) -> Bool` | prefix test |
| `Str.split(s, sep) -> List<Str>` | split on a separator |
| `Str.replace(s, from, to) -> Str` | substitute all |

The payoff demo: `Str.contains` (substring) coexisting with the flat `contains`
(element membership) — the exact name-clash namespaces exist to resolve.

TDD increments, green between each: `upper`/`lower`, `length`, `trim`,
`contains`/`startsWith`, `split`, `replace`. All eight shipped + verified through
a real piped program (`s |> Str.trim |> Str.lower |> Str.replace(",", "") |> Str.split(" ")`).

**Mechanism note (the `contains` collision):** `Str.contains` (substring) would
have clashed with the prelude's flat `contains` (element membership) — the
prelude shadowed the native in the flat namespace, so the view grabbed the wrong
one. Fixed by making `BUILTIN_MODULE_SPECS` members `(exported, source)` pairs:
string ops are `str`-prefixed natives (`strContains`, `strUpper`, …) exposed
under clean names in `Str`, keeping generic names out of the flat namespace and
letting `Str.contains` source `strContains`. The clash *was* the payoff demo —
two `contains`, one namespaced, both correct.

## Deferred follow-ons

- **Flat polymorphic combinators** — `reverse`, `sortBy`/`sort`, `zip`,
  `flatMap`, `enumerate`, `drop`/`dropWhile`, `first`/`last`, `min`/`max`.
  Written in Stitch where derivable from `fold`; native where they need new
  primitives (cons/concat/index).
- **More `Seq` producers** — `cycle`, `unfold`.
- **`Str` extras** — `endsWith`, `chars`, `words`, `lines`, `padLeft`.
- **Embedded-source stdlib modules** — when a stdlib fn is written in Stitch
  rather than assembled from natives in Rust.
- **Methods on primitives** (`"hi".upper()`) — a language feature, not a stdlib one.
