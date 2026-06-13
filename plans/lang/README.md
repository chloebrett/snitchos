# plans/lang — Stitch implementation track

Implementation plans for **Stitch**, the SnitchOS side-project language (`.st`). High-level design lives in [docs/language-design.md](../../docs/language-design.md); this directory is the build-facing track (specs, milestones, decisions).

Convention: numbered files in implementation order.

- [01-grammar-and-precedence.md](01-grammar-and-precedence.md) — tokens, precedence table, the placeholder-lambda + pipe rules, v0 parser scope. **The prerequisite for any parser code.**
- [samples.st](samples.st) — illustrative everyday Stitch (feel doc + future parser/eval test corpus). Pre-implementation, not validated.

Planned next:
- `02-*` — crate layout + the walking-skeleton milestone (lexer → Pratt parser → tree-walk eval of the v0 subset, `insta` snapshots).
