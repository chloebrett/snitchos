# snemu interactive harness — live console I/O for the fidelity audit

**Status: SHIPPED + validated.** The fidelity audit now drives each scenario
against a *live* snemu machine (stepped on demand) instead of a pre-captured
frame buffer, with a modelled UART RX so `send_input` reaches the guest. This
unlocked the interactive scenarios (console echo + the Stitch REPL family) that
batch replay structurally could not.

## Why batch replay couldn't do it

Interactive scenarios do: **wait for a telemetry marker → `send_input` → wait for
a result frame**, where the result frames *depend on* the injected input. A batch
capture is a single fixed-budget run with no injection, so the post-injection
frames never exist. The input has to be fed *during* the run.

## What shipped

- **snemu UART RX model** (`snemu/src/uart.rs`): RBR pops received bytes, LSR bit
  0 (DR) signals data-ready — the bits the kernel's `read_byte` polls. The RX
  queue is a `RefCell<VecDeque>` so a side-effecting MMIO *read* stays behind the
  bus's `&self` read path (no `&mut` ripple through fetch/load). `Bus`/`Machine`
  expose `push_console_input`.
- **Live `View`** (`xtask/src/itest/harness.rs`): a `View` gains an optional
  `LiveSnemu` (a `Machine` + decode cursor + step budget). `wait_for` steps the
  machine until the next frame (incremental `try_decode_frame`), `send_input`
  pushes to the UART RX, `wait_for_log` scans `machine.uart_output()`. All 108
  scenario bodies stay `fn(&mut View)` — no scenario changes. A live view is
  `batch`, so a spent budget is a clean end (a miss for `wait_for`, a clean window
  for `assert_absent`).
- **Audit is live per-scenario** (`snemu_audit.rs`): each scenario loads its own
  machine (decode cache on) and drives it. A passing scenario short-circuits at
  its last marker; only a failing one runs the full budget — so this is *faster*
  than batch for the passing majority. Added `--only <substr>` to target scenarios.

## Validated

- `console-echo-round-trips` — PASS in 0.7 s (inject `snitch\n` → guest
  `ConsoleRead` → `Log` echo).
- Stitch REPL family — **9/10 PASS** (reads-a-line, print, hold-lists-caps,
  view-reads-a-file, cross-pipe, grant-revoke, hold-endpoint-name, fs-nested, +
  telemetry). The one fail (`stitch-fs-loads-and-runs`) is a *real* assertion
  ("primes didn't compute"), not an input error.
- `shell-view-command-revokes-cap` — fails on real behaviour (`viewer.bytes_read =
  0`), not missing input.

So of the 11 previously input-blocked scenarios, ~9 now pass; the remaining 2 are
genuine behaviour/timing gaps to investigate, not harness limits.

## Foundation for later

The live per-scenario model is also the basis for the **parallel snemu pool**
(each host thread owns a live `Machine`) — deterministic, no QEMU process/socket,
more stable than QEMU under load.

## Result: 99/108 (from 74/108 at the start of the fidelity push)

Full live audit + the default step budget raised to 400M (recovers
`sched-yield-round-trips`, which needs the bigger budget).

## Remaining fidelity gaps (not this harness) — all scheduler/clock, deterministic

Confirmed *not* budget or harness (each runs its full budget and fails
deterministically):

- **`shell-view-command-revokes-cap`** — the input works (shell parses `view
  bin/spawnee`, spawns the viewer, revokes); `viewer.bytes_read = 0` because under
  snemu's deterministic round-robin the shell's **revoke wins the race** against
  the viewer's first read. QEMU's timing lets the viewer read ≥1 byte first. The
  scenario is over-specified for one interleaving.
- **`workload-cooperative-baseline`** — `samples_consumed` never reaches 200; the
  cooperative producer/consumer gets a different interleaving under snemu's
  schedule (fails at 400M in 16 s, i.e. ran the budget).
- **`priorities-ordered-but-fair`** — preemption granularity.
- **`heartbeat-cadence`** — instret clock ≠ wall-clock (fundamental).
- **`frame-allocator-oom` / `heap-oom` / `spawn-reclaims-×2`** — OOM leak/heartbeat
  dynamics key off "instructions per timer tick"; didn't flip at 400M.
- **`stitch-fs-loads-and-runs`** — "primes didn't compute" (a real behaviour gap,
  not input).

These are the *interesting* residue: nearly all trace back to snemu's
instruction-count clock and deterministic round-robin interleaving differing from
QEMU's wall-clock timing — the same determinism that makes the oracle valuable.
Not quick fidelity wins; a good investigation/post, or "won't-fix under snemu."
