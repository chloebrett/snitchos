# Corpus MVP — candidate record

Raw output from [corpus-mvp-spike.md](../corpus-mvp-spike.md). One `.md` record
per candidate; `.st` alongside where a program was emitted, so the S4 checker can
be run over the whole directory at once.

**Verdicts in the findings are unverified eyeball estimates.** Nothing here has
been through `parse_program → lower_items_to_core → check_program`. Running the
checker over these seven is the first job — it is retroactive, and it may
overturn several findings.

| # | Prompt | Program | Expected verdict |
|---|---|---|---|
| [001](001.md) | v1 | [partial](001.st) | parse ✗ — repetition collapse |
| [002](002.md) | v1 | none | n/a — deadlocked |
| [003](003.md) | v1 | [yes](003.st) | parse ✗ |
| [004](004.md) | v2 | none | n/a — interrupted |
| [005](005.md) | v3 | [yes](005.st) | parse ✓ · type ✓ · **tests ✗** |
| [006](006.md) | v4 | [yes](006.st) | parse ✓ · type ? · logically wrong, tests pass |
| [007](007.md) | v5 | [yes](007.st) | parse ✓ · **type ✗** |

Why keep the failures: model-produced broken code plus its diagnostic is the
scarcest input the RL branch has — real confusion, which reverse-corruption
cannot synthesise. See [kvetch-rl-design.md](../../docs/kvetch-rl-design.md) §5.
