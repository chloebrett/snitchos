# plans/lang — language implementation track

Implementation plans for the SnitchOS side-project language. High-level design lives in [docs/language-design.md](../../docs/language-design.md); this directory is the build-facing track (specs, milestones, decisions).

Convention: numbered files in implementation order.

- [01-grammar-and-precedence.md](01-grammar-and-precedence.md) — tokens, precedence table, the placeholder-lambda + pipe rules, v0 parser scope. **The prerequisite for any parser code.**

Planned next:
- `02-*` — crate layout + the walking-skeleton milestone (lexer → Pratt parser → tree-walk eval of the v0 subset, `insta` snapshots).
