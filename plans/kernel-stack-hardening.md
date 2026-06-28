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

## Phase 2 — retire the per-task canary panic (keep the gauge) — ✅ SHIPPED (2026-06-28)

**Done.** Removed: the `prepare_switch` canary check (+ the `overflowed` tuple
plumbing), `report_stack_overflow`, `check_stack_canaries` + its heartbeat call,
`clobber_current_stack_canary`, `KernelStack::{canary_intact,clobber_canary}`, the
`stack-canary` workload (`WorkloadKind::StackCanary` + storm body + dispatch +
parse + host test) and the `stack-overflow-detected` itest, and
`kernel_core::stack::{canary_intact, CANARY_BYTES}` + their tests. **Kept**
`SENTINEL` + `fill_sentinel` (the high-water scan needs the sentinel fill),
`high_water_bytes`, and the `stack_high_water_bytes` gauge (verified by
`task-stack-high-water`). Guard pages (Tier B) + the exception stack are now the
sole overflow-detection mechanism. Full suite 90/0, clippy clean.

## Phase 3 — boot-stack guard page — ✅ SHIPPED (2026-06-28)

**Done.** `kernel_core::mmu::split_huge_leaf(root, va, mem)` (host-tested, 2 tests)
breaks the 2 MiB kernel-image leaf into 512 4 KiB leaves preserving the mapping;
kernel `mmu::split_huge_leaf` wraps it. `linker.ld` page-aligns a `__boot_stack_guard`
page below `__stack_bottom`. `mmu::guard_boot_stack()` (called in `kmain` after the
frame allocator is up) splits the leaf covering the guard then `unmap`s it. The trap
handler's S-mode-fault arm checks the boot guard page range (alongside the kstack
window) → `sched::report_boot_stack_guard_fault` (lock-free named `Log` + panic).
Proven by `boot-stack-guard-fault-detected` (`workload=boot-stack-guard`,
`touch_boot_stack_guard` stores into the guard → named Log). 10/10 on `--repeat`.

## Milestone complete

All three phases shipped 2026-06-28. Overflow story is now uniform: per-task **and**
boot stacks fault on a guard page; the per-hart exception stack reports cleanly
(deep overflows included); the canary panic is retired; the high-water gauge stays.
Full itest suite 92/0. (Sequencing was Phase 1 keystone → Phase 2 cleanup → Phase 3,
as planned.)
