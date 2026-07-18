# xtask crate split — cut the incremental rebuild

## Problem (measured, not guessed)

Editing any file under `xtask/src/` recompiles the entire `xtask` binary
crate. It is one ~15k-line crate and the terminal node of the build graph, so
its compile can't overlap with anything.

Measured incremental rebuild after `touch xtask/src/itest.rs`:

- Wall: **~10.7s**, and `--timings` shows a **single unit, zero parallelism**
  (it starts alone and runs alone — nothing else is left to build).
- Phase split (`cargo rustc -p xtask -- -Ztime-passes`, uncontended, ~8s total):

  | phase | time | note |
  |---|---|---|
  | frontend: typeck + trait-solving + mono-collection (residual) | **~5.5s** | serial, per-crate |
  | codegen → LLVM IR | 1.0s | parallel across CGUs, but nothing to overlap |
  | LLVM passes | 0.9s | ditto |
  | incr-comp bookkeeping | 0.65s | fixed per-crate |
  | macro_expand | 0.14s | |
  | **link** | **0.18s** | **negligible** |

**Conclusions:**
1. **A faster linker is worthless here** — link is 0.18s / 8s (2%). Dropped.
2. The pole is the **serial per-crate frontend** (~5.5s). clap-derive + serde
   make it worse. The only structural fix is to stop compiling 15k lines as one
   crate: split into libs so (a) an edit rebuilds one small crate + relinks the
   thin bin, and (b) the untouched clusters stay cached and, when they do
   rebuild, compile in parallel.

Precedent: `itest-harness` is already a sibling crate carved out of xtask
(runner/signature/otlp/prom, no snemu/qemu). This plan extends that pattern.

## Current internal dependency graph

Clean, acyclic:

```
source ← audit
qemu   ← itest, measure, snemu_diff
snemu_diff ← snemu_bench, snemu_profile, itest/snemu_audit
```

Line counts by cluster:

- **itest** (~8.1k): `itest.rs` 1293 · `scenarios.rs` 4168 · `harness.rs` 1341 ·
  `snemu_audit.rs` 1447 · `baseline` 391 · `snapshot_tree` 259 · `schedule` 190 ·
  `matchers` 70
- **snemu tooling** (~1.9k): `snemu_diff` 1233 · `snemu_bench` 415 · `snemu_profile` 207
- **misc cmds** (~2.9k): `audit` 791 · `diagram_cmd` 449 · `measure` 387 · `snip`
  375 · `loc` 281 · `links` 264 · `qemu` 182 · `source` 127
- **bin/CLI**: `main.rs` 1902 (incl. ~700 lines of inline CLI-surface tests)

`scenarios.rs` (4168) is the single hottest-edited file — every new itest.

## Proposed decomposition (libs + thin bin)

| crate | contents | deps | why |
|---|---|---|---|
| `xtask-qemu` | `qemu.rs` | — | tiny shared leaf |
| `xtask-snemu` | `snemu_diff`, `snemu_bench`, `snemu_profile` | `xtask-qemu`, `snemu`, `protocol`, `object` | isolates the heavy `snemu` (opt-3) dep tree |
| `xtask-itest` | `harness`, `snemu_audit`, `baseline`, `schedule`, `snapshot_tree`, `matchers`, `itest.rs` glue | `xtask-snemu`, `xtask-qemu`, `itest-harness` | the harness/driving layer |
| `xtask-scenarios` | `scenarios.rs` | `xtask-itest` | **the hot-edit crate** — isolated so a scenario edit rebuilds only ~4.2k lines + relink |
| `xtask-cmds` | `audit`, `diagram_cmd`, `loc`, `links`, `measure`, `snip`, `source` | `diagram`, `snip`, `xtask-qemu` | independent commands |
| `xtask` (bin) | `main.rs`: clap CLI + dispatch + CLI-surface tests | all above | thin; small frontend, 0.18s link |

### Payoff (estimate — verify after Phase 1)

- Edit a **scenario** → rebuild `xtask-scenarios` (~4.2k, generic-light) + relink
  thin bin. Expect roughly **3–4s** vs today's ~10.7s.
- Edit a **misc command** or **snemu tool** → doesn't touch itest at all.
- First-touch after any of the sibling crates changes: they compile **in
  parallel** instead of as one serial 15k-line lump.

The estimate is a guess until measured — `scenarios.rs` is mostly `Harness`
calls + assertions (light on generics), so its frontend should be well under
proportional. Re-run the `-Ztime-passes` recipe after Phase 1 to confirm.

## Cost / risks

- **Visibility churn**: `pub(crate)` items crossing new crate lines become `pub`.
  Mechanical but wide.
- **Shared constants** (`COLLECTOR_BIN`, `TELEMETRY_SOCKET`, `EXTRA_TEST_ARGS`,
  `NOT_HOST_TESTED`) must land in a crate everyone can see (`xtask-qemu` or a new
  `xtask-support`).
- **Inline tests** in `main.rs` (`cli_surface_tests`, `retired_command_tests`,
  `env_scrub_tests`, `mutant_plan_tests`) stay with whatever they test; some
  reference `crate::itest::*` → repoint to `xtask_itest::*`.
- Net Cargo.toml boilerplate for 5 new crates.

Safety net is strong: the whole itest suite + unit tests + `cargo xtask test`
characterise xtask's behaviour. This is a pure refactor — behaviour must not
change.

## Phase 1 RESULT (measured 2026-07-18)

`xtask-qemu`, `xtask-snemu`, `xtask-cmds` extracted; bin dropped from ~15.1k to
~10.2k lines. Cross-crate fixes needed: promote 5 `snemu_diff` fns `pub(crate)`
→ `pub`; move `postcard` dev-dep + add `diagram` dep to `xtask-snemu`.

**The scenario-edit loop did NOT get faster.** `-Ztime-passes` total for the bin
after `touch scenarios.rs`: **7.996s**, vs **7.98s** pre-split. Identical.

Why: the ~5k lines extracted were compile-*cheap*. The bin's ~8s is concentrated
in what remains — clap-derive expansion in `main.rs` + monomorphization of
`itest-harness`/`snemu`/`protocol` generics instantiated by the itest cluster +
`scenarios.rs` itself. Removing simple command modules doesn't touch that.

**What Phase 1 DID buy:** edits to a *non-itest* path — a snemu tool
(`xtask-snemu`), an auditor/measure command (`xtask-cmds`) — now rebuild only
that ~1–3k-line lib + a 0.18s relink, instead of the whole bin. The cold paths
are isolated; the hot path (scenarios) is not, because scenarios still lives in
the bin.

**Implication for Phase 2/3:** the scenario-loop win requires getting
`scenarios.rs` into its own crate (Phase 3, which needs Phase 2's `xtask-itest`
lib first, since scenarios depends on `Harness`). Its payoff hinges on clap's
share of the 8s: a scenarios crate rebuild skips clap-derive entirely, so if clap
is a big fixed chunk the win is large; if the 8s is mostly itest-harness mono
(which the scenarios crate still pays) the win is modest. `cargo llvm-lines`
would predict this, but it's not installed and the sandbox blocks installing it —
so Phase 3 is its own definitive measurement.

## Phasing (each phase leaves a green tree)

1. **Extract the independent clusters first** (lowest coupling, immediate win):
   `xtask-qemu`, then `xtask-snemu`, then `xtask-cmds`. After this, re-measure —
   the bin is already much lighter.
2. **Extract `xtask-itest`** (harness + audit + baseline…), leaving `main.rs` +
   `scenarios.rs` in the bin.
3. **Extract `xtask-scenarios`** — the hot path. Measure the scenario-edit loop;
   this is the phase the whole exercise is for.
4. If a scenario edit is still frontend-bound inside a 4.2k-line crate, consider
   splitting `scenarios.rs` by area (sched / mem / smp / caps) — but only with a
   measurement showing it's worth it.

## Open decisions

- **Granularity**: full 5-crate split, or stop after Phase 1–2 (extract the
  heavy independents, leave itest+scenarios+bin together)? Phase 1 alone may buy
  most of the win for a fraction of the churn — the measurement after Phase 1
  decides.
- **`xtask-support` vs `xtask-qemu`** as the home for shared consts.
- Whether `itest.rs` (the dispatch/`SCENARIOS` table) belongs in `xtask-itest`
  or stays in the bin next to the CLI.
