# Corpus MVP — candidate record

Raw output from [corpus-mvp-spike.md](../corpus-mvp-spike.md). One `.md` record
per candidate; `.st` alongside where a program was emitted.

**Verdicts below are checked, not estimated.** Sweep the directory with:

```
cargo run -p stitch --bin check -- plans/corpus-candidates/*.st
```

| # | Model | Prompt | Thinking | Verdict |
|---|---|---|---|---|
| [001](001.md) | qwen3-vl-4b | v1 | — | `parse` — repetition collapse ([001.st](001.st), partial) |
| [002](002.md) | qwen3.5-4b | v1 | on | none emitted — deadlocked on `loyly` |
| [003](003.md) | qwen3.5-4b | v1 | on | `parse` — unexpected `&` ([003.st](003.st)) |
| [004](004.md) | qwen3.5-4b | v2 | on | none emitted — interrupted mid line-count |
| [005](005.md) | qwen3.5-4b | v3 | on | `tests` — 1 passed, "cedar allows disjoint" failed ([005.st](005.st)) |
| [006](006.md) | qwen3.5-4b | v4 | on | `tests` — bare `removeAt`; **patched, it passes the whole gate while still wrong** ([006.st](006.st)) |
| [007](007.md) | qwen3.5-4b | v5 | on | `tests` — **type-checked clean** despite a signature mismatch ([007.st](007.st)) |
| [008](008.md) | qwen3.5-9b | v5 | **off** | `parse` — `let (b1, b2) = pair` ([008.st](008.st)) |
| 009 | qwen3.5-9b | v5 + repair | off | self-repair of 008: 2 of ~9 errors fixed, both syntactic |
| [011](011.md) | qwen3.5-27b | v5 | on | **`ok` — 4 tests passed** ([011.st](011.st)) — first accepted |

*(010 is a findings entry, not a candidate — the checker landing.)*

## What the sweep established

- **The type rung caught nothing.** Parse caught 003 and 008; tests caught 005,
  006 and 007; the type checker caught none of them. Stitch's typing is gradual
  and advisory, so **requiring a `test` block in every program is load-bearing** —
  without them, three of these would have passed.
- **006 passes the entire gate while being wrong**, once its one name error is
  fixed. Code and tests written from the same misunderstanding agree with each
  other. This is the concrete case for scoring suites by mutants killed rather
  than by passing.
- **Two language facts the candidates found**: `let (b1, b2) = pair` does not
  parse (tuple patterns work in `match`, not in a `let`); bare `removeAt` does not
  resolve — it is `List.removeAt`.

Why keep the failures: model-produced broken code plus its diagnostic is the
scarcest input the RL branch has — real confusion, which reverse-corruption
cannot synthesise. See [kvetch-rl-design.md](../../docs/kvetch-rl-design.md) §5.
