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

## NEXT TASK — parallel / multi-core snemu pool

The live per-scenario model is the basis for a **parallel snemu pool**: run the
~108-scenario audit across host threads, each owning its own live `Machine`.
snemu is in-process, deterministic, and has no QEMU process/socket — so there's
no cross-run CPU thrash or port contention (the very things that make QEMU flake
under `--repeat`). More stable than QEMU under load, and the throughput win is
real: the audit is ~355s single-threaded but ~90% CPU-bound on a handful of
compute-heavy scenarios, so spreading it across ~8 cores should hit **~44s wall
— beating QEMU's 57s single run** (and its ~570s `--repeat 10` flake gate).

Concretely:
- `snemu_audit::run` loops scenarios **sequentially** today
  (`for s in selected { … View::live(machine) … }`). Parallelize it — a thread
  pool (or `rayon`) over `selected`, collecting `(name, Outcome)` results.
- Per-scenario state is already isolated (each gets its own `Machine`), so
  there's no shared mutable emulator state to guard.
- Share the read-only `kernel`/`dtb` bytes across threads (`Arc<[u8]>`).
- Buffer each scenario's stderr/console so parallel output doesn't interleave;
  print the report in scenario order at the end.
- Cap concurrency at ~cpu-count; the big-budget scenarios (OOM/Stitch @2.5–3B)
  dominate wall-clock, so schedule longest-first if possible.

### Speed baseline (full 108-scenario suite, for measuring the pool against)
- QEMU: **57s wall / 158s CPU** (~2.8 cores, parallel), 108/108.
- snemu: **~355s wall**, single-threaded, 106/108. Slow tail = the interpreter
  (M5/M6 JIT target) + the honest cost of running compute to completion.

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
