# Kernel stack guard pages (fault-on-overflow)

**Status:** Plan / not started (2026-06-21). Motivated by the v0.11 spawn-with-caps
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
