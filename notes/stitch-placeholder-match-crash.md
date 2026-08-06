# A `$` placeholder as a match subject panics lowering

Found 2026-07-29 by the batch11 generation run, which it killed at 910 of 1000
candidates. Unfixed as of 2026-08-06 — `stitch/src/lower.rs` has not been touched
since `0a6a15a`, and no test covers the construct.

## The crash

```
thread 'main' panicked at stitch/src/lower.rs:111:13:
internal error: entered unreachable code:
surface-only node survived lowering: Placeholder(Some("a"))
```

## The repro

A `$a` placeholder used as the **subject of a `match`**:

```
ext sum Union = Piece | Applause | Interval

totalMixed(xs: List<Union>) -> Maybe<Int> = {
    let getDur = match $a {
        Piece(d) => d
        Applause(d) => d
        Interval(d) => d
    }
    fold(xs, None, (acc, item) -> acc)
}
```

Reconstructed from the generation log. The candidate's `.st` was **never written
to disk** — the gate panicked before the file was saved — so this is the only
surviving copy of it.

## Why it happens

`core_ir.rs` states the contract: lowering folds the surface-only nodes
(`Placeholder`, `OperatorRef`, `SubjectlessMatch`, `Stmt::Use`) into ordinary core
nodes, and `ast.rs:238` says outright that "a `Placeholder` surviving into a final
AST is a bug". The placeholder-lambda rewrite exists (`lower.rs`, the
`Placeholder` → `Var("$x")` collection around lines 370–390) but does not cover the
match-subject position, so the node reaches `to_core` and hits the `unreachable!`.

## The design question, which is genuinely open

Should `match $a { … }` be legal — desugaring to a lambda, the way other
placeholder positions do — or rejected? Either answer is defensible; what is not
defensible is the current behaviour, because **the `unreachable!` is reachable from
user input.** At minimum it should become a diagnostic that cites `line:col` like
every other Stitch fault.

## The second bug, which is arguably the more important one

A panic anywhere in the gate kills the **whole generation run**. This one cost ~90
candidates and would have cost all 1000 had it landed early. A candidate that
crashes the gate is *data about the language*, not a reason to abandon a 15-hour
job.

The fix in kind: catch per candidate and record it as a verdict — a `crash` stage
alongside `parse`/`type`/`tests`/`long` — so the funnel counts it and the run
continues. Same lesson as the incremental `write_manifest` that saved batch9, and
the same family as the harness swallowing snemu's halt reason (post 74): **the
failure mode of a long-running process is decided by what it has already written
down.**

Worth naming what the generator actually is here: a fuzzer for the Stitch front
end, running at volume against a real toolchain. It found a reachable
`unreachable!` that no hand-written test had.
