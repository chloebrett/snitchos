# Plan: B6 — real multi-hart topology (drop the `{0,1}` / `1 - hart_id` assumption)

**Branch**: main (repo works directly on main per CLAUDE.md; user commits)
**Status**: COMPLETE — all steps (1, 2, 3, 4, 5, 6a, 6b) shipped. Gate green:
`cargo xtask test` + `itest` 121/121 + `itest --scramble` 121/121.

## Key finding (6b): a real release-build miscompile, caught by snemu

The 4-hart bring-up hung, and snemu pinned it: `hart_start` for logical hart 2
received `start_addr = 0` (hart 1 got the correct `0x80200032`), so hart 2 executed
from PC 0 and faulted `Bus(OutOfRange { addr: 0 })`. Root cause: `entry_pa`
(`va_to_pa(_secondary_start)`), hoisted **out** of the bring-up loop as a
loop-invariant, read back **0** on the 2nd iteration under the release optimizer —
the same address-materialization hazard as the `tp`-truncation bug
(`plans/v0.4-memory-findings.md`). Neither a side-effecting `lla` nor `black_box`
on the hoisted value fixed it; **computing `entry_pa` fresh inside the loop** (with
the non-pure `lla`) did. This bug would bite on QEMU/hardware too — snemu found a
real kernel bug, exactly the kind of multi-hart fidelity the port needs.

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

Two hidden deps found while reading `secondary.rs` pushed the multi-secondary
*machinery* out of 6a into 6b (where the 4-hart test exercises it):
(1) `SECONDARY_STACK` is a **single shared** 16 KiB stack the secondary runs on for
life → >1 secondary needs **per-hart stacks**; (2) `SECONDARY_READY` means "fully
up" (init + trap vector + timer, `secondary.rs:187`), *later* than the
`SMP_ONLINE_HARTS` bit (set in `percpu::init`, before trap setup) — and the
cross-hart probe (`main.rs:483`) needs the target's trap vector up, so the barrier
must be "fully up." Hence `SECONDARY_READY` → **bitmap** (not a reuse of
`SMP_ONLINE_HARTS`).

### Step 6a: Kernel wiring — enumerate → assign → fill map; delete `1 - hart_id`

**Acceptance criteria**: `kmain` fills `LOGICAL_TO_MHARTID[0..N]` via
`assign_logical(enumerate_harts(&dtb, …), boot_mhartid, …)` (early, before the `let
_ = dtb` drop at `main.rs:451`), computing `num_harts`. Both `1 - hart_id` /
`1u64 - boot_mhartid` sites gone; `secondary_mhartid` read from
`LOGICAL_TO_MHARTID[1]`. **Single-secondary bringup unchanged** but guarded by `if
num_harts >= 2` (single-hart boot — board M1 — starts no phantom secondary). Still
exactly one secondary at 2 harts → behavior-preserving.
**RED**: none new — behavior-preserving at 2 harts, guarded by the existing SMP
itests (`smp-secondary-hart-boots`, `smp-spans-carry-hart-id`,
`smp-producer-consumer-correctness`). (The `num_harts < 2` guard is board-only-
exercised — no 1-cpu DTB — but trivially correct.)
**GREEN**: enumerate/assign/fill; delete the arithmetic; add the guard.
**Done when**: full gate green at 2 harts; `1 - hart_id` gone.

### Step 6b: multi-secondary machinery + snemu 4-cpu DTB + the 4-hart scenario

**Acceptance criteria**:
- **Machinery**: `SECONDARY_STACK` → `SECONDARY_STACKS[MAX_HARTS]` (per-hart,
  **indexed by logical id** — slot 0 = boot hart is unused, traded for index=hartid
  clarity; "secondary" kept to match the module's `SECONDARY_*` family);
  `prepare_for_secondary(logical_id)` picks the stack; `SECONDARY_READY: AtomicBool`
  → `AtomicU64` bitmap (secondary sets its bit after full setup); `kmain` **loops**
  logical `1..num_harts` — `prepare(i)` → `hart_start(LOGICAL_TO_MHARTID[i], …, i)` →
  wait bit `i`. Sequential (the per-hart stack is latched in `secondary.S` before the
  next `prepare`).
- **DTB + scenario**: a checked-in `snemu/virt-smp4.dtb` (QEMU `-machine virt -smp 4
  -machine dumpdtb=…`; documented one-liner) with 4 `cpu@N` nodes; the snemu path
  selects it + `hart_count=4` for the new scenario. Exactly one **4-hart** itest
  asserts `HartRegister` for logical 0–3 with distinct mhartids + continued
  heartbeat, **under snemu**.
**RED**: the 4-hart scenario — fails until the loop starts harts 2–3 *and* the 4-cpu
DTB is presented.
**GREEN**: per-hart stacks + bitmap barrier + loop; regen/commit the DTB; thread
`hart_count=4`/DTB selection; verify `hart_start`/IPI/percpu for hartids 2–3.
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
