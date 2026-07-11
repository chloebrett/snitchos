# Scheduler O(1) task lookup + auto-reap of ownerless tasks

**Motivation.** `cargo xtask snemu-profile --workload spawn-storm` showed
`kernel::sched::prepare_switch` is **~66%** of that workload's instret. Root cause:
`prepare_switch` scans the whole task table every switch
(`for task in sched.tasks.iter_mut()`, O(tasks)), and the table **accumulates
`Exited` kernel tasks that are never reaped** — spawn-storm's 200 fire-and-forget
`spawn_on(1, body)`/`exit_now()` tasks all linger (a documented leak:
`Box<Task>` + 16 KiB stack each). So switches are O(200) over ~199 dead entries.

Two independent fixes; both are real kernel improvements, not benchmark-gaming.

## (A) O(1) task lookup — `TaskDirectory`

The Scheduler looks up tasks by id via linear scan at several sites
(`prepare_switch`, `wake`, `task_arg`, `reap_task`, `set_ready`). Replace with an
id→slot directory maintained alongside `tasks: Vec<Box<Task>>`.

- **`kernel_core::sched::TaskDirectory`** (pure, host-tested): `BTreeMap<u32, usize>`
  mapping `TaskId → index into tasks`. API mirrors the `Vec` ops that mutate it:
  - `insert(id, slot)` on `tasks.push` (slot = old len).
  - `slot_of(id) -> Option<usize>` for lookups.
  - `swap_remove(removed_id, moved_id)` — mirrors `Vec::swap_remove`: drop
    `removed_id`, repoint `moved_id` to the removed slot. Returns the slot to hand
    `Vec::swap_remove`.
- Scheduler holds a `TaskDirectory`; every `tasks.push` / `swap_remove` updates it
  in lockstep (behind the same `SCHEDULER` lock, so no new race).
- `prepare_switch`'s scan becomes two `slot_of` lookups (current, next) → direct
  `tasks[slot]` access. The single-pass mutation (cpu_time, state, runs) splits into
  two indexed accesses (current != next always, so no aliasing).

**O(1) assertion.** Expose `SCHED_LOOKUP_PROBES` — count of task **entries examined**
to resolve a lookup. Old scan: += N. New directory: += 1 (the map returns a slot;
we touch exactly the target). Emit it as `snitchos.sched.lookup_probes_per_switch`.
Unit-test the directory directly (host); itest asserts the per-switch probe count is
**constant** as live-task count grows (the frame-oom O(1) precedent — a killed mutant,
not a vibe). Revert to the scan → the itest fails, every behavioural test still passes.

## (B) Auto-reap ownerless (kernel) exited tasks

Ownership of an `Exited` task's reclamation:
- **Userspace** `handle_exit` calls `note_exit` (registers a REAP zombie) → a parent
  `Wait`s → `reap_task`. These are **owned**; leave them (parent reaps; a
  never-`Wait`ing parent is a separate, deliberate case — see scenario below).
- **Kernel** tasks call `exit_now()` directly with **no** REAP registration → nothing
  ever reaps them. These are **ownerless** → auto-reap.

Make the ownership explicit at the call site (no cross-lock REAP check):
- `exit_now()` (kernel, fire-and-forget) arms auto-reap.
- Userspace exits via `exit_now_owned()` (no auto-reap; REAP owns it).

**Deferred free** (can't free your own stack mid-switch): a per-hart
`REAP_PENDING: PerCpu<Option<TaskId>>`. The exiting task, after `prepare_switch`
picks its successor, records itself as pending and `switch_into`s away. The **next**
`prepare_switch` on that hart (now on another task's stack) reaps the pending at the
top of its locked section — `swap_remove` from `tasks` (frees `Box<Task>` + `Stack`)
and `TaskDirectory::swap_remove`. At most one unreaped exited task per hart at a time.

Guard: never reap the current or next task; the pending is always a *previously*
switched-out exiting task, so its context pointers are no longer in use.

## Scenario: 200 genuinely-live, unreapable tasks

(B) shrinks spawn-storm's table, so it no longer stresses (A). Add a workload whose
tasks **stay live** (so the directory genuinely holds N and only (A) saves the
switch): `workload=live-tasks` — spawn K (=200) tasks that each block on a
notification / loop-yield forever, held by a supervisor, so the table has K live
entries. Drive M yields and assert `lookup_probes_per_switch` stays constant as K
scales — the O(1) proof under a large *live* table, independent of the leak fix.

## Order

1. `kernel_core::sched::TaskDirectory` — TDD (insert / slot_of / swap_remove fixup).
2. Wire it into Scheduler; replace scan sites; add the probe counter. (A) done.
3. `exit_now` auto-reap + `exit_now_owned` split; deferred pending-reap. (B) done.
4. `live-tasks` workload + `sched-task-lookup-is-o1` scenario + probe-constant assert.
5. Re-profile spawn-storm; confirm `prepare_switch` share collapses.

## Outcome (both landed)

**(A) O(1) lookup — done, mutation-proven.** `TaskDirectory` (unit-tested) replaced
the scans; `prepare_switch` went **66% → 19%** of spawn-storm instret (profiler).
`sched-task-lookup-is-o1` (workload `live-tasks`, 200 loop-yield tasks) asserts
probes/switch ≤ 4; observed **2.0**. Killed mutant: reverting `task_mut` to a scan
→ **79.9** probes/switch, that scenario fails while behavioural tests pass.

**(B) reap — done via the stack-slot pool (not per-reap unmap).** The first cut
reaped by dropping `KernelStack`, whose `teardown` does `mmu::unmap` + a cross-hart
TLB **shootdown** per page — run from the heartbeat sweep it *wedged* cross-hart
(`rpc/ipc-telemetry`, `heartbeat-cadence`, `kernel-heap-metrics` hung to budget). Fix:
`MAPPED_STACK_POOL` — `Drop` returns the slot **mapped** to a pool (no unmap, no
shootdown); `new` reuses a pooled slot (no map). Reclamation is now a cheap slot
push, so the heartbeat's `reap_ownerless_exited` sweep (ownerless kernel tasks only;
userspace stays owned by `Wait`) can't wedge. Mapped-stack memory is bounded by
*peak concurrent* tasks, reused. Bonus: the userspace `reap_task` path is now
shootdown-free too. After (B), `TaskDirectory::slot_of` drops out of the spawn-storm
profile entirely (table no longer accumulates zombies). Release **110/110 ~4.3s**;
debug 108/110 (only the two supervision-blocked scenarios).
