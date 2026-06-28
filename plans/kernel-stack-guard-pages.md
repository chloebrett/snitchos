# Kernel stack guard pages (fault-on-overflow)

**Status:** **Tier A SHIPPED (2026-06-26).** **Tier B IN PROGRESS (2026-06-28)** —
guard pages, fault-on-overflow. Increment chain below. (Teardown reclaim landed in
v0.12, so the stack-lifecycle hook in `reap_task` already exists.)

## Tier B increment chain (TDD, each RED→GREEN)

1. **kernel-core `unmap`** — walk to the existing 4 KiB leaf, clear it (`Pte::INVALID`),
   return the old PA so the caller frees the frame. `NotMapped` if absent/huge.
   Host-tested via the `PtMem` mock, mirroring `map`/`remap`. *Foundation.*
2. **kernel-core stack-window bookkeeping** — slot↔VA math (guard page below
   `STACK_SIZE` mapped pages) + a slot allocator (alloc/free, recycle freed slots).
   Pure, host-tested.
3. **kernel `mmu::unmap`** — `KernelPtMem` wrapper + local `sfence` + cross-hart
   `shootdown` (the leaf is being invalidated).
4. **kernel `KernelStack`** — alloc slot + frames, `mmu::map` the stack pages (guard
   left unmapped), sentinel-fill; `Drop` unmaps + frees frames + releases the slot.
   Replace `Box<Stack>` in `Task`; rewire `spawn_on_with_arg` + `reap_task`. Tier A
   canary/high-water read the mapped VA range.
5. **trap handler** — a kernel (S-mode) load/store page fault with `stval` in a guard
   region → snitch a **named** `Log` then panic (the observable Tier B proof).
6. **workload + itest** — controlled overflow → guard fault → assert the named Log.

**Decisions:** boot stack (task 0) stays unguarded *in this work* (future iteration
below); slot stride = 5 pages (1 guard + 4 stack), no padding; window at root PTE
257 (`0xffffffc0_40000000`, immediately above the heap's full 1 GiB slot).

## Future iteration — guard the boot stack (task 0)

The boot stack lives in the kernel image `.bss`, set up in `entry.S` before the
MMU/heap exist, and the kernel image is mapped with **2 MiB huge-page leaves**
(`kernel/src/mem/mmu.rs` `map_higher_kernel`, `PAGE_2MIB` stride). So neither the
Tier A canary nor the Tier B window covers it today — a boot-stack overflow is
silent. Two levels, both deferred (decided 2026-06-28):

- **Level 1 — detection (cheap, no MMU): a boot-stack canary.** Give the boot stack
  a linker-symbol bottom, sentinel-fill its lowest bytes early, and check it in the
  heartbeat's `check_stack_canaries` → a *named* panic on overflow instead of silent
  corruption. Small Tier-A-style extension; reuses `kernel_core::stack::canary_intact`.
- **Level 2 — prevention (real fault-on-overflow): harder.** A 4 KiB guard page below
  the boot stack is blocked by the 2 MiB huge leaf — `unmap` refuses a huge leaf, and
  clearing the 2 MiB leaf unmaps too much. Needs *either* a huge-page **split** at the
  boot-stack region (break the 2 MiB leaf into 4 KiB leaves, then `unmap` one) *or*
  **relocating task 0** onto a window stack early in boot (can't relocate a running
  stack trivially — would `switch` task 0 to a fresh window stack once the window is
  up). Independent of the per-task guard pages.

---

(Original deferral note, retained for context:) Tier B was deferred
The specific v0.11 bug is already prevention-fixed (heap-direct
`Box::<Stack>::new_zeroed()` at `kernel/src/sched/mod.rs`), so Tier B is not urgent
— Tier A is the cheap insurance; Tier B is the real fault-on-overflow fix.
Motivated by the v0.11 spawn-with-caps
stack overflow: `Box::new(Stack::new_zeroed())` built a 16 KiB stack temporary on
the spawner's own 16 KiB kernel stack (deep userspace-`Spawn` syscall path),
overflowing into adjacent heap/task memory. It manifested as *unrelated* crashes
(null-deref in `prepare_switch` one run, alloc-error the next) — the classic
overflow signature: the corruption surfaces at the victim, not the cause. Took
multiple runs + instrumentation to localize. See memory
`feedback-corruption-crash-signature`.

## Problem

Kernel stacks are `Box<Stack>` where `Stack = [u8; 16384]`, allocated in the
linear-mapped heap (`kernel/src/sched/mod.rs`). There is **no guard** below a
stack — an overflow silently writes into whatever heap object sits beneath it.
Symptoms appear far from the cause and vary per run, so diagnosis is slow.

## Goal

An overflow should **fault at the overflowing store** (exact PC), not corrupt a
neighbour. Plus cheap always-on detection so an overflow is named, not guessed.

## Options (cheapest → strongest)

### Tier A — stack canary + high-water gauge ✅ SHIPPED

> **Landed.** Pure checks in host-tested `kernel_core::stack` (`canary_intact`,
> `high_water_bytes`, `SENTINEL`, `CANARY_BYTES`; 6 tests, mutants 8/8 caught).
> Each `Box<Stack>` is sentinel-filled at spawn. **Detection has two paths**, both
> via `report_stack_overflow` (snitch + panic): the *prompt* one — `prepare_switch`
> checks the outgoing task's canary on every switch (runs on its possibly-damaged
> stack) — and the *safe backstop* — `check_stack_canaries` on the heartbeat (runs
> on the heartbeat's healthy stack, catches a task that hasn't switched out). On
> breach the kernel **snitches an observable `Log`** ("kernel stack overflow:
> task N (name) …") *then panics* — the failure is on the telemetry wire, named,
> not just UART. Heartbeat also emits `snitchos.task.<name>.stack_high_water_bytes`
> (a `NO_EMITTER` gauge). (Task 0 / boot stack has no canary — Tier B covers it.)
>
> **Verified end-to-end:** `workload=stack-canary` (`storms::stack_canary` +
> `sched::clobber_current_stack_canary`, itest-workloads only) has a kernel task
> controllably clobber its own canary; itest `stack-overflow-detected` asserts the
> snitched `Log` names it (deterministic — controlled clobber, intact stack, no
> corruption roulette). Plus `task-stack-high-water` for the gauge. Full suite
> **75/0** (no false-positive detection), 20/20 on `--repeat 10`, clippy clean.

### Tier A — stack canary + high-water gauge (cheap, ~afternoon)
- Write a sentinel (e.g. `0xC0DE…`) in the bottom N bytes of each `Box<Stack>` at
  creation. Check it on every context switch (`prepare_switch` already walks the
  outgoing task) and on the heartbeat; a clobbered canary → panic "task X
  overflowed its stack" — *names the task*, after the fact but unambiguous.
- Emit a `snitchos.task.<name>.stack_bytes_used` gauge (sp vs stack bounds at
  trap entry / switch). Would have shown the spawner near 16 KiB before it blew.
- Detection only (not prevention); no MMU work. Good first step.

### Tier B — guard pages (the real fix, Linux `VMAP_STACK` style)
- Allocate kernel stacks in a **dedicated VA window** (like the heap window) via
  `mmu::map`, leaving an **unmapped page below each stack**. An overflow store
  hits the hole → immediate page fault at the overflow site.
- Requires: a stack VA allocator (window + per-stack slot with a guard gap);
  stacks no longer plain `Box<Stack>` in the linear map → a `KernelStack` type
  owning its mapped VA region; teardown unmaps on task exit (ties into the
  deferred Exit/teardown reclaim work).
- Composes with existing MMU machinery (`mmu::map`/`remap`, the heap window
  precedent). The proper long-term answer.

## Prevention (do regardless, trivial)
- **Never `Box::new(BigValue)`** — it materializes the value on the caller's
  stack first. Provide `Stack::boxed_zeroed()` = `Box::<Stack>::new_zeroed().assume_init()`
  and use it everywhere; add a grep/clippy guard against `Box::new(Stack::`.
  (The v0.11 fix already switched `spawn_on_with_arg` to the heap-direct form.)

## Sequencing
Tier A first (cheap insurance, names overflows immediately). Tier B when the
Exit/teardown reclaim milestone lands (shared stack-lifecycle machinery), or
sooner if another overflow bites. Prevention now.
