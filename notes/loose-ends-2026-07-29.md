# Loose ends, swept 2026-07-29

> **Status after the same-day pass:** #1, #3, #10 fixed; #7 (debt #16) root-caused
> and its reproducible half fixed. #2 is now covered by the working tree. #4, #5,
> #6, #8, #9 remain open. Per-item notes inline.

A read of the docs touched in the last week (`plans/kvetch-drivel-on-target.md`,
`plans/floating-point.md`, `plans/repl-completion.md`, `plans/corpus-recipe-axes.md`,
`notes/batch10-pilots.md`, `plans/glitch.md`, `plans/stitch-native-tests.md`,
`docs/debt-register.md`), plus what the tools themselves say when you run them.
Ordered roughly by how cheap the fix is against how loud the thing is.

---

## 1. ✅ FIXED — Every `x` invocation printed 8 dead-code warnings

Resolved with a documented `#[allow(dead_code, reason = …)]` on each item rather
than `#[cfg(test)]`: gating them out of the non-test build would drop `LINTS_EXEMPT`
from `cargo doc`, breaking `RUSTDOC_EXEMPT`'s intra-doc link to it — and broken
intra-doc links are themselves a gate (debt #14). These tables exist to be read, so
they stay compiled. `check_all` turned out **not** to be orphaned: it is called by
`diagram_drift_tests` in `xtask-itest/src/main.rs:1274`; its doc comment claimed the
lean `xtask test` gate called it, which was stale since the crate split. Same stale
claim fixed on `run_unit_tests`, plus a `clippy::empty_line_after_doc_comment` there.
`cargo build -p xtask -p xtask-itest` is now silent.

<details><summary>original</summary>

`cargo build -p xtask` → 7 warnings, `-p xtask-itest` → 1. So `x snemu boot`,
`x itest`, `x test` all open with a wall of yellow before doing anything.

They are not unused *vars* — they are test-only gate machinery that isn't
`#[cfg(test)]`-scoped, so the binary build sees it as dead:

- `xtask/src/plan.rs:34,44,52,67` — `RAW_ECALL_INTENTIONAL`,
  `RAW_ECALL_GRANDFATHERED`, `raw_ecall_sites`, `fn_name_on`. Used only by
  `plan::raw_ecall_ratchet_tests`.
- `xtask/src/plan.rs:135,143,161` — `LINTS_EXEMPT`, `opts_into_workspace_lints`,
  `lints_optin_gaps`. Used only by `plan::lints_policy_tests`.
- `xtask-itest/src/diagram_cmd.rs:24` — `check_all`, which the doc comment says is
  the drift gate; nothing calls it.

The first two are ratchets whose whole point is to be load-bearing:

> "**This number may only ever go down.** It is a ratchet, not a budget"
> — `xtask/src/plan.rs:41-44`

`check_all` is the more interesting one — is the diagram drift check still wired
through some other path, or did the xtask split orphan it?

</details>

## 2. An uncommitted bug fix in the working tree, with no test

`git diff` carries the `workload_features` extraction (`xtask-qemu/src/lib.rs:143`,
called from `xtask/src/main.rs:840` and `xtask-itest/src/main.rs:1318`). Its own
comment says what it fixes:

> "`snemu boot --workload stitch-drivel` promptly booted a kernel with an empty ELF
> stub and panicked `Parse(BadMagic)` — a mapping duplicated per call site is a
> mapping that is wrong at all but one of them."
> — `xtask-qemu/src/lib.rs:130-133`

No test pins it. The failure is a one-line mapping omission at a *third* call site,
which is exactly the shape the fix says it is preventing.

## 3. ✅ FIXED — Two plans carried stale, self-contradicting headers

`repl-completion.md` now has one accurate status (all seven increments done, gated),
and the increment-1 fragment that had been spliced mid-sentence into the "Status of
the pieces" paragraph is restored under its own heading. `floating-point.md` lost its
duplicate Increment 5 section and its dangling pre-DONE 4b paragraph; the
`stitch-kvetch-completes is still not registered` claim is corrected (it is, at
`xtask-itest/src/itest.rs:218`); Increment 3 is marked DONE, and its step-2 "still to
do: compressed FP forms" is struck through, since step 6 of the same list says they
are done. `cargo xtask links` passes (2176 files).

<details><summary>original</summary>

- **`plans/repl-completion.md:3` and `:7`** — two stacked `**Status:**` blocks. The
  first says all seven increments are built end to end; the second, directly under
  it, says "increments 1, 2, 3 done."
- **`plans/floating-point.md:380` and `:408`** — two `## Increment 5 — Stitch floats
  work on target` headings. The first is marked DONE; the second describes it as
  pending and claims `stitch-kvetch-completes` still doesn't register. It does —
  `xtask-itest/src/itest.rs:218`, alongside `stitch-drivel-completes` at `:217`.
- Same file, `:341-345`: a "Needed before two processes can use FP simultaneously…
  Removes the `RefuseBusy` guard above" paragraph left dangling *below* the
  `Increment 4b — DONE (2026-07-28)` block that removed it.

</details>

## 4. A dead server hangs its clients silently

The most substantive finding buried in the FP work, and flagged as such:

> "**A server's death should be visible to its clients.** The REPL blocked
> indefinitely on an endpoint with no receiver and no diagnostic […] Arguably the
> more serious of the two findings, and independent of FP."
> — `plans/floating-point.md:371-375`

Nothing in `kernel-proc/src/ipc.rs` refuses a call on an endpoint whose only
receiver has exited. In a codebase whose rule is "refusals snitch, never silent,"
this is the one place a failure is silent — and it presents two processes away from
its cause ("the REPL stopped responding" rather than "a process died").

## 5. `kvetch-drivel-on-target` is 🚧 with every acceptance box unticked

`plans/kvetch-drivel-on-target.md:4` — "Steps 1–4 done and green; step 7 partly done
ahead of schedule." Steps 5 and 6 are unstarted, and all six acceptance criteria at
`:192-201` are still `- [ ]` despite the feature demonstrably working on target. So
the plan can't be archived to `plans/legacy/` and can't be read as a record either.

## 6. Three named performance leads from the KV-cache profile, all unpulled

`plans/kvetch-drivel-on-target.md:343-357`, listed "in the order the profile now
argues for":

- **The kernel dominates a short completion** — at one token, ~22% telemetry
  serialization vs 19.7% userspace. Optimising the model further won't move the gate.
- **Borrowed weights** — `Model::decode` copies into an owned `Vec<f32>`, so the
  process holds ~4.2 MB rodata *plus* ~4.2 MB heap; borrowing is what lets the 64 MiB
  machine come back down.
- **snemu's JIT ends a block at every FP op** — `block.rs::compile_op` lowers no FP
  family and no `MULDIV`, so a matmul inner loop compiles to two-instruction blocks
  and the block machinery is pure overhead. Pays for audio and on-target Stitch
  floats too, not just drivel.

The step's own done-condition is unresolved: "a Tab under snemu is bearable, **or we
have decided in writing** that it is a board-only feature" (`:358-359`).

## 7. ✅ ROOT-CAUSED (and it was not what the register said)

Not UB. `memhog`'s 4 MiB `Vec::with_capacity` was dead code that LLVM legally
deletes at opt≥2 — `buf.capacity() != 0` does not keep an allocation alive.
Measured: `li a7, 0x4` (`MapAnon`) present at opt-1, absent at opt-2. No syscall →
no frames committed → nothing to reclaim → the scenario's `freed_total` assertion
fails. One `core::hint::black_box(buf.as_ptr())` fixes it; the suite is now
**130/130 at opt-2 and opt-3**, and green on QEMU at opt-3 where a hang was
documented.

The register's "it's a spin, not a fault" was inferred from an instret count and is
wrong: the scenario fails on its *second* assertion, so the reaper completes all 15
cycles, and the profile shows no `[user:…]` bucket at all. Full write-up, including
the FS-path symptom that is *not* closed (merely not reproducing) and the
inline-asm hypothesis eliminated along the way: `docs/debt-register.md` #16.

**Still open, deliberately:** the pin itself. Removing it collapses `OptLevel::Mid`
into `Max` and none of the evidence covers the VF2 board — a decision, not a cleanup.

<details><summary>original</summary>

`docs/debt-register.md:167-203`. The diagnosis is complete and the instruction is
explicit; it just hasn't been executed:

> "Next step: read the hot PC off that profile → objdump the owning program →
> compare that one function's codegen at opt-1 vs opt-2."
> — `docs/debt-register.md:200-202`

Everything needed exists (`--opt hi`, `snemu profile --user-detail`), it's narrowed
to two scenarios, and it's confirmed on the QEMU oracle so it isn't a snemu artifact.

</details>

## 8. batch10's three open corpus items

`notes/batch10-pilots.md:105-115`:

- **Ambiguous clauses** — "spare-parts bin"'s reorder-point clause read as *geometry*
  by 6 of 10 attempts. A vocabulary-overlap heuristic found no siblings, "so the
  class needs an eye, not a script."
- **`mut` fields** — two candidates wrote `prod Point(mut x: Int, …)` and died on
  `unexpected token: Mut`. "Possibly a `reference.md` gap."
- **The second-pass (medium/large) crossings have never been generated.**

Related and unclosed: `plans/corpus-recipe-axes.md:69` — clauses for domains added
later still have to be hand-written.

## 9. ❌ WRONG — glitch was not stalled; I read a stale header

**Corrected 2026-08-06.** The claim below was read off `plans/glitch.md`'s *status
header* and is false. The body of that same file marks increments 1–8 ✅ DONE, carries a
`## v1 COMPLETE` section, and records the in-kernel beep as retired — **5a had landed,
so the layering violation was already fixed.** v2 (the async ring) has since shipped
increments 1–5 as well. The header has been corrected.

This is the same failure as #7 one row up and as post 79's: the header is the part
everyone reads and the part nothing checks, so a stale one doesn't sit inertly — it
manufactures a confident wrong finding in the next document that cites it. The tell I
missed: I quoted `:6-9` without reading `:70+`.

<details><summary>original (wrong)</summary>

`plans/glitch.md:6-9` — kernel spine (1–4) shipped and mutation-verified;
**Increment 5 not started.** The first move is a structural one, not a feature:

> "start with **5a** (extract `synth` crate, `Tone`/`Gain` out of `kernel-devices`;
> `user/` must not depend on `kernel-devices`)"

So the layering violation it exists to fix is still standing.

</details>

## 10. ✅ FIXED — `prelude.st` had never had a test

20 native tests added, canon suite count 69 → 89 (the `canon.rs` ratchet raised to
match). Verified falsifiable: breaking `first`'s fold arm fails
`first and last pick opposite ends` with `expect failed: 9 == 7`. Measured cost,
since the prelude is parsed at every program start: 3505 → 8456 bytes, parse
517µs → 725µs, `build_env` unchanged (`Item::Test` lowers to a `CoreItem::Test`
nothing registers, so it is parse-only). `each` is still uncovered — its whole
effect is the side effect, and it needs an effect-handler double.

<details><summary>original</summary>

`plans/stitch-native-tests.md:212` — "`prelude.st` gets tests, which it has never
had. (Next.)" — the last unticked item in a section otherwise done. Alongside it,
`:201-203`: native snapshot assertions stay deferred pending a file convention +
accept workflow, and that is now *the only reason any stim test is still in Rust*.

</details>

---

## Also noted

- `plans/floating-point.md:222-224` — `StepError::UnsupportedRoundingMode` reports
  the mode numerically because `StepError` has no `Display` impl. Small, and the doc
  asked for it named.
- **A stale record to retire:** the note that `cargo xtask test` carries a
  pre-existing red from a mutant-plan characterisation list predating the
  cram/kvetch crates is no longer true — `cargo nextest run -p xtask` is 44/44,
  including `mutant_plan_tests::the_derived_plan_matches_the_previously_hardcoded_set`.
