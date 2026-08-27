# Plans

Per-milestone and per-refactor implementation plans. **This file is the index** —
it exists so "what is actually live?" is a five-second read instead of a walk over
every status header. That walk has now been done at least three times (post 27's
sweep at ~45 files, [../notes/stock-take-2026-08-06.md](../notes/stock-take-2026-08-06.md),
and again on 2026-08-25); this index is the artifact those walks kept reproducing.

**Finished plans move to [`legacy/`](legacy/)** (89 of them, and counting) rather than
being deleted — the historical record is the point. Archiving is a `git mv` **plus** a
link sweep in both directions; see *Archiving* at the bottom, because this repo has
broken links on every sweep it has ever done.

## How to read this

Thirty-odd files is not thirty-odd live efforts. They sort into four kinds, and only
the first is work in progress:

| Bucket | Count | What it means |
|---|---|---|
| [In flight](#in-flight) | 17 | Real work, partially done. |
| [Not started](#not-started) | 7 | Written down, zero code. A plan is cheap; that's deliberate. |
| [Done bar a detail](#done-bar-a-detail) | 1 | Delivered; something small or deliberate holds back the archive. |
| [Reference, not plans](#reference-not-plans) | 5 | Living documents that will never "finish". |

## In flight

Recently active — the board/network cluster is the current front:

| Plan | What it is | State |
|---|---|---|
| [visionfive2-port.md](visionfive2-port.md) | The VF2 hardware port, M0–M4 | M1 achieved on hardware; **M3 shipped** (B6 ✓, all four U74s). M0 bar the TFTP loop. **M2 is code-complete but board-unverified** (uart step 10b), **M2.5** waits on the GMAC, **M4** (B5, DTB-driven MMIO) deferred — `collect_mmio_regions` still parked |
| [uart-telemetry.md](uart-telemetry.md) | Frames off the board over a physical UART (M2) | Steps 0–4, 6, 8, 9, **10a and 10b all landed and gate-green**. The critical path is now **one hardware run** to verify 10b — until it passes, B3/M2 are not done; 5b (interactive relay) follows; step 7 deferred |
| [network-telemetry.md](network-telemetry.md) | Telemetry over UDP (M2.5) | PRs 1–7 shipped and gate-green. **PR 8 is the GMAC driver** — its own plan, below |
| [board-bridge.md](board-bridge.md) | Letting an agent drive the real board | **Steps 1–2 done** — `xtask-board` carries `reach.rs` (failure taxonomy) and `stop.rs` (stop-condition evaluator), both pure host logic; no `board` subcommand wired up yet, so steps 3–9 are open. Two phases: host bridge (1–6b) then ESP32 transport (7–9). **On the critical path** — [vf2-gmac-driver.md](vf2-gmac-driver.md) names both phases as prerequisites |
| [kvetch-next-measurements.md](kvetch-next-measurements.md) | What the ladder's nats actually buy | 7 steps, none done. Answers the batch11 gap: a 0.43-nat gain showed **no** perceivable output difference |
| [stitch-native-tests.md](stitch-native-tests.md) | `test "…" { expect … }` in Stitch | Increments 1–8 done; **9 split and part-done** — 9a (`stitch test` verb) landed 2026-08-27. Left: 9b telemetry (span per test, event per assertion) and 9c on-target, which needs the runner wired since `test` items are inert on the target |
| [vf2-gmac-driver.md](vf2-gmac-driver.md) | The JH7110 GMAC driver (M2.5) | **Started** — `kernel-devices/src/gmac.rs` has the pure register model; no kernel MMIO glue yet. **Not a small one**: the plan calls it "the monster — weeks", bigger than the rest of the port combined. Phase 0's desk half is done ([../docs/vf2-gmac-design.md](../docs/vf2-gmac-design.md)); five board-side checks remain. **Gated on [board-bridge.md](board-bridge.md)** |
| [tour-v1.md](tour-v1.md) | The tour's tracer chapter: prose beside a live guest | **Started** — the `tour` crate owns the chapter schema + anchor predicate, host-tested and linked into `snemu-wasm`. Prose, chapters and the gate assertions remain |
| [glitch-v2-async-ring.md](glitch-v2-async-ring.md) | The async audio RT ring | Increments 1–5 shipped; **the XRun observable is armed and proven firing** (2026-08-25, inc 9's prerequisite). **6–9 remain** — mixing, init-delegated AudioSink, snemu PCM capture, the last two acceptance itests |

Parked — nothing since mid-July:

| Plan | What it is | State |
|---|---|---|
| [stim-v1.md](stim-v1.md) | A modal editor as a Stitch program | Group 1 complete; Group 2 at 2.1–2.5. Gated on the stim-vs-bytecode-VM ordering call |
| [stitch-type-system.md](stitch-type-system.md) | Bidirectional + gradual types | Stages 1, 2, 3, 5, 6 + G1–G2 done. **G3–G6 (generics) left** |
| [stage-0-validator-funnel.md](stage-0-validator-funnel.md) | The corpus candidate gate | Funnel built and in daily use; increments 4–8 and 11 (diversity + augmentation) unbuilt; splitting out `sift` still open |
| [userspace-runtime-maturity.md](userspace-runtime-maturity.md) | alloc → `main()` → heap → std | Steps 1–3 shipped, 4a in progress. **Cannot be retired** — `user/std/src/lib.rs` names it as the tracker |
| [xtask-crate-split.md](xtask-crate-split.md) | Cut the xtask incremental rebuild | Phases 1–2 shipped. **Phase 1 did not speed the hot loop** (measured, 7.996s vs 7.98s). Phase 2 took `scenarios.rs` into `xtask-itest` rather than leaving it in the bin, so **Phase 3's win may already be partly banked — unmeasured since the move** |
| [xtask-surface-consolidation.md](xtask-surface-consolidation.md) | Trim the xtask CLI surface | Phases 0–2 done, Phase 3 part done. Open: `View` as one type; deferred: reverse-direction `--engine qemu` gating. **The trim did not hold** — 24 → 20 then back to **24** (measured 2026-08-27) |
| [snemu-milestone-4-measurement.md](snemu-milestone-4-measurement.md) | snemu's measurement spine | Steps 2/3/4 shipped, 5 in substance. **Steps 6 (dashboard) and 7 (`H/G`) not built**; 2 of 4 criteria unmet |
| [snemu-page-straddle-fix.md](snemu-page-straddle-fix.md) | The page-straddle access bug | Fix 1 + follow-up D shipped and gated. Open: Fix 2 (data straddle), follow-ups A/B/C, the clock-skew verdict |

## Not started

Written down, nothing built. Ordered roughly by what unblocks what:

| Plan | What it is |
|---|---|
| [board-image-opt-level.md](board-image-opt-level.md) | Debt #19: the board image's opt level |
| [kitsch-v1.md](kitsch-v1.md) | The desktop: surfaces as caps, cell-mode tiling on real pixels ([../docs/kitsch-design.md](../docs/kitsch-design.md)). Increment 0 is four numbers; increment 1 is memory-object capabilities, which several other things want |
| [vf2-display.md](vf2-display.md) | JH7110 DC8200/HDMI — capture a vendor MMIO trace and replay it |
| [corpus-mvp-spike.md](corpus-mvp-spike.md) | Increment 0 of corpus-mvp: a decision and four numbers, not code |
| [tinynerd.md](tinynerd.md) | Pretrain on purpose-built simple technical prose, then fine-tune on Stitch — so the ~46% of the budget that goes on comment English stops buying nothing. Steps 1–3 are cheap probes that gate the generation compute; step 3 is the never-run arm the [batch9 findings](../notes/batch9-findings.md) asked for. Shares its split-metric step with [kvetch-next-measurements.md](kvetch-next-measurements.md) — build it once |
| [stim-grammar.md](stim-grammar.md) | Post-v1 grammar follow-up for stim |
| [open-sourcing-extractables.md](open-sourcing-extractables.md) | Which pieces could stand alone outside the repo |

## Done bar a detail

Delivered. Each is one small thing from `legacy/`:

| Plan | What holds it back |
|---|---|
| [glitch.md](glitch.md) | v1 complete. **Deliberately unarchived** — [glitch-v2-async-ring.md](glitch-v2-async-ring.md) cites it |

## Reference, not plans

Living documents that happen to live here. They will never be archived:

| Document | Why it stays |
|---|---|
| [v0.4-memory-findings.md](v0.4-memory-findings.md) | CLAUDE.md: read **before** touching boot order or any address-translation site |
| [scaling-corners.md](scaling-corners.md) | CLAUDE.md: the corners v0.1 sidesteps |
| [stitch-examples-findings.md](stitch-examples-findings.md) | The lab notebook behind the 30-program corpus |
| [stitch-language-improvements.md](stitch-language-improvements.md) | The proposal catalogue other plans derive from |
| [corpus-recipe-axes.md](corpus-recipe-axes.md) | A data spec, shipped as `batch9.toml`; cited from `cram-gen/src/recipe.rs` |

## Archiving

`git mv plans/X.md plans/legacy/X.md` is the easy quarter of the job. The rest:

1. **Fix the moved file's own outbound links.** `../docs/` → `../../docs/`, and a
   sibling `other-plan.md` → `../other-plan.md`. This direction has bitten every
   sweep this repo has done — from `plans/legacy/`, `../docs/` means `plans/docs/`,
   which has never existed.
2. **Fix every inbound link**, repo-wide.
3. **Fix doc-path citations in `.rs` comments.** `cargo xtask links` only walks files
   with a `.md` extension, so these rot silently:
   `grep -rn --include='*.rs' -oE 'plans/[a-z0-9/-]+\.md' .`
4. **Fix a stale status header before moving**, not after. Archiving a
   self-contradicting doc preserves the contradiction — `glitch`,
   `stitch-examples-corpus` and `supervision-v2` were each found stale this way.
5. **Leave dated snapshots alone.** A note that quotes a plan's old status as a
   historical finding is correct as written; don't rewrite it.
6. `cargo xtask links` to confirm.

Then update this index. **Two of those steps are now gated**, by `cargo xtask
plan-status` (inside `cargo xtask test`, beside `links` and `counters`):

- every plan carries a **dated** `**Status (YYYY-MM-DD)**:` header, and
- every plan is linked from this file — so an archive that forgets step 2, or a
  new plan nobody indexed, fails the gate instead of going quiet.

Run it alone for a staleness readout: it prints every plan sorted by status date,
oldest first. It deliberately does **not** fail on age — a gate that reddens by
the passage of time gets ignored, and the only way to green it without doing the
work would be to lie about the date, corrupting the one signal the convention
carries.

**What is still unchecked is the prose in the tables above** — the "State" column
is human judgement no parser can derive from a status header, and it goes stale
the same way it always did. The date beside each plan is what tells you how far
to trust it.
