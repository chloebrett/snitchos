# Kernel stack hardening — exception stack → canary retirement → boot-stack guard

**Status:** In progress (2026-06-28). Follow-on to the shipped per-task guard pages
([`plans/kernel-stack-guard-pages.md`](kernel-stack-guard-pages.md), Tier A + B).
Completes the two limitations that work documented.

## Why

Per-task guard pages (Tier B) make an overflow **fault at the exact store** instead
of silently corrupting a neighbour. But two gaps remain:

1. **Deep-overflow reporting double-faults.** In-kernel traps reuse the *current*
   kernel stack (`sscratch == 0` convention in `trap.S`), so when the guard fault
   fires the trap handler tries to build its frame on the just-overflowed stack and
   faults again. Today the Tier-A **canary** (checked from the heartbeat's healthy
   stack) is the clean-report backstop for exactly this case — so the canary is
   *not* redundant yet, only because we lack an exception stack.
2. **Boot stack (task 0) unguarded** — it's in `entry.S` `.bss`, inside the
   kernel image's **2 MiB huge-leaf** mapping, so there's no 4 KiB granularity to
   poke a guard hole.

The **per-hart exception stack** is the keystone: it makes the guard fault report
cleanly (fixing #1), which then makes the canary genuinely redundant *and* makes the
boot-stack guard page cheap (its clean report falls out for free).

## Phase 1 — per-hart exception stack (the keystone) — ✅ SHIPPED (2026-06-28)

**Done.** `PerHartData.exc_stack_top` (offset 24, `const`-asserted) set by
`percpu::init` from a per-hart `EXCEPTION_STACKS` static (16 KiB each). `trap.S`
from-kernel path parks `t0` in `sscratch`, loads `exc_stack_top` off `tp`, builds
the frame on the exception stack with the original ksp at offset 8 (the shared exit
`ld sp, 8(sp)` returns onto it), then heals `sscratch = 0`; from-user path unchanged;
a `beqz` pre-init fallback uses the current stack. Proven by the new
`deep-overflow-reports-cleanly` itest (`stack_overflow_deep` recurses into the guard
→ clean named `Log`, which would double-fault/hang without the exception stack).
Full suite **91/0**; SMP correctness, deep-overflow, mutex-storm all **10/10** on
`--repeat`. Original design notes below.



Run the trap handler on a separate, known-good per-hart stack when a trap is taken
**from S-mode** (SPP=1). Safe because in-kernel traps never context-switch (v0.8
preemption gates on SPP=User; timer/IPI-in-kernel just handle + return; faults are
the bug case) — they always `sret` back to the interrupted PC with the original sp
restored. So a shared per-hart stack works; only from-*user* traps still use the
per-task kernel stack (via the existing `sscratch` swap — syscalls need it).

**Design:**
- `PerHartData` (`#[repr(C, align(64))]`, `tp`-relative) gains an `exc_stack_top:
  usize` field at a known offset; `percpu::init` sets it to this hart's exception
  stack top. A static `EXCEPTION_STACKS: [ExcStack; MAX_HARTS]` (16 KiB each) backs it.
- `trap.S` from-kernel path (the `sp == 0` branch after the entry swap): park a
  scratch reg in `sscratch`, `ld` `exc_stack_top` off `tp`, build the frame on the
  exception stack, store the **original ksp** at frame offset 8, restore the scratch,
  heal `sscratch = 0`. The existing exit (`ld sp, 8(sp)`) already restores the caller
  sp from offset 8 → returns to the original kernel stack, no exit change needed.
  From-user path unchanged.
- A const `EXC_STACK_TOP_OFFSET` shared between Rust (`PerHartData`) and the asm; a
  `const_assert` / `offset_of!` guard against drift.

**Risk:** this is the trap entry core — every trap goes through it. Verify with the
FULL itest suite + heavy `--repeat`, both harts (SMP scenarios), and a new
**deep-overflow** itest (real recursion to the guard → clean named report — the
capability the exception stack adds; without it this test hangs/double-faults).

## Phase 2 — retire the per-task canary panic (keep the gauge)

Once the guard fault reports cleanly on its own, the canary-breach panic is
redundant. Remove: the `prepare_switch` canary check, `check_stack_canaries`
(heartbeat), `clobber_current_stack_canary`, the `stack-canary` workload + the
`stack-overflow-detected` itest, and `kernel_core::stack::canary_intact` /
`CANARY_BYTES` / sentinel-fill-for-canary. **Keep** `high_water_bytes` + the
`snitchos.task.<name>.stack_high_water_bytes` gauge — it's independent proactive
telemetry (the trend), not superseded by the binary guard page.

## Phase 3 — boot-stack guard page

- **Huge-leaf split**: `kernel_core::mmu::split_huge_leaf(root, va, mem)` — allocate
  an L0 table, fill 512 4 KiB leaves replicating the 2 MiB leaf's PA+perms, replace
  the huge leaf with a branch. Host-tested via the `PtMem` mock. Kernel wrapper +
  shootdown.
- **Linker**: page-align the boot stack with a reserved guard page below
  (`__boot_stack_guard` / `__boot_stack` / `__boot_stack_top` symbols).
- At boot, split the 2 MiB leaf covering the boot stack, then `mmu::unmap` the guard
  page. Task 0 now faults on overflow; the exception stack (Phase 1) reports it.
- itest: a boot-stack overflow → clean named report.

## Sequencing

Phase 1 first (keystone — also immediately upgrades the shipped per-task guard pages
from "faults, maybe double-faults on report" to "always reports cleanly"). Then
Phase 2 (cleanup enabled by Phase 1). Then Phase 3 (cheap once Phase 1 exists).
