# Plan: B6 — real multi-hart topology (drop the `{0,1}` / `1 - hart_id` assumption)

**Branch**: main (repo works directly on main per CLAUDE.md; user commits)
**Status**: Active

## Context

VF2 ground truth (measured, 2026-07-23): 4× SiFive U74 (physical harts 1–4) + 1×
S7 monitor (physical hart 0, `status="disabled"`). OpenSBI hands us a boot hartid
that is **arbitrary in 1..4, never 0**. Today the kernel hardwires exactly two
harts with ids `{0,1}`:

- `MAX_HARTS = 2` (`kernel/src/smp/percpu.rs:80`).
- `1 - hart_id` "the other hart" arithmetic (`main.rs:138-139`, `460-461`).
- Single-secondary bringup (`main.rs:453-476` starts one hart, waits for one
  `SECONDARY_READY`).

**Correction to the port plan's B6:** the boot hart is *not* at risk of an OOB at
`percpu::init` — `kmain` calls `percpu::init(0)` (hardcoded logical 0,
`main.rs:132`), and `current_hartid()` returns the *logical* id. So single-hart M1
first-light is already topology-safe on a non-zero boot hartid. B6's real bite is
at **SMP bringup (M3)**: `secondary_mhartid = 1u64 - boot_mhartid` **underflows**
on VF2 (`1 - 2`), so `sbi::hart_start` gets a garbage mhartid and the `assert!`
panics. This plan does the full M3 generalization.

## Goal

Bring up all usable U74 harts on any boot-hartid topology by enumerating `/cpus`
from the DTB and assigning dense logical ids (boot → 0, other `status=okay` harts →
1..N), with no `{0,1}` / `1 - hart_id` assumption anywhere.

## Design (the load-bearing decisions)

1. **Dense logical ids, already the codebase's model.** `current_hartid()` →
   logical; `LOGICAL_TO_MHARTID[logical] → mhartid` is the reverse map. Keep it.
2. **Filter on DTB `status`, not on `id == 0`.** One code path for both platforms:
   JH7110's S7 is hart 0 `disabled` → filtered; QEMU harts are all `okay` → kept.
   Boot hart is always logical 0 regardless of its mhartid.
3. **`MAX_HARTS = 4` is capacity** (the 4 U74s); actual count is runtime from the
   DTB. Bring up `min(enumerated_usable, MAX_HARTS)`.
4. **Selection is pure → host-tested in `kernel-boot`.** Input: `&[HartInfo {
   mhartid, usable }]` + `boot_mhartid`. Output: the logical→mhartid assignment
   (boot first, then usable harts in mhartid order, unusable skipped). `kernel/dtb.rs`
   supplies the raw list as thin, untested glue (like `uart_addr`).

## Acceptance Criteria

- [ ] Given a hart list with an unusable hart 0 (S7) and usable harts 1–4 and a
      boot mhartid of e.g. 2, the selection logic maps boot→logical 0, and the
      other usable harts→logical 1,2,3 (mhartid order), skipping hart 0.
- [ ] Given QEMU's `{0,1}` both-usable list with boot mhartid 1, selection maps
      logical 0→mhartid 1, logical 1→mhartid 0 (the current `1 - hart_id` result),
      proving no regression of the two-hart case.
- [ ] `current_hartid()` returns the logical id that `percpu::init(k)` was called
      with, for any `k < MAX_HARTS` (per-slot `hart_id` no longer depends on a
      static literal).
- [ ] With `MAX_HARTS = 4`, the whole gate (`cargo xtask test && itest && itest
      --scramble`) stays green under the existing `-smp 2` config.
- [ ] The itest harness takes a per-scenario `-smp` count; most scenarios run at
      the cheap default (1 or 2), SMP/cross-hart scenarios keep 2, and exactly one
      new scenario exercises **4 harts**.
- [ ] The 4-hart scenario brings up 4 harts: `HartRegister` frames for logical 0–3
      with the real mhartids, no panic, heartbeat continues.
- [ ] No `1 - hart_id` / `1u64 - boot_mhartid` remains in the tree.

## Steps

Every step: RED → GREEN → MUTATE → KILL → REFACTOR, gate green at each stop.
Work directly on main; **present + wait for commit approval** after each step
(user commits). Confirm each step's acceptance criteria before writing code.

### Step 1: Pure hart-selection logic in `kernel-boot`

**Acceptance criteria**: A pure fn (e.g. `kernel_boot::harts::assign_logical`)
takes `&[HartInfo { mhartid, usable }]` + `boot_mhartid` and returns the ordered
logical→mhartid vec: boot mhartid at index 0, then the other `usable` harts in
ascending mhartid order, unusable harts omitted; capped at `MAX_HARTS`. Covers the
VF2 case (boot=2, S7 hart 0 unusable, U74s 1–4) and the QEMU `{0,1}` case (matches
`1 - hart_id`).
**RED**: table test in `kernel-boot` asserting the mapping for both cases + a
boot-hart-marked-unusable guard + an over-capacity truncation case.
**GREEN**: implement the ordering/filter/cap.
**MUTATE / KILL / REFACTOR**: standard.
**Done when**: criteria 1, 2 met; host tests green.

### Step 2: `init(hartid)` sets `PerHartData.hart_id`; generalize the length-2 statics

**Acceptance criteria**: `PER_HART_DATA`, `LOGICAL_TO_MHARTID`, and the trap tick
statics (`TICK_COUNT`/`TICK_PENDING`/`LAST_IRQ_DURATION`) are `[const { … };
MAX_HARTS]` forms; `percpu::init(k)` writes `hart_id = k` so `current_hartid()`
returns `k` after init regardless of the static default. `MAX_HARTS` still 2. Gate
green.
**RED**: test that `current_hartid()` == the value passed to `init` (needs a small
host-testable seam over the `hart_id` set/read, or an itest asserting logical ids —
decide the seam at CONFIRM time; the `tp`/asm parts stay kernel-only).
**GREEN**: add the field write to `init`; convert the literals.
**Done when**: criterion 3 met; gate green; no behavior change at `MAX_HARTS=2`.

### Step 3: Bump `MAX_HARTS` 2 → 4 (capacity only)

**Acceptance criteria**: `MAX_HARTS = 4`; all `; MAX_HARTS` arrays size to 4;
slots 2–3 idle under `-smp 2`; gate green. No bringup change yet (still single
secondary via the soon-to-be-removed arithmetic — acceptable intermediate).
**RED**: none new (capacity change); rely on the gate + a compile-time `MAX_HARTS
>= boot-hart-count` assert if useful.
**GREEN**: change the const; fix any newly-exposed hardcoded `2`.
**Done when**: criterion 4 met.

### Step 4: DTB hart enumeration glue in `kernel/dtb.rs`

**Acceptance criteria**: `dtb::enumerate_harts(&Fdt) -> Vec<HartInfo>` (or a
fixed-capacity array, no alloc pre-heap — decide at CONFIRM) walks `/cpus`,
reads each `cpu@N`'s `reg` (mhartid) and `status`, marking `usable = status ==
"okay"` (absent status ⇒ okay). Thin glue, matched against the real board DTB
later; unit-tested only where the parse is non-trivial.
**RED**: if host-testable with a DTB fixture, assert the JH7110-shaped parse;
else keep glue minimal and cover via Step 5's boot smoke.
**GREEN**: implement the walk.
**Done when**: enumeration returns the expected shape on QEMU's DTB.

Investigation refined the original Steps 5–6 into 5 / 6a / 6b. The snemu hart
count is a single `const HART_COUNT: usize = 2` (`xtask-snemu/src/snemu_diff.rs:27`)
feeding every `load_machine`; the DTB is the fixed 2-cpu `snemu/virt.dtb`. "Most
itests at 1 or 2" is read as **keep the 2-hart default** (no 1-cpu-DTB churn); we
only *add* a 4-hart capability.

### Step 5: Thread a per-scenario hart count through the snemu path

**Acceptance criteria**: `load_workload_machine` (and the other `load_machine`
callers in `snemu_diff.rs`) take a `hart_count` parameter instead of the global
`HART_COUNT` const; default stays 2 so every scenario is unchanged. Gate green.
**RED**: none new — a pure parameter-threading refactor; the gate is the guard.
**GREEN**: replace the const with a plumbed argument, default 2.
**Done when**: gate green; no behavior change.

### Step 6a: Kernel wiring — enumerate → assign → multi-secondary bringup loop

**Acceptance criteria**: `kmain` fills `LOGICAL_TO_MHARTID` via
`assign_logical(enumerate_harts(&dtb, …), boot_mhartid, …)`, then loops logical
`1..N` starting each secondary (real mhartid + logical id), waiting for each. No
`1 - hart_id` / `1u64 - boot_mhartid` remains. DTB enumerated before the `let _ =
dtb` drop (`main.rs:451`). **Still 2 harts** (DTB lists 2), so this is
behavior-preserving — the generalized loop brings up exactly one secondary, same as
before. `SECONDARY_READY` single-flag → per-hart ready (reuse the `SMP_ONLINE_HARTS`
bitmap as the barrier).
**RED**: none new — behavior-preserving refactor of bringup, guarded by the
existing SMP itests (`smp-secondary-hart-boots`, `smp-spans-carry-hart-id`,
`smp-producer-consumer-correctness`).
**GREEN**: implement the enumerate/assign/loop; delete the arithmetic.
**Done when**: full gate green at 2 harts; `1 - hart_id` gone.

### Step 6b: snemu 4-cpu DTB + the 4-hart scenario (the payoff)

**Acceptance criteria**: a checked-in `snemu/virt-smp4.dtb` (QEMU
`-machine virt -smp 4 -machine dumpdtb=…`; a documented one-liner) advertising 4
`cpu@N` nodes; the snemu path selects it + `hart_count=4` for the new scenario.
Exactly one **4-hart** itest scenario asserts `HartRegister` for logical 0–3 with
distinct mhartids + continued heartbeat, **under snemu**.
**RED**: the 4-hart scenario — fails until both the 4-cpu DTB is presented and the
Step-6a bringup loop starts harts 2–3.
**GREEN**: regenerate + commit the DTB; thread `hart_count=4`/DTB selection; verify
`hart_start`/IPI/percpu for hartids 2–3.
**Done when**: 4-hart scenario green under snemu; 2-hart gate still green.

## Risks / open questions

- **snemu 4-cpu DTB (Step 6b).** snemu's DTB is the fixed 2-cpu `snemu/virt.dtb`;
  `Machine::new` already takes any hart_count. Plan: check in a QEMU-dumped
  `virt-smp4.dtb` and select it for the 4-hart scenario. QEMU 11.0.1 is on PATH, so
  the dump is a one-liner. (There are two `virt.dtb` copies — `snemu/`, `dtb/`;
  the snemu path reads `snemu/virt.dtb`.) Verify `hart_start`/IPI/percpu for hartids
  2–3 when the scenario first runs.
- **`SECONDARY_READY` is a single flag → per-hart barrier.** Decided (Step 6a):
  reuse the `SMP_ONLINE_HARTS` bitmap — after starting logical `1..N`, spin until
  all their bits are set, rather than one boolean.
- **Board DTB `status` spelling.** Assumed `status="okay"` / `"disabled"` — matches
  the `is_usable` predicate (Step 4, done). Verify against the VF2 DTS (user can
  `dtc`/dump `${fdtcontroladdr}` at the U-Boot prompt); it's the one board-specific
  bit and is isolated to `is_usable`, which already handles the `"ok"`/NUL variants.
- **Pre-heap alloc — resolved.** `enumerate_harts`/`assign_logical` fill
  caller-provided slices; no alloc, so enumeration order vs heap init is a non-issue.

## Pre-PR Quality Gate

1. Mutation testing on `kernel-boot::harts` (the pure logic).
2. Refactoring assessment.
3. `cargo xtask clippy` + `cargo xtask test && itest && itest --scramble`.

---
*On completion move to `plans/legacy/` via `git mv` (per CLAUDE.md), not delete.
Run `cargo xtask links` after the move.*
