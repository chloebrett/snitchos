# Stitch examples corpus — findings

Running notes from writing [stitch-examples-corpus.md](stitch-examples-corpus.md)'s
30 programs. One entry per program once it lands (or as it's in progress, if
something is worth capturing before the file is finished). Optimized for
"what would I want to know before writing the next one" — language
limitations, awkward constructs, stdlib/prelude gaps, checker surprises.
Fixed bugs and shipped workarounds belong in `.claude/CLAUDE.md` or
`docs/language-design.md` once they're load-bearing; this doc is the lab
notebook they get promoted from.

Gate: `stitch/tests/examples.rs` — parses + type-checks every file under
`examples/stitch/`, and asserts every `test` item in every file passes. Run
with `cargo nextest run -p stitch --test examples`.

---

## Template for each entry

```
### N. name.st

- Lines: NNN. Tests: N (N pass).
- What it exercises: ...
- Friction / limitations hit: ...
- Anything surprising: ...
```
