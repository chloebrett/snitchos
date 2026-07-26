//! Prompt assembly.
//!
//! **The prompt is derived from the language, not maintained beside it.** The
//! prelude and the exemplars are `include_str!`d from their real locations, so
//! they cannot drift; only the reference is hand-written, and it lives in
//! `assets/reference.md` rather than in a doc that has to be kept in sync.
//!
//! That matters because a stale prompt does not fail loudly — it silently caps
//! program quality. `plans/corpus-mvp-spike.md` Findings 005a: `filter`, `sort`
//! and the whole `Str` module existed for six candidates while being invisible
//! to the model, which dutifully avoided them.
//!
//! Layout is ordered for prefix caching: everything invariant comes first, the
//! per-recipe task last.

const REFERENCE: &str = include_str!("../assets/reference.md");
const PRELUDE: &str = include_str!("../../stitch/src/prelude.st");
const TEXT_ST: &str = include_str!("../../fs-image/lib/text.st");
const STATS_ST: &str = include_str!("../../fs-image/lib/stats.st");

/// The rules a token prior breaks even when the model can recite them.
///
/// These sit in the system prompt because it is closest to the generation
/// point; the prelude below carries the *evidence* for them, which is what
/// actually moves behaviour (Findings 003: a prose rule alone did not).
#[must_use]
pub fn system() -> String {
    "\
You write Stitch, a small statically-typed functional language. You have not
seen Stitch before — learn it from the reference, the standard library, and the
examples that follow.

Rules that are easy to get wrong:
- There are no loop keywords. Use recursion or the list combinators below.
- There is no if/else. Conditionals are `cond => a | b`, or a `match` block.
- Booleans are the words `and` / `or` / `not`. `&&` and `||` do not exist.
- There is no `return`. A block's value is its last expression.
- Exported items are prefixed `ext`; everything else is module-private.
- Comments explain *why*, never *what*. Keep them to one or two lines.
- Include `test \"…\" { expect … }` items covering the core logic.

Reply with exactly one fenced ```stitch block and nothing else."
        .to_string()
}

/// The invariant body plus `task`, which is the only part that varies per
/// recipe and so is kept last.
#[must_use]
pub fn user(task: &str) -> String {
    format!(
        "{REFERENCE}\n\
         # Standard library\n\n\
         Defined in Stitch itself on top of the built-ins above. Between the two\n\
         lists, this is everything that exists — and they are complete, so do not\n\
         avoid a function listed here.\n\n\
         {PRELUDE}\n\n\
         # Example programs\n\n\
         ==== TEXT.ST ====\n\n{TEXT_ST}\n\n\
         ==== STATS.ST ====\n\n{STATS_ST}\n\n\
         # Your task\n\n{task}\n"
    )
}
