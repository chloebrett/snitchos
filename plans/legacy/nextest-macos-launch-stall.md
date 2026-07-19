# Plan: Kill the macOS first-exec launch stall on host tests

**Branch**: main (investigation; no feature branch)
**Status**: Active

## Goal

Find and eliminate the ~48s near-zero-CPU stall that hits `cargo nextest run -p <crate>`
on the **first run after a recompile**, so host-test iteration on macOS is bound by
actual test work (sub-second) instead of a per-binary OS security assessment.

## Why this is not the doctest fix already landed

A separate, already-committed change (CLAUDE.md "use nextest", `doctest = false` on lib
crates) explains why *`cargo test`* was ~6s: rustdoc compiles doctests even when there
are zero to run. That is real and orthogonal. It does **not** explain the symptom this
plan targets, because **nextest never touches doctests** and is still slow:

```
cargo nextest run -p snemu       # build cached (0.06s), 207 tests (0.267s) → 49.7s wall, 3% CPU
cargo nextest run -p hitch-pod   # 2 tests, wifi OFF, tiny binary → 60.7s wall, 7% CPU
cargo nextest run -p snemu       # (re-run, no recompile) → fast
```

## Evidence, and what it rules out

| Fact | Rules out |
|---|---|
| `nextest --no-run` = 0.3s | Build / test-listing / cargo. Not the cause. |
| 3–7% CPU across the whole stall | Anything compute-bound. It's **waiting**. |
| Cost scales with process count (207 procs → 49s; 1 proc `cargo test` → ~6s) | It's **per-process-launch** overhead. |
| **Wi-Fi off, still ~60s** | An **online** check (Gatekeeper OCSP / notarization fetch). Interface-down fails fast. |
| **Tiny binary (hitch-pod) still ~60s** | A **size-scaled** scan (XProtect content scan, Spotlight). Cost is ~fixed per binary. |
| **Second run (no recompile) is fast** | Serial nextest (would be constant); confirms the cost is **per-newly-written binary, cached by code-hash after first exec**. |
| First `-p kernel-obs` run was 1:49; had VS Code / rust-analyzer open | (Earlier) some of that was additionally r-a holding the `target/` lock — a *separate* amplifier, now closed by quitting VS Code. |

**Surviving lead:** macOS **first-execution security assessment** of freshly-built
binaries — `syspolicyd` (Gatekeeper) / `XProtect` / `amfid`. Local (survives Wi-Fi
off), ~fixed per binary (survives tiny binary), cached by cdhash (fast on re-run),
waits on retry timers (near-zero CPU). The offline ~48s magnitude is consistent with
`syspolicyd`/`trustd` exhausting offline notarization-ticket retries before allowing.

**Leading fix (to be confirmed, not assumed):** the macOS **Developer Tools exemption**
— `sudo DevToolsSecurity -enable` + adding the terminal app to *System Settings →
Privacy & Security → Developer Tools*. This exempts processes spawned by that terminal
from the per-exec assessment and is the standard cure for this exact symptom.

## Constraints

- Investigation only until a fix is confirmed; no production/xtask code until Phase 2,
  and only then under TDD.
- The machine is the variable, not the repo. Prefer fixes that are (a) durable across
  reboots, (b) don't weaken security globally (`spctl --master-disable` is a last
  resort, not a default), (c) reproducible enough to document for future clones.

## Phase 0 — Localize the daemon (experiments, no code)

Each experiment names the *predicted* outcome and what it rules in/out. Run each once;
stop early if one decisively fingerprints the daemon.

### 0a. Baseline the per-exec cost on a bare binary
Loop-exec a freshly-built trivial binary vs a system binary:
- `time` a 200× loop of `/usr/bin/true` (system, notarized).
- `time` a 200× loop of a just-compiled Rust hello-world (ad-hoc signed, new cdhash).

**Predicts:** if `true` is instant but the fresh Rust binary is ~seconds/exec on the
**first pass** and fast on the **second**, the cost is the first-run assessment of the
new cdhash → confirms the class. If even `/usr/bin/true` is slow → system-wide exec
stall (opendirectoryd), redirect to 0d.

### 0b. Name the daemon with a log capture
During one slow `cargo nextest run -p hitch-pod` (force a recompile first: `touch`),
capture:
`log stream --style compact --predicate 'process == "syspolicyd" OR process == "trustd" OR process == "amfid" OR process == "XProtectService" OR process == "mds" OR process == "mds_stores"'`

**Predicts:** the daemon that logs continuously across the stall window is the culprit.
`syspolicyd`/`trustd` → Gatekeeper/notarization; `amfid` → code-sign validation;
`XProtect*` → malware scan; `mds*` → Spotlight.

### 0c. Where is the wall time actually spent?
`sudo fs_usage -w -f exec` (or `sample <pid>`) on one slow run to see whether launches
block in `execve`/`posix_spawn` (security assessment) vs elsewhere.

**Predicts:** blocked-in-`execve` with the security daemon active confirms the
assessment path.

### 0d. Directory-services fallback check (only if 0a implicates system-wide exec)
`time` the `true` loop again with `dscacheutil`/network account disabled, or check
`scutil --dns` for a slow search domain.

**Done when:** one daemon / path is named with evidence. Record it in this file.

### Phase 0 — RESULT (confirmed 2026-07-19)

Faithful repro (compile + exec in one invocation, log streaming across it):
`hitch-pod` → **60.5s wall, 4.5s CPU**. Splitting build (`--no-run`) from run made
the exec **0.24s** — proving the cost is a scan racing the *just-written* binary, not
exec intrinsically.

Log during the stall: **1532 `trustd` + 530 `syspolicyd`** lines (+143 `mds`), doing
`SecTrustEvaluateIfNecessary` / `SecKeyVerifySignature`. Causal lines:

```
syspolicyd … [com.apple.securityd:SecError] Error checking with notarization daemon: 3
syspolicyd … [com.apple.network:connection] [C14] event: client:connection_idle @24.953s
```

**Root cause:** Gatekeeper (`syspolicyd`) assesses each freshly-built, ad-hoc-signed,
un-notarized test binary by contacting Apple's **online notarization service**. With no
usable network the connection does **not** fail fast — it **idles ~25s per attempt**
before falling back to "allow." nextest's process-per-test multiplies it; each fresh
code-hash re-triggers it (hence: recompile = slow, re-run = cached/fast; Wi-Fi off
doesn't help because the wait is a fixed idle timeout, not a fast failure).

Remaining hypotheses (#2 opendirectoryd, #3 size-scan, #4 dyld) are **ruled out**.
`mds`/Spotlight is a minor secondary (143 lines); the Hamachi-kext spam is ambient.

## Phase 1 — Apply and measure the candidate fix

Order by reversibility (least invasive first). Measure `time cargo nextest run -p
hitch-pod` **after a forced recompile** before and after each, and confirm a re-run
stays fast.

1. **Developer Tools exemption** — `sudo DevToolsSecurity -enable`; add the terminal
   (Terminal.app / iTerm / the app hosting this shell) under *Privacy & Security →
   Developer Tools*. Re-measure.
2. **Exclude `target/` from Spotlight** — add to *Privacy* in Spotlight settings (or
   `.metadata_never_index`). Re-measure. (Only expected to help if 0b fingerprints `mds`.)
3. **Confirm no third-party AV/EDR** is scanning execs (fresh personal Mac, so unlikely,
   but rule it out via `log`/Activity Monitor during a run).

**Done when:** a forced-recompile `cargo nextest run -p hitch-pod` drops from ~60s to
its test-bound floor (well under ~2s), and we know *which* setting did it.

### Phase 1 — RESULT (confirmed 2026-07-19)

Fix that worked: **added wezterm to System Settings → Privacy & Security → Developer
Tools, toggled ON, and relaunched wezterm** (the relaunch matters — TCC grants don't
apply to an already-running app). wezterm, not Terminal.app, is the app to exempt: it's
the *responsible process* for interactively-typed shell commands. The `spctl
developer-mode enable-terminal` helper is hardcoded to Terminal.app and was a red
herring.

Measured in the user's own wezterm (a forced `touch` each time):
`cargo nextest run -p hitch-pod` → **14.7s** and **34.0s** total, both now ≈ the
reported compile time (`Finished in 14.39s` / `33.74s`) — i.e. **~0s post-compile
stall**, down from ~48s. CPU rose from 3–7% (waiting) to 24–34% (compiling): the wait
is gone.

**Measurement caveat learned:** the fix could NOT be validated from the Claude Code
Bash tool — harness-spawned commands aren't children of wezterm, so TCC attributes them
elsewhere and Gatekeeper still assessed them (storm persisted in-probe). Only an
interactive wezterm run reflects the exemption.

Remaining wall-clock is now ordinary (and variable) **compile+link** time for the
crate, not a stall — a separate, optional lever (sccache for the dep tree, fast linker),
tracked outside this plan.

**Claude Code's own commands are covered too** (verified): a Claude session launched
from wezterm spawns its Bash-tool commands as wezterm descendants, so the same TCC grant
applies — the causal `notarization daemon` + `connection_idle` signature dropped 12 → 0
and a harness-run forced recompile is compile-bound (~13s). Caveat: the exemption
follows the *launch context*. Start Claude (or bare cargo) from a non-exempt host
(Terminal.app, `launchd`/cron/headless) and the stall returns for those commands; exempt
whatever terminal actually hosts the process. Moot on CI (Linux, no Gatekeeper).

## Phase 2 — Make it durable and documented (code only if warranted)

Pick from, depending on Phase 1:

- **Doc-only (likely sufficient):** add a "macOS: first-run test stall" note to
  CLAUDE.md and/or README pointing at the confirmed setting, so a fresh clone / new
  machine fixes it in one step. No code → no TDD; just the note + a link.
- **Optional `cargo xtask doctor` preflight (if we want it enforced):** a check that
  warns when `DevToolsSecurity` is disabled on macOS. This *is* production code →
  full TDD:
  - **RED:** a host test over a pure `assess_devtools(status: DevToolsStatus) ->
    Advice` function asserting `Disabled → Advice::Warn{fix-string}` and
    `Enabled/NonMacos → Advice::Ok`.
  - **GREEN:** minimum mapping.
  - **MUTATE / KILL:** per `mutation-testing`.
  - Keep the impure `DevToolsSecurity`-probing shell-out behind a thin seam so the
    decision logic stays host-tested (see `finding-seams`).

**Done when:** the fix survives a reboot (re-measure next session) and a fresh-machine
reader can reach the sub-2s floor from the documented steps alone.

## Pre-PR quality gate (only if Phase 2 ships code)

1. `cargo xtask test` (host checks) green.
2. Mutation testing on any new decision function (`mutation-testing` skill).
3. `cargo xtask clippy` clean.
4. `cargo xtask links` — if CLAUDE.md/README links were added.

---
*On completion, `git mv` this file to `plans/legacy/` per the CLAUDE.md override (keep
the historical record), rather than deleting it.*
