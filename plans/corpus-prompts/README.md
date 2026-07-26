# Corpus MVP — prompt versions

Copy/paste prompts for [../corpus-mvp-spike.md](../corpus-mvp-spike.md). Each
file holds a system prompt and a user message, ready to paste. Only the
`# Your task` section varies per recipe; everything above it is invariant and
belongs in the cached prefix.

**Current: [v5](v5.md).**

| Version | Change | Evidence |
|---|---|---|
| [v1](v1.md) | First draft: syntax reference + two exemplars | — |
| [v2](v2.md) | `prelude.st` verbatim as a stdlib section; `ext prod`/`ext sum`; pipe desugaring stated; both `match` forms | Findings 003 — `&&` survived a prose rule; the rule had no code-shaped backup |
| [v3](v3.md) | Size counted in declarations, not lines; no-`if` counter-example inside a lambda; `toStr` | Findings 004 — the model cannot count lines and deleted working code trying to |
| [v4](v4.md) | Must-use-identifiers line removed | Findings 005 — the words displaced meaningful function names |
| [v5](v5.md) | **Built-in functions** section, generated from the `NativeFn` table in `stitch/src/natives.rs` | Findings 005a — `filter`/`sort`/`Str.*` exist but were invisible, so the model avoided them |

**These should eventually be generated, not maintained.** The reference is
already derived from `natives.rs` and `prelude.st`; a stale prompt does not fail
loudly, it silently caps program quality (005a). See
[../corpus-mvp.md](../corpus-mvp.md) Increment 4.
